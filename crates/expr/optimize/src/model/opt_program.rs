//! The lowered, heap-form program (`OptProgram`) the optimizer rewrites.
//!
//! It pairs the ordered variable bindings with the result expression. Like
//! [`OptExpr`](crate::model::opt_expr::OptExpr) it is an internal IR type — only
//! the compacted [`CompactProgram`](crate::model::program::CompactProgram) is
//! public.

use crate::model::opt_expr::OptExpr;

/// A lowered program: ordered bindings plus a result expression.
#[derive(Debug)]
pub(crate) struct OptProgram {
    pub(crate) statements: Vec<OptStatement>,
    pub(crate) result: OptExpr,
    pub(crate) register_count: u16,
}

/// A lowered variable binding: evaluate `value` into `register`.
#[derive(Debug)]
pub(crate) struct OptStatement {
    pub(crate) register: u16,
    pub(crate) value: OptExpr,
}
