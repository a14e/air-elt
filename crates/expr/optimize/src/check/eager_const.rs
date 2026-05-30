//! Eager constant-evaluation check.

use air_elt_types::Value;

use super::{Check, CheckCx};
use crate::error::OptimizeError;
use crate::model::opt_expr::OptExpr;

/// Fully evaluate an all-constant pure call in an eager position. A constant
/// call that const folding could not reduce only survives because evaluation
/// failed (e.g. `1 / 0`); reproduce that failure as a compile error. Skipped in
/// lazy positions, where the failure depends on the path being taken.
pub(crate) struct EagerConstEval;

impl Check for EagerConstEval {
    fn check(&self, node: &OptExpr, eager: bool, cx: &CheckCx) -> Result<(), OptimizeError> {
        if !eager {
            return Ok(());
        }
        let OptExpr::Call { func, args } = node else {
            return Ok(());
        };
        let constants: Option<Vec<Value>> =
            args.iter().map(|arg| arg.as_const().cloned()).collect();
        let Some(values) = constants else {
            return Ok(());
        };
        let function = cx.registry.get_by_ref(*func);
        let const_flags = vec![true; values.len()];
        if !function.purity(&const_flags) {
            return Ok(());
        }
        match function.evaluate(values, cx.eval_context) {
            Ok(_) => Ok(()),
            Err(error) => Err(OptimizeError::ConstEval {
                function: function.name().to_owned(),
                error: error.to_string(),
            }),
        }
    }
}
