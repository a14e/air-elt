//! `field(...)` collapse.
//!
//! `field(<expr>)` is only meaningful once its argument is a constant string
//! column name. After const folding, two shapes collapse to a resolved
//! [`OptExpr::SourceField`]:
//!
//! * `field("x")`        → the argument folded to `Const(Text "x")`
//! * `field(field("x"))` / `field(` `` `x` `` `)` → the argument already collapsed
//!   to a `SourceField`
//!
//! A `Field` whose argument is neither (a non-const, non-field expression)
//! survives unchanged; the type-check pass (Phase 3) rejects it with the
//! "non-const field argument" rule.

use air_elt_types::Value;

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::OptExpr;

pub(crate) struct FieldCollapse;

impl Rule for FieldCollapse {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let OptExpr::Field(id, inner) = node else {
            return Rewrite::Same(node);
        };

        match *inner {
            OptExpr::Const(_, Value::Text(name)) | OptExpr::SourceField(_, name) => {
                Rewrite::Changed(OptExpr::SourceField(cx.node_counter.fresh_id(), name))
            }
            other => Rewrite::Same(OptExpr::Field(id, Box::new(other))),
        }
    }
}
