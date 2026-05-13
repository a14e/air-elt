//! MongoDB CDC source — driven by `collection.watch()` change streams.
//!
//! Pagination is the resume token, not user-defined fields. The flow's
//! `cursor.fields` must therefore be empty (validated in
//! `core::validation::pipeline::assemble`); the resume token is
//! persisted via the `Storage::save_resume_token` API and round-trips
//! through `bson::Document` ↔ `serde_json::Value`.
//!
//! Modes (selected per-flow, see `MongoCdcFlowOptions`):
//!
//! * `PostImage` — `fullDocument: "required"` on the watch options.
//!   Requires `changeStreamPreAndPostImages` enabled on the collection
//!   (Mongo 6+).
//! * `LookupOnUpdate` — open the stream without `fullDocument`. After
//!   each batch of `update` events we issue a single
//!   `find({_id: {$in: ids}})` to attach the current post-image. Skips
//!   rows whose document was deleted between event and lookup (a
//!   subsequent `delete` event will arrive on the same stream).
//!
//! Operation mapping:
//!
//! * `insert`, `replace`, `update` (with fullDocument) → `Row { op: Upsert }`
//! * `delete` → `Row { op: Delete }` populated from `documentKey`
//! * `drop`/`rename`/`invalidate` → fail the iteration; runner retry
//!   may reopen the stream if the resume token is still in oplog.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bson::{Bson, Document, doc};
use futures::stream::TryStreamExt;
use mongodb::change_stream::event::{ChangeStreamEvent, OperationType, ResumeToken};
use mongodb::options::{ChangeStreamOptions, FindOptions, FullDocumentType};
use mongodb::{Client, Collection};
use tracing::{debug, info, warn};

use air_elt_commons_mongodb::client::{PoolSettings, connect, database_from_url};
use air_elt_commons_mongodb::key_bson::KeyBson;
use air_elt_commons_mongodb::task::detached;
use air_elt_commons_mongodb::types::BsonObjectValue;
use air_elt_commons_mongodb::{bson_value, identifier, path, sampling};
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::mapping::FieldPath;
use air_elt_core::model::raw::{RawBatch, RawRow};
use air_elt_core::model::{
    CursorFieldValue, CursorState, ReadSpec, RowOp, Schema, SchemaProvider, SourceCtx,
};
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};

use crate::config::{MongoCdcFlowOptions, MongoCdcSourceConfig, UpdateMode};

const DEFAULT_SCHEMA_SAMPLE: usize = 100;
const DEFAULT_MAX_AWAIT: Duration = Duration::from_secs(1);
/// Synthetic single-field cursor name carrying the BSON resume token
/// serialised as `Value::Json`. The runner's CDC path goes through
/// `Storage::save_resume_token`, but we still surface a `CursorState`
/// so the existing `Batch::next_cursor` shape is preserved (helps
/// `--once` tests that only assert against `CursorState`).
pub const RESUME_TOKEN_FIELD: &str = "__resume_token";

pub struct MongoCdcSource {
    client: Client,
    database: String,
    name: String,
    schema_sample: usize,
    operation_timeout: Duration,
    max_await_time: Duration,
}

