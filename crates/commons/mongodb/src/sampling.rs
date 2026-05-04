//! Document sampling helpers shared by every Mongo-backed source.
//!
//! Mongo collections are schemaless, so connectors describe schemas
//! by sampling N documents and folding the per-field types via
//! [`crate::infer::infer_schema_from_sample`]. This module owns the
//! `$sample` aggregation, the no-rows error shape, and the
//! `Document → Vec<Row>` projection that maps documents to a flow's
//! `column_paths`.
//!
//! Used by:
//! * `air-elt-source-mongodb::MongoSource::describe_schema` /
//!   `sample_fresh`
//! * `air-elt-source-mongo-cdc::MongoCdcSource::describe_schema` /
//!   `sample`. CDC has no static "snapshot" — sampling-validation
//!   uses the underlying collection directly.

use std::time::Duration;

use bson::{Document, doc};
use futures::stream::TryStreamExt;
use mongodb::Collection;
use mongodb::options::AggregateOptions;

use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::mapping::FieldPath;
use air_elt_core::model::{Row, Schema};
use air_elt_core::types::Value;

use crate::{bson_value, infer, path};

pub async fn sample_documents(
    collection: &Collection<Document>,
    n: usize,
    operation_timeout: Duration,
) -> RuntimeResult<Vec<Document>> {
    let pipeline = vec![doc! { "$sample": { "size": n as i64 } }];
    let opts = AggregateOptions::builder()
        .max_time(operation_timeout)
        .build();
    let mut cursor = collection
        .aggregate(pipeline)
        .with_options(opts)
        .await
        .map_err(RuntimeError::backend)?;
    let mut out = Vec::with_capacity(n);
    while let Some(d) = cursor.try_next().await.map_err(RuntimeError::backend)? {
        out.push(d);
    }
    Ok(out)
}

pub async fn describe_collection_schema(
    collection: &Collection<Document>,
    n: usize,
    operation_timeout: Duration,
) -> RuntimeResult<Schema> {
    let docs = sample_documents(collection, n, operation_timeout).await?;
    if docs.is_empty() {
        return Err(RuntimeError::Other(format!(
            "mongo: collection {:?} returned no documents — cannot infer schema",
            collection.name()
        )));
    }
    infer::infer_schema_from_sample(&docs).map_err(|e| RuntimeError::Other(e.to_string()))
}

/// Project sampled documents into rows using the flow's column paths.
/// Missing paths produce `Value::Null`; rows are emitted with the
/// default `RowOp::Upsert` (sampling-validation never uses `op`).
pub fn rows_from_documents(
    docs: &[Document],
    column_paths: &[FieldPath],
) -> RuntimeResult<Vec<Row>> {
    let mut rows = Vec::with_capacity(docs.len());
    for d in docs {
        let mut values = Vec::with_capacity(column_paths.len());
        for p in column_paths {
            let v = match path::get(d, p) {
                Some(b) => bson_value::from_bson(b)?,
                None => Value::Null,
            };
            values.push(v);
        }
        rows.push(Row::upsert(values));
    }
    Ok(rows)
}
