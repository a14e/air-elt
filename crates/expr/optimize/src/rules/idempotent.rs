//! Idempotent unary collapse: `f(f(x))` → `f(x)` for an idempotent `f`.
//!
//! `upper`, `lower`, `trim` satisfy `f(f(x)) == f(x)`, so the inner application
//! is redundant. Unlike the round-trip identities ([`super::round_trip`]), the
//! OUTER call survives — so its type/null contract stays inline and no
//! `TypeAssert` is needed: `f(x)` still errors on a non-string `x` exactly as the
//! original did, and still propagates null. Size-reducing (one fewer call), so it
//! runs in the fixpoint.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::OptExpr;

pub(crate) struct IdempotentCollapse {
    operators: Vec<FuncRef>,
}

impl IdempotentCollapse {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        let operators = ["upper", "lower", "trim"]
            .into_iter()
            .filter_map(|name| registry.get_ref(name, Some(1)).ok())
            .collect();
        Self { operators }
    }
}

impl Rule for IdempotentCollapse {
    fn apply(&self, node: OptExpr, _cx: &RuleCx) -> Rewrite {
        let OptExpr::Call { func, args } = node else {
            return Rewrite::Same(node);
        };

        let nested = args.len() == 1
            && self.operators.contains(&func)
            && matches!(
                &args[0],
                OptExpr::Call { func: inner, args: inner_args }
                    if *inner == func && inner_args.len() == 1
            );
        if !nested {
            return Rewrite::Same(OptExpr::Call { func, args });
        }

        // `f(f(x))` → `f(x)`: keep the outer `func`, take the inner call's operand.
        let mut args = args;
        let inner_call = args.swap_remove(0);
        let OptExpr::Call {
            args: inner_args, ..
        } = inner_call
        else {
            return Rewrite::Same(OptExpr::Call {
                func,
                args: vec![inner_call],
            });
        };
        Rewrite::Changed(OptExpr::Call {
            func,
            args: inner_args,
        })
    }
}
