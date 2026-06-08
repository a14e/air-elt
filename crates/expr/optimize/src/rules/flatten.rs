//! Flattening of nested associative variadic calls.
//!
//! `concat(concat(a, b), c)` → `concat(a, b, c)`. Only genuinely variadic
//! (`max_args == None`) AND associative functions qualify, so flattening
//! neither changes arity validity nor the resolved result type. It removes an
//! intermediate call and exposes adjacent constants to [`super::const_fold`].
//!
//! `min`/`max` are deliberately **excluded**: they are NOT associative under
//! the cross-numeric `compare_values` order, which is lossy for large
//! `Int`/`BigInt` vs `Float` (values ≳ 2^53 collapse onto one `f64`, breaking
//! transitivity). Regrouping such a `min`/`max` chain changes the result value
//! and type. Flattening them is safe only for operands of a single proven
//! numeric type, so it belongs to the type-aware pass (Phase 3), not here.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::OptExpr;

pub(crate) struct Flatten {
    operators: Vec<FuncRef>,
}

impl Flatten {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        // `None` arity selects the variadic overload directly. Only `concat`
        // is truly associative; `min`/`max` are excluded (see module docs).
        let operators = ["concat"]
            .into_iter()
            .filter_map(|name| registry.get_ref(name, None).ok())
            .collect();
        Self { operators }
    }
}

impl Rule for Flatten {
    fn apply(&self, node: OptExpr, _cx: &RuleCx) -> Rewrite {
        let OptExpr::Call { id, func, args } = node else {
            return Rewrite::Same(node);
        };

        if !self.operators.contains(&func) {
            return Rewrite::Same(OptExpr::Call { id, func, args });
        }

        let has_nested = args
            .iter()
            .any(|arg| matches!(arg, OptExpr::Call { func: inner, .. } if *inner == func));
        if !has_nested {
            return Rewrite::Same(OptExpr::Call { id, func, args });
        }

        let mut flattened = Vec::with_capacity(args.len());
        for arg in args {
            match arg {
                OptExpr::Call {
                    func: inner,
                    args: inner_args,
                    ..
                } if inner == func => flattened.extend(inner_args),
                other => flattened.push(other),
            }
        }

        Rewrite::Changed(OptExpr::Call {
            id,
            func,
            args: flattened,
        })
    }
}
