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
use air_elt_commons_mongodb::{bson_value, identifier, path, sampling};
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::mapping::FieldPath;
use air_elt_core::model::{
    Batch, CursorFieldValue, CursorState, ReadSpec, Row, RowOp, Schema, SourceCtx,
};
use air_elt_core::traits::Source;
use air_elt_core::types::Value;

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
}

impl SourceCtx for MongoCdcCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl Source for MongoCdcSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn cancel_safe(&self) -> bool {
        // The mongodb 3.x driver is not cancellation-safe.
        false
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
        self.client
            .database(&self.database)
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(RuntimeError::backend)?;
        let coll = self.collection(&spec.table)?;
        let opts = FindOptions::builder()
            .limit(Some(1))
            .max_time(self.operation_timeout)
            .build();
        let _ = coll
            .find(doc! {})
            .with_options(opts)
            .await
            .map_err(RuntimeError::backend)?;
        info!(collection = %spec.table, "mongo-cdc source access validated");
        Ok(())
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
        Ok(Arc::new(MongoCdcCtx {
            column_paths,
            mode: opts.mode,
        }))
    }

    async fn read_batch<'a>(
        &self,
        spec: &ReadSpec,
        ctx: Arc<dyn SourceCtx>,
        cursor: Option<&'a CursorState>,
    ) -> RuntimeResult<Batch> {
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
        let mut stream = coll
            .watch()
            .with_options(watch_opts)
            .await
            .map_err(RuntimeError::backend)?;

        let mut events: Vec<ChangeStreamEvent<Document>> = Vec::with_capacity(spec.limit);
        let mut last_token: Option<ResumeToken> = None;
        while events.len() < spec.limit {
            match stream.try_next().await.map_err(RuntimeError::backend)? {
                Some(event) => {
                    last_token = stream.resume_token();
                    events.push(event);
                }
                None => break,
            }
        }
        // After draining, also pick up the post-batch-resume-token so a
        // long quiescent window still advances the cursor.
        if last_token.is_none() {
            last_token = stream.resume_token();
        }

        // Mode = LookupOnUpdate: collect _ids of update events that
        // arrived without fullDocument and one-shot fetch. Bson is
        // neither Hash nor Eq so we use a Vec — N is bounded by the
        // batch limit, linear lookup is fine.
        let ids_to_lookup: Vec<Bson> = if my_ctx.mode == UpdateMode::LookupOnUpdate {
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
            let opts = FindOptions::builder()
                .max_time(self.operation_timeout)
                .build();
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

        let mut out_rows: Vec<Row> = Vec::with_capacity(events.len());
        for event in events {
            match event.operation_type {
                OperationType::Insert | OperationType::Replace => {
                    if let Some(doc) = event.full_document {
                        out_rows.push(map_row(&my_ctx.column_paths, &doc, RowOp::Upsert)?);
                    } else {
                        warn!(op = ?event.operation_type, "mongo-cdc: insert/replace event without fullDocument; skipping");
                    }
                }
                OperationType::Update => {
                    let doc = match my_ctx.mode {
                        UpdateMode::PostImage => event.full_document,
                        UpdateMode::LookupOnUpdate => event
                            .document_key
                            .as_ref()
                            .and_then(|dk| dk.get("_id").cloned())
                            .and_then(|id| lookup_by_id(&id)),
                    };
                    match doc {
                        Some(d) => out_rows.push(map_row(&my_ctx.column_paths, &d, RowOp::Upsert)?),
                        None => {
                            warn!(
                                "mongo-cdc: update event without retrievable fullDocument; skipping (a delete event will follow)"
                            );
                        }
                    }
                }
                OperationType::Delete => {
                    let key_doc = event.document_key.unwrap_or_default();
                    out_rows.push(map_row(&my_ctx.column_paths, &key_doc, RowOp::Delete)?);
                }
                OperationType::Drop
                | OperationType::Rename
                | OperationType::DropDatabase
                | OperationType::Invalidate => {
                    return Err(RuntimeError::Other(format!(
                        "mongo-cdc: collection-level event {:?} on {:?} invalidated the change stream — \
                         operator action required (recreate flow / restart)",
                        event.operation_type, spec.table
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

        Ok(Batch {
            rows: out_rows,
            next_cursor,
        })
    }

    async fn sample(&self, spec: &ReadSpec, n: usize) -> RuntimeResult<Vec<Row>> {
        // CDC streams are open-ended; sampling-validation needs static
        // rows. Use the same `$sample` path as the regular mongo source.
        let coll = self.collection(&spec.table)?;
        let docs = sampling::sample_documents(&coll, n, self.operation_timeout).await?;
        let column_paths: Vec<FieldPath> = spec
            .columns
            .iter()
            .map(|s| FieldPath::parse(s).map_err(|e| RuntimeError::Other(e.to_string())))
            .collect::<RuntimeResult<_>>()?;
        sampling::rows_from_documents(&docs, &column_paths)
    }

    async fn sample_fresh(&self, spec: &ReadSpec, n: usize) -> RuntimeResult<Vec<Row>> {
        self.sample(spec, n).await
    }
}

fn map_row(paths: &[FieldPath], doc: &Document, op: RowOp) -> RuntimeResult<Row> {
    let mut values = Vec::with_capacity(paths.len());
    for p in paths {
        let v = match path::get(doc, p) {
            Some(b) => bson_value::from_bson(b)?,
            None => Value::Null,
        };
        values.push(v);
    }
    Ok(Row { values, op })
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
