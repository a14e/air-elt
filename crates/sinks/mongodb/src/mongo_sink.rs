//! MongoDB sink connector.
//!
//! Write modes are driven by the flow-level `[flow.<name>.conflict]`
//! block (see `air_elt_core::config::conflict`):
//! - **No conflict block** → plain `insertMany` (default `ordered=true`
//!   — a duplicate-key error aborts the batch).
//! - **`strategy = "ignore"`** → `insertMany(ordered=false)` and we
//!   swallow E11000 duplicate-key write errors. Other write errors
//!   still propagate.
//! - **`strategy = "overwrite"`** — server-version dependent. On
//!   server >=8.0 we use `Client::bulk_write` to ship every row's
//!   `ReplaceOneModel { upsert=true }` in a single round-trip. On
//!   older servers `Client::bulk_write` is unavailable (the cross-
//!   collection admin command is an 8.0 feature), so we fall back to
//!   issuing one `update` command via `run_command` with N
//!   `{ q, u, upsert: true }` entries — also a single round-trip,
//!   honoured by every server since 2.6. `ordered=false` on both
//!   paths so a single bad row doesn't abort the rest. The choice is
//!   decided once at `connect()` (via `commons-mongodb::version::detect`)
//!   and cached in `MongoSink::server_version` + `MongoSinkCtx::server_version`.
//! - **`_id` fast path**: when `conflict.key == ["_id"]` (single key,
//!   exact `_id`) we still use replaceOne but skip the FieldPath
//!   round-trip — `_id` is the primary key, indexed natively, no need
//!   to reach into the document twice.
//!
//! `describe_schema` returns the *mapped* sink schema. Mongo has no
//! authoritative schema, so the validation pipeline builds the sink
//! schema from the source's declared types via `Sink::schemaless()`.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bson::{Bson, Document, doc};
use mongodb::error::ErrorKind;
use mongodb::options::InsertManyOptions;
use mongodb::{Client, Collection};
use tracing::{debug, info, warn};

use air_elt_commons_mongodb::MongoPoolStatsReader;
use air_elt_commons_mongodb::client::{PoolSettings, connect, database_from_url};
use air_elt_commons_mongodb::task::detached;
use air_elt_commons_mongodb::types::BsonObjectType;
use air_elt_commons_mongodb::version::{self, MongoVersion};
use air_elt_commons_mongodb::{bson_value, identifier, path};
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::mapping::FieldPath;
use air_elt_core::model::{Batch, RowOp, Schema, SchemaProvider, SinkCtx, WriteReport, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;

use crate::config::MongoSinkConfig;

/// MongoDB duplicate-key error code; emitted on unique-index violations.
const E11000_DUPLICATE_KEY: i32 = 11_000;

/// Clone is cheap: `Client` is internally `Arc`-wrapped and the other
/// fields are small (`String`, `MongoVersion`). Each trait method
/// clones `self` to move into `tokio::spawn` — see `task::detached`
/// and the cancel-safety rationale on `air_elt_commons_mongodb::client`.
#[derive(Clone)]
pub struct MongoSink {
    client: Client,
    database: String,
    /// Detected once at `connect()` so `write_batch` can branch on
    /// `bulk_write` availability without an extra round-trip per call.
    server_version: MongoVersion,
    /// Per-operation cap; applied as `max_time` / `maxTimeMS` on every
    /// server-side call. Bounds runaway server work after a detach.
    operation_timeout: Duration,
    pool_max_connections: u32,
}

impl MongoSink {
    /// Read-only accessor used by tests to assert the version the
    /// connector saw on the wire matches the server we're targeting.
    pub fn server_version(&self) -> MongoVersion {
        self.server_version
    }

    /// See [`MongoSource::connect`] for the reader lifecycle contract.
    pub async fn connect(
        config: MongoSinkConfig,
        reader: Arc<MongoPoolStatsReader>,
    ) -> RuntimeResult<Self> {
        let database = config
            .database
            .clone()
            .or_else(|| database_from_url(&config.url))
            .ok_or_else(|| {
                RuntimeError::Other(
                    "mongodb sink: `database` not set and url has no path component".into(),
                )
            })?;
        identifier::validate_name(&database).map_err(RuntimeError::from)?;

        let settings = PoolSettings::from_options(
            config.connect_timeout,
            config.acquire_timeout,
            config.idle_timeout,
            None,
            config.operation_timeout,
            config.max_connections,
            config.min_connections,
        )?;
        let operation_timeout = settings.statement;
        let pool_max_connections = settings.max_connections;
        let client = connect(&config.url, settings, reader).await?;
        let server_version = version::detect(&client).await?;
        info!(
            major = server_version.major,
            minor = server_version.minor,
            bulk_write = server_version.supports_bulk_write(),
            "mongodb sink connected"
        );
        Ok(Self {
            client,
            database,
            server_version,
            operation_timeout,
            pool_max_connections,
        })
    }

    fn collection(&self, name: &str) -> RuntimeResult<Collection<Document>> {
        identifier::validate_name(name).map_err(RuntimeError::from)?;
        Ok(self.client.database(&self.database).collection(name))
    }

    fn max_time_ms(&self) -> i64 {
        i64::try_from(self.operation_timeout.as_millis()).unwrap_or(i64::MAX)
    }
}

/// Resolved upsert plan for a single batch. Built once in
/// `build_context` and reused by every `write_batch`.
#[derive(Clone)]
enum UpsertPlan {
    /// No conflict block — plain insertMany.
    None,
    /// `strategy = "ignore"` — insertMany(ordered=false), swallow E11000.
    Ignore,
    /// `strategy = "overwrite"` keyed by mapping indices that resolve
    /// each `conflict.key` entry. The corresponding `FieldPath`s are
    /// duplicated here for cheap filter construction.
    Overwrite { keys: Vec<UpsertKey> },
}

#[derive(Clone)]
struct UpsertKey {
    /// Index into `column_paths` so `write_batch` can read the value
    /// straight off the row without re-parsing.
    column_idx: usize,
    /// Mapped sink path — used as the filter field name.
    path: FieldPath,
}

struct MongoSinkCtx {
    column_paths: Vec<FieldPath>,
    plan: UpsertPlan,
    /// True when the overwrite plan reduces to a single `_id` key —
    /// lets `write_batch` build the filter as `{ "_id": ... }` directly.
    id_fast_path: bool,
    /// Cached at `build_context`-time so `write_batch` doesn't reach
    /// back into the sink to ask "can we bulk_write?".
    server_version: MongoVersion,
    /// Mongo has no authoritative sink schema — the validation pipeline
    /// derives a permissive schema from the source's declared types via
    /// `Sink::schemaless()`. We still expose the field for schema-on-ctx
    /// parity, populating it from `describe_schema` (which today
    /// returns `Schema::default()` — empty). Stays `None` only if a
    /// future implementation makes `describe_schema` fallible.
    pub schema: Option<Schema>,
}

impl SinkCtx for MongoSinkCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        if self.schema.is_some() {
            Some(self)
        } else {
            None
        }
    }
}

