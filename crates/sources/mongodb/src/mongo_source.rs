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
use tracing::{debug, info};

use air_elt_commons_mongodb::client::{PoolSettings, connect, database_from_url};
use air_elt_commons_mongodb::{bson_value, identifier, path, sampling};
use air_elt_core::config::model::CursorOrder;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::mapping::FieldPath;
use air_elt_core::model::{Batch, CursorFieldValue, CursorState, ReadSpec, Row, Schema, SourceCtx};
use air_elt_core::traits::Source;
use air_elt_core::types::Value;

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
    /// Server-enforced — bounds runaway work even when the runner
    /// detaches the spawned future after a client-side shutdown /
    /// timeout (the `mongodb` 3.x driver is not cancellation-safe;
    /// see `Source::cancel_safe`).
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
}

impl SourceCtx for MongoSourceCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl Source for MongoSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn cancel_safe(&self) -> bool {
        // The `mongodb` 3.x Rust driver is not cancellation-safe —
        // dropping a future mid-await can leave driver internals
        // inconsistent. The runner therefore spawns + detaches our
        // calls instead of wrapping them in `tokio::time::timeout`.
        false
    }

    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()> {
        // Ping + a tiny `find().limit(1)` against the target collection to
        // confirm read access. We avoid `count` because some deployments
        // disallow it on shared-tier clusters.
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
        info!(collection = %spec.table, "mongodb source access validated");
        Ok(())
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
        Ok(Arc::new(MongoSourceCtx {
            column_paths,
            cursor_paths,
            cursor_in_columns,
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
                .downcast_ref::<MongoSourceCtx>()
                .ok_or(RuntimeError::ContextMismatch {
                    expected: "MongoSourceCtx",
                })?;
        let coll = self.collection(&spec.table)?;

        let filter = build_filter(&my_ctx.cursor_paths, spec.cursor_order, cursor)?;
        let sort_doc = build_sort(&my_ctx.cursor_paths, spec.cursor_order);
        let limit = i64::try_from(spec.limit).map_err(|_| {
            RuntimeError::Other(format!("batch_limit {} does not fit in i64", spec.limit))
        })?;
        let opts = FindOptions::builder()
            .sort(sort_doc)
            .limit(Some(limit))
            .max_time(self.operation_timeout)
            .build();
        debug!(filter = ?filter, "mongodb read_batch");
        let mut find_cursor = coll
            .find(filter)
            .with_options(opts)
            .await
            .map_err(RuntimeError::backend)?;

        let mut out_rows: Vec<Row> = Vec::with_capacity(spec.limit);
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
            out_rows.push(Row::upsert(values));
        }

        let next_cursor = last_cursor_values.map(|values| {
            let fields = spec
                .cursor_fields
                .iter()
                .zip(values)
                .map(|(name, value)| CursorFieldValue {
                    name: name.clone(),
                    value,
                })
                .collect();
            CursorState::new(fields)
        });
        Ok(Batch {
            rows: out_rows,
            next_cursor,
        })
    }

    async fn sample_fresh(&self, spec: &ReadSpec, n: usize) -> RuntimeResult<Vec<Row>> {
        // Random representative slice via `aggregate([{ $sample: ...}])`
        // — complements the cursor-ordered head produced by the default
        // `sample` impl. Sampling-validation unions both before running
        // the conversion plan.
        let coll = self.collection(&spec.table)?;
        let docs = sampling::sample_documents(&coll, n, self.operation_timeout).await?;
        let column_paths: Vec<FieldPath> = spec
            .columns
            .iter()
            .map(|s| FieldPath::parse(s).map_err(|e| RuntimeError::Other(e.to_string())))
            .collect::<RuntimeResult<_>>()?;
        let rows = sampling::rows_from_documents(&docs, &column_paths)?;
        Ok(rows)
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
