//! Round-trip collapse: an outer call that inverts its inner call over a
//! **dynamic** operand reduces to a `TypeAssert` (identity yield).
//!
//! `reverse(reverse(x))`, `not(not(x))`, `bytesFromHex(hex(x))`,
//! `bytesFromBase64(base64(x))` → `TypeAssert{ <class>, Identity }`. The pairs
//! live in the flat [`ROUND_TRIPS`] table (`outer`, `inner`, asserted class);
//! `create` resolves the names to [`FuncRef`]s once. The driver walks the tree,
//! so nested round-trips collapse bottom-up.
//!
//! **Soundness.** Only **total** inverses qualify — the inner op must never fail
//! on an operand of the asserted class, AND the pair must be value- AND
//! type-preserving, or eliminating it would change the result. So `reverse`/`not`
//! (involutions) and `hex`/`base64` round-trips (total encode then total decode)
//! qualify; the rewrite keeps exactly the operand's contract: null → `Null`,
//! wrong class → the same `TypeMismatch` the inner op raised, in-class → operand.
//! Deliberately EXCLUDED: `bitNot` (coerces to `i64`, widening narrow ints — a
//! type change), `toSeconds(fromSeconds(n))` (`fromSeconds` range-fails) — both
//! need the type-aware pass (Phase 3).

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::{AssertYield, OptExpr};
use crate::model::program::TypeClass;

/// The replacement table: `outer(inner(x))` collapses to `TypeAssert{class,
/// Identity}`. Involutions repeat the name; encoding round-trips pair a decoder
/// with its encoder. Every entry must be a TOTAL, value+type-preserving inverse
/// over `class` (see the module soundness note).
const ROUND_TRIPS: &[(&str, &str, TypeClass)] = &[
    ("reverse", "reverse", TypeClass::String),
    ("not", "not", TypeClass::Bool),
    ("bytesFromHex", "hex", TypeClass::Bytes),
    ("bytesFromBase64", "base64", TypeClass::Bytes),
];

/// One resolved inverse pair: `outer(inner(x))` asserts `expect` and yields `x`.
struct InversePair {
    outer: FuncRef,
    inner: FuncRef,
    expect: TypeClass,
}

pub(crate) struct RoundTripCollapse {
    inverses: Vec<InversePair>,
}

impl RoundTripCollapse {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        let inverses = ROUND_TRIPS
            .iter()
            .filter_map(|(outer, inner, expect)| {
                let outer = registry.get_ref(outer, Some(1)).ok()?;
                let inner = registry.get_ref(inner, Some(1)).ok()?;
                Some(InversePair {
                    outer,
                    inner,
                    expect: *expect,
                })
            })
            .collect();
        Self { inverses }
    }

    /// The asserted class if `outer` totally inverts `inner`.
    fn matching_class(&self, outer: FuncRef, inner: FuncRef) -> Option<TypeClass> {
        self.inverses
            .iter()
            .find(|pair| pair.outer == outer && pair.inner == inner)
            .map(|pair| pair.expect)
    }
}

impl Rule for RoundTripCollapse {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let OptExpr::Call {
            id,
            func: outer,
            args,
        } = node
        else {
            return Rewrite::Same(node);
        };
        if args.len() != 1 {
            return Rewrite::Same(OptExpr::Call {
                id,
                func: outer,
                args,
            });
        }

        let mut args = args;
        let inner_call = args.swap_remove(0);
        let matched = match &inner_call {
            OptExpr::Call { func, args, .. } if args.len() == 1 => {
                self.matching_class(outer, *func)
            }
            _ => None,
        };
        // A `Some` match already proved `inner_call` is a single-arg `Call`; the
        // catch-all restores the original node for every miss.
        match (matched, inner_call) {
            (
                Some(expect),
                OptExpr::Call {
                    args: mut inner_args,
                    ..
                },
            ) => {
                let operand = inner_args.swap_remove(0);
                Rewrite::Changed(OptExpr::TypeAssert {
                    id: cx.node_counter.fresh_id(),
                    inner: Box::new(operand),
                    expect,
                    on_present: AssertYield::Identity,
                })
            }
            (_, inner_call) => Rewrite::Same(OptExpr::Call {
                id,
                func: outer,
                args: vec![inner_call],
            }),
        }
    }
}
