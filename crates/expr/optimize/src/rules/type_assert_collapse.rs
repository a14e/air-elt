//! Nested-`TypeAssert` collapse: an assert whose operand is an identity-yielding
//! assert of the SAME class is redundant.
//!
//! `TypeAssert{ TypeAssert{x, A, Identity}, A, y }` → `TypeAssert{x, A, y}`. The
//! inner assert yields `x` only when `x` is non-null and of class `A`, so the
//! outer re-check of the same class can never fail where the inner passed — the
//! two layers fold into one, keeping the OUTER yield. Composing rewrites stack
//! these asserts (e.g. `concat(reverse(reverse(x)), "")` becomes a `String`
//! assert wrapped in another); this flattens them.
//!
//! Only an inner `Identity` yield collapses. A `Const` inner yield produces a
//! fixed value whose class is not necessarily `A`, so the outer assert over it is
//! not provably redundant and is left in place.

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::{AssertYield, OptExpr};

pub(crate) struct TypeAssertCollapse;

impl Rule for TypeAssertCollapse {
    fn apply(&self, node: OptExpr, _cx: &RuleCx) -> Rewrite {
        let OptExpr::TypeAssert {
            inner,
            expect,
            on_present,
        } = node
        else {
            return Rewrite::Same(node);
        };

        let inner_is_redundant = matches!(
            &*inner,
            OptExpr::TypeAssert {
                expect: inner_expect,
                on_present: AssertYield::Identity,
                ..
            } if *inner_expect == expect
        );
        if !inner_is_redundant {
            return Rewrite::Same(OptExpr::TypeAssert {
                inner,
                expect,
                on_present,
            });
        }

        match *inner {
            OptExpr::TypeAssert { inner: operand, .. } => Rewrite::Changed(OptExpr::TypeAssert {
                inner: operand,
                expect,
                on_present,
            }),
            // `inner_is_redundant` already proved the inner is a TypeAssert.
            other => Rewrite::Same(OptExpr::TypeAssert {
                inner: Box::new(other),
                expect,
                on_present,
            }),
        }
    }
}
