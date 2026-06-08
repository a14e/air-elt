//! Type-gated unary reductions over integer operands.
//!
//! - **`isNaN(x) → false`** when `x` is any integer or `BigInt`: an integer never
//!   converts to a NaN `f64`, so the predicate is statically `false`. The operand
//!   is dropped, so it must be [`is_droppable`] (non-null, infallible, pure) —
//!   `isNaN(null)` is `null`, not `false`, so a nullable operand keeps the call.
//! - **`isInfinite(x) → false`** when `x` is a **fixed-width** integer: its
//!   magnitude is below `2^64`, far under `f64::MAX`, so the `f64` conversion is
//!   always finite. `BigInt` is excluded — a large one overflows `f64` to infinity,
//!   making `isInfinite` genuinely `true`. Same drop gate as `isNaN`.
//! - **`abs(x) → x`** when `x` is an unsigned integer: it is always non-negative,
//!   so `abs` is the identity. `abs` preserves the operand's type, so stripping it
//!   is type-preserving; the operand is KEPT (not dropped), so it needs no
//!   null/fail/purity gate.
//!
//! Constant operands never reach here — `isNaN(1)` / `abs(1)` already const-fold in
//! the untyped pass.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::Value;

use super::engine::{TypedRewrite, TypedRule, TypedRuleCx};
use crate::model::opt_expr::OptExpr;
use crate::util::type_utils::{
    is_droppable, is_fixed_width_integer, is_integer_or_bigint, is_unsigned_integer,
};

pub(crate) struct UnaryReduce {
    is_nan: Option<FuncRef>,
    is_infinite: Option<FuncRef>,
    abs: Option<FuncRef>,
}

impl UnaryReduce {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        let get = |name| registry.get_ref(name, Some(1)).ok();
        Self {
            is_nan: get("isNaN"),
            is_infinite: get("isInfinite"),
            abs: get("abs"),
        }
    }
}

impl TypedRule for UnaryReduce {
    fn apply(&self, node: OptExpr, cx: &TypedRuleCx) -> TypedRewrite {
        let OptExpr::Call { id, func, args } = node else {
            return TypedRewrite::Same(node);
        };
        let some = Some(func);
        let unchanged = |args| TypedRewrite::Same(OptExpr::Call { id, func, args });
        let [operand] = &args[..] else {
            return unchanged(args);
        };

        // `isNaN` / `isInfinite` over an integer operand fold to `false`. The
        // operand is dropped, so it must be droppable; `isInfinite` additionally
        // needs a fixed-width integer (a `BigInt` can overflow `f64` to infinity).
        if some == self.is_nan || some == self.is_infinite {
            let operand_type = cx.type_map.get(&operand.id());
            let type_ok = operand_type.is_some_and(|operand_type| {
                if some == self.is_nan {
                    is_integer_or_bigint(&operand_type.data_type)
                } else {
                    is_fixed_width_integer(&operand_type.data_type)
                }
            });
            if type_ok && is_droppable(operand, cx) {
                return TypedRewrite::Changed(OptExpr::Const(id, Value::Bool(false)));
            }
            return unchanged(args);
        }

        // `abs(x) → x` for an unsigned-integer operand. `abs` preserves the type,
        // so the call's resolved type already equals the operand's — assert that
        // before stripping, keeping the rewrite type-preserving.
        if some == self.abs {
            let call_dt = cx.type_map.get(&id).map(|node_type| &node_type.data_type);
            let operand_dt = cx
                .type_map
                .get(&operand.id())
                .map(|node_type| &node_type.data_type);
            if let (Some(call_dt), Some(operand_dt)) = (call_dt, operand_dt)
                && is_unsigned_integer(operand_dt)
                && call_dt == operand_dt
            {
                let mut args = args;
                return TypedRewrite::Changed(args.swap_remove(0));
            }
        }

        unchanged(args)
    }
}