impl SchemaProvider for MongoSinkCtx {
    fn schema(&self) -> &Schema {
        // Guarded by `as_schema_provider`: a `None` schema means the
        // caller bypassed the gate and reached us in error.
        self.schema
            .as_ref()
            .expect("schemaless ctx asked for schema — caller skipped as_schema_provider gate")
    }
}

#[async_trait]
impl Sink for MongoSink {
    fn max_connections(&self) -> u32 {
        self.pool_max_connections
    }

    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        let client = self.client.clone();
        let database = self.database.clone();
        let coll = self.collection(&spec.table)?;
        let table = spec.table.clone();
        let max_time_ms = self.max_time_ms();
        detached(async move {
            client
                .database(&database)
                .run_command(doc! { "ping": 1, "maxTimeMS": max_time_ms })
                .await
                .map_err(RuntimeError::backend)?;
            // Probe write privilege via insert+delete of a sentinel doc.
            // mongodb 3.6 does not expose `max_time` on the typed
            // `insert_one`/`delete_one` builders; the server-side cap
            // comes from the connection's `connect_timeout` and the
            // run-time `task::detached` bound. Documented gap.
            let sentinel = doc! {
                "_air_elt_probe": true,
                "_air_elt_probe_at": bson::DateTime::now(),
            };
            let res = coll
                .insert_one(sentinel)
                .await
                .map_err(RuntimeError::backend)?;
            let id = res.inserted_id;
            coll.delete_one(doc! { "_id": id })
                .await
                .map_err(RuntimeError::backend)?;
            info!(collection = %table, "mongodb sink access validated");
            Ok(())
        })
        .await
    }

    async fn validate_delete_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        // Index-backed: every Mongo collection has a mandatory unique
        // `_id` index, so a filter on `_id` resolves via that index even
        // when nothing matches — no COLLSCAN. We use `$in: []` which the
        // server short-circuits to "match nothing" without touching any
        // documents, while still exercising the delete privilege /
        // collection visibility path. `validate_access` above already
        // round-trips an insert + delete sentinel; this method exists
        // for parity with the SQL backends and to log the explicit
        // DELETE-path check.
        let coll = self.collection(&spec.table)?;
        let table = spec.table.clone();
        detached(async move {
            let empty: Vec<Bson> = Vec::new();
            coll.delete_many(doc! { "_id": { "$in": empty } })
                .await
                .map_err(RuntimeError::backend)?;
            info!(collection = %table, "mongodb sink delete access validated");
            Ok(())
        })
        .await
    }

    async fn describe_schema(&self, _table: &str) -> RuntimeResult<Schema> {
        // Mongo collections have no authoritative schema. The runner
        // builds a permissive sink schema from the source's declared
        // types via `Sink::schemaless()`; we return a `Schemaless` schema
        // and let that path drive the matrix check.
        Ok(Schema::schemaless())
    }

    async fn build_context(&self, spec: &WriteSpec) -> RuntimeResult<Arc<dyn SinkCtx>> {
        let column_paths = spec
            .columns
            .iter()
            .map(|s| FieldPath::parse(s).map_err(|e| RuntimeError::Other(e.to_string())))
            .collect::<RuntimeResult<Vec<_>>>()?;

        let (plan, id_fast_path) = match &spec.conflict {
            None => (UpsertPlan::None, false),
            Some(c) => match c.strategy {
                ConflictStrategy::Ignore => (UpsertPlan::Ignore, false),
                ConflictStrategy::Overwrite => {
                    let keys = resolve_keys(c, &column_paths)?;
                    let id_fast_path = keys.len() == 1 && keys[0].path.to_string() == "_id";
                    (UpsertPlan::Overwrite { keys }, id_fast_path)
                }
            },
        };
        // Schema-on-ctx parity. Mongo's `describe_schema` returns
        // an empty `Schema::default()` because the collection has no
        // authoritative shape — keep the field as `Some(empty)` so
        // `as_schema_provider()` does advertise it. (The validation
        // pipeline already takes the schemaless path for this sink and
        // derives the dst schema from the source side.)
        let schema = self.describe_schema(&spec.table).await.ok();
        Ok(Arc::new(MongoSinkCtx {
            column_paths,
            plan,
            id_fast_path,
            server_version: self.server_version,
            schema,
        }))
    }

    fn schemaless(&self) -> bool {
        true
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
        let coll = self.collection(&spec.table)?;
        let me = self.clone();
        let ctx = Arc::clone(ctx);
        let table = spec.table.clone();
        detached(async move {
            let my_ctx = ctx.as_any().downcast_ref::<MongoSinkCtx>().ok_or(
                RuntimeError::ContextMismatch {
                    expected: "MongoSinkCtx",
                },
            )?;
            if dry_run {
                // Dry-run path: build the same docs we would write, then
                // ship them via `replaceOne(filter={$expr:false}, doc, upsert=false)`.
                // The server parses every BSON document (catching schema /
                // identifier / encoding bugs at the wire level) but the
                // never-matching filter combined with `upsert=false` means
                // matchedCount=0, modifiedCount=0, no mutation. Asymmetry to
                // document: collection-level `$jsonSchema` validators run
                // only when a write actually changes a document, so
                // validator-rejection bugs will *not* surface in dry-run —
                // they remain visible only on the real production path.
                me.write_dry_run(my_ctx, &table, &coll, batch.rows).await?;
                return Ok(WriteReport::default());
            }

            // Order matters within a CDC batch: insert(_id=42) followed
            // by delete(_id=42) must apply upsert first; delete-first
            // would let the upsert recreate the row we just removed.
            let mut upserts_written: u64 = 0;
            let mut deletes_written: u64 = 0;
            // Split the owned rows by op so each branch consumes its half
            // by value — `into_columns` and the raw-passthrough fast-path
            // can both move payloads without cloning.
            let mut upsert_rows: Vec<air_elt_core::model::Row> = Vec::new();
            let mut delete_rows: Vec<air_elt_core::model::Row> = Vec::new();
            for row in batch.rows {
                match row.op {
                    RowOp::Upsert => upsert_rows.push(row),
                    RowOp::Delete => delete_rows.push(row),
                }
            }
            if !upsert_rows.is_empty() {
                upserts_written += me
                    .write_upsert_rows(my_ctx, &coll, &table, upsert_rows)
                    .await?;
            }

            if !delete_rows.is_empty() {
                let keys = match &my_ctx.plan {
                    UpsertPlan::Overwrite { keys } => keys.as_slice(),
                    _ => {
                        return Err(RuntimeError::Other(
                            "mongodb sink received Delete row but flow has no \
                             [flow.<x>.conflict] block (or strategy=ignore) — \
                             Delete needs an overwrite key to target documents"
                                .into(),
                        ));
                    }
                };
                deletes_written +=
                    write_delete_many(&coll, delete_rows, keys, my_ctx.id_fast_path).await?;
            }
            Ok(WriteReport {
                upserts: upserts_written,
                deletes: deletes_written,
                skipped: 0,
            })
        })
        .await
    }
}

