//! Redis/Valkey sink.
//!
//! The sink writes one of five per-flow modes (kv / kv-delete / list /
//! stream / pubsub) over a `deadpool-redis` connection pool. The per-mode
//! columns (`key` / `value` / `ttl`) have **known canonical types**, so
//! `describe_schema` returns them (the superset) and the validation
//! matrix type-checks the mapped columns at config time. The per-mode
//! required/optional *subset* — which the matrix can't express — is
//! enforced by `resolve_layout` in `validate_access` (and defensively in
//! `build_context`, so flows with `validation.inserts = false` still
//! fail before any write).
//!
//! ## Delivery semantics
//!
//! At-least-once **send to Redis** — not consumer delivery; Redis itself
//! may drop data (eviction, pubsub with no subscriber, restart without
//! persistence). `kv` / `kv-delete` are idempotent under retry; `list`
//! (RPUSH), `stream` (XADD) and `pubsub` (PUBLISH) may **duplicate** on a
//! batch retry. The runner owns the cursor and re-delivers a failed
//! batch, so a partially-applied pipeline can replay.
//!
//! ## Conflict blocks
//!
//! `[flow.<name>.conflict]` is rejected — redis writes are always
//! last-write-wins upserts (SET) or unconditional appends; there is no
//! on-conflict arbitration to configure.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::info;

use air_elt_commons_redis::RedisPool;
use air_elt_core::error::{ConfigError, RuntimeError, RuntimeResult};
use air_elt_core::model::{
    Batch, ConfigWriteSpec, Field, Schema, SchemaProvider, SinkCtx, WriteReport, WriteSpec,
};
use air_elt_core::traits::Sink;
use air_elt_core::types::data_type::DataType;

use crate::commands::{BuiltCommand, build_access_probe, build_command};
use crate::flow_options::{COL_KEY, COL_TTL, COL_VALUE, ColumnLayout, RedisFlowOptions, RedisMode};

/// Per-flow write context: the resolved mode, column layout, and the
/// connector's column schema. Built once in `build_context`; immutable
/// afterwards.
pub(crate) struct RedisSinkCtx {
    pub(crate) mode: RedisMode,
    pub(crate) layout: ColumnLayout,
    schema: Schema,
}

impl SinkCtx for RedisSinkCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        Some(self)
    }
}

impl SchemaProvider for RedisSinkCtx {
    fn schema(&self) -> &Schema {
        &self.schema
    }
}

/// The `key` column: a `Text` suffix concatenated onto the flow prefix.
/// `nullable` is mode-dependent — required (non-null) for kv/kv-delete/
/// stream, optional (nullable, falls back to the bare prefix) for
/// list/pubsub.
fn key_field(nullable: bool) -> Field {
    Field {
        name: COL_KEY.to_string(),
        data_type: DataType::Text { size: None },
        nullable,
    }
}

/// The `value` column: the JSON payload. Always required where a mode
/// has it.
fn value_field() -> Field {
    Field {
        name: COL_VALUE.to_string(),
        data_type: DataType::Json,
        nullable: false,
    }
}

/// The `ttl` column: an optional `Interval` expiry. Only kv carries it,
/// always optional.
fn ttl_field() -> Field {
    Field {
        name: COL_TTL.to_string(),
        data_type: DataType::Interval,
        nullable: true,
    }
}

/// The mode's exact column schema, with canonical types and per-mode
/// nullability. Unlike a mode-blind superset, this lets the validation
/// matrix check both the *type* and the *nullability* of each mapped
/// column at config time — `Int → key`, `Text → ttl`, or a nullable
/// source feeding a required `key`/`value` all fail at validate rather
/// than at runtime. The required/optional column *set* is still enforced
/// by [`RedisMode::resolve_layout`] (the matrix can't express "this
/// column must be present").
fn per_mode_schema(mode: RedisMode) -> Schema {
    let fields = match mode {
        RedisMode::Kv => vec![key_field(false), value_field(), ttl_field()],
        RedisMode::KvDelete => vec![key_field(false)],
        RedisMode::List => vec![value_field(), key_field(true)],
        RedisMode::Stream => vec![key_field(false), value_field()],
        RedisMode::Pubsub => vec![value_field(), key_field(true)],
    };
    Schema::new(fields)
}

pub struct RedisSink {
    pool: RedisPool,
}

impl RedisSink {
    /// Wrap an already-built pool. Construction (and the eager first
    /// dial) happens in the factory so a dead redis surfaces at
    /// component-build time.
    pub fn new(pool: RedisPool) -> Self {
        Self { pool }
    }

