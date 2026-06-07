//! Type-gated flattening and string-concatenation normalization.
//!
//! Three rewrites, all preserving the node's id (they splice existing children or
//! swap a function), so none mints a fresh node:
//!
//! - **`min`/`max` flatten** — `max(max(a,b),c) → max(a,b,c)`. `min`/`max` reduce
//!   via `compare_values`, so flattening is sound exactly when the operand order
//!   is total and transitive: all the same comparable type, all integer/`BigInt`
//!   (exact cross-width compares), or all floating-point (the NaN-absorbing
//!   reduction is order-independent). It is unsound when an integer/`BigInt`
//!   operand meets a `Float` operand — the `as f64`/`to_f64` compare is lossy past
//!   2^53, breaking transitivity.
//! - **`concat` flatten** — `concat(concat(a,b),c) → concat(a,b,c)`. String
//!   concatenation is associative and concat propagates null uniformly, so
//!   splicing a nested `concat` is always sound (no type gate needed beyond the
//!   nodes already being `concat`s).
//! - **string `+` → `concat`** — `add(a,b) → concat(a,b)` when both operands are
//!   `Text`. The `add` builtin's `Text` arm IS string concatenation (same value,
//!   same null propagation, same `Text` result type), so the swap is sound; doing
//!   it lets a string `a + b + c` chain collapse into one variadic `concat(a,b,c)`
//!   (the conversion splices any operand already a `concat`).
//!
//! Bottom-up traversal means a nested `min`/`max`/`concat` child is already
//! flattened when its parent is reached, so splicing one level per node suffices.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::DataType;

use super::engine::{TypedRewrite, TypedRule, TypedRuleCx};
use crate::model::node_id::NodeId;
use crate::model::opt_expr::OptExpr;
use crate::util::type_utils::{is_float, is_integer_or_bigint};

pub(crate) struct TypedFlatten {
    min: Option<FuncRef>,
    max: Option<FuncRef>,
    add: Option<FuncRef>,
    concat: Option<FuncRef>,
}

impl TypedFlatten {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        Self {
            min: registry.get_ref("min", Some(2)).ok(),
            max: registry.get_ref("max", Some(2)).ok(),
            add: registry.get_ref("add", Some(2)).ok(),
            concat: registry.get_ref("concat", Some(2)).ok(),
        }
    }

    fn is_extremum(&self, func: FuncRef) -> bool {
        self.min == Some(func) || self.max == Some(func)
    }
}

impl TypedRule for TypedFlatten {
    fn apply(&self, node: OptExpr, cx: &TypedRuleCx) -> TypedRewrite {
        let OptExpr::Call { id, func, args } = node else {
            return TypedRewrite::Same(node);
        };
        if self.is_extremum(func) {
            return self.flatten_extremum(id, func, args, cx);
        }
        if Some(func) == self.concat {
            return self.flatten_concat(id, func, args);
        }
        if Some(func) == self.add {
            return self.add_to_concat(id, func, args, cx);
        }
        TypedRewrite::Same(OptExpr::Call { id, func, args })
    }
}

impl TypedFlatten {
    /// `max(max(a,b),c) → max(a,b,c)` when the flattened operand types keep the
    /// compare total and transitive.
    fn flatten_extremum(
        &self,
        id: NodeId,
        func: FuncRef,
        args: Vec<OptExpr>,
        cx: &TypedRuleCx,
    ) -> TypedRewrite {
        let has_nested = args
            .iter()
            .any(|arg| matches!(arg, OptExpr::Call { func: inner, .. } if *inner == func));
        if !has_nested {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        }
        // Gate on the data types of the FLATTENED operand set, read without moving
        // anything (so we can bail unchanged if the gate fails).
        let mut data_types: Vec<&DataType> = Vec::new();
        let mut all_known = true;
        for arg in &args {
            match arg {
                OptExpr::Call {
                    func: inner,
                    args: inner_args,
                    ..
                } if *inner == func => {
                    collect_types(inner_args, cx, &mut data_types, &mut all_known)
                }
                other => collect_types(
                    std::slice::from_ref(other),
                    cx,
                    &mut data_types,
                    &mut all_known,
                ),
            }
        }
        if !all_known || !extremum_safe(&data_types) {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        }
        let flattened = splice_same(args, func);
        TypedRewrite::Changed(OptExpr::Call {
            id,
            func,
            args: flattened,
        })
    }

    /// `concat(concat(a,b),c) → concat(a,b,c)` — always sound (associative).
    fn flatten_concat(&self, id: NodeId, func: FuncRef, args: Vec<OptExpr>) -> TypedRewrite {
        let has_nested = args
            .iter()
            .any(|arg| matches!(arg, OptExpr::Call { func: inner, .. } if *inner == func));
        if !has_nested {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        }
        let flattened = splice_same(args, func);
        TypedRewrite::Changed(OptExpr::Call {
            id,
            func,
            args: flattened,
        })
    }

    /// `add(a,b) → concat(a,b)` when both operands are `Text`, splicing any operand
    /// that is already a `concat` so a string `a + b + c` chain collapses flat.
    fn add_to_concat(
        &self,
        id: NodeId,
        func: FuncRef,
        args: Vec<OptExpr>,
        cx: &TypedRuleCx,
    ) -> TypedRewrite {
        let Some(concat) = self.concat else {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        };
        let [_, _] = args[..] else {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        };
        let both_text = args.iter().all(|arg| is_text(arg, cx));
        if !both_text {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        }
        let spliced = splice_same(args, concat);
        TypedRewrite::Changed(OptExpr::Call {
            id,
            func: concat,
            args: spliced,
        })
    }
}

/// Collect the resolved data types of `nodes` into `out`, clearing `all_known` if
/// any is unknown.
fn collect_types<'a>(
    nodes: &'a [OptExpr],
    cx: &'a TypedRuleCx,
    out: &mut Vec<&'a DataType>,
    all_known: &mut bool,
) {
    for node in nodes {
        match cx.type_map.get(&node.id()) {
            Some(node_type) => out.push(&node_type.data_type),
            None => *all_known = false,
        }
    }
}

/// Splice each direct argument that is a `Call` to `func` inline (its children are
/// consumed, so their ids survive uniquely — no clone), leaving other args as-is.
fn splice_same(args: Vec<OptExpr>, func: FuncRef) -> Vec<OptExpr> {
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
    flattened
}

/// Whether the node's resolved type is `Text`.
fn is_text(node: &OptExpr, cx: &TypedRuleCx) -> bool {
    cx.type_map
        .get(&node.id())
        .is_some_and(|node_type| matches!(node_type.data_type, DataType::Text { .. }))
}

/// Whether a flattened `min`/`max` over these operand types is value-preserving:
/// all the same type, all integer/`BigInt`, or all floating-point. A mix of
/// integer/`BigInt` with float is excluded (lossy compare beyond 2^53).
fn extremum_safe(data_types: &[&DataType]) -> bool {
    if data_types.is_empty() {
        return false;
    }
    let all_same = data_types.windows(2).all(|pair| pair[0] == pair[1]);
    let all_integer = data_types.iter().all(|dt| is_integer_or_bigint(dt));
    let all_float = data_types.iter().all(|dt| is_float(dt));
    all_same || all_integer || all_float
}