impl MongoSink {
    /// Dry-run: ship every Upsert row as a `replaceOne` with a filter
    /// that never matches and `upsert=false`. The server parses each
    /// BSON document (so type / identifier / encoding bugs surface) but
    /// no document is mutated. Delete rows are issued as `deleteMany`
    /// with a never-matching filter for the same purpose.
    async fn write_dry_run(
        &self,
        my_ctx: &MongoSinkCtx,
        table: &str,
        coll: &Collection<Document>,
        rows: Vec<air_elt_core::model::Row>,
    ) -> RuntimeResult<()> {
        let mut upsert_rows: Vec<air_elt_core::model::Row> = Vec::new();
        let mut delete_rows: Vec<air_elt_core::model::Row> = Vec::new();
        for row in rows {
            match row.op {
                RowOp::Upsert => upsert_rows.push(row),
                RowOp::Delete => delete_rows.push(row),
            }
        }
        if !upsert_rows.is_empty() {
            let docs = build_docs(my_ctx, upsert_rows)?;
            self.dry_run_replace(table, docs).await?;
        }
        if !delete_rows.is_empty() {
            // Mirror the production filter shape so per-row keys flow
            // through `bson_value::to_bson_owned` (catches wire-encoding
            // bugs in dry-run). Then AND it with `{ $expr: false }` —
            // `$expr: false` short-circuits any AND'ed predicate, so the
            // server matches zero documents and no row is deleted, while
            // still parsing every key BSON encoded above.
            let keys = match &my_ctx.plan {
                UpsertPlan::Overwrite { keys } => keys.as_slice(),
                _ => {
                    return Err(RuntimeError::Other(
                        "mongodb sink received Delete row but flow has no \
                         [flow.<x>.conflict] block (or strategy=ignore) — \
                         Delete needs an overwrite key to target documents"
                            .into(),
                    ));
                }
            };
            let row_count = delete_rows.len();
            let prod_filter = build_delete_filter(delete_rows, keys, my_ctx.id_fast_path)?;
            let filter = doc! { "$and": [prod_filter, doc! { "$expr": false }] };
            debug!(
                rows = row_count,
                "mongodb deleteMany (dry-run, never-matching filter)"
            );
            coll.delete_many(filter)
                .await
                .map_err(RuntimeError::backend)?;
        }
        Ok(())
    }

