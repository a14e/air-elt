pub mod context;
pub mod error;
pub mod patcher;
pub mod program;
pub mod type_resolver;

// The heap (AST) evaluator is retained only as the test-time differential
// oracle: production const / default / switch / patch evaluation all runs
// through the optimizer's arena evaluator (see [`ExpressionContext::evaluate_const`]
// and [`program::RuntimeProgram`]). The proptest in `evaluator` asserts the two
// agree.
#[cfg(test)]
mod evaluator;

pub use context::ExpressionContext;
pub use error::ExprError;
pub use patcher::ConfigExprPatcher;
pub use program::{RowFields, RuntimeProgram};
pub use type_resolver::TypeResolver;
