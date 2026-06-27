use std::sync::Arc;

use async_trait::async_trait;

use crate::error::RuntimeResult;
use crate::model::{
    Batch, ConfigWriteSpec, CursorState, ReadSpec, Schema, SinkCtx, SourceCtx, WriteReport,
    WriteSpec,
};
use crate::types::DataType;

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Source: Send + Sync {
    /// The connector instance's config-given name (`[[sources]] name = ...`).
    /// Used by the validation pipeline to group flows that share a
    /// source pool: each source name becomes one async worker so the
    /// shared pool isn't fanned out under contention.
    fn name(&self) -> &str;

    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()>;
    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema>;
    async fn build_context(&self, spec: &ReadSpec) -> RuntimeResult<Arc<dyn SourceCtx>>;
    /// Read a batch. `ctx` is shared via `Arc`; we pass it by reference
    /// because most adapters only need to read from it. Adapters that
    /// need owned access (e.g. to move into a `tokio::spawn` for
    /// cancel-safety shielding) call `Arc::clone(ctx)` at that exact
    /// site — there is no point bumping the refcount at the trait
    /// boundary.
    async fn read_batch<'a>(
        &self,
        spec: &ReadSpec,
        ctx: &Arc<dyn SourceCtx>,
        cursor: Option<&'a CursorState>,
    ) -> RuntimeResult<Batch>;

    /// Probe read for sampling-validation. Returns rows in the same
    /// pre-Transform shape `read_batch` produces — the runner applies
    /// the `Transform` program afterwards, so the sampling probe
    /// exercises the same projection / body-folding / conversion path
    /// the live tick does. The default delegates to `read_batch` with
    /// `spec.limit` overridden by `n`; CDC sources override because
    /// their `read_batch` blocks on the change stream.
    async fn sample(
        &self,
        spec: &ReadSpec,
        ctx: &Arc<dyn SourceCtx>,
        n: usize,
    ) -> RuntimeResult<Batch> {
        let mut sample_spec = spec.clone();
        sample_spec.limit = n;
        self.read_batch(&sample_spec, ctx, None).await
    }

    /// `true` when this source can emit `Row { op: Delete }`. Pull-based
    /// connectors (postgres / mysql / mongodb cursor) only ever emit
    /// `Upsert`, so they keep the default `false`. CDC connectors
    /// (`mongo-cdc`) override to `true`. The validation pipeline uses
    /// this flag to decide whether to also pre-flight `Sink::validate_delete_access`
    /// — without it, a missing DELETE privilege on the sink only
    /// surfaces at runtime on the first delete batch.
    fn emits_deletes(&self) -> bool {
        false
    }

    /// `true` iff this source's schema is sampled / unauthoritative —
    /// notably MongoDB, where collections accept any BSON shape.
    /// Mirrors `Sink::schemaless()`.
    ///
    /// Two consequences for schemaless sources:
    ///
    /// 1. **Wildcard expansion**: when **both** source and sink are
    ///    schemaless, `*` falls back to raw passthrough rather than
    ///    column enumeration.
    /// 2. **Transform compile**: the per-column conversion plan keys
    ///    off the **sink** column type only (the authoritative side).
    ///    The compiler emits dynamic-source `TransformOp::Convert` ops
    ///    (with `ColumnConversionPlan.source = None`); the runtime
    ///    resolves the source `DataType` per cell from the actual
    ///    `Value` variant.
    ///    This stops a sample-derived "source type" hypothesis from
    ///    blowing up the runtime on legitimate cross-doc shape drift
    ///    (e.g. 99 docs with `Int32` + one `Int64`).
    ///
    /// SQL connectors keep the default `false`: `information_schema`
    /// is an authoritative DDL contract, and a row with a different
    /// type from what was declared is a database integrity violation,
    /// correctly surfaced at the Convert layer.
    fn schemaless(&self) -> bool {
        false
    }

    /// The canonical [`DataType`] of the body payload this source
    /// attaches to `Row.body` when `ReadSpec.needs_body` is `true`.
    /// Drives the per-body-target conversion plan's source `DataType`,
    /// the matrix check on body sink columns, and the Transform
    /// compiler's object-shape assertion. Must satisfy
    /// `body_data_type().is_object()` for body folds to compile.
    /// Default: `DataType::Json` (relational sources). Mongo overrides
    /// to `DataType::Custom(BsonObjectType)`.
    fn body_data_type(&self) -> crate::types::DataType {
        crate::types::DataType::Json
    }

    /// Upper bound on concurrent uses of this source's underlying
    /// connection pool. The validation pipeline (and any other
    /// shared-pool caller) builds a `tokio::sync::Semaphore` per
    /// component sized to this value and gates every probe / call
    /// through it, so a config with thousands of flows referencing the
    /// same source does not stampede the server's accept queue.
    ///
    /// Default is unbounded (`u32::MAX`). Connectors that own a
    /// connection pool MUST override this to mirror their pool's
    /// `max-connections` so the semaphore matches the pool's actual
    /// permit budget.
    fn max_connections(&self) -> u32 {
        u32::MAX
    }
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Sink: Send + Sync {
    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()>;
    /// Pre-flight the DELETE path. Default returns `Ok(())` so backends
    /// with no DELETE distinction (or that already cover it inside
    /// `validate_access`) opt out for free. Called by the validation
    /// pipeline only when the source `emits_deletes()` *and* the flow
    /// declares a `[flow.x.conflict]` block — both are required for
    /// the runner to ever invoke the DELETE path.
    async fn validate_delete_access(&self, _spec: &WriteSpec) -> RuntimeResult<()> {
        Ok(())
    }
    /// Describe the sink's column schema. Takes the config-time write
    /// descriptor (table + per-flow `sink_options`), NOT just the table
    /// name, so a sink whose schema depends on per-flow options (the redis
    /// sink, whose columns are fixed by `mode`) can return the exact
    /// schema. Runs before mapping expansion, so it must not depend on the
    /// (not-yet-known) mapped columns.
    async fn describe_schema(&self, spec: &ConfigWriteSpec) -> RuntimeResult<Schema>;
    async fn build_context(&self, spec: &WriteSpec) -> RuntimeResult<Arc<dyn SinkCtx>>;
    async fn write_batch(
        &self,
        spec: &WriteSpec,
        ctx: &Arc<dyn SinkCtx>,
        batch: Batch,
        dry_run: bool,
    ) -> RuntimeResult<WriteReport>;

    /// `true` for sinks that have no authoritative column schema —
    /// notably MongoDB, where collections accept any BSON shape.
    /// When true, validation skips the type-compatibility matrix
    /// check for this sink (the matrix has nothing to compare
    /// against) and instead treats every mapped column as accepting
    /// the source's declared type.
    fn schemaless(&self) -> bool {
        false
    }

    /// `false` for append-only sinks whose engine has no cheap
    /// `DELETE`/`UPDATE` (notably ClickHouse MergeTree). When this is
    /// `false`:
    /// * the runner ships the WHOLE batch (delete rows included) to
    ///   `write_batch`. The sink is responsible for dropping
    ///   `Row { op: Delete }` rows itself and returning a `WriteReport`
    ///   whose `rows_written` counts the upserts only. The cursor
    ///   still advances on the strength of that report — even an
    ///   all-delete batch must call through (it commits `rows_written: 0`
    ///   and lets the flow move past the range);
    /// * the validation pipeline skips `validate_delete_access` for
    ///   this sink, even if the source emits deletes;
    /// * CDC sources may omit the otherwise-mandatory `[flow.<x>.conflict]`
    ///   block (append-only ingest: every CDC event becomes a plain
    ///   INSERT, deletes are silently dropped).
    fn supports_deletes(&self) -> bool {
        true
    }

    /// Number of underlying connections (pool size). See
    /// [`Source::max_connections`] for the full rationale. The per-sink
    /// concurrency semaphore is sized directly to this value — one permit
    /// per connection, so it bounds concurrent flow-ticks by the number of
    /// connections the sink's pool can hand out.
    ///
    /// Default is unbounded (`u32::MAX`). Connectors that own a
    /// connection pool MUST override.
    fn max_connections(&self) -> u32 {
        u32::MAX
    }

    /// `true` for sinks that consume per-flow options — the developed
    /// `sink = { name, <options> }` form (today: the redis sink's `mode`).
    /// The validation pipeline rejects the developed form on any sink that
    /// returns `false`, so a stray or misplaced option fails loudly at
    /// assemble instead of being silently dropped. Default `false` keeps
    /// `core` connector-agnostic — capability lives on the trait, not as a
    /// hardcoded kind string in the pipeline.
    fn accepts_flow_options(&self) -> bool {
        false
    }
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Storage: Send + Sync {
    async fn validate_access(&self) -> RuntimeResult<()>;
    async fn migrate(&self) -> RuntimeResult<()>;
    /// Load the persisted cursor for `flow`. `cursor_types` is the
    /// list of expected canonical [`DataType`]s for each cursor field,
    /// in the same order as `ReadSpec.cursor_fields` — resolved by the
    /// caller from the source schema. Storage impls dispatch the JSON
    /// payload through [`DataType::decode_cursor_json`] per field so
    /// `Value::Custom` cursor values (e.g. `MongoObjectIdValue`) can
    /// be reconstructed without a global registry.
    async fn load_cursor(
        &self,
        flow: &str,
        cursor_types: &[DataType],
    ) -> RuntimeResult<Option<CursorState>>;
    async fn save_cursor(
        &self,
        flow: &str,
        state: &CursorState,
        dry_run: bool,
    ) -> RuntimeResult<()>;

    /// CDC resume tokens are conceptually distinct from column-based
    /// cursors — they are opaque per-stream blobs (BSON for Mongo)
    /// keyed by flow name. Stored in their own table / collection.
    /// Default impls error so a misrouted call surfaces clearly.
    async fn load_resume_token(&self, _flow: &str) -> RuntimeResult<Option<serde_json::Value>> {
        Err(crate::error::RuntimeError::Other(
            "this storage backend does not implement CDC resume-token \
             persistence — switch to a backend that does (postgres / mysql / mongodb)"
                .into(),
        ))
    }
    async fn save_resume_token(
        &self,
        _flow: &str,
        _token: &serde_json::Value,
        _dry_run: bool,
    ) -> RuntimeResult<()> {
        Err(crate::error::RuntimeError::Other(
            "this storage backend does not implement CDC resume-token \
             persistence — switch to a backend that does (postgres / mysql / mongodb)"
                .into(),
        ))
    }

    /// Upper bound on concurrent uses of this storage's underlying
    /// connection pool. See [`Source::max_connections`] for the full
    /// rationale — the validation pipeline gates probes through a
    /// per-storage `tokio::sync::Semaphore` sized to this value.
    ///
    /// Default is unbounded (`u32::MAX`). Storage backends that own a
    /// connection pool MUST override.
    fn max_connections(&self) -> u32 {
        u32::MAX
    }
}