    async fn dry_run_replace(&self, table: &str, docs: Vec<Document>) -> RuntimeResult<()> {
        debug!(rows = docs.len(), "mongodb dry-run replaceOne never-match");
        if self.server_version.supports_bulk_write() {
            let ns = mongodb::Namespace {
                db: self.database.clone(),
                coll: table.to_string(),
            };
            let mut models: Vec<mongodb::options::WriteModel> = Vec::with_capacity(docs.len());
            for d in docs {
                let model = mongodb::options::ReplaceOneModel::builder()
                    .namespace(ns.clone())
                    .filter(doc! { "$expr": false })
                    .replacement(d)
                    .upsert(false)
                    .build();
                models.push(model.into());
            }
            // `Client::bulk_write` (8.0+) has no per-call `max_time`
            // typed builder in mongodb 3.6 — the corresponding option
            // arrived later. Server still honours `maxTimeMS` if
            // present in the command body, but the typed action does
            // not surface it. Cap stays on the connection-level
            // settings (`PoolSettings::statement`) until the driver
            // exposes it; documented gap.
            self.client
                .bulk_write(models)
                .ordered(false)
                .await
                .map_err(RuntimeError::backend)?;
            return Ok(());
        }
        // Fallback for servers <8.0 — single `update` command via
        // `run_command` carrying N never-matching entries. One
        // round-trip, parses every doc, mutates nothing.
        let mut updates: Vec<Document> = Vec::with_capacity(docs.len());
        for d in docs {
            updates.push(doc! {
                "q": doc! { "$expr": false },
                "u": d,
                "upsert": false,
            });
        }
        let cmd = doc! {
            "update": table,
            "updates": updates,
            "ordered": false,
            "maxTimeMS": self.max_time_ms(),
        };
        let res = self
            .client
            .database(&self.database)
            .run_command(cmd)
            .await
            .map_err(RuntimeError::backend)?;
        // Reuse the same response parser — n will be 0 (no matches),
        // but writeErrors / writeConcernError / ok=0 still surface.
        parse_update_response(&res)?;
        Ok(())
    }

    async fn write_upsert_rows(
        &self,
        my_ctx: &MongoSinkCtx,
        coll: &Collection<Document>,
        table: &str,
        rows: Vec<air_elt_core::model::Row>,
    ) -> RuntimeResult<u64> {
        let docs = build_docs(my_ctx, rows)?;

        let written = match &my_ctx.plan {
            UpsertPlan::None => {
                debug!(rows = docs.len(), "mongodb insertMany");
                let len = docs.len();
                coll.insert_many(docs)
                    .await
                    .map_err(RuntimeError::backend)?;
                len as u64
            }
            UpsertPlan::Ignore => write_insert_ignore(coll, docs).await?,
            UpsertPlan::Overwrite { keys } => {
                if my_ctx.server_version.supports_bulk_write() {
                    write_upsert_bulk(
                        &self.client,
                        &self.database,
                        table,
                        docs,
                        keys,
                        my_ctx.id_fast_path,
                    )
                    .await?
                } else {
                    write_upsert_via_update(
                        &self.client,
                        &self.database,
                        table,
                        docs,
                        keys,
                        my_ctx.id_fast_path,
                        self.max_time_ms(),
                    )
                    .await?
                }
            }
        };
        Ok(written)
    }
}

