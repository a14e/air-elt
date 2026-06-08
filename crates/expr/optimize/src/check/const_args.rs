//! Per-function constant-argument validation check.

use air_elt_types::Value;

use super::{Check, CheckCx};
use crate::error::OptimizeError;
use crate::model::opt_expr::OptExpr;

/// Hand each call's constant-argument subset to its own
/// [`validate_const_args`](air_elt_expr_funcs::ExprFunction::validate_const_args)
/// — the function flags a malformed format literal or a categorically invalid
/// constant. Fires in every position (such an argument is never valid).
pub(crate) struct ConstArgsValidation;

impl Check for ConstArgsValidation {
    fn check(&self, node: &OptExpr, _eager: bool, cx: &CheckCx) -> Result<(), OptimizeError> {
        let OptExpr::Call { func, args, .. } = node else {
            return Ok(());
        };
        let const_args: Vec<Option<&Value>> = args.iter().map(OptExpr::as_const).collect();
        let function = cx.registry.get_by_ref(*func);
        function
            .validate_const_args(&const_args, cx.eval_context)
            .map_err(|error| OptimizeError::InvalidConstArg {
                function: function.name().to_owned(),
                error: error.to_string(),
            })
    }
}
