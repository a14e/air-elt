//! Empty-needle predicate collapse: a substring/prefix/suffix test against a
//! constant empty string over a **dynamic** haystack is always `true`.
//!
//! `contains(x, "")`, `startsWith(x, "")`, `endsWith(x, "")` → `TypeAssert{
//! String, Const(true) }`. Every string contains / starts with / ends with the
//! empty string, so the only work left is the haystack's own type/null contract.
//!
//! **Soundness.** The needle is a constant `""` (infallible — nothing to
//! preserve when dropped), so the rewrite keeps exactly the haystack's contract:
//! `x` null → `Null`, `x` non-string → the same `TypeMismatch` the predicate
//! raised, `x` string → `true`. A non-empty or non-constant needle does not
//! match — those keep the call (a dynamic needle could still fail, and a
//! non-empty one is not unconditionally true).

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::Value;

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::{AssertYield, OptExpr};
use crate::model::program::TypeClass;

pub(crate) struct EmptyNeedle {
    /// The binary `String → String → Bool` predicates that are vacuously true on
    /// an empty needle.
    predicates: Vec<FuncRef>,
}

impl EmptyNeedle {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        let predicates = ["contains", "startsWith", "endsWith"]
            .into_iter()
            .filter_map(|name| registry.get_ref(name, Some(2)).ok())
            .collect();
        Self { predicates }
    }
}

impl Rule for EmptyNeedle {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let OptExpr::Call { id, func, args } = node else {
            return Rewrite::Same(node);
        };

        let needle_is_empty = args.len() == 2
            && self.predicates.contains(&func)
            && matches!(&args[1], OptExpr::Const(_, Value::Text(text)) if text.is_empty());
        if !needle_is_empty {
            return Rewrite::Same(OptExpr::Call { id, func, args });
        }

        // Keep arg 0 (haystack), drop the constant empty needle.
        let mut args = args;
        let haystack = args.swap_remove(0);
        Rewrite::Changed(OptExpr::TypeAssert {
            id: cx.node_counter.fresh_id(),
            inner: Box::new(haystack),
            expect: TypeClass::String,
            on_present: AssertYield::Const(Value::Bool(true)),
        })
    }
}
