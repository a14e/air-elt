//! Const folding: evaluate subtrees that are fully known at compile time.
//!
//! Three rules, all reducing a node to a single [`OptExpr::Const`]:
//! * [`ConstFold`] — a function/operator call whose every argument is constant
//!   and whose [`purity`](air_elt_expr_funcs::ExprFunction::purity) admits
//!   compile-time evaluation.
//! * [`InterpolationFold`] — an interpolation whose every segment is literal
//!   text or a constant expression.
//! * [`ObjectFold`] — an object literal whose every value is constant.
//!
//! Folding mirrors the canonical heap evaluator. If evaluation *errors* (e.g.
//! `1 / 0`), the call is left in place rather than failing the build on the
//! spot: it may sit in a branch [`super::dce`] is about to prune, in which case
//! the error never reaches the runtime. Once the rewrite fixpoint converges,
//! the post-pass static [`check`](crate::check) re-evaluates any surviving
//! constant call that sits in an always-reached position and turns its failure
//! into a compile-time [`OptimizeError::ConstEval`].

use air_elt_expr_funcs::{FuncArgVec, OwnedArgWindow};
use air_elt_expr_types::limits::MAX_EXPR_STRING_BYTES;
use air_elt_types::Value;
use air_elt_types::value_to_string;

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::OptExpr;

pub(crate) struct ConstFold;

impl Rule for ConstFold {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let OptExpr::Call { func, args } = node else {
            return Rewrite::Same(node);
        };

        // Cheap pre-scan first: bail without cloning anything if any argument is
        // non-constant, or if the call is not compile-time pure. Only once the
        // whole call is known foldable do we clone the argument values.
        if !args.iter().all(|arg| arg.as_const().is_some()) {
            return Rewrite::Same(OptExpr::Call { func, args });
        }

        let function = cx.registry.get_by_ref(func);
        let const_flags = vec![true; args.len()];
        if !function.purity(&const_flags) {
            return Rewrite::Same(OptExpr::Call { func, args });
        }

        // Every argument is constant and the call is pure: it is fully known at
        // compile time. A successful result folds to a constant; a failure
        // leaves the call untouched for the static `check` pass (its
        // `EagerConstEval`) to judge once dead branches are gone.
        let values: FuncArgVec = args
            .iter()
            .map(|arg| arg.as_const().expect("pre-scanned as constant").clone())
            .collect();
        let mut window = OwnedArgWindow::create(values);
        match function.evaluate(&mut window, cx.eval_context) {
            Ok(value) => Rewrite::Changed(OptExpr::Const(value)),
            Err(_) => Rewrite::Same(OptExpr::Call { func, args }),
        }
    }
}

pub(crate) struct InterpolationFold;

impl Rule for InterpolationFold {
    fn apply(&self, node: OptExpr, _cx: &RuleCx) -> Rewrite {
        let OptExpr::Interpolation(segments) = node else {
            return Rewrite::Same(node);
        };

        // Pre-scan: bail without building any string if a segment is
        // non-constant. Only an all-constant interpolation is rendered.
        if !segments.iter().all(|segment| segment.as_const().is_some()) {
            return Rewrite::Same(OptExpr::Interpolation(segments));
        }

        let mut rendered = String::new();
        for segment in &segments {
            let value = segment.as_const().expect("pre-scanned as constant");
            rendered.push_str(&value_to_string(value));
            // Preserve the runtime size cap: an oversized fold would mask the
            // error the runtime must raise, so leave it for evaluation.
            if rendered.len() > MAX_EXPR_STRING_BYTES {
                return Rewrite::Same(OptExpr::Interpolation(segments));
            }
        }

        Rewrite::Changed(OptExpr::Const(Value::Text(rendered)))
    }
}

pub(crate) struct ObjectFold;

impl Rule for ObjectFold {
    fn apply(&self, node: OptExpr, _cx: &RuleCx) -> Rewrite {
        let OptExpr::Object(entries) = node else {
            return Rewrite::Same(node);
        };

        // Pre-scan: bail without building the map if any value is non-constant.
        if !entries
            .iter()
            .all(|(_, value)| matches!(value, OptExpr::Const(_)))
        {
            return Rewrite::Same(OptExpr::Object(entries));
        }

        let mut fields = Vec::with_capacity(entries.len());
        for (key, value) in &entries {
            let OptExpr::Const(constant) = value else {
                continue; // pre-scanned: every value is constant
            };
            fields.push((key.clone(), constant.clone()));
        }

        Rewrite::Changed(OptExpr::Const(Value::Object(fields)))
    }
}
