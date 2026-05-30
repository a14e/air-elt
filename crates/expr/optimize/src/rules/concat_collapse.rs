//! `concat` algebra over the now-strict `concat` (every argument must be text).
//!
//! [`ConcatCollapse`] cleans up a `concat` call:
//! * empty-string constant arguments contribute nothing, so they are dropped
//!   (`concat(a, "", b)` → `concat(a, b)`);
//! * a `concat` that reduces to a single **dynamic** operand is exactly a string
//!   type-check on it — `concat(x)` (and `concat(x, "")`) → `TypeAssert{String,
//!   Identity}`. Strict `concat` yields `x` unchanged when `x` is text, raises
//!   the same `TypeMismatch` otherwise, and propagates null; the assert
//!   reproduces that contract. A single *constant* operand is left for
//!   [`const_fold`](super::const_fold).
//!
//! [`TrimConcat`] strips whitespace-only constant edges of a concatenation that
//! is then trimmed: in `trim(concat(e1, …, en))`, a leading run of
//! whitespace-only constants is dropped and a leading constant's own leading
//! whitespace is removed (symmetrically at the trailing edge), since `trim`
//! consumes exactly that. The outer `trim` is kept — interior/dynamic segments
//! may still carry whitespace. Whitespace uses the same `str` predicate as the
//! `trim` builtin, so the edge edit matches what `trim` would have removed.

use std::collections::VecDeque;

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::Value;

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::{AssertYield, OptExpr};
use crate::model::program::TypeClass;

/// Whether a node is the constant empty string `""`.
fn is_empty_text(expr: &OptExpr) -> bool {
    matches!(expr, OptExpr::Const(Value::Text(text)) if text.is_empty())
}

pub(crate) struct ConcatCollapse {
    concat: Option<FuncRef>,
}

impl ConcatCollapse {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        Self {
            concat: registry.get_ref("concat", None).ok(),
        }
    }
}

impl Rule for ConcatCollapse {
    fn apply(&self, node: OptExpr, _cx: &RuleCx) -> Rewrite {
        let OptExpr::Call { func, args } = node else {
            return Rewrite::Same(node);
        };
        if Some(func) != self.concat {
            return Rewrite::Same(OptExpr::Call { func, args });
        }

        let original_len = args.len();
        let mut kept: Vec<OptExpr> = args.into_iter().filter(|arg| !is_empty_text(arg)).collect();
        let dropped = kept.len() != original_len;

        match kept.len() {
            // Every argument was an empty string ⇒ the concatenation is "".
            0 => Rewrite::Changed(OptExpr::Const(Value::Text(String::new()))),
            1 => {
                let only = kept.pop().expect("len checked");
                if only.as_const().is_some() {
                    // A single constant folds via const_fold; only report a change
                    // if an empty argument was actually dropped here.
                    let node = OptExpr::Call {
                        func,
                        args: vec![only],
                    };
                    if dropped {
                        Rewrite::Changed(node)
                    } else {
                        Rewrite::Same(node)
                    }
                } else {
                    // Strict concat of a single dynamic operand is a string assert.
                    Rewrite::Changed(OptExpr::TypeAssert {
                        inner: Box::new(only),
                        expect: TypeClass::String,
                        on_present: AssertYield::Identity,
                    })
                }
            }
            _ => {
                let node = OptExpr::Call { func, args: kept };
                if dropped {
                    Rewrite::Changed(node)
                } else {
                    Rewrite::Same(node)
                }
            }
        }
    }
}

pub(crate) struct TrimConcat {
    trim: Option<FuncRef>,
    concat: Option<FuncRef>,
}

impl TrimConcat {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        Self {
            trim: registry.get_ref("trim", Some(1)).ok(),
            concat: registry.get_ref("concat", None).ok(),
        }
    }
}

impl Rule for TrimConcat {
    fn apply(&self, node: OptExpr, _cx: &RuleCx) -> Rewrite {
        let OptExpr::Call { func, args } = node else {
            return Rewrite::Same(node);
        };
        let is_trim = Some(func) == self.trim && args.len() == 1;
        let wraps_concat = is_trim
            && matches!(&args[0], OptExpr::Call { func: inner, .. } if Some(*inner) == self.concat);
        if !wraps_concat {
            return Rewrite::Same(OptExpr::Call { func, args });
        }

        let mut args = args;
        let inner = args.pop().expect("len checked");
        let OptExpr::Call {
            func: concat,
            args: concat_args,
        } = inner
        else {
            // `wraps_concat` already proved the inner shape.
            return Rewrite::Same(OptExpr::Call { func, args });
        };

        let (trimmed_args, changed) = strip_whitespace_edges(concat_args);
        if !changed {
            let inner = OptExpr::Call {
                func: concat,
                args: trimmed_args,
            };
            return Rewrite::Same(OptExpr::Call {
                func,
                args: vec![inner],
            });
        }

        // Every segment was a whitespace-only constant ⇒ `trim` yields "".
        if trimmed_args.is_empty() {
            return Rewrite::Changed(OptExpr::Const(Value::Text(String::new())));
        }
        let inner = OptExpr::Call {
            func: concat,
            args: trimmed_args,
        };
        Rewrite::Changed(OptExpr::Call {
            func,
            args: vec![inner],
        })
    }
}

/// Drop whitespace-only constant segments at the edges of a soon-to-be-trimmed
/// concatenation, and left/right-trim a partially-whitespace constant edge.
/// Returns the rewritten segments and whether anything changed.
fn strip_whitespace_edges(args: Vec<OptExpr>) -> (Vec<OptExpr>, bool) {
    let mut args: VecDeque<OptExpr> = args.into();
    let mut changed = false;

    while let Some(OptExpr::Const(Value::Text(text))) = args.front() {
        let trimmed = text.trim_start();
        if trimmed.is_empty() {
            args.pop_front();
            changed = true;
        } else if trimmed.len() != text.len() {
            let trimmed = trimmed.to_owned();
            args.pop_front();
            args.push_front(OptExpr::Const(Value::Text(trimmed)));
            changed = true;
            break;
        } else {
            break;
        }
    }

    while let Some(OptExpr::Const(Value::Text(text))) = args.back() {
        let trimmed = text.trim_end();
        if trimmed.is_empty() {
            args.pop_back();
            changed = true;
        } else if trimmed.len() != text.len() {
            let trimmed = trimmed.to_owned();
            args.pop_back();
            args.push_back(OptExpr::Const(Value::Text(trimmed)));
            changed = true;
            break;
        } else {
            break;
        }
    }

    (args.into(), changed)
}