/// Build mapped Mongo documents from owned upsert rows. Shared
/// between the production write path and the dry-run probe path.
/// Owns the rows so each `Value` is moved into `to_bson_owned` —
/// notably `Value::Json` no longer pays a `serde_json::Value` clone
/// per cell.
///
/// Schemaless-both `["*"]` raw-passthrough rows arrive here as a
/// single-column row whose only value is
/// `Value::Custom(BsonObjectValue(doc))`. `build_docs` recognises
/// that shape and emits `doc` verbatim — no per-field
/// `path::set` traversal. The check is value-shape based so it
/// applies regardless of the synthetic column name (`_root`).
fn build_docs(
    my_ctx: &MongoSinkCtx,
    rows: Vec<air_elt_core::model::Row>,
) -> RuntimeResult<Vec<Document>> {
    let mut docs: Vec<Document> = Vec::with_capacity(rows.len());
    for (row_idx, mut row) in rows.into_iter().enumerate() {
        // `row.values` carries every sink column post-Transform,
        // matching the order of `column_paths` (one entry per
        // `WriteSpec.columns`).
        let column_count = row.values.len();
        if column_count < my_ctx.column_paths.len() {
            return Err(RuntimeError::Other(format!(
                "row produced fewer values ({}) than mapping declared ({})",
                column_count,
                my_ctx.column_paths.len()
            )));
        }
        // Root-document fast path: a single sink column receiving a
        // `Value::Custom(BsonObjectValue)` (the lowering of mongo→mongo
        // `["*"]`). Move the document out without cloning. Ineligible
        // when the conflict block would consume the cell as a key.
        if my_ctx.column_paths.len() == 1
            && row.values.len() == 1
            && matches!(my_ctx.plan, UpsertPlan::None | UpsertPlan::Ignore)
            && is_bson_object_value(&row.values[0])
        {
            let v = row.values.swap_remove(0);
            let bson = bson_value::to_bson_owned(v)?;
            if let Bson::Document(d) = bson {
                docs.push(d);
                continue;
            }
            return Err(RuntimeError::Other(
                "raw passthrough: BsonObjectValue did not decode to a Document".into(),
            ));
        }
        let mut d = Document::new();
        for ((i, p), v) in my_ctx.column_paths.iter().enumerate().zip(row.values) {
            // NULL conflict-key values would leave the filter
            // without a usable identifier — reject explicitly.
            if let UpsertPlan::Overwrite { keys } = &my_ctx.plan {
                if keys.iter().any(|k| k.column_idx == i) && v.is_null() {
                    return Err(RuntimeError::Other(format!(
                        "row {row_idx}: conflict key {:?} cannot be NULL",
                        p.to_string()
                    )));
                }
            }
            let bson = bson_value::to_bson_owned(v)?;
            if matches!(bson, Bson::Null) {
                // Skip NULL writes for non-key fields — Mongo
                // distinguishes missing from explicit null and the
                // pipeline semantics align with "missing".
                continue;
            }
            path::set(&mut d, p, bson);
        }
        docs.push(d);
    }
    Ok(docs)
}

/// `true` when `v` is `Value::Custom(BsonObjectValue)` — the lowered
/// shape of mongo→mongo `["*"]` raw passthrough.
fn is_bson_object_value(v: &Value) -> bool {
    match v {
        Value::Custom(inner) => {
            let dt = inner.dyn_type();
            dt.kind() == BsonObjectType::KIND
        }
        _ => false,
    }
}

/// Build the production-shape `deleteMany` filter for the given rows.
///
/// Single-key fast path (`id_fast_path`) emits `{ _id: { $in: [...] } }`.
/// Compound keys emit `{ $or: [{k1: v1, k2: v2}, ...] }`. Every key
/// value flows through `bson_value::to_bson_owned` so wire-encoding
/// errors surface here rather than at the driver boundary.
///
/// Used by both the production delete path and the dry-run path; the
/// dry-run path additionally wraps the result in
/// `{ $and: [<this filter>, { $expr: false }] }` so the server matches
/// zero documents while still parsing every per-row key value.
fn build_delete_filter(
    rows: Vec<air_elt_core::model::Row>,
    keys: &[UpsertKey],
    id_fast_path: bool,
) -> RuntimeResult<Document> {
    let row_count = rows.len();
    if id_fast_path {
        // keys.len() == 1 && keys[0].path == "_id"
        let id_idx = keys[0].column_idx;
        let mut ids: Vec<Bson> = Vec::with_capacity(row_count);
        for row in rows {
            let mut cols: Vec<Value> = row.values;
            if id_idx >= cols.len() {
                return Err(RuntimeError::Other("delete row has no _id slot".into()));
            }
            let v = cols.swap_remove(id_idx);
            if v.is_null() {
                return Err(RuntimeError::Other(
                    "mongodb delete: _id cannot be NULL".into(),
                ));
            }
            ids.push(bson_value::to_bson_owned(v)?);
        }
        Ok(doc! { "_id": { "$in": ids } })
    } else {
        // Compound key — emit `$or` of per-row equality filters.
        let mut clauses: Vec<Document> = Vec::with_capacity(row_count);
        for row in rows {
            let mut clause = Document::new();
            // Pre-collect once per row so we can move each key value
            // out by index. `Option::take` lets us move values
            // out-of-order without disturbing the surrounding slots.
            let mut cols: Vec<Option<Value>> = row.values.into_iter().map(Some).collect();
            for k in keys {
                let slot = cols
                    .get_mut(k.column_idx)
                    .ok_or_else(|| RuntimeError::Other("delete row missing key slot".into()))?;
                let v = slot
                    .take()
                    .ok_or_else(|| RuntimeError::Other("delete row missing key slot".into()))?;
                if v.is_null() {
                    return Err(RuntimeError::Other(format!(
                        "mongodb delete: key {:?} cannot be NULL",
                        k.path.to_string()
                    )));
                }
                clause.insert(k.path.to_string(), bson_value::to_bson_owned(v)?);
            }
            clauses.push(clause);
        }
        Ok(doc! { "$or": clauses })
    }
}

