use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use crate::config::model::CursorOrder;
use crate::error::RuntimeError;
use crate::model::{
    AssembledFlow, ConfigReadSpec, ConfigWriteSpec, CursorFieldValue, CursorState, DerivedPlans,
    Field, FlowState, ReadSpec, Schema, SchemaProvider, SinkCtx, SourceCtx, WriteReport, WriteSpec,
};
use crate::model::{Batch, Row};
use crate::traits::{MockSink, MockSource, MockStorage};
use crate::types::DataType;
use crate::types::value::Value;

/// Default test ctx schema. Matches `test_flow_named`'s direct
/// mapping (`id` → `id`, Int64). Exposing a `SchemaProvider` lets
/// the runner's `ensure_derived` rebuild without needing a
/// `describe_schema` fallback path.
fn unit_test_schema() -> Schema {
    Schema::new(vec![Field {
        name: "id".into(),
        data_type: DataType::Int64,
        nullable: false,
    }])
}

pub struct UnitSourceCtx;
impl SourceCtx for UnitSourceCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        Some(self)
    }
}
impl SchemaProvider for UnitSourceCtx {
    fn schema(&self) -> &Schema {
        // Static initialiser via OnceLock so the trait method can
        // return a borrow without per-call allocation.
        static SCHEMA: std::sync::OnceLock<Schema> = std::sync::OnceLock::new();
        SCHEMA.get_or_init(unit_test_schema)
    }
}

pub struct UnitSinkCtx;
impl SinkCtx for UnitSinkCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        Some(self)
    }
}
impl SchemaProvider for UnitSinkCtx {
    fn schema(&self) -> &Schema {
        static SCHEMA: std::sync::OnceLock<Schema> = std::sync::OnceLock::new();
        SCHEMA.get_or_init(unit_test_schema)
    }
}

/// Bare `MockSource` with the universal expectation preset:
/// `schemaless = false`. Most validation-pipeline tests only need to
/// layer additional expectations on top — call this and configure the
/// rest via mut access.
pub fn default_source_mock() -> MockSource {
    let mut s = MockSource::new();
    s.expect_schemaless().return_const(false);
    s.expect_body_data_type()
        .returning(|| crate::types::DataType::Json);
    s
}

/// `MockSource` preset for the raw-passthrough Mongo path:
/// `schemaless = true` and `body_data_type = Json` (object-shaped, so
/// `is_object()` is true — what raw-passthrough validation requires).
pub fn raw_passthrough_source_mock() -> MockSource {
    let mut s = MockSource::new();
    s.expect_schemaless().return_const(true);
    s.expect_body_data_type()
        .returning(|| crate::types::DataType::Json);
    s
}

pub fn one_row_batch() -> Batch {
    Batch {
        rows: vec![Row::upsert(vec![Value::Int64(1)])],
        next_cursor: Some(CursorState::new(vec![CursorFieldValue {
            name: "id".into(),
            value: Value::Int64(1),
        }])),
    }
}

pub fn mock_source_ok() -> MockSource {
    let call = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut s = MockSource::new();
    s.expect_schemaless().return_const(false);
    s.expect_body_data_type()
        .returning(|| crate::types::DataType::Json);
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
    s.expect_schemaless().return_const(false);
    s.expect_body_data_type()
        .returning(|| crate::types::DataType::Json);
    s.expect_build_context()
        .returning(|_| Ok(Arc::new(UnitSourceCtx)));
    s.expect_read_batch()
        .returning(|_, _ctx, _| Ok(Batch::default()));
    s
}

pub fn mock_source_no_cursor() -> MockSource {
    let call = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut s = MockSource::new();
    s.expect_schemaless().return_const(false);
    s.expect_body_data_type()
        .returning(|| crate::types::DataType::Json);
    s.expect_build_context()
        .returning(|_| Ok(Arc::new(UnitSourceCtx)));
    s.expect_read_batch().returning(move |_, _ctx, _| {
        let n = call.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            Ok(Batch {
                rows: vec![Row::upsert(vec![Value::Int64(1)])],
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
    s.expect_schemaless().return_const(false);
    s.expect_body_data_type()
        .returning(|| crate::types::DataType::Json);
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
    s.expect_schemaless().return_const(false);
    s.expect_supports_deletes().return_const(true);
    s.expect_describe_schema().returning(|_| {
        Ok(crate::model::Schema::new(vec![crate::model::Field {
            name: "id".into(),
            data_type: crate::types::DataType::Int64,
            nullable: false,
        }]))
    });
    s.expect_build_context()
        .returning(|_| Ok(Arc::new(UnitSinkCtx)));
    s.expect_write_batch().returning(|_, _ctx, batch, _dry| {
        Ok(WriteReport {
            rows_written: batch.rows.len() as u64,
        })
    });
    s
}

pub fn mock_storage_ok() -> MockStorage {
    let mut s = MockStorage::new();
    s.expect_load_cursor().returning(|_| Ok(None));
    s.expect_save_cursor().returning(|_, _, _| Ok(()));
    s
}

pub fn mock_storage_save_fails() -> MockStorage {
    let mut s = MockStorage::new();
    s.expect_load_cursor().returning(|_| Ok(None));
    s.expect_save_cursor()
        .returning(|_, _, _| Err(RuntimeError::Other("storage boom".into())));
    s
}

pub fn test_flow_named(
    name: &str,
    mut source: MockSource,
    sink: MockSink,
    storage: MockStorage,
) -> FlowState {
    // mockall: the validation pipeline groups by `source.name()`, so the
    // mock must answer if any test routes through `validate`. Runner-only
    // tests never call it but having the expectation set is harmless.
    source.expect_name().return_const(name.to_string());
    let assembled = AssembledFlow {
        name: name.into(),
        source: Arc::new(source),
        sink: Arc::new(sink),
        storage: Arc::new(storage),
        rules: vec![crate::mapping::ColumnMapping::Direct {
            from: "id".into(),
            to: "id".into(),
            truncate: false,
            default_literal: None,
        }],
        config_read_spec: ConfigReadSpec {
            table: "public.t".into(),
            cursor_fields: vec!["id".into()],
            cursor_order: CursorOrder::Asc,
            limit: 1,
            source_options: toml::Table::new(),
        },
        config_write_spec: ConfigWriteSpec {
            table: "public.t".into(),
            conflict: None,
        },
        interval: Duration::from_millis(10),
        query_timeout: Duration::from_secs(5),
        sampling: crate::config::validation::SamplingConfig::Disabled,
        access_check: true,
        fields_check: true,
        inserts_check: true,
        cursor_persistence: crate::model::CursorPersistence::ColumnCursor,
    };
    let derived = DerivedPlans {
        transform: crate::transform::Transform::new(
            vec![crate::transform::TransformOp::Take { source_index: 0 }],
            vec!["id".into()],
        ),
        read_spec: ReadSpec {
            columns: vec!["id".into()],
            table: "public.t".into(),
            cursor_fields: vec!["id".into()],
            cursor_order: CursorOrder::Asc,
            limit: 1,
            source_options: toml::Table::new(),
            needs_body: false,
        },
        write_spec: WriteSpec {
            columns: vec!["id".into()],
            table: "public.t".into(),
            conflict: None,
        },
    };
    FlowState::new(assembled, derived)
}

pub fn test_flow(source: MockSource, sink: MockSink, storage: MockStorage) -> FlowState {
    test_flow_named("test_flow", source, sink, storage)
}