impl MongoCdcSource {
    pub async fn connect(name: String, config: MongoCdcSourceConfig) -> RuntimeResult<Self> {
        let database = config
            .database
            .clone()
            .or_else(|| database_from_url(&config.url))
            .ok_or_else(|| {
                RuntimeError::Other(
                    "mongo-cdc source: `database` not set in config and url has no path \
                     component"
                        .into(),
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
        );
        let operation_timeout = settings.statement;
        let client = connect(&config.url, settings).await?;
        Ok(Self {
            client,
            database,
            name,
            schema_sample: config.schema_sample_size.unwrap_or(DEFAULT_SCHEMA_SAMPLE),
            operation_timeout,
            max_await_time: config.max_await_time.unwrap_or(DEFAULT_MAX_AWAIT),
        })
    }

    fn collection(&self, name: &str) -> RuntimeResult<Collection<Document>> {
        identifier::validate_name(name).map_err(RuntimeError::from)?;
        Ok(self.client.database(&self.database).collection(name))
    }
}

struct MongoCdcCtx {
    column_paths: Vec<FieldPath>,
    mode: UpdateMode,
    /// Sample-derived schema for `spec.table`. Mongo is schemaless —
    /// `None` means sampling failed and downstream consumers must fall
    /// back through `as_schema_provider() -> None`. See
    /// [`MongoSourceCtx::schema`] for the full contract.
    pub schema: Option<Schema>,
}

impl SourceCtx for MongoCdcCtx {
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

impl SchemaProvider for MongoCdcCtx {
    fn schema(&self) -> &Schema {
        // Programming-error guard — see `MongoSourceCtx::schema`.
        self.schema
            .as_ref()
            .expect("schemaless ctx asked for schema — caller skipped as_schema_provider gate")
    }
}

#[async_trait]
impl Source for MongoCdcSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn body_data_type(&self) -> DataType {
        // CDC attaches the source `bson::Document` as a
        // `Value::Custom(BsonObjectValue)` directly on `RawRow.body`.
        // Delete events with no full document attach an empty document
        // so `Transform::Body` always sees a value (sinks rely on key
        // columns for delete, not body).
        DataType::Custom(Box::new(air_elt_commons_mongodb::types::BsonObjectType))
    }

    fn schemaless(&self) -> bool {
        // Mongo collections accept any BSON shape — same rationale as
        // the cursor-driven `MongoSource`. CDC raw-passthrough is not
        // currently supported, but we still advertise the flag
        // so the wildcard expansion gates correctly.
        true
    }

    fn emits_deletes(&self) -> bool {
        // Change-stream `delete` events translate to Row::delete; the
        // validation pipeline pre-flights the sink's DELETE path off
        // the back of this flag.
        true
    }

    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()> {
        // Ping + tiny `find().limit(1)` to confirm read access. We
        // don't open a watch() here — that would require running on a
        // replica-set deployment, which we want to surface only when
        // the flow actually drains.
        let client = self.client.clone();
        let database = self.database.clone();
        let coll = self.collection(&spec.table)?;
        let opts = FindOptions::builder()
            .limit(Some(1))
            .max_time(self.operation_timeout)
            .build();
        let table = spec.table.clone();
        detached(async move {
            client
                .database(&database)
                .run_command(doc! { "ping": 1 })
                .await
                .map_err(RuntimeError::backend)?;
            coll.find(doc! {})
                .with_options(opts)
                .await
                .map_err(RuntimeError::backend)?;
            info!(collection = %table, "mongo-cdc source access validated");
            Ok(())
        })
        .await
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        let coll = self.collection(table)?;
        sampling::describe_collection_schema(&coll, self.schema_sample, self.operation_timeout)
            .await
    }

    async fn build_context(&self, spec: &ReadSpec) -> RuntimeResult<Arc<dyn SourceCtx>> {
        // Per-flow opts arrive as a free-form table on ReadSpec; we
        // deserialise into a typed shape that requires `mode`.
        let opts: MongoCdcFlowOptions = spec
            .source_options
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| {
                RuntimeError::Other(format!(
                    "mongo-cdc: invalid per-flow source options ({e}) — expected {{ name = \"...\", \
                     mode = \"post-image\" | \"lookup-on-update\" }}"
                ))
            })?;
        let column_paths = spec
            .columns
            .iter()
            .map(|s| FieldPath::parse(s).map_err(|e| RuntimeError::Other(e.to_string())))
            .collect::<RuntimeResult<Vec<_>>>()?;
        // Schema-on-ctx parity (sample-derived). Sampling failure
        // is non-fatal under the schemaless-source contract — see
        // `MongoSourceCtx::build_context`.
        let schema = match self.describe_schema(&spec.table).await {
            Ok(s) => Some(s),
            Err(e) => {
                warn!(
                    collection = %spec.table,
                    error = %e,
                    "mongo-cdc: schema sample unavailable; ctx will report schemaless"
                );
                None
            }
        };
        Ok(Arc::new(MongoCdcCtx {
            column_paths,
            mode: opts.mode,
            schema,
        }))
    }

    async fn read_batch<'a>(
        &self,
        spec: &ReadSpec,
        ctx: &Arc<dyn SourceCtx>,
        cursor: Option<&'a CursorState>,
    ) -> RuntimeResult<RawBatch> {
        let my_ctx =
            ctx.as_any()
                .downcast_ref::<MongoCdcCtx>()
                .ok_or(RuntimeError::ContextMismatch {
                    expected: "MongoCdcCtx",
                })?;
        let coll = self.collection(&spec.table)?;

        let resume_after = resume_token_from_cursor(cursor)?;
        let full_document = match my_ctx.mode {
            UpdateMode::PostImage => Some(FullDocumentType::Required),
            UpdateMode::LookupOnUpdate => None,
        };
        // Typed-builder consumes self at each setter, so we set
        // everything in a single chain. `Option<ResumeToken>` /
        // `Option<FullDocumentType>` fields accept None to opt out.
        let watch_opts = ChangeStreamOptions::builder()
            .max_await_time(self.max_await_time)
            .resume_after(resume_after.clone())
            .full_document(full_document)
            .build();

        debug!(mode = ?my_ctx.mode, resume_after = resume_after.is_some(), "mongo-cdc opening change stream");

        // Bump the Arc once at the spawn boundary; downcast inside.
        // Avoids cloning `column_paths` and `mode` is `Copy`.
        let ctx = Arc::clone(ctx);
        let table = spec.table.clone();
        let limit = spec.limit;
        let needs_body = spec.needs_body;
        let operation_timeout = self.operation_timeout;

        detached(async move {
            let my_ctx =
                ctx.as_any()
                    .downcast_ref::<MongoCdcCtx>()
                    .ok_or(RuntimeError::ContextMismatch {
                        expected: "MongoCdcCtx",
                    })?;
            let column_paths = my_ctx.column_paths.as_slice();
            let mode = my_ctx.mode;
        let mut stream = coll
            .watch()
            .with_options(watch_opts)
            .await
            .map_err(RuntimeError::backend)?;

        let mut events: Vec<ChangeStreamEvent<Document>> = Vec::with_capacity(limit);
        let mut last_token: Option<ResumeToken> = None;
        // Whole-batch deadline. Without it, an awaitData change-stream
        // cursor blocks `try_next` indefinitely when the workload is
        // quiet — `read_batch` would hang until the runner's outer
        // `query_timeout` fires (or, in tests that bypass the runner,
        // forever). Bounding here means an idle drain returns whatever
        // we have so the runner can persist forward progress.
        //
        // Why we track `hit_deadline`: the post-loop PBRT fallback
        // below is unsafe when we time out mid-batch. The driver may
        // have buffered events that `try_next` hasn't returned yet;
        // their post-batch resume token is already cached in
        // `stream.resume_token()`. If we use that token as the saved
        // cursor, those buffered events are skipped on the next
        // `read_batch` (cursor advances past them). Only fall back to
        // PBRT when the loop drained cleanly (stream end or limit
        // reached). On timeout we keep `last_token` at the last
        // *delivered* event's token, so the server replays anything
        // that didn't make it across the API boundary.
        let deadline = tokio::time::Instant::now() + operation_timeout;
        let mut hit_deadline = false;
        'drain: while events.len() < limit {
            if tokio::time::Instant::now() >= deadline {
                hit_deadline = true;
                break 'drain;
            }
            // Per-poll cancel-safety: the driver future is awaited to
            // completion even when the deadline fires. We pin the
            // future, race it against the deadline, and on the deadline
            // arm finish driving the same pinned future — so
            // `try_next` is never dropped mid-await. Bounded extra
            // wait: `ChangeStreamOptions::max_await_time` (default 1s).
            let mut try_next_fut = std::pin::pin!(stream.try_next());
            let res = tokio::select! {
                biased;
                res = &mut try_next_fut => res,
                _ = tokio::time::sleep_until(deadline) => {
                    hit_deadline = true;
                    try_next_fut.await
                }
            };
            match res {
                Ok(Some(event)) => {
                    last_token = stream.resume_token();
                    events.push(event);
                    if hit_deadline {
                        break 'drain;
                    }
                }
                Ok(None) => break 'drain,
                Err(e) => return Err(RuntimeError::backend(e)),
            }
        }
        // After a clean drain (stream end or limit reached) pick up
        // the post-batch resume token so a long quiescent window
        // still advances the cursor. Skipped on timeout — see the
        // `hit_deadline` rationale above.
        if !hit_deadline && last_token.is_none() {
            last_token = stream.resume_token();
        }

        // Why dedup-by-`_id` here, last-event-wins:
        //   * delete(k) → insert(k): the sink applies upserts before
        //     deletes (preserves insert→delete ordering on distinct
        //     keys). If both arrive for the same key, the upsert
        //     would land first and the delete would erase it,
        //     inverting intent. Only the latest event must survive.
        //   * N updates for the same `_id` in LookupOnUpdate mode
        //     would otherwise emit N rows with the same post-image
        //     after one `find`. Dedup-before-lookup makes `find`
        //     issue over unique `_id`s only.
        // Events without a `documentKey` (collection-level — Drop /
        // Invalidate / etc.) are kept as-is so the per-event match
        // arm below can surface them as runtime errors.
        if events.len() > 1 {
            let mut seen: ahash::AHashSet<KeyBson> = ahash::AHashSet::with_capacity(events.len());
            let mut kept_rev: Vec<ChangeStreamEvent<Document>> = Vec::with_capacity(events.len());
            for ev in events.into_iter().rev() {
                let id = ev.document_key.as_ref().and_then(|d| d.get("_id").cloned());
                match id {
                    Some(id) => {
                        if seen.insert(KeyBson(id)) {
                            kept_rev.push(ev);
                        }
                    }
                    None => kept_rev.push(ev),
                }
            }
            kept_rev.reverse();
            events = kept_rev;
        }

        // Mode = LookupOnUpdate: collect _ids of update events that
        // arrived without fullDocument for one-shot `find($in)`. The
        // events list is already deduped above, so the resulting list
        // has unique `_id`s by construction.
        let ids_to_lookup: Vec<Bson> = if mode == UpdateMode::LookupOnUpdate {
            events
                .iter()
                .filter_map(|e| match e.operation_type {
                    OperationType::Update => e
                        .document_key
                        .as_ref()
                        .and_then(|dk| dk.get("_id").cloned()),
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };

        let lookup_docs: Vec<(Bson, Document)> = if ids_to_lookup.is_empty() {
            Vec::new()
        } else {
            let filter = doc! { "_id": { "$in": &ids_to_lookup } };
            // Pass the remaining budget (operation_timeout minus what
            // the change-stream drain already consumed) so the lookup
            // can't extend past the outer deadline. Fall back to a
            // minimum tick if we've already overrun.
            let remaining = deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .max(std::time::Duration::from_millis(1));
            let opts = FindOptions::builder().max_time(remaining).build();
            let mut find_cursor = coll
                .find(filter)
                .with_options(opts)
                .await
                .map_err(RuntimeError::backend)?;
            let mut out = Vec::with_capacity(ids_to_lookup.len());
            while let Some(d) = find_cursor
                .try_next()
                .await
                .map_err(RuntimeError::backend)?
            {
                if let Some(id) = d.get("_id").cloned() {
                    out.push((id, d));
                }
            }
            out
        };
        let lookup_by_id = |id: &Bson| -> Option<Document> {
            lookup_docs
                .iter()
                .find(|(k, _)| k == id)
                .map(|(_, d)| d.clone())
        };

        let mut out_rows: Vec<RawRow> = Vec::with_capacity(events.len());
        for event in events {
            match event.operation_type {
                OperationType::Insert | OperationType::Replace => {
                    if let Some(doc) = event.full_document {
                        // Cost-guarded body attach.
                        let body = if needs_body {
                            Some(Value::Custom(Box::new(BsonObjectValue(doc.clone()))))
                        } else {
                            None
                        };
                        out_rows.push(map_row(column_paths, &doc, RowOp::Upsert, body)?);
                    } else {
                        warn!(op = ?event.operation_type, "mongo-cdc: insert/replace event without fullDocument; skipping");
                    }
                }
                OperationType::Update => {
                    let doc = match mode {
                        UpdateMode::PostImage => event.full_document,
                        UpdateMode::LookupOnUpdate => event
                            .document_key
                            .as_ref()
                            .and_then(|dk| dk.get("_id").cloned())
                            .and_then(|id| lookup_by_id(&id)),
                    };
                    match doc {
                        Some(d) => {
                            let body = if needs_body {
                                Some(Value::Custom(Box::new(BsonObjectValue(d.clone()))))
                            } else {
                                None
                            };
                            out_rows.push(map_row(column_paths, &d, RowOp::Upsert, body)?);
                        }
                        None => {
                            warn!(
                                "mongo-cdc: update event without retrievable fullDocument; skipping (a delete event will follow)"
                            );
                        }
                    }
                }
                OperationType::Delete => {
                    // CDC delete events carry only the `documentKey`,
                    // not the deleted document. We still attach an
                    // empty `Value::Custom(BsonObjectValue(empty))`
                    // when `needs_body=true` so `Transform::Body` never
                    // sees a missing value — body content for delete
                    // rows is harmless because sinks key off the
                    // `op = Delete` route, not the body.
                    let key_doc = event.document_key.unwrap_or_default();
                    let body = if needs_body {
                        Some(Value::Custom(Box::new(BsonObjectValue(Document::new()))))
                    } else {
                        None
                    };
                    out_rows.push(map_row(column_paths, &key_doc, RowOp::Delete, body)?);
                }
                OperationType::Drop
                | OperationType::Rename
                | OperationType::DropDatabase
                | OperationType::Invalidate => {
                    return Err(RuntimeError::Other(format!(
                        "mongo-cdc: collection-level event {:?} on {:?} invalidated the change stream — \
                         operator action required (recreate flow / restart)",
                        event.operation_type, table
                    )));
                }
                _ => {
                    debug!(op = ?event.operation_type, "mongo-cdc: ignoring non-data event");
                }
            }
        }

        let next_cursor = last_token.and_then(|tok| {
            // ResumeToken is Serialize → bson::to_bson yields a
            // Bson::Document we can carry through Value::Json.
            match bson::to_bson(&tok) {
                Ok(b) => match bson_value::from_bson(&b) {
                    Ok(v) => Some(CursorState::new(vec![CursorFieldValue {
                        name: RESUME_TOKEN_FIELD.into(),
                        value: v,
                    }])),
                    Err(e) => {
                        warn!(error = %e, "mongo-cdc: failed to encode resume token; advancing without persistence");
                        None
                    }
                },
                Err(e) => {
                    warn!(error = %e, "mongo-cdc: failed to serialise resume token");
                    None
                }
            }
        });

        Ok(RawBatch {
            rows: out_rows,
            next_cursor,
        })
        })
        .await
    }

    async fn sample(
        &self,
        spec: &ReadSpec,
        _ctx: &Arc<dyn SourceCtx>,
        n: usize,
    ) -> RuntimeResult<RawBatch> {
        // CDC streams are open-ended: the default `sample` impl
        // (which delegates to `read_batch`) would block on the change
        // stream until `operation_timeout` fires, returning nothing.
        // Override to read the watched collection's current state via
        // `aggregate([{ $sample: ...}])` so sampling-validation gets
        // representative rows for the conversion-plan probe.
        let coll = self.collection(&spec.table)?;
        let docs = sampling::sample_documents(&coll, n, self.operation_timeout).await?;
        let column_paths: Vec<FieldPath> = spec
            .columns
            .iter()
            .map(|s| FieldPath::parse(s).map_err(|e| RuntimeError::Other(e.to_string())))
            .collect::<RuntimeResult<_>>()?;
        // The shared helper returns a pre-Transform `Vec<Row>`; the
        // runner applies the Transform program afterwards. Wrap into a
        // `RawBatch` (`body` omitted — sampling never flexes the
        // body-fold path).
        let rows = sampling::rows_from_documents(docs, &column_paths)?;
        let raw_rows = rows
            .into_iter()
            .map(|r| RawRow {
                values: r.values,
                body: None,
                op: r.op,
            })
            .collect();
        Ok(RawBatch {
            rows: raw_rows,
            next_cursor: None,
        })
    }
}

/// Build a `RawRow` from a change-stream document. `body` is attached
/// when the flow has body targets (cost-guarded by the caller). For
/// delete events the caller passes
/// `Some(Value::Custom(BsonObjectValue(empty Document)))` so
/// `Transform::Body` always sees a value; non-body flows pass `None`.
fn map_row(
    paths: &[FieldPath],
    doc: &Document,
    op: RowOp,
    body: Option<Value>,
) -> RuntimeResult<RawRow> {
    let mut values = Vec::with_capacity(paths.len());
    for p in paths {
        let v = match path::get(doc, p) {
            Some(b) => bson_value::from_bson(b)?,
            None => Value::Null,
        };
        values.push(v);
    }
    let row = match op {
        RowOp::Upsert => RawRow::upsert(values),
        RowOp::Delete => RawRow::delete(values),
    };
    Ok(row.with_body(body))
}

fn resume_token_from_cursor(cursor: Option<&CursorState>) -> RuntimeResult<Option<ResumeToken>> {
    let Some(state) = cursor else {
        return Ok(None);
    };
    let Some(field) = state.fields.first() else {
        return Ok(None);
    };
    if field.name != RESUME_TOKEN_FIELD {
        return Err(RuntimeError::Other(format!(
            "mongo-cdc: unexpected cursor field {:?} (expected {RESUME_TOKEN_FIELD:?})",
            field.name
        )));
    }
    let bson = bson_value::to_bson(&field.value)?;
    if !matches!(bson, Bson::Document(_)) {
        return Err(RuntimeError::Other(
            "mongo-cdc: resume token is not a BSON document".into(),
        ));
    }
    let token: ResumeToken = bson::from_bson(bson).map_err(RuntimeError::backend)?;
    Ok(Some(token))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use air_elt_core::model::{Field, Schema};
    use air_elt_core::types::DataType;

    fn sample_schema() -> Schema {
        Schema::schemaless_with_sample(vec![Field {
            name: "_id".into(),
            data_type: DataType::Bytes { size: Some(12) },
            nullable: false,
        }])
    }

    #[test]
    fn ctx_with_schema_advertises_provider() {
        // Schema-on-ctx parity for mongo-cdc — same contract as
        // the regular MongoSource ctx: schema present → SchemaProvider
        // surfaces, schema None → None.
        let ctx = MongoCdcCtx {
            column_paths: vec![],
            mode: UpdateMode::PostImage,
            schema: Some(sample_schema()),
        };
        let dyn_ctx: &dyn SourceCtx = &ctx;
        let provider = dyn_ctx
            .as_schema_provider()
            .expect("schema present → provider Some");
        assert_eq!(provider.schema().fields().len(), 1);
        assert_eq!(provider.schema().fields()[0].name, "_id");
    }

    #[test]
    fn ctx_without_schema_returns_none_provider() {
        let ctx = MongoCdcCtx {
            column_paths: vec![],
            mode: UpdateMode::PostImage,
            schema: None,
        };
        let dyn_ctx: &dyn SourceCtx = &ctx;
        assert!(dyn_ctx.as_schema_provider().is_none());
    }

    #[test]
    fn flow_options_parse_post_image() {
        let mut t = toml::Table::new();
        t.insert("mode".into(), toml::Value::String("post-image".into()));
        let opts: MongoCdcFlowOptions = t.try_into().unwrap();
        assert_eq!(opts.mode, UpdateMode::PostImage);
    }

    #[test]
    fn flow_options_parse_lookup() {
        let mut t = toml::Table::new();
        t.insert(
            "mode".into(),
            toml::Value::String("lookup-on-update".into()),
        );
        let opts: MongoCdcFlowOptions = t.try_into().unwrap();
        assert_eq!(opts.mode, UpdateMode::LookupOnUpdate);
    }

    #[test]
    fn flow_options_reject_unknown_field() {
        let mut t = toml::Table::new();
        t.insert("mode".into(), toml::Value::String("post-image".into()));
        t.insert("typo".into(), toml::Value::Boolean(true));
        let r: Result<MongoCdcFlowOptions, _> = t.try_into();
        assert!(r.is_err());
    }

    #[test]
    fn flow_options_require_mode() {
        let t = toml::Table::new();
        let r: Result<MongoCdcFlowOptions, _> = t.try_into();
        assert!(r.is_err(), "missing mode must error");
    }

    #[test]
    fn resume_token_round_trip() {
        let token_doc = doc! { "_data": "82..." };
        let v = bson_value::from_bson(&Bson::Document(token_doc.clone())).unwrap();
        let state = CursorState::new(vec![CursorFieldValue {
            name: RESUME_TOKEN_FIELD.into(),
            value: v,
        }]);
        let token = resume_token_from_cursor(Some(&state)).unwrap().unwrap();
        let back = bson::to_bson(&token).unwrap();
        assert_eq!(back, Bson::Document(token_doc));
    }

    #[test]
    fn resume_token_rejects_wrong_field_name() {
        let state = CursorState::new(vec![CursorFieldValue {
            name: "id".into(),
            value: Value::Int64(1),
        }]);
        let err = resume_token_from_cursor(Some(&state)).unwrap_err();
        assert!(err.to_string().contains("unexpected cursor field"));
    }

    #[test]
    fn resume_token_none_on_no_cursor() {
        assert!(resume_token_from_cursor(None).unwrap().is_none());
    }
}
