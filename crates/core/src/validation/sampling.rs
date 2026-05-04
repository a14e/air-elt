//! Sampling-validation: pull a small batch from the source and
//! actually run the per-column conversion plan against the data.
//!
//! Schema-level checks (`check_mapping`, `check_cursor`) only consult
//! declared types. They will not catch source columns that *say* they
//! hold canonical UUIDs but in practice store malformed strings, or
//! `Int64` columns whose actual values exceed an `Int32` sink even
//! though the matrix accepts the cast in principle. Sampling pulls a
//! representative slice — by default 100 rows — and exercises the
//! full conversion path for each cell.
//!
//! Off by default for SQL backends (the schema is authoritative and
//! sampling adds a real round-trip cost). On by default for MongoDB,
//! since Mongo's per-document type heterogeneity makes the schema
//! inferred at validation time only a hint.

use tracing::info;

use crate::error::ValidationError;
use crate::model::{AssembledFlow, ConversionPlan, Row};
use crate::types::convert;

/// Drive one sample probe (`sample` or `sample_fresh`) with a timeout
/// strategy that matches the runner: cancel-safe sources use
/// `tokio::time::timeout`; cancel-unsafe sources (Mongo) get
/// `tokio::spawn` + detach so the driver future is never dropped
/// mid-await.
async fn run_sample(
    flow: &AssembledFlow,
    component: &'static str,
    op: &'static str,
    size: usize,
    fresh: bool,
) -> Result<Vec<Row>, ValidationError> {
    let cancel_safe = flow.source.cancel_safe();
    let source = flow.source.clone();
    let spec = flow.read_spec.clone();
    let fut = async move {
        if fresh {
            source.sample_fresh(&spec, size).await
        } else {
            source.sample(&spec, size).await
        }
    };
    let rows_res: Result<Vec<Row>, crate::error::RuntimeError> = if cancel_safe {
        match tokio::time::timeout(flow.query_timeout, fut).await {
            Ok(Ok(rows)) => Ok(rows),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(crate::error::RuntimeError::Timeout {
                flow: flow.name.clone(),
                op,
                after: flow.query_timeout,
            }),
        }
    } else {
        let mut handle = tokio::spawn(fut);
        tokio::select! {
            res = &mut handle => match res {
                Ok(Ok(rows)) => Ok(rows),
                Ok(Err(e)) => Err(e),
                Err(join_err) => Err(crate::error::RuntimeError::Other(format!(
                    "spawned op {op} panicked: {join_err}"
                ))),
            },
            _ = tokio::time::sleep(flow.query_timeout) => {
                drop(handle);
                Err(crate::error::RuntimeError::Timeout {
                    flow: flow.name.clone(),
                    op,
                    after: flow.query_timeout,
                })
            }
        }
    };
    rows_res.map_err(|source| ValidationError::AccessFailed {
        component,
        name: flow.name.clone(),
        source: Box::new(source),
    })
}