async fn write_delete_many(
    coll: &Collection<Document>,
    rows: Vec<air_elt_core::model::Row>,
    keys: &[UpsertKey],
    id_fast_path: bool,
) -> RuntimeResult<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let row_count = rows.len();
    let filter = build_delete_filter(rows, keys, id_fast_path)?;
    debug!(rows = row_count, "mongodb deleteMany");
    let res = coll
        .delete_many(filter)
        .await
        .map_err(RuntimeError::backend)?;
    Ok(res.deleted_count)
}

fn resolve_keys(
    conflict: &ConflictConfig,
    column_paths: &[FieldPath],
) -> RuntimeResult<Vec<UpsertKey>> {
    let mut out = Vec::with_capacity(conflict.key.len());
    for k in &conflict.key {
        let idx = column_paths
            .iter()
            .position(|p| p.to_string() == *k)
            .ok_or_else(|| {
                RuntimeError::Other(format!(
                    "mongodb sink: conflict.key {k:?} not found in mapping.to"
                ))
            })?;
        out.push(UpsertKey {
            column_idx: idx,
            path: column_paths[idx].clone(),
        });
    }
    Ok(out)
}

async fn write_insert_ignore(
    coll: &Collection<Document>,
    docs: Vec<Document>,
) -> RuntimeResult<u64> {
    let total = docs.len() as u64;
    let opts = InsertManyOptions::builder().ordered(Some(false)).build();
    debug!(rows = total, "mongodb insertMany ordered=false (ignore)");
    match coll.insert_many(docs).with_options(opts).await {
        Ok(res) => Ok(res.inserted_ids.len() as u64),
        Err(err) => {
            // ordered=false collects per-document errors into an
            // InsertManyError; if every write error is E11000 we treat
            // the duplicates as ignored. The driver's `inserted_ids`
            // field is private, so we infer the inserted count by
            // subtracting the duplicate-error count from the batch
            // size — accurate for the only-dup-keys case.
            if let Some(inserted) = inserted_count_if_only_dup_keys(&err, total) {
                warn!(inserted, "mongodb insertMany ignore: duplicates dropped");
                return Ok(inserted);
            }
            Err(RuntimeError::backend(err))
        }
    }
}

fn inserted_count_if_only_dup_keys(err: &mongodb::error::Error, batch_size: u64) -> Option<u64> {
    if let ErrorKind::InsertMany(info) = &*err.kind {
        let write_errors = info.write_errors.as_ref()?;
        let only_dups = write_errors.iter().all(|e| e.code == E11000_DUPLICATE_KEY);
        if only_dups && info.write_concern_error.is_none() {
            let dups = write_errors.len() as u64;
            return Some(batch_size.saturating_sub(dups));
        }
    }
    None
}

/// Server-side batched upsert for deployments without `Client::bulk_write`
/// (server <8.0). Issues one `update` command via `run_command` carrying
/// N `{ q, u, upsert: true }` entries — a single round-trip, equivalent
/// in semantics to N `replace_one(filter, upsert=true)` calls but
/// without the per-row latency. `ordered=false` so a single failure
/// doesn't abort the rest, mirroring the modern `bulk_write` path.
///
/// `rows_written` is taken from `n` in the response (matched + upserted),
/// matching the `bulk_write` accounting.
async fn write_upsert_via_update(
    client: &Client,
    database: &str,
    collection: &str,
    docs: Vec<Document>,
    keys: &[UpsertKey],
    id_fast_path: bool,
    max_time_ms: i64,
) -> RuntimeResult<u64> {
    debug!(
        rows = docs.len(),
        id_fast_path, "mongodb update command upsert"
    );
    let mut updates: Vec<Document> = Vec::with_capacity(docs.len());
    for d in docs {
        let filter = build_upsert_filter(&d, keys, id_fast_path)?;
        updates.push(doc! {
            "q": filter,
            "u": d,
            "upsert": true,
        });
    }
    let cmd = doc! {
        "update": collection,
        "updates": updates,
        "ordered": false,
        "maxTimeMS": max_time_ms,
    };
    let res = client
        .database(database)
        .run_command(cmd)
        .await
        .map_err(RuntimeError::backend)?;
    parse_update_response(&res)
}

