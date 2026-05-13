//! MongoDB source connector.
//!
//! Cursoring uses `find(<filter>).sort(<cursor>: ±1).limit(batch_limit)`.
//! Compound cursors up to `MAX_CURSOR_FIELDS` are supported and compile
//! to an `$or` cascade that mirrors SQL lex-tuple comparison
//! `(c1,c2,...) > ($1,$2,...)`. NULL handling follows the project rule
//! (`NULL` is the minimum, `NULL == NULL`) — see the docstring on
//! `build_filter` for the full predicate shape.
//!
//! Schema introspection samples up to `schema_sample_size` documents
//! (default 100) and folds per-field types via
//! `air_elt_commons_mongodb::infer`.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bson::{Bson, Document, doc};
use futures::stream::TryStreamExt;
use mongodb::{Client, Collection, options::FindOptions};
use tracing::{debug, info, warn};

use air_elt_commons_mongodb::client::{PoolSettings, connect, database_from_url};
use air_elt_commons_mongodb::task::detached;
use air_elt_commons_mongodb::types::BsonObjectValue;
use air_elt_commons_mongodb::{bson_value, identifier, path, sampling};
use air_elt_core::config::model::CursorOrder;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::mapping::FieldPath;
use air_elt_core::model::raw::{RawBatch, RawRow};
use air_elt_core::model::{
    CursorFieldValue, CursorState, ReadSpec, Schema, SchemaProvider, SourceCtx,
};
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};

use crate::config::MongoSourceConfig;

const DEFAULT_SCHEMA_SAMPLE: usize = 100;

/// Maximum number of cursor fields the mongo source accepts. Compound
/// cursors compile to an `$or` cascade where each extra field doubles
/// the predicate-shape complexity, so we cap at three (covers the
/// common `updated_at + _id` and `tenant + updated_at + _id` shapes
/// without inviting unbounded cardinality).
const MAX_CURSOR_FIELDS: usize = 3;

pub struct MongoSource {
    client: Client,
    database: String,
    schema_sample: usize,
    name: String,
    /// Per-operation cap, applied via `*Options::max_time` on every
    /// driver call that supports it (Find, Aggregate, FindOne, …).
    /// Server-enforced — bounds runaway work even when the adapter
    /// detaches its spawned driver future after the runner's
    /// client-side shutdown / timeout (the `mongodb` 3.x driver is not
    /// cancellation-safe; see `task::detached`).
    operation_timeout: Duration,
}

impl MongoSource {
    pub async fn connect(name: String, config: MongoSourceConfig) -> RuntimeResult<Self> {
        let database = config
            .database
            .clone()
            .or_else(|| database_from_url(&config.url))
            .ok_or_else(|| {
                RuntimeError::Other(
                    "mongodb source: `database` not set in config and url has no path \
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
            schema_sample: config.schema_sample_size.unwrap_or(DEFAULT_SCHEMA_SAMPLE),
            name,
            operation_timeout,
        })
    }

    fn collection(&self, name: &str) -> RuntimeResult<Collection<Document>> {
        identifier::validate_name(name).map_err(RuntimeError::from)?;
        Ok(self.client.database(&self.database).collection(name))
    }
}

struct MongoSourceCtx {
    column_paths: Vec<FieldPath>,
    cursor_paths: Vec<FieldPath>,
    cursor_in_columns: Vec<Option<usize>>,
    /// Sample-derived schema for `spec.table`. Mongo collections have
    /// no authoritative schema, so this field is `None` whenever
    /// sampling fails (collection empty, $sample unsupported on the
    /// deployment, etc.). The schemaless-source contract (see
    /// `Source::schemaless`) explicitly allows downstream consumers to
    /// proceed without a schema — the wildcard-expansion path
    /// uses the `as_schema_provider()` indirection to discover whether
    /// this ctx actually has one.
    pub schema: Option<Schema>,
}

impl SourceCtx for MongoSourceCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        // Only advertise as a provider when sampling produced a schema.
        // Without this gate `SchemaProvider::schema()` would have to
        // return `Option<&Schema>` and pollute every other ctx's impl.
        if self.schema.is_some() {
            Some(self)
        } else {
            None
        }
    }
}

