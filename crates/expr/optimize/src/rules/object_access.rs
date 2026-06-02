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
//! so a value that [`can_fail`](crate::fallibility) must still be evaluated even
//! when its key is not the one read. Each rewrite therefore fires only when the
//! values it would drop are infallible. The matched value of an `objectGet` hit
//! is returned (and thus still evaluated), so only the *other* entries gate that
//! case; a miss / `objectLength` / `objectHasKey` drops every value, so all must
//! be infallible. Object literals never carry duplicate keys (the converter
//! rejects them), so a key matches at most one entry.

use air_elt_expr_funcs::FuncRef;
use air_elt_types::Value;

use super::{Rewrite, Rule, RuleCx};
use crate::fallibility::can_fail;
use crate::model::opt_expr::OptExpr;

pub(crate) struct ObjectAccessFold;

impl Rule for ObjectAccessFold {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let OptExpr::Call { func, args } = node else {
            return Rewrite::Same(node);
        };
        // Only an object-access call over a literal object first argument.
        if !matches!(args.first(), Some(OptExpr::Object(_))) {
            return Rewrite::Same(OptExpr::Call { func, args });
        }
        match cx.registry.get_by_ref(func).name() {
            "objectGet" => fold_object_get(func, args, cx),
            "objectHasKey" => fold_object_has_key(func, args, cx),
            "objectLength" => fold_object_length(func, args, cx),
            _ => Rewrite::Same(OptExpr::Call { func, args }),
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
        Some(OptExpr::Object(entries)) => entries,
        _ => &[],
    }
}

fn fold_object_get(func: FuncRef, args: Vec<OptExpr>, cx: &RuleCx) -> Rewrite {
    let Some(key) = const_text_key(&args) else {
        return Rewrite::Same(OptExpr::Call { func, args });
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
        return Rewrite::Same(OptExpr::Call { func, args });
    }

    match matched {
        None => Rewrite::Changed(OptExpr::Const(Value::Null)),
        Some(index) => {
            let mut args = args;
            let OptExpr::Object(mut entries) = args.swap_remove(0) else {
                unreachable!("first argument confirmed Object")
            };
            Rewrite::Changed(entries.swap_remove(index).1)
        }
    }
}

fn fold_object_has_key(func: FuncRef, args: Vec<OptExpr>, cx: &RuleCx) -> Rewrite {
    let Some(key) = const_text_key(&args) else {
        return Rewrite::Same(OptExpr::Call { func, args });
    };
    let entries = object_entries(&args);
    // The answer is static, but every value is still dropped — keep the call
    // when any could fail so its error still surfaces.
    if entries
        .iter()
        .any(|(_, value)| can_fail(value, cx.registry))
    {
        return Rewrite::Same(OptExpr::Call { func, args });
    }
    let present = entries.iter().any(|(name, _)| name == &key);
    Rewrite::Changed(OptExpr::Const(Value::Bool(present)))
}

fn fold_object_length(func: FuncRef, args: Vec<OptExpr>, cx: &RuleCx) -> Rewrite {
    let entries = object_entries(&args);
    if entries
        .iter()
        .any(|(_, value)| can_fail(value, cx.registry))
    {
        return Rewrite::Same(OptExpr::Call { func, args });
    }
    let length = entries.len() as i64;
    Rewrite::Changed(OptExpr::Const(Value::Int64(length)))
}