/// Extract `rows_written` from an `update` command reply.
///
/// Surfaces three error shapes that the typed driver helpers
/// (`replace_one`, `Client::bulk_write`) would normally raise on our
/// behalf — `run_command` returns the raw `Document` instead, so we
/// have to inspect them here:
///
/// - **Top-level `ok: 0`** → command-level failure (auth denied,
///   namespace not found, oversized batch, ...). Carries `code` /
///   `errmsg`. Without this check a rejected command would silently
///   report "0 rows written" instead of erroring.
/// - **Non-empty `writeErrors`** → per-document failures.
/// - **`writeConcernError`** → durability failure on an otherwise
///   acknowledged write.
///
/// Numeric parsing: `n` is documented as Int32 on modern servers but
/// older 3.x-era replies sometimes used Double — accept both via
/// `try_from`. Refuse silent precision loss / sign loss.
fn parse_update_response(res: &Document) -> RuntimeResult<u64> {
    let ok = match res.get("ok") {
        Some(Bson::Double(v)) => *v,
        Some(Bson::Int32(v)) => f64::from(*v),
        Some(Bson::Int64(v)) => *v as f64,
        _ => 0.0,
    };
    if ok != 1.0 {
        let code = res.get_i32("code").ok();
        let errmsg = res.get_str("errmsg").unwrap_or("(no errmsg)");
        return Err(RuntimeError::Other(format!(
            "mongodb update command failed: ok={ok} code={code:?} errmsg={errmsg:?}"
        )));
    }
    if let Ok(errors) = res.get_array("writeErrors") {
        if !errors.is_empty() {
            return Err(RuntimeError::Other(format!(
                "mongodb update command write errors: {errors:?}"
            )));
        }
    }
    if let Ok(wce) = res.get_document("writeConcernError") {
        return Err(RuntimeError::Other(format!(
            "mongodb update command write concern error: {wce:?}"
        )));
    }
    let n_i64 = match res.get("n") {
        Some(Bson::Int32(v)) => i64::from(*v),
        Some(Bson::Int64(v)) => *v,
        Some(Bson::Double(v)) => {
            if !v.is_finite() {
                return Err(RuntimeError::Other(format!(
                    "mongodb update response 'n' is non-finite: {v}"
                )));
            }
            // `n` is a counter — bounded by `maxWriteBatchSize` (~10^5),
            // so any plausible value lands well within Int53 precision.
            *v as i64
        }
        other => {
            return Err(RuntimeError::Other(format!(
                "mongodb update response missing or non-numeric 'n': {other:?}"
            )));
        }
    };
    u64::try_from(n_i64.max(0))
        .map_err(|e| RuntimeError::Other(format!("mongodb update response 'n' overflow: {e}")))
}

/// Server-side batched upsert via `Client::bulk_write` (MongoDB 8.0+).
/// Builds one `ReplaceOneModel { upsert: true }` per row and ships
/// them to the server in a single `bulkWrite` command — the whole
/// batch becomes one round-trip instead of N. `ordered=false` so a
/// single bad row doesn't abort the rest.
///
/// `bulk_write` returns `matched + upserted + modified` counts; we
/// report `matched + upserted` as `rows_written` to mirror the
/// `replace_one`-loop semantics ("each row succeeded as either an
/// update or an insert").
async fn write_upsert_bulk(
    client: &Client,
    database: &str,
    collection: &str,
    docs: Vec<Document>,
    keys: &[UpsertKey],
    id_fast_path: bool,
) -> RuntimeResult<u64> {
    debug!(rows = docs.len(), id_fast_path, "mongodb bulk_write upsert");
    let ns = mongodb::Namespace {
        db: database.to_string(),
        coll: collection.to_string(),
    };
    let mut models: Vec<mongodb::options::WriteModel> = Vec::with_capacity(docs.len());
    for d in docs {
        let filter = build_upsert_filter(&d, keys, id_fast_path)?;
        let model = mongodb::options::ReplaceOneModel::builder()
            .namespace(ns.clone())
            .filter(filter)
            .replacement(d)
            .upsert(true)
            .build();
        models.push(model.into());
    }
    let res = client
        .bulk_write(models)
        .ordered(false)
        .await
        .map_err(RuntimeError::backend)?;
    Ok((res.matched_count + res.upserted_count).max(0) as u64)
}