    /// Redis upserts are always last-write-wins; an on-conflict directive
    /// is meaningless. Reject it with a clear hint instead of silently
    /// ignoring it.
    fn reject_conflict_block(spec: &WriteSpec) -> RuntimeResult<()> {
        if spec.conflict.is_some() {
            return Err(ConfigError::ConflictNotSupported {
                sink: "redis".into(),
                hint: "redis writes are always last-write-wins (SET) or unconditional appends \
                       (RPUSH / XADD / PUBLISH) — remove the [flow.<name>.conflict] block."
                    .into(),
            }
            .into());
        }
        Ok(())
    }

    /// Resolve the per-flow write mode from a `sink_options` table. An
    /// empty table (bare `sink = "redis"`) defaults to `kv`. Takes the
    /// raw table — not a spec — so both `WriteSpec` and `ConfigWriteSpec`
    /// (which carry it under the same field) can call it.
    fn resolve_mode(sink_options: &toml::Table) -> RuntimeResult<RedisMode> {
        let opts: RedisFlowOptions =
            sink_options
                .clone()
                .try_into()
                .map_err(|e| ConfigError::Invalid {
                    reason: format!("redis sink: invalid per-flow options: {e}"),
                })?;
        Ok(opts.mode)
    }
}

#[async_trait]
impl Sink for RedisSink {
    // NOT schemaless: the per-mode columns have known canonical types
    // (`describe_schema` returns them), so the validation matrix
    // type-checks the mapped columns at config time. The per-mode
    // required/optional contract — which the matrix can't express — is
    // enforced by `resolve_layout`.

    /// `kv-delete` issues real `DEL`s, so the sink can delete.
    fn supports_deletes(&self) -> bool {
        true
    }

    /// The redis sink reads the per-flow `mode` from the developed
    /// `sink = { name, mode }` form, so it accepts per-flow options.
    fn accepts_flow_options(&self) -> bool {
        true
    }

