use std::any::Any;

use crate::error::{RuntimeError, RuntimeResult};

/// Per-flow read context created by `Source::build_context`.
///
/// Shared via `Arc<dyn SourceCtx>`: the runner holds one clone, every
/// in-flight `read_batch` future holds another. This makes ctx
/// cancellation-safe — dropping the future drops only its clone, the
/// runner-side state survives. Implementations are immutable after
/// construction; all caches must be computed in `build_context`.
pub trait SourceCtx: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

/// Per-flow write context — same ownership model as [`SourceCtx`].
pub trait SinkCtx: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
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
