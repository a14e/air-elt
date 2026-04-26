use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use crate::config::model::CursorOrder;
use crate::error::RuntimeError;
use crate::model::{
    AssembledFlow, Batch, CursorFieldValue, CursorState, FlowState, ReadSpec, Row, SinkCtx,
    SourceCtx, WriteReport, WriteSpec,
};
use crate::traits::{MockSink, MockSource, MockStorage};
use crate::types::value::Value;

pub struct UnitSourceCtx;
impl SourceCtx for UnitSourceCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct UnitSinkCtx;
impl SinkCtx for UnitSinkCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn one_row_batch() -> Batch {
    Batch {
        rows: vec![Row {
            values: vec![Value::Int64(1)],
        }],
        next_cursor: Some(CursorState::new(vec![CursorFieldValue {
            name: "id".into(),
            value: Value::Int64(1),
        }])),
    }
}

pub fn mock_source_ok() -> MockSource {
    let call = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut s = MockSource::new();
    s.expect_build_context()
        .returning(|_| Ok(Arc::new(UnitSourceCtx)));
    s.expect_read_batch().returning(move |_, _ctx, _| {
        let n = call.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            Ok(one_row_batch())
        } else {
            Ok(Batch::default())
        }
    });
    s
}

pub fn mock_source_empty() -> MockSource {
    let mut s = MockSource::new();
    s.expect_build_context()
        .returning(|_| Ok(Arc::new(UnitSourceCtx)));
    s.expect_read_batch()
        .returning(|_, _ctx, _| Ok(Batch::default()));
    s
}

pub fn mock_source_no_cursor() -> MockSource {
    let call = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut s = MockSource::new();
    s.expect_build_context()
        .returning(|_| Ok(Arc::new(UnitSourceCtx)));
    s.expect_read_batch().returning(move |_, _ctx, _| {
        let n = call.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            Ok(Batch {
                rows: vec![Row {
                    values: vec![Value::Int64(1)],
                }],
                next_cursor: None,
            })
        } else {
            Ok(Batch::default())
        }
    });
    s
}

pub fn mock_source_failing(times: u32) -> MockSource {
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(times));
    let call = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut s = MockSource::new();
    s.expect_build_context()
        .returning(|_| Ok(Arc::new(UnitSourceCtx)));
    s.expect_read_batch().returning(move |_, _ctx, _| {
        let remaining = counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        if remaining > 0 {
            Err(RuntimeError::Other("source boom".into()))
        } else {
            let n = call.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Ok(one_row_batch())
            } else {
                Ok(Batch::default())
            }
        }
    });
    s
}

pub fn mock_sink_ok() -> MockSink {
    let mut s = MockSink::new();
    s.expect_build_context()
        .returning(|_| Ok(Arc::new(UnitSinkCtx)));
    s.expect_write_batch().returning(|_, _ctx, batch| {
        Ok(WriteReport {
            rows_written: batch.rows.len() as u64,
        })
    });
    s
}

pub fn mock_storage_ok() -> MockStorage {
    let mut s = MockStorage::new();
    s.expect_load_cursor().returning(|_| Ok(None));
    s.expect_save_cursor().returning(|_, _| Ok(()));
    s
}

pub fn mock_storage_save_fails() -> MockStorage {
    let mut s = MockStorage::new();
    s.expect_load_cursor().returning(|_| Ok(None));
    s.expect_save_cursor()
        .returning(|_, _| Err(RuntimeError::Other("storage boom".into())));
    s
}

pub fn test_flow_named(
    name: &str,
    source: MockSource,
    sink: MockSink,
    storage: MockStorage,
) -> FlowState {
    let assembled = AssembledFlow {
        name: name.into(),
        source: Arc::new(source),
        sink: Arc::new(sink),
        storage: Arc::new(storage),
        mappings: vec![crate::mapping::ColumnMapping {
            from: "id".into(),
            to: "id".into(),
            truncate: false,
            default_literal: None,
        }],
        read_spec: ReadSpec {
            columns: vec!["id".into()],
            table: "public.t".into(),
            cursor_fields: vec!["id".into()],
            cursor_order: CursorOrder::Asc,
            limit: 1,
        },
        write_spec: WriteSpec {
            columns: vec!["id".into()],
            table: "public.t".into(),
        },
        interval: Duration::from_millis(10),
        query_timeout: Duration::from_secs(5),
    };
    FlowState::new_unchecked(assembled, Vec::new())
}

pub fn test_flow(source: MockSource, sink: MockSink, storage: MockStorage) -> FlowState {
    test_flow_named("test_flow", source, sink, storage)
}
