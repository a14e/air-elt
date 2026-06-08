//! Self-comparison folding: `x ⋈ x` for a comparison operator `⋈`.
//!
//! When both operands of a comparison are the SAME pure, infallible expression,
//! the result is value-independent and folds to a boolean constant. Two gates are
//! always required:
//!
//! - **structural identity** (`args[0] == args[1]`, which ignores node ids) — the
//!   two sides are the same expression, AND
//! - **pure + infallible** — the operand is evaluated twice and then dropped, so
//!   both evaluations must agree (`random() > random()` is NOT always `false`, so
//!   the purity gate excludes it) and neither may raise (dropping it would drop the
//!   error the original raised).
//!
//! Beyond that the soundness splits by operator and operand type:
//!
//! - **`x > x` / `x < x` → `false`** for EVERY operand. `NaN > NaN` is `false`, the
//!   ordering comparisons return `false` on a null operand, and `v > v` is `false`
//!   for any concrete value — so no type or null gate is needed.
//! - **`x == x` → `true` / `x != x` → `false`** for **non-float** operands. `==`/
//!   `!=` treat null as a value (`null == null` is `true`), so this holds even for
//!   a nullable operand. Floats are excluded: `NaN == NaN` is `false`, so the fold
//!   would be wrong.
//! - **`x >= x` / `x <= x` → `true`** for **non-float, non-null** operands. These
//!   return `false` on a null operand, so the operand must be non-null; floats are
//!   excluded for the same NaN reason.
//!
//! Float self-equality is deliberately left untouched (it is the canonical NaN
//! test) — folding it would need an `isNaN`-based rewrite, parked for later.
//! Constant operands never reach here: `equals(c, c)` already const-folds in the
//! untyped pass.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::Value;

use super::engine::{TypedRewrite, TypedRule, TypedRuleCx};
use crate::model::opt_expr::OptExpr;
use crate::util::fallibility::can_fail;
use crate::util::type_utils::{is_float, is_pure};

pub(crate) struct SelfCompare {
    equals: Option<FuncRef>,
    not_equals: Option<FuncRef>,
    greater: Option<FuncRef>,
    less: Option<FuncRef>,
    greater_eq: Option<FuncRef>,
    less_eq: Option<FuncRef>,
}

impl SelfCompare {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        let get = |name| registry.get_ref(name, Some(2)).ok();
        Self {
            equals: get("equals"),
            not_equals: get("notEquals"),
            greater: get("greater"),
            less: get("less"),
            greater_eq: get("greaterOrEquals"),
            less_eq: get("lessOrEquals"),
        }
    }

    fn is_comparison(&self, func: FuncRef) -> bool {
        let some = Some(func);
        some == self.equals
            || some == self.not_equals
            || some == self.greater
            || some == self.less
            || some == self.greater_eq
            || some == self.less_eq
    }
}

impl TypedRule for SelfCompare {
    fn apply(&self, node: OptExpr, cx: &TypedRuleCx) -> TypedRewrite {
        let OptExpr::Call { id, func, args } = node else {
            return TypedRewrite::Same(node);
        };
        if !self.is_comparison(func) {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        }
        // The folded boolean, or `None` to leave the call unchanged. Computed
        // inside a block so the operand borrows are released before we move `args`.
        let folded: Option<bool> = {
            let [left, right] = &args[..] else {
                return TypedRewrite::Same(OptExpr::Call { id, func, args });
            };
            // Same expression on both sides, evaluated twice then dropped → both
            // evaluations must agree and neither may raise.
            if left != right || !is_pure(left, cx.registry) || can_fail(left, cx.registry) {
                None
            } else if Some(func) == self.greater || Some(func) == self.less {
                // `x > x` / `x < x` is `false` for every operand (NaN and null
                // included) — no type or null gate.
                Some(false)
            } else if let Some(operand_type) = cx.type_map.get(&left.id()) {
                let non_float = !is_float(&operand_type.data_type);
                if !non_float {
                    None // float self-equality is the NaN test — leave it
                } else if Some(func) == self.equals {
                    Some(true) // null == null is true, so nullable is fine
                } else if Some(func) == self.not_equals {
                    Some(false)
                } else if (Some(func) == self.greater_eq || Some(func) == self.less_eq)
                    && !operand_type.nullable
                {
                    // `>=` / `<=` return false on a null operand, so require non-null.
                    Some(true)
                } else {
                    None
                }
            } else {
                None // operand type unknown → cannot prove the non-float gate
            }
        };
        match folded {
            Some(value) => TypedRewrite::Changed(OptExpr::Const(id, Value::Bool(value))),
            None => TypedRewrite::Same(OptExpr::Call { id, func, args }),
        }
    }
}
