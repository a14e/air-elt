use std::any::Any;

use crate::error::{RuntimeError, RuntimeResult};
use crate::model::Schema;

/// Companion trait for ctx structs that carry a per-flow schema computed
/// once during `build_context`. Implemented by source / sink ctx structs
/// whose backend has an authoritative column schema (Postgres, MySQL).
/// Schemaless backends (MongoDB) implement it conditionally — see A3.
///
/// Lookup is intentionally a plain field read — no caches, no locks.
/// Refresh = drop the ctx Arc and let the runner rebuild on the next
/// tick (`FlowRunner` does this on `RuntimeError::Backend`).
pub trait SchemaProvider {
    fn schema(&self) -> &Schema;
}

/// Per-flow read context created by `Source::build_context`.
///
/// Shared via `Arc<dyn SourceCtx>`: the runner holds one clone, every
/// in-flight `read_batch` future holds another. This makes ctx
/// cancellation-safe — dropping the future drops only its clone, the
/// runner-side state survives. Implementations are immutable after
/// construction; all caches must be computed in `build_context`.
pub trait SourceCtx: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;

    /// Optional view of this ctx as a [`SchemaProvider`]. Default
    /// implementation returns `None` for ctx structs with no
    /// authoritative schema. Concrete ctx types that carry a schema
    /// (PgSourceCtx, MySqlSourceCtx, …) override to `Some(self)`.
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        None
    }
}

/// Per-flow write context — same ownership model as [`SourceCtx`].
pub trait SinkCtx: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;

    /// Optional view of this ctx as a [`SchemaProvider`]. See
    /// [`SourceCtx::as_schema_provider`] for the contract.
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        None
    }
}

impl dyn SourceCtx {
    /// Downcast a borrowed source context to a concrete type.
    pub fn downcast_ref_to<T: Any>(&self) -> RuntimeResult<&T> {
        self.as_any()
            .downcast_ref::<T>()
            .ok_or(RuntimeError::ContextMismatch {
                expected: std::any::type_name::<T>(),
            })
    }
}

impl dyn SinkCtx {
    /// Downcast a borrowed sink context to a concrete type.
    pub fn downcast_ref_to<T: Any>(&self) -> RuntimeResult<&T> {
        self.as_any()
            .downcast_ref::<T>()
            .ok_or(RuntimeError::ContextMismatch {
                expected: std::any::type_name::<T>(),
            })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::{Field, Schema};
    use crate::types::data_type::DataType;

    struct WithSchema {
        schema: Schema,
    }
    impl SourceCtx for WithSchema {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
            Some(self)
        }
    }
    impl SinkCtx for WithSchema {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
            Some(self)
        }
    }
    impl SchemaProvider for WithSchema {
        fn schema(&self) -> &Schema {
            &self.schema
        }
    }

    struct NoSchema;
    impl SourceCtx for NoSchema {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    impl SinkCtx for NoSchema {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    fn sample_schema() -> Schema {
        Schema::new(vec![Field {
            name: "id".into(),
            data_type: DataType::Int64,
            nullable: false,
        }])
    }

    #[test]
    fn source_ctx_with_schema_exposes_provider() {
        let ctx: Box<dyn SourceCtx> = Box::new(WithSchema {
            schema: sample_schema(),
        });
        let provider = ctx.as_schema_provider().expect("provider must be Some");
        assert_eq!(provider.schema().fields().len(), 1);
        assert_eq!(provider.schema().fields()[0].name, "id");
    }

    #[test]
    fn sink_ctx_with_schema_exposes_provider() {
        let ctx: Box<dyn SinkCtx> = Box::new(WithSchema {
            schema: sample_schema(),
        });
        let provider = ctx.as_schema_provider().expect("provider must be Some");
        assert_eq!(provider.schema().fields()[0].data_type, DataType::Int64);
    }

    #[test]
    fn source_ctx_without_schema_returns_none() {
        let ctx: Box<dyn SourceCtx> = Box::new(NoSchema);
        assert!(ctx.as_schema_provider().is_none());
    }

    #[test]
    fn sink_ctx_without_schema_returns_none() {
        let ctx: Box<dyn SinkCtx> = Box::new(NoSchema);
        assert!(ctx.as_schema_provider().is_none());
    }
}