/// Pull `size` rows from the source and run every non-identity
/// `ConversionPlan` against the sampled values. Identity plans are
/// skipped (no work to do).
pub async fn run(
    flow: &AssembledFlow,
    conversions: &[ConversionPlan],
    size: usize,
) -> Result<(), ValidationError> {
    info!(flow = %flow.name, sample_size = size, "running sampling validation");

    // Honour the same per-call timeout as the runner so a wedged source can't
    // hang validate forever. We run two probes:
    // 1. `sample` — drives the cursor query (validates the SELECT shape).
    // 2. `sample_fresh` — random slice for backends that have one (Mongo
    //    `$sample`); other backends return an empty Vec.
    // The union is fed through the conversion plan.
    let cursor_rows = run_sample(flow, "source:sample", "sampling", size, false).await?;
    let fresh_rows = run_sample(flow, "source:sample_fresh", "sampling-fresh", size, true).await?;

    let mut rows = cursor_rows;
    rows.extend(fresh_rows);

    if rows.is_empty() {
        info!(flow = %flow.name, "sampling validation: source returned 0 rows, nothing to validate");
        return Ok(());
    }

    for (row_index, row) in rows.iter().enumerate() {
        for (col_idx, plan) in conversions.iter().enumerate() {
            if plan.is_identity() {
                continue;
            }
            let value = row
                .values
                .get(col_idx)
                .ok_or_else(|| ValidationError::SamplingFailed {
                    flow: flow.name.clone(),
                    row_index,
                    field: flow
                        .read_spec
                        .columns
                        .get(col_idx)
                        .cloned()
                        .unwrap_or_else(|| format!("col[{col_idx}]")),
                    source_type: plan.source.clone(),
                    sink_type: plan.sink.clone(),
                    detail: "row produced fewer values than the read spec declared".into(),
                })?;
            if let Err(err) = convert::convert(value.clone(), &plan.source, &plan.sink, &plan.ctx) {
                let field_name = flow
                    .read_spec
                    .columns
                    .get(col_idx)
                    .cloned()
                    .unwrap_or_else(|| format!("col[{col_idx}]"));
                return Err(ValidationError::SamplingFailed {
                    flow: flow.name.clone(),
                    row_index,
                    field: field_name,
                    source_type: plan.source.clone(),
                    sink_type: plan.sink.clone(),
                    detail: err.to_string(),
                });
            }
        }
    }

    info!(
        flow = %flow.name,
        rows_sampled = rows.len(),
        "sampling validation passed"
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::flow::test_utils;
    use crate::traits::{MockSink, MockSource, MockStorage};
    use crate::types::{ConversionContext, DataType, Value};

    fn plan(src: DataType, dst: DataType) -> ConversionPlan {
        ConversionPlan {
            source: src,
            sink: dst,
            ctx: ConversionContext::passthrough(),
        }
    }

    fn build_flow_with(source: MockSource) -> crate::model::FlowState {
        // The runner-level test scaffolding wires up minimal sinks /
        // storages so we can construct an `AssembledFlow` cheaply.
        // Sampling never touches sink or storage, so empty mocks
        // suffice for these tests.
        let sink = MockSink::new();
        let storage = MockStorage::new();
        test_utils::test_flow(source, sink, storage)
    }

    #[tokio::test]
    async fn empty_sample_passes() {
        let mut src = MockSource::new();
        src.expect_cancel_safe().return_const(true);
        src.expect_sample().returning(|_, _| Ok(Vec::new()));
        src.expect_sample_fresh().returning(|_, _| Ok(Vec::new()));
        let flow = build_flow_with(src);
        let conversions = vec![plan(DataType::Int32, DataType::Int64)];
        let r = run(&flow, &conversions, 5).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn identity_plan_skipped() {
        let mut src = MockSource::new();
        src.expect_cancel_safe().return_const(true);
        src.expect_sample()
            .returning(|_, _| Ok(vec![crate::model::Row::upsert(vec![Value::Int32(7)])]));
        src.expect_sample_fresh().returning(|_, _| Ok(Vec::new()));
        let flow = build_flow_with(src);
        // Identity plan should be skipped — no convert call, no error
        // even though the row is "wrong shape" for any real conversion.
        let conversions = vec![plan(DataType::Int32, DataType::Int32)];
        let r = run(&flow, &conversions, 1).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn convert_failure_surfaces_as_sampling_failed() {
        // Source claims Uuid, but the row carries a malformed text
        // value. A `Uuid -> Text(36)` plan would normally pass
        // identity; flip the direction so convert actually runs and
        // the bad value triggers a parse error.
        let mut src = MockSource::new();
        src.expect_cancel_safe().return_const(true);
        src.expect_sample().returning(|_, _| {
            Ok(vec![crate::model::Row::upsert(vec![Value::Text(
                "not-a-uuid".into(),
            )])])
        });
        src.expect_sample_fresh().returning(|_, _| Ok(Vec::new()));
        let flow = build_flow_with(src);
        let conversions = vec![plan(DataType::Text { size: Some(36) }, DataType::Uuid)];
        let err = run(&flow, &conversions, 1).await.unwrap_err();
        assert!(matches!(err, ValidationError::SamplingFailed { .. }));
    }
}
