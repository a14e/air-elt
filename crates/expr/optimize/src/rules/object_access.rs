//! Object-literal access folding: simplify an `object*` call whose object
//! argument is a literal `{...}`.
//!
//! An **all-constant** object is already collapsed by
//! [`ObjectFold`](super::const_fold) into a `Const`, and the resulting
//! `objectGet(const, const)` folds via [`ConstFold`](super::const_fold). This
//! rule therefore earns its keep only on objects with **dynamic** values —
//! `objectGet({"k" = field("x")}, "k") → field("x")` — which folding cannot
//! reach.
//!
//! Soundness rests on eager evaluation: building an object evaluates every value,
//! so a value that [`can_fail`](crate::util::fallibility) must still be evaluated even
//! when its key is not the one read. Each rewrite therefore fires only when the
//! values it would drop are infallible. The matched value of an `objectGet` hit
//! is returned (and thus still evaluated), so only the *other* entries gate that
//! case; a miss / `len` / `objectHasKey` drops every value, so all must
//! be infallible. Object literals never carry duplicate keys (the converter
//! rejects them), so a key matches at most one entry. `len` also folds over a
//! literal array (element count) under the same fallibility gate.

use air_elt_expr_funcs::FuncRef;
use air_elt_types::Value;

use super::{Rewrite, Rule, RuleCx};
use crate::model::node_id::NodeId;
use crate::model::opt_expr::OptExpr;
use crate::util::fallibility::can_fail;

pub(crate) struct ObjectAccessFold;

impl Rule for ObjectAccessFold {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let OptExpr::Call { id, func, args } = node else {
            return Rewrite::Same(node);
        };
        // Only a collection-access call over a literal object/array first
        // argument. `objectGet`/`objectHasKey` are object-specific; `len` is
        // polymorphic and folds over both literal kinds.
        let first_is_object = matches!(args.first(), Some(OptExpr::Object(_, _)));
        let first_is_array = matches!(args.first(), Some(OptExpr::Array(_, _)));
        if !first_is_object && !first_is_array {
            return Rewrite::Same(OptExpr::Call { id, func, args });
        }
        match cx.registry.get_by_ref(func).name() {
            "objectGet" if first_is_object => fold_object_get(id, func, args, cx),
            "objectHasKey" if first_is_object => fold_object_has_key(id, func, args, cx),
            "len" => fold_len(id, func, args, cx),
            _ => Rewrite::Same(OptExpr::Call { id, func, args }),
        }
    }
}

/// The constant text key of a 2-arg object-access call, or `None` if the call
/// is not `(object, <const text>)`-shaped.
fn const_text_key(args: &[OptExpr]) -> Option<String> {
    let [_, key] = args else {
        return None;
    };
    match key.as_const() {
        Some(Value::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

/// Borrow the literal-object entries of `args[0]` (the caller has already
/// confirmed the first argument is an `Object`).
fn object_entries(args: &[OptExpr]) -> &[(String, OptExpr)] {
    match args.first() {
        Some(OptExpr::Object(_, entries)) => entries,
        _ => &[],
    }
}

fn fold_object_get(id: NodeId, func: FuncRef, args: Vec<OptExpr>, cx: &RuleCx) -> Rewrite {
    let Some(key) = const_text_key(&args) else {
        return Rewrite::Same(OptExpr::Call { id, func, args });
    };

    let entries = object_entries(&args);
    let matched = entries.iter().position(|(name, _)| name == &key);
    let sound = match matched {
        // Hit: the matched value is returned (still evaluated); only the other
        // entries are dropped, so only they must be infallible.
        Some(index) => entries
            .iter()
            .enumerate()
            .all(|(i, (_, value))| i == index || !can_fail(value, cx.registry)),
        // Miss: every value is dropped, so all must be infallible.
        None => entries
            .iter()
            .all(|(_, value)| !can_fail(value, cx.registry)),
    };
    if !sound {
        return Rewrite::Same(OptExpr::Call { id, func, args });
    }

    match matched {
        None => Rewrite::Changed(OptExpr::Const(cx.node_counter.fresh_id(), Value::Null)),
        Some(index) => {
            let mut args = args;
            let OptExpr::Object(_, mut entries) = args.swap_remove(0) else {
                unreachable!("first argument confirmed Object")
            };
            Rewrite::Changed(entries.swap_remove(index).1)
        }
    }
}

fn fold_object_has_key(id: NodeId, func: FuncRef, args: Vec<OptExpr>, cx: &RuleCx) -> Rewrite {
    let Some(key) = const_text_key(&args) else {
        return Rewrite::Same(OptExpr::Call { id, func, args });
    };
    let entries = object_entries(&args);
    // The answer is static, but every value is still dropped — keep the call
    // when any could fail so its error still surfaces.
    if entries
        .iter()
        .any(|(_, value)| can_fail(value, cx.registry))
    {
        return Rewrite::Same(OptExpr::Call { id, func, args });
    }
    let present = entries.iter().any(|(name, _)| name == &key);
    Rewrite::Changed(OptExpr::Const(
        cx.node_counter.fresh_id(),
        Value::Bool(present),
    ))
}

/// Fold `len(<literal object|array>)` to the static element count. Sound only
/// when no dropped value can fail — building the collection evaluates every
/// value eagerly, so a fallible value must still be evaluated even though `len`
/// ignores the contents.
fn fold_len(id: NodeId, func: FuncRef, args: Vec<OptExpr>, cx: &RuleCx) -> Rewrite {
    let length = match args.first() {
        Some(OptExpr::Object(_, entries)) => {
            if entries
                .iter()
                .any(|(_, value)| can_fail(value, cx.registry))
            {
                return Rewrite::Same(OptExpr::Call { id, func, args });
            }
            entries.len()
        }
        Some(OptExpr::Array(_, elements)) => {
            if elements
                .iter()
                .any(|element| can_fail(element, cx.registry))
            {
                return Rewrite::Same(OptExpr::Call { id, func, args });
            }
            elements.len()
        }
        _ => return Rewrite::Same(OptExpr::Call { id, func, args }),
    };
    Rewrite::Changed(OptExpr::Const(
        cx.node_counter.fresh_id(),
        Value::Int64(length as i64),
    ))
}