fn build_upsert_filter(
    doc: &Document,
    keys: &[UpsertKey],
    id_fast_path: bool,
) -> RuntimeResult<Document> {
    if id_fast_path {
        // Single key, exact `_id` — read it off the top-level doc and
        // skip the FieldPath traversal.
        let id = doc
            .get("_id")
            .cloned()
            .ok_or_else(|| RuntimeError::Other("upsert key \"_id\" missing in row".into()))?;
        return Ok(doc! { "_id": id });
    }
    let mut filter = Document::new();
    for k in keys {
        let value = path::get(doc, &k.path).cloned().ok_or_else(|| {
            RuntimeError::Other(format!(
                "conflict key {:?} (mapping index {}) is missing in row",
                k.path.to_string(),
                k.column_idx
            ))
        })?;
        filter.insert(k.path.to_string(), value);
    }
    Ok(filter)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bson::doc;

    fn key(idx: usize, path: &str) -> UpsertKey {
        UpsertKey {
            column_idx: idx,
            path: FieldPath::parse(path).unwrap(),
        }
    }

    fn empty_sink_ctx(schema: Option<Schema>) -> MongoSinkCtx {
        MongoSinkCtx {
            column_paths: vec![],
            plan: UpsertPlan::None,
            id_fast_path: false,
            server_version: MongoVersion { major: 8, minor: 0 },
            schema,
        }
    }

    #[test]
    fn ctx_with_schema_advertises_provider() {
        // Schema-on-ctx: the sink ctx must surface its schema to
        // any consumer that goes through `as_schema_provider`. Mongo's
        // `describe_schema` returns an empty Schema; the *presence* of
        // the field — even when empty — is what the wildcard expansion
        // path discriminates on.
        let ctx = empty_sink_ctx(Some(Schema::default()));
        let dyn_ctx: &dyn SinkCtx = &ctx;
        let provider = dyn_ctx
            .as_schema_provider()
            .expect("schema present → provider Some");
        assert_eq!(provider.schema().fields().len(), 0);
    }

    #[test]
    fn ctx_without_schema_returns_none_provider() {
        let ctx = empty_sink_ctx(None);
        let dyn_ctx: &dyn SinkCtx = &ctx;
        assert!(dyn_ctx.as_schema_provider().is_none());
    }

    #[test]
    fn id_fast_path_filter_uses_top_level_id() {
        let d = doc! { "_id": 42_i64, "name": "alice" };
        let f = build_upsert_filter(&d, &[key(0, "_id")], true).unwrap();
        assert_eq!(f, doc! { "_id": 42_i64 });
    }

    #[test]
    fn multi_key_filter_walks_field_paths() {
        let d = doc! {
            "tenant": "acme",
            "addr": { "city": "Berlin" },
        };
        let keys = vec![key(0, "tenant"), key(2, "addr.city")];
        let f = build_upsert_filter(&d, &keys, false).unwrap();
        assert_eq!(f, doc! { "tenant": "acme", "addr.city": "Berlin" });
    }

    #[test]
    fn parse_update_response_reports_n_as_int32() {
        let res = doc! { "ok": 1.0, "n": 5_i32, "nModified": 3_i32 };
        assert_eq!(parse_update_response(&res).unwrap(), 5);
    }

    #[test]
    fn parse_update_response_reports_n_as_int64() {
        let res = doc! { "ok": 1.0, "n": 7_i64, "nModified": 7_i64 };
        assert_eq!(parse_update_response(&res).unwrap(), 7);
    }

    #[test]
    fn parse_update_response_surfaces_write_errors() {
        let res = doc! {
            "ok": 1.0,
            "n": 1_i32,
            "writeErrors": [ doc! { "index": 1_i32, "code": 11000_i32, "errmsg": "dup" } ],
        };
        let err = parse_update_response(&res).unwrap_err();
        assert!(err.to_string().contains("write errors"));
    }

    #[test]
    fn parse_update_response_ignores_empty_write_errors_array() {
        let empty: Vec<Bson> = Vec::new();
        let res = doc! { "ok": 1.0, "n": 2_i32, "writeErrors": empty };
        assert_eq!(parse_update_response(&res).unwrap(), 2);
    }

    #[test]
    fn parse_update_response_surfaces_write_concern_error() {
        let res = doc! {
            "ok": 1.0,
            "n": 1_i32,
            "writeConcernError": doc! { "code": 64_i32, "errmsg": "majority" },
        };
        let err = parse_update_response(&res).unwrap_err();
        assert!(err.to_string().contains("write concern"));
    }

    #[test]
    fn parse_update_response_errors_when_n_missing() {
        let res = doc! { "ok": 1.0 };
        assert!(parse_update_response(&res).is_err());
    }

    #[test]
    fn parse_update_response_zero_n_is_ok() {
        let res = doc! { "ok": 1.0, "n": 0_i32 };
        assert_eq!(parse_update_response(&res).unwrap(), 0);
    }

    #[test]
    fn parse_update_response_accepts_double_n() {
        let res = doc! { "ok": 1.0, "n": 4.0_f64 };
        assert_eq!(parse_update_response(&res).unwrap(), 4);
    }

    #[test]
    fn parse_update_response_surfaces_ok_zero() {
        // Command-level rejection (auth, namespace, oversized batch).
        // Without an `ok` check the driver would hand us this Document
        // verbatim and we'd silently report "0 rows written".
        let res = doc! {
            "ok": 0.0,
            "code": 13_i32,
            "errmsg": "not authorized",
            "n": 0_i32,
        };
        let err = parse_update_response(&res).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ok=0"), "msg should reference ok=0: {msg}");
        assert!(
            msg.contains("not authorized"),
            "msg should include errmsg: {msg}"
        );
    }

    #[test]
    fn parse_update_response_clamps_negative_n() {
        // Defensive: real servers don't emit negative `n`, but our cast
        // path explicitly clamps via `.max(0)` — pin the behaviour so
        // a refactor can't silently turn a stray negative into a giant
        // u64 wrap.
        let res = doc! { "ok": 1.0, "n": -1_i64 };
        assert_eq!(parse_update_response(&res).unwrap(), 0);
    }

    #[test]
    fn missing_key_in_row_errors() {
        let d = doc! { "name": "alice" };
        let err = build_upsert_filter(&d, &[key(0, "_id")], true).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("_id"),
            "error must mention the missing key, got: {msg}"
        );
    }
}