    /// Pool size (connection count). The assemble concurrency semaphore is
    /// sized to this: a flow-tick checks out one connection for its
    /// whole-batch pipeline, so the pool's concurrent-pipeline capacity is
    /// exactly the connection count.
    fn max_connections(&self) -> u32 {
        self.pool.max_connections()
    }

    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        Self::reject_conflict_block(spec)?;
        let mode = Self::resolve_mode(&spec.sink_options)?;
        // Enforce the per-mode required/optional column contract (the
        // matrix only checks the types of mapped columns, not which
        // columns a mode requires).
        mode.resolve_layout(&spec.columns)?;
        // Real write probe: run the mode's own command against a
        // self-cleaning sentinel key. Unlike a bare PING this proves the
        // sink can actually write (catches a read-only replica, a missing
        // write ACL, or a disabled command) and leaves no trace.
        let probe = build_access_probe(mode, &spec.table);
        let mut conn = self.pool.acquire().await.map_err(RuntimeError::backend)?;
        conn.query_pipeline::<()>(&probe)
            .await
            .map_err(RuntimeError::backend)?;
        info!(table = %spec.table, mode = mode.as_str(), "redis sink access validated");
        Ok(())
    }

    async fn describe_schema(&self, spec: &ConfigWriteSpec) -> RuntimeResult<Schema> {
        // The mode lives in `sink_options`, available on the config-time
        // write descriptor — so we can return the precise per-mode schema
        // (exact columns + nullability), not a mode-blind superset. The
        // matrix then checks types AND nullability of the mapped columns.
        let mode = Self::resolve_mode(&spec.sink_options)?;
        Ok(per_mode_schema(mode))
    }

    async fn build_context(&self, spec: &WriteSpec) -> RuntimeResult<Arc<dyn SinkCtx>> {
        // Re-check the conflict block and column contract here so a flow
        // with `validation.inserts = false` (which skips `validate_access`)
        // still surfaces a typed error before any write hits the wire.
        Self::reject_conflict_block(spec)?;
        let mode = Self::resolve_mode(&spec.sink_options)?;
        let layout = mode.resolve_layout(&spec.columns)?;
        info!(table = %spec.table, mode = mode.as_str(), "redis sink build_context");
        Ok(Arc::new(RedisSinkCtx {
            mode,
            layout,
            schema: per_mode_schema(mode),
        }))
    }

    async fn write_batch(
        &self,
        spec: &WriteSpec,
        ctx: &Arc<dyn SinkCtx>,
        batch: Batch,
        dry_run: bool,
    ) -> RuntimeResult<WriteReport> {
        if batch.rows.is_empty() {
            return Ok(WriteReport::default());
        }
        let rctx = ctx.downcast_ref_to::<RedisSinkCtx>()?;
        let to = spec.table.as_str();

        // Lower every row, type-checking it as we go. Building the
        // pipeline validates the full batch even on the dry-run path.
        // Reserve up-front: at most one command per row.
        let mut pipe = redis::Pipeline::with_capacity(batch.rows.len());
        let mut report = WriteReport::default();
        for row in &batch.rows {
            match build_command(rctx.mode, &rctx.layout, to, row)? {
                BuiltCommand::Upsert(cmd) => {
                    pipe.add_command(cmd);
                    report.upserts += 1;
                }
                BuiltCommand::Delete(cmd) => {
                    pipe.add_command(cmd);
                    report.deletes += 1;
                }
                BuiltCommand::Skipped => {
                    report.skipped += 1;
                }
            }
        }

        if dry_run {
            // Dry-run validates row shapes/types (the build above) but
            // does NOT round-trip to the server — unlike the SQL sinks'
            // never-matching probe. Redis has no schema/permission
            // surface to probe, and a true no-op write isn't possible
            // without side effects, so building the pipeline is the
            // dry-run.
            return Ok(WriteReport::default());
        }

        // Single round-trip for the whole batch over one pooled
        // connection. A server error on any command in the pipeline
        // surfaces as `Err` from `query_async` regardless of the `()`
        // target (redis-rs collects every failing command's error into one
        // pipeline error); it bubbles to the runner, which re-delivers the
        // batch. A broken connection is recycled by deadpool on next
        // checkout.
        if report.upserts + report.deletes > 0 {
            let mut conn = self.pool.acquire().await.map_err(RuntimeError::backend)?;
            conn.query_pipeline::<()>(&pipe)
                .await
                .map_err(RuntimeError::backend)?;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};

    fn spec(conflict: Option<ConflictConfig>, mode: &str, columns: &[&str]) -> WriteSpec {
        let mut sink_options = toml::Table::new();
        if !mode.is_empty() {
            sink_options.insert("mode".to_string(), toml::Value::String(mode.to_string()));
        }
        WriteSpec {
            columns: columns.iter().map(|c| c.to_string()).collect(),
            table: "p:".to_string(),
            conflict,
            sink_options,
        }
    }

    #[test]
    fn conflict_block_is_rejected() {
        let s = spec(
            Some(ConflictConfig {
                key: vec!["key".to_string()],
                strategy: ConflictStrategy::Overwrite,
            }),
            "kv",
            &["key", "value"],
        );
        let err = RedisSink::reject_conflict_block(&s).expect_err("conflict must be rejected");
        match err {
            RuntimeError::Config(ConfigError::ConflictNotSupported { sink, .. }) => {
                assert_eq!(sink, "redis");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn no_conflict_block_is_accepted() {
        RedisSink::reject_conflict_block(&spec(None, "kv", &["key", "value"])).expect("ok");
    }

    #[test]
    fn resolve_mode_defaults_to_kv_when_empty() {
        let mode = RedisSink::resolve_mode(&spec(None, "", &[]).sink_options).expect("ok");
        assert_eq!(mode, RedisMode::Kv);
    }

    #[test]
    fn resolve_mode_reads_developed_mode() {
        let mode = RedisSink::resolve_mode(&spec(None, "list", &[]).sink_options).expect("ok");
        assert_eq!(mode, RedisMode::List);
    }

    #[test]
    fn resolve_mode_rejects_unknown_option() {
        let mut s = spec(None, "kv", &[]);
        s.sink_options
            .insert("bogus".to_string(), toml::Value::Integer(1));
        assert!(
            RedisSink::resolve_mode(&s.sink_options).is_err(),
            "deny_unknown_fields must reject an unknown per-flow option"
        );
    }

    #[test]
    fn per_mode_schema_kv_has_all_three_with_optional_ttl() {
        // kv is the only mode with a ttl; key and value are required.
        let schema = per_mode_schema(RedisMode::Kv);
        let key = schema.find(COL_KEY).expect("key field");
        assert!(matches!(key.data_type, DataType::Text { .. }));
        assert!(!key.nullable, "kv key is required");
        let value = schema.find(COL_VALUE).expect("value field");
        assert_eq!(value.data_type, DataType::Json);
        assert!(!value.nullable, "kv value is required");
        let ttl = schema.find(COL_TTL).expect("ttl field");
        assert_eq!(ttl.data_type, DataType::Interval);
        assert!(ttl.nullable, "kv ttl is optional");
    }

    #[test]
    fn per_mode_schema_list_has_no_ttl_and_optional_key() {
        // list carries value (required) + key (optional), and never a ttl.
        let schema = per_mode_schema(RedisMode::List);
        assert!(schema.find(COL_TTL).is_none(), "list has no ttl column");
        assert!(
            !schema.find(COL_VALUE).expect("value field").nullable,
            "list value is required"
        );
        assert!(
            schema.find(COL_KEY).expect("key field").nullable,
            "list key is optional"
        );
    }
}
