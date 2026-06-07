//! Non-constant `field(...)` argument check.

use super::{Check, CheckCx};
use crate::error::OptimizeError;
use crate::model::opt_expr::OptExpr;

/// A `field(...)` that survived optimization carries a non-constant column name
/// (`field("x")` and the backtick form already collapsed to a resolved column).
/// The column a field reads must be statically known, so this is a compile
/// error in any position.
pub(crate) struct FieldArgCheck;

impl Check for FieldArgCheck {
    fn check(&self, node: &OptExpr, _eager: bool, _cx: &CheckCx) -> Result<(), OptimizeError> {
        match node {
            OptExpr::Field(..) => Err(OptimizeError::NonConstFieldArg),
            _ => Ok(()),
        }
    }
}