impl SchemaProvider for MongoSourceCtx {
    fn schema(&self) -> &Schema {
        // Guarded by `as_schema_provider`: callers that reach this
        // method went through `Option<&dyn SchemaProvider>` first, so
        // `None` here is a programming error elsewhere.
        self.schema
            .as_ref()
            .expect("schemaless ctx asked for schema — caller skipped as_schema_provider gate")
    }
}

#[async_trait]
impl Source for MongoSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn schemaless(&self) -> bool {
        // Mongo collections accept any BSON shape — no authoritative
        // column schema. The wildcard expansion uses this flag
        // (together with `Sink::schemaless`) to admit raw passthrough.
        true
    }

    fn body_data_type(&self) -> DataType {
        // Mongo attaches the source `bson::Document` as a
        // `Value::Custom(BsonObjectValue)` directly on `RawRow.body`
        // when the flow has body targets — `BsonObjectType::is_object()`
        // is `true` so the Transform compiler accepts it.
        DataType::Custom(Box::new(air_elt_commons_mongodb::types::BsonObjectType))
    }

    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()> {
        // Ping + a tiny `find().limit(1)` against the target collection to
        // confirm read access. We avoid `count` because some deployments
        // disallow it on shared-tier clusters.
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
            info!(collection = %table, "mongodb source access validated");
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
        if spec.cursor_fields.len() > MAX_CURSOR_FIELDS {
            return Err(RuntimeError::Other(format!(
                "mongodb source: compound cursors are limited to {MAX_CURSOR_FIELDS} fields \
                 (got {}) — beyond that, the $or cascade emitted by the source becomes \
                 expensive enough that operators should rethink the cursor shape rather than \
                 paying that cost per batch",
                spec.cursor_fields.len()
            )));
        }
        let column_paths = spec
            .columns
            .iter()
            .map(|s| FieldPath::parse(s).map_err(|e| RuntimeError::Other(e.to_string())))
            .collect::<RuntimeResult<Vec<_>>>()?;
        let cursor_paths = spec
            .cursor_fields
            .iter()
            .map(|s| FieldPath::parse(s).map_err(|e| RuntimeError::Other(e.to_string())))
            .collect::<RuntimeResult<Vec<_>>>()?;
        let cursor_in_columns = cursor_paths
            .iter()
            .map(|cp| spec.columns.iter().position(|c| c == &cp.to_string()))
            .collect();
        // Sample-derive the schema for schema-on-ctx parity with
        // the SQL connectors. Mongo is schemaless: a sampling failure
        // (empty collection, $sample disabled, ...) is not fatal here —
        // we just leave the ctx without a schema and let downstream
        // consumers fall back through `as_schema_provider() -> None`.
        let schema = match self.describe_schema(&spec.table).await {
            Ok(s) => Some(s),
            Err(e) => {
                warn!(
                    collection = %spec.table,
                    error = %e,
                    "mongodb: schema sample unavailable; ctx will report schemaless"
                );
                None
            }
        };
        Ok(Arc::new(MongoSourceCtx {
            column_paths,
            cursor_paths,
            cursor_in_columns,
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
                .downcast_ref::<MongoSourceCtx>()
                .ok_or(RuntimeError::ContextMismatch {
                    expected: "MongoSourceCtx",
                })?;
        let coll = self.collection(&spec.table)?;

        let filter = build_filter(&my_ctx.cursor_paths, spec.cursor_order, cursor)?;
        let sort_doc = build_sort(&my_ctx.cursor_paths, spec.cursor_order);
        let limit_i64 = i64::try_from(spec.limit).map_err(|_| {
            RuntimeError::Other(format!("batch_limit {} does not fit in i64", spec.limit))
        })?;
        let opts = FindOptions::builder()
            .sort(sort_doc)
            .limit(Some(limit_i64))
            .max_time(self.operation_timeout)
            .build();
        debug!(filter = ?filter, "mongodb read_batch");

        // Bump the Arc once at the spawn boundary — that's the single
        // place that actually needs owned-ctx. Inside the spawn we
        // downcast to read by reference, so per-field Vec clones are
        // unnecessary.
        let ctx = Arc::clone(ctx);
        let cursor_fields = spec.cursor_fields.clone();
        let columns_empty = spec.columns.is_empty();
        let limit = spec.limit;
        let needs_body = spec.needs_body;

        detached(async move {
            let my_ctx = ctx.as_any().downcast_ref::<MongoSourceCtx>().ok_or(
                RuntimeError::ContextMismatch {
                    expected: "MongoSourceCtx",
                },
            )?;
            let mut find_cursor = coll
                .find(filter)
                .with_options(opts)
                .await
                .map_err(RuntimeError::backend)?;

            // Raw passthrough mode: wildcard expansion against a
            // schemaless-both flow leaves `spec.columns` empty (no per-
            // column projection) and `spec.cursor_fields` empty (raw
            // passthrough is incompatible with column-based cursors —
            // enforced at validation time). Emit one `RawRow` per document
            // carrying the whole document on `body` as
            // `Value::Custom(BsonObjectValue(...))`; the Transform
            // interpreter folds it into `Row::passthrough(BsonObjectValue)`
            // for the mongo sink fast path.
            if columns_empty {
                let mut rows: Vec<RawRow> = Vec::with_capacity(limit);
                while let Some(d) = find_cursor
                    .try_next()
                    .await
                    .map_err(RuntimeError::backend)?
                {
                    rows.push(
                        RawRow::upsert(Vec::new())
                            .with_body(Some(Value::Custom(Box::new(BsonObjectValue(d))))),
                    );
                }
                return Ok(RawBatch {
                    rows,
                    next_cursor: None,
                });
            }

            let mut out_rows: Vec<RawRow> = Vec::with_capacity(limit);
            let mut last_cursor_values: Option<Vec<Value>> = None;

            while let Some(d) = find_cursor
                .try_next()
                .await
                .map_err(RuntimeError::backend)?
            {
                let mut values = Vec::with_capacity(my_ctx.column_paths.len());
                for p in &my_ctx.column_paths {
                    let v = match path::get(&d, p) {
                        Some(b) => bson_value::from_bson(b)?,
                        None => Value::Null,
                    };
                    values.push(v);
                }
                let mut cursor_values = Vec::with_capacity(my_ctx.cursor_paths.len());
                for (i, cp) in my_ctx.cursor_paths.iter().enumerate() {
                    let value = match my_ctx.cursor_in_columns[i] {
                        Some(idx) => values[idx].clone(),
                        None => match path::get(&d, cp) {
                            Some(b) => bson_value::from_bson(b)?,
                            None => Value::Null,
                        },
                    };
                    cursor_values.push(value);
                }
                last_cursor_values = Some(cursor_values);
                // Cost-guarded body attach: the Transform interpreter's
                // `Body` op consumes the `Value::Custom(BsonObjectValue)`.
                // Non-body flows skip the move entirely.
                let body = if needs_body {
                    Some(Value::Custom(Box::new(BsonObjectValue(d))))
                } else {
                    None
                };
                out_rows.push(RawRow::upsert(values).with_body(body));
            }

            let next_cursor = last_cursor_values.map(|values| {
                let fields = cursor_fields
                    .into_iter()
                    .zip(values)
                    .map(|(name, value)| CursorFieldValue { name, value })
                    .collect();
                CursorState::new(fields)
            });
            Ok(RawBatch {
                rows: out_rows,
                next_cursor,
            })
        })
        .await
    }
}

/// Build the WHERE-equivalent for the next batch.
///
/// Single-key trivially translates to `{ cursor: { $gt: last } }`
/// (ASC) / `{ $lt: last }` (DESC). Compound cursors compile to an
/// `$or` cascade implementing lex-tuple comparison:
///
/// ```text
/// (c1,c2,c3) > (v1,v2,v3) ≡
///   c1 > v1
///   OR (c1 = v1 AND c2 > v2)
///   OR (c1 = v1 AND c2 = v2 AND c3 > v3)
/// ```
///
/// NULL semantics follow the project rule (`NULL` is the minimum;
/// `NULL == NULL`):
/// - "Equality" against `NULL` uses `{ field: null }` (BSON matches
///   missing-or-null, which is the closest analogue to SQL `NULL = NULL`
///   under our `NULLS FIRST/LAST` convention).
/// - "`>` than `NULL`" under ASC means "anything non-null" (`{ $ne: null }`).
/// - "`>` than `NULL`" under DESC is unsatisfiable — the branch is dropped.
///   If every branch ends up unsatisfiable we return a sentinel
///   never-match (`$expr: $eq: [1,0]`).
fn build_filter(
    cursor_paths: &[FieldPath],
    order: CursorOrder,
    cursor: Option<&CursorState>,
) -> RuntimeResult<Document> {
    let Some(state) = cursor else {
        return Ok(Document::new());
    };
    if cursor_paths.is_empty() {
        return Ok(Document::new());
    }
    if state.fields.len() != cursor_paths.len() {
        return Err(RuntimeError::Other(format!(
            "cursor state has {} fields, expected {}",
            state.fields.len(),
            cursor_paths.len()
        )));
    }

    let mut branches: Vec<Document> = Vec::with_capacity(cursor_paths.len());
    for (k, _) in cursor_paths.iter().enumerate() {
        // Equality prefix: keys [0..k) match the recorded values.
        let mut branch = Document::new();
        for (path, field_state) in cursor_paths.iter().zip(state.fields.iter()).take(k) {
            let bson = bson_value::to_bson(&field_state.value)?;
            branch.insert(path.to_string(), bson);
        }
        // Strict ordering on key k.
        let kth_field = cursor_paths[k].to_string();
        let kth_value = &state.fields[k].value;
        if kth_value.is_null() {
            match order {
                CursorOrder::Asc => {
                    branch.insert(kth_field, doc! { "$ne": Bson::Null });
                }
                // No value is strictly less than NULL — branch unreachable.
                CursorOrder::Desc => continue,
            }
        } else {
            let op = match order {
                CursorOrder::Asc => "$gt",
                CursorOrder::Desc => "$lt",
            };
            let bson = bson_value::to_bson(kth_value)?;
            branch.insert(kth_field, doc! { op: bson });
        }
        branches.push(branch);
    }

    match branches.len() {
        // Every branch was DESC-after-NULL — sentinel never-match.
        0 => Ok(doc! { "$expr": { "$eq": [1, 0] } }),
        // Single branch: emit it directly so the planner doesn't see a
        // pointless `$or: [ ... ]` wrapper.
        1 => Ok(branches.into_iter().next().expect("len checked")),
        _ => Ok(doc! { "$or": branches }),
    }
}

fn build_sort(cursor_paths: &[FieldPath], order: CursorOrder) -> Document {
    let mut sort = Document::new();
    let dir = match order {
        CursorOrder::Asc => 1_i32,
        CursorOrder::Desc => -1_i32,
    };
    for p in cursor_paths {
        sort.insert(p.to_string(), dir);
    }
    sort
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use air_elt_core::model::{Field, Schema};
    use air_elt_core::types::DataType;

    fn sample_schema() -> Schema {
        Schema::new(vec![Field {
            name: "id".into(),
            data_type: DataType::Int64,
            nullable: false,
        }])
    }

    #[test]
    fn ctx_with_schema_advertises_provider() {
        // Schema-on-ctx: when sampling produced a schema, the
        // mongo source ctx must expose it through `as_schema_provider`
        // — same parity contract as PgSourceCtx / MySqlSourceCtx.
        let ctx = MongoSourceCtx {
            column_paths: vec![],
            cursor_paths: vec![],
            cursor_in_columns: vec![],
            schema: Some(sample_schema()),
        };
        let dyn_ctx: &dyn SourceCtx = &ctx;
        let provider = dyn_ctx
            .as_schema_provider()
            .expect("schema present → provider Some");
        assert_eq!(provider.schema().fields().len(), 1);
        assert_eq!(provider.schema().fields()[0].name, "id");
    }

    #[test]
    fn ctx_without_schema_returns_none_provider() {
        // Schemaless-source contract: sampling failure is allowed.
        // The ctx must NOT advertise as a SchemaProvider in that case
        // — otherwise the `schema()` method would have to lie.
        let ctx = MongoSourceCtx {
            column_paths: vec![],
            cursor_paths: vec![],
            cursor_in_columns: vec![],
            schema: None,
        };
        let dyn_ctx: &dyn SourceCtx = &ctx;
        assert!(dyn_ctx.as_schema_provider().is_none());
    }

    #[test]
    fn build_filter_initial_is_empty() {
        let p = FieldPath::parse("id").unwrap();
        let f = build_filter(&[p], CursorOrder::Asc, None).unwrap();
        assert_eq!(f, Document::new());
    }

    #[test]
    fn build_filter_asc_uses_gt() {
        let p = FieldPath::parse("id").unwrap();
        let state = CursorState::new(vec![CursorFieldValue {
            name: "id".into(),
            value: Value::Int64(7),
        }]);
        let f = build_filter(&[p], CursorOrder::Asc, Some(&state)).unwrap();
        assert_eq!(f, doc! { "id": { "$gt": 7_i64 } });
    }

    #[test]
    fn build_filter_desc_uses_lt() {
        let p = FieldPath::parse("id").unwrap();
        let state = CursorState::new(vec![CursorFieldValue {
            name: "id".into(),
            value: Value::Int64(7),
        }]);
        let f = build_filter(&[p], CursorOrder::Desc, Some(&state)).unwrap();
        assert_eq!(f, doc! { "id": { "$lt": 7_i64 } });
    }

    #[test]
    fn build_filter_null_desc_never_matches() {
        let p = FieldPath::parse("id").unwrap();
        let state = CursorState::new(vec![CursorFieldValue {
            name: "id".into(),
            value: Value::Null,
        }]);
        let f = build_filter(&[p], CursorOrder::Desc, Some(&state)).unwrap();
        assert_eq!(f, doc! { "$expr": { "$eq": [1, 0] } });
    }

    #[test]
    fn build_filter_null_asc_ignores_nulls() {
        let p = FieldPath::parse("id").unwrap();
        let state = CursorState::new(vec![CursorFieldValue {
            name: "id".into(),
            value: Value::Null,
        }]);
        let f = build_filter(&[p], CursorOrder::Asc, Some(&state)).unwrap();
        assert_eq!(f, doc! { "id": { "$ne": Bson::Null } });
    }

    #[test]
    fn sort_direction() {
        let p = FieldPath::parse("id").unwrap();
        assert_eq!(
            build_sort(std::slice::from_ref(&p), CursorOrder::Asc),
            doc! { "id": 1 }
        );
        assert_eq!(build_sort(&[p], CursorOrder::Desc), doc! { "id": -1 });
    }

    fn paths(names: &[&str]) -> Vec<FieldPath> {
        names.iter().map(|n| FieldPath::parse(n).unwrap()).collect()
    }

    fn state(values: &[(&str, Value)]) -> CursorState {
        CursorState::new(
            values
                .iter()
                .map(|(n, v)| CursorFieldValue {
                    name: (*n).to_string(),
                    value: v.clone(),
                })
                .collect(),
        )
    }

    #[test]
    fn build_filter_compound_asc_emits_or_cascade() {
        // (updated_at, id) > (T, 7)
        //   ≡ updated_at > T  OR  (updated_at = T AND id > 7)
        let cs = paths(&["updated_at", "id"]);
        let st = state(&[("updated_at", Value::Int64(1000)), ("id", Value::Int64(7))]);
        let f = build_filter(&cs, CursorOrder::Asc, Some(&st)).unwrap();
        assert_eq!(
            f,
            doc! {
                "$or": [
                    doc! { "updated_at": { "$gt": 1000_i64 } },
                    doc! { "updated_at": 1000_i64, "id": { "$gt": 7_i64 } },
                ]
            }
        );
    }

    #[test]
    fn build_filter_compound_desc_uses_lt() {
        let cs = paths(&["updated_at", "id"]);
        let st = state(&[("updated_at", Value::Int64(1000)), ("id", Value::Int64(7))]);
        let f = build_filter(&cs, CursorOrder::Desc, Some(&st)).unwrap();
        assert_eq!(
            f,
            doc! {
                "$or": [
                    doc! { "updated_at": { "$lt": 1000_i64 } },
                    doc! { "updated_at": 1000_i64, "id": { "$lt": 7_i64 } },
                ]
            }
        );
    }

    #[test]
    fn build_filter_compound_null_at_secondary_asc() {
        // (updated_at, id) with id NULL — second branch becomes
        // "updated_at = T AND id is non-null".
        let cs = paths(&["updated_at", "id"]);
        let st = state(&[("updated_at", Value::Int64(1000)), ("id", Value::Null)]);
        let f = build_filter(&cs, CursorOrder::Asc, Some(&st)).unwrap();
        assert_eq!(
            f,
            doc! {
                "$or": [
                    doc! { "updated_at": { "$gt": 1000_i64 } },
                    doc! { "updated_at": 1000_i64, "id": { "$ne": Bson::Null } },
                ]
            }
        );
    }

    #[test]
    fn build_filter_compound_null_at_secondary_desc_drops_branch() {
        // DESC after NULL on the second key has no satisfying rows;
        // only the first-key strict branch survives.
        let cs = paths(&["updated_at", "id"]);
        let st = state(&[("updated_at", Value::Int64(1000)), ("id", Value::Null)]);
        let f = build_filter(&cs, CursorOrder::Desc, Some(&st)).unwrap();
        assert_eq!(f, doc! { "updated_at": { "$lt": 1000_i64 } });
    }

    #[test]
    fn build_filter_compound_all_null_desc_is_unsatisfiable() {
        let cs = paths(&["a", "b"]);
        let st = state(&[("a", Value::Null), ("b", Value::Null)]);
        let f = build_filter(&cs, CursorOrder::Desc, Some(&st)).unwrap();
        assert_eq!(f, doc! { "$expr": { "$eq": [1, 0] } });
    }

    #[test]
    fn build_filter_compound_state_length_mismatch_errors() {
        let cs = paths(&["a", "b"]);
        let st = state(&[("a", Value::Int64(1))]);
        let err = build_filter(&cs, CursorOrder::Asc, Some(&st)).unwrap_err();
        assert!(
            err.to_string().contains("cursor state has 1"),
            "expected length-mismatch error, got: {err}"
        );
    }

    #[test]
    fn build_filter_three_keys_supported() {
        let cs = paths(&["tenant", "updated_at", "id"]);
        let st = state(&[
            ("tenant", Value::Text("acme".into())),
            ("updated_at", Value::Int64(1000)),
            ("id", Value::Int64(7)),
        ]);
        let f = build_filter(&cs, CursorOrder::Asc, Some(&st)).unwrap();
        let or = f.get_array("$or").unwrap();
        assert_eq!(or.len(), 3, "compound 3-key cursor must produce 3 branches");
    }
}
