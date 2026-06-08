//! Type-gated power reduction: `pow(x, 0)` and `pow(x, 1)`.
//!
//! `power` always resolves to `Float64`, and its Float64 evaluation is
//! `x.powf(exp)`. Only the exponents whose `powf` result is an EXACT, portable
//! identity are reduced:
//!
//! - `pow(x, 1) → x` (`powf(x, 1.0) == x`).
//! - `pow(x, 0) → 1.0` when `x` is non-null and infallible — `powf(_, 0.0)` is
//!   `1.0` for every finite/NaN/Inf operand, but `pow(null, 0)` is `null`, so a
//!   nullable/fallible `x` must keep the call.
//!
//! `pow(x, 2) → x*x` is deliberately **NOT** done: `powf(x, 2.0)` is not
//! guaranteed bit-equal to `x*x` (`powf` is not correctly-rounded on every libm),
//! and `powf(x, 3.0) != (x*x)*x` for a large fraction of inputs — so the square/
//! cube reduction would silently change per-row values. An exact integer power
//! would require lowering to integer multiplication; left to a later step.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::{DataType, Value};

use super::engine::{TypedRewrite, TypedRule, TypedRuleCx};
use crate::model::opt_expr::OptExpr;
use crate::util::type_utils::is_droppable;

pub(crate) struct PowerReduce {
    power: Option<FuncRef>,
}

impl PowerReduce {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        Self {
            power: registry.get_ref("power", Some(2)).ok(),
        }
    }
}

impl TypedRule for PowerReduce {
    fn apply(&self, node: OptExpr, cx: &TypedRuleCx) -> TypedRewrite {
        let OptExpr::Call { id, func, args } = node else {
            return TypedRewrite::Same(node);
        };
        let unchanged = |args| TypedRewrite::Same(OptExpr::Call { id, func, args });
        if Some(func) != self.power || args.len() != 2 {
            return unchanged(args);
        }
        // The exponent must be a `0`/`1` constant; the base must be `Float64`.
        let Some(exponent) = args[1].as_const().and_then(unit_exponent) else {
            return unchanged(args);
        };
        let base_is_float = cx
            .type_map
            .get(&args[0].id())
            .is_some_and(|base_type| base_type.data_type == DataType::Float64);
        if !base_is_float {
            return unchanged(args);
        }
        // `pow(x, 0)` drops `x`, so it needs the non-null + infallible gate.
        if exponent == 0 && !is_droppable(&args[0], cx) {
            return unchanged(args);
        }
        let mut args = args;
        let base = args.swap_remove(0); // args was [base, exp]; take the base
        match exponent {
            1 => TypedRewrite::Changed(base),
            _ => TypedRewrite::Changed(OptExpr::Const(id, Value::Float64(1.0))),
        }
    }
}

/// The exponent if the constant is `0` or `1` (an integer literal or an exact
/// float like `1.0`).
fn unit_exponent(value: &Value) -> Option<i64> {
    let exponent = match value {
        Value::Int8(v) => i64::from(*v),
        Value::Int16(v) => i64::from(*v),
        Value::Int32(v) => i64::from(*v),
        Value::Int64(v) => *v,
        Value::Float32(v) if v.fract() == 0.0 => *v as i64,
        Value::Float64(v) if v.fract() == 0.0 => *v as i64,
        _ => return None,
    };
    (0..=1).contains(&exponent).then_some(exponent)
}
