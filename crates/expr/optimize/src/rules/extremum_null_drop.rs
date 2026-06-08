//! Dropping NULL literals from `min`/`max`.
//!
//! `min`/`max` skip NULL arguments and yield NULL only when EVERY argument is
//! NULL (`fold_extremum`'s SQL semantics). So a literal `null` operand
//! contributes nothing and may be removed without consulting any type — the
//! rewrite is value-exact regardless of the other operands' types:
//!
//! - `max(a, null, b)` → `max(a, b)` (drop the null literal),
//! - `max(x, null)` → `x` (a one-argument extremum is just its argument),
//! - `max(null, null)` → `null` (every argument was a null literal).
//!
//! This is purely structural (the null-skip rule is type-independent), so it
//! lives in the untyped fixpoint rather than the typed pass. It also pre-empts a
//! spurious type error: the null literal currently resolves to `Bool`, so a
//! mixed `max(int_field, null)` would otherwise fail `comparable_join`; dropping
//! the null first leaves a well-typed extremum (or none at all).

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::Value;

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::OptExpr;

pub(crate) struct ExtremumNullDrop {
    extremums: Vec<FuncRef>,
}

impl ExtremumNullDrop {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        // `None` arity selects the single variadic `min`/`max` overload, which a
        // call of any arity resolves to.
        let extremums = ["min", "max"]
            .into_iter()
            .filter_map(|name| registry.get_ref(name, None).ok())
            .collect();
        Self { extremums }
    }
}

impl Rule for ExtremumNullDrop {
    fn apply(&self, node: OptExpr, _cx: &RuleCx) -> Rewrite {
        let OptExpr::Call { id, func, args } = node else {
            return Rewrite::Same(node);
        };
        if !self.extremums.contains(&func) {
            return Rewrite::Same(OptExpr::Call { id, func, args });
        }
        if !args.iter().any(is_null_literal) {
            return Rewrite::Same(OptExpr::Call { id, func, args });
        }
        let mut kept: Vec<OptExpr> = args
            .into_iter()
            .filter(|arg| !is_null_literal(arg))
            .collect();
        match kept.len() {
            // Every argument was a null literal → the extremum is NULL.
            0 => Rewrite::Changed(OptExpr::Const(id, Value::Null)),
            // A single surviving argument: `min`/`max` of one value is that value.
            1 => Rewrite::Changed(kept.swap_remove(0)),
            // Two or more remain: keep the extremum over just the non-null args.
            _ => Rewrite::Changed(OptExpr::Call {
                id,
                func,
                args: kept,
            }),
        }
    }
}

/// Whether `node` is the `null` literal constant.
fn is_null_literal(node: &OptExpr) -> bool {
    matches!(node.as_const(), Some(Value::Null))
}
