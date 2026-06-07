//! Label-matched encode/decode round-trip collapse.
//!
//! `decode(encode(x, A), B)` → `TypeAssert{Bytes, Identity}` over `x`, but ONLY
//! when `A` and `B` are equal constant algorithm labels from the binary-to-text
//! allow-list (`hex` / `base64` / `base64url`). For those algorithms `encode` is
//! a TOTAL function over `Bytes` (it never fails) and `decode` of its output
//! reproduces the original bytes exactly, so the pair is value- and
//! type-preserving: null `x` → `Null`, non-`Bytes` `x` → the same `TypeMismatch`
//! `encode` raised, `Bytes` `x` → `x`.
//!
//! The label match is the crux the unary [`round_trip`](super::round_trip) table
//! cannot express. A mismatched (`A != B`) or non-constant label is left alone —
//! the result genuinely differs (or is unknown). An equal-but-invalid label can
//! never reach here: it is in neither allow-list, and the const-args check
//! already fails the build on it. Only `decode(encode(...))` collapses, never
//! `encode(decode(...))` — `decode` can fail on malformed text, so that error
//! must be preserved.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::Value;

use super::{Rewrite, Rule, RuleCx};
use crate::model::node_id::{NodeCounter, NodeId};
use crate::model::opt_expr::{AssertYield, OptExpr};
use crate::model::program::TypeClass;

/// The binary-to-text algorithms whose `decode∘encode` is a total round-trip.
/// Mirrors `ENCODE_ALGORITHMS` in the `encoding` builtin.
const ROUND_TRIP_ALGORITHMS: [&str; 3] = ["hex", "base64", "base64url"];

fn is_round_trip_algorithm(label: &str) -> bool {
    ROUND_TRIP_ALGORITHMS.contains(&label)
}

pub(crate) struct EncodeRoundTrip {
    encode: Option<FuncRef>,
    decode: Option<FuncRef>,
}

impl EncodeRoundTrip {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        Self {
            encode: registry.get_ref("encode", Some(2)).ok(),
            decode: registry.get_ref("decode", Some(2)).ok(),
        }
    }
}

impl Rule for EncodeRoundTrip {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let (encode, decode) = match (self.encode, self.decode) {
            (Some(encode), Some(decode)) => (encode, decode),
            _ => return Rewrite::Same(node),
        };
        let counter = cx.node_counter;
        let OptExpr::Call { id, func, args } = node else {
            return Rewrite::Same(node);
        };
        if func != decode || args.len() != 2 {
            return Rewrite::Same(OptExpr::Call { id, func, args });
        }

        // The outer label `B` must be a constant round-trip algorithm.
        let outer_label = match &args[1] {
            OptExpr::Const(_, Value::Text(label)) if is_round_trip_algorithm(label) => {
                label.clone()
            }
            _ => return Rewrite::Same(OptExpr::Call { id, func, args }),
        };

        let mut args = args;
        let inner = args.swap_remove(0); // `encode(x, A)`; `args` now holds `[B]`.
        match inner {
            OptExpr::Call {
                func: inner_func,
                args: inner_args,
                ..
            } if inner_func == encode && inner_args.len() == 2 => {
                let labels_match = matches!(
                    &inner_args[1],
                    OptExpr::Const(_, Value::Text(inner_label)) if *inner_label == outer_label
                );
                if !labels_match {
                    return restore(id, func, inner_func, inner_args, outer_label, counter);
                }
                let mut inner_args = inner_args;
                let operand = inner_args.swap_remove(0);
                Rewrite::Changed(OptExpr::TypeAssert {
                    id: counter.fresh_id(),
                    inner: Box::new(operand),
                    expect: TypeClass::Bytes,
                    on_present: AssertYield::Identity,
                })
            }
            other => Rewrite::Same(OptExpr::Call {
                id,
                func,
                args: vec![
                    other,
                    OptExpr::Const(counter.fresh_id(), Value::Text(outer_label)),
                ],
            }),
        }
    }
}

/// Rebuild `decode(encode(x, A), B)` unchanged when the labels did not match.
fn restore(
    decode_id: NodeId,
    decode: FuncRef,
    encode: FuncRef,
    inner_args: Vec<OptExpr>,
    outer_label: String,
    counter: &NodeCounter,
) -> Rewrite {
    let inner = OptExpr::Call {
        id: counter.fresh_id(),
        func: encode,
        args: inner_args,
    };
    Rewrite::Same(OptExpr::Call {
        id: decode_id,
        func: decode,
        args: vec![
            inner,
            OptExpr::Const(counter.fresh_id(), Value::Text(outer_label)),
        ],
    })
}
