//! Type-gated algebraic identities and annihilation.
//!
//! Two families, both gated on the static type map:
//!
//! - **Identities that keep the operand** (`x*1`, `x/1`, `x+0`, `x-0`, `x|0`,
//!   `x^0`, `x<<0`, `x>>0`, boolean `x&&true`, `x||false`): replace the operation
//!   with the operand. Sound only when the operation's resolved data type already
//!   equals the operand's, so stripping it does not change the program's resolved
//!   type (the int_bound algebra can promote `x+0` to a wider type — then the
//!   identity does NOT fire). These keep the operand's evaluation, so they need no
//!   null/fail gate. Additive identities are restricted to integer operands to
//!   avoid the float `-0.0` subtlety.
//! - **Annihilation that drops the operand** (`x*0→0`, `x&0→0`, `x-x→0`,
//!   `x^x→0`, boolean `x&&false→false`, `x||true→true`): replace with a constant.
//!   Dropping `x` is sound only when `x` is non-null, infallible, and pure (so no
//!   null, error, or non-deterministic value is lost — the [`is_droppable`] gate).
//!   The arithmetic/bitwise annihilations are further restricted to `Int64`
//!   operands — the one integer type that evaluates cleanly through these ops and
//!   has no NaN — so the constant zero is value-exact.
//! - **`min(x,x)→x` / `max(x,x)→x`**: when every argument is the same frozen
//!   operand (so duplicating/dropping it is observation-neutral).
//! - **Saturation against the operand type's bound** (`max(x, c)→c` when
//!   `c ≥ TYPE_MAX(x)`, `min(x, c)→c` when `c ≤ TYPE_MIN(x)`): a fixed-width
//!   integer `x` can never exceed its type's range, so the constant always wins.
//!   `min`/`max` skip NULLs, so the result is `c` even for a NULL `x` — the
//!   dropped operand need only be infallible (`!can_fail`), not non-null. Sound
//!   for fixed-width integers only: `BigInt` is unbounded and floats carry `NaN`
//!   (which `min`/`max` propagate rather than saturate). Type-preserved by the
//!   data-type-match gate.

use std::cmp::Ordering;

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::{DataType, Value, compare_values};

use super::engine::{TypedRewrite, TypedRule, TypedRuleCx};
use crate::model::node_id::NodeId;
use crate::model::opt_expr::OptExpr;
use crate::util::fallibility::can_fail;
use crate::util::type_utils::{
    is_droppable, is_integer_or_bigint, is_integer_zero, is_numeric_one,
};

pub(crate) struct AlgebraicIdentities {
    add: Option<FuncRef>,
    subtract: Option<FuncRef>,
    multiply: Option<FuncRef>,
    divide: Option<FuncRef>,
    bit_and: Option<FuncRef>,
    bit_or: Option<FuncRef>,
    bit_xor: Option<FuncRef>,
    shift_left: Option<FuncRef>,
    shift_right: Option<FuncRef>,
    min: Option<FuncRef>,
    max: Option<FuncRef>,
}

impl AlgebraicIdentities {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        let get = |name| registry.get_ref(name, Some(2)).ok();
        Self {
            add: get("add"),
            subtract: get("subtract"),
            multiply: get("multiply"),
            divide: get("divide"),
            bit_and: get("bitAnd"),
            bit_or: get("bitOr"),
            bit_xor: get("bitXor"),
            shift_left: get("bitShiftLeft"),
            shift_right: get("bitShiftRight"),
            min: get("min"),
            max: get("max"),
        }
    }
}

impl TypedRule for AlgebraicIdentities {
    fn apply(&self, node: OptExpr, cx: &TypedRuleCx) -> TypedRewrite {
        match node {
            OptExpr::Call { id, func, args } => self.rewrite_call(id, func, args, cx),
            OptExpr::And { id, left, right } => self.rewrite_bool(id, true, left, right, cx),
            OptExpr::Or { id, left, right } => self.rewrite_bool(id, false, left, right, cx),
            other => TypedRewrite::Same(other),
        }
    }
}

impl AlgebraicIdentities {
    fn rewrite_call(
        &self,
        id: NodeId,
        func: FuncRef,
        args: Vec<OptExpr>,
        cx: &TypedRuleCx,
    ) -> TypedRewrite {
        let some = Some(func);
        // `min(x,x,…)` / `max(x,x,…)`: every argument the same frozen operand.
        if (some == self.min || some == self.max) && all_same_frozen(&args) {
            let mut args = args;
            let first = args.swap_remove(0);
            return TypedRewrite::Changed(first);
        }
        let [_, _] = args[..] else {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        };

        // Saturation against the operand type's bound: `max(x, c) → c` when `c`
        // reaches `x`'s integer-type MAX, `min(x, c) → c` when `c` reaches its
        // MIN. The dropped operand `x` keeps no value (the constant always wins),
        // so it need only be infallible — null `x` is fine, NULLs are skipped.
        if some == self.max {
            if let Some(kept) = saturates(&args, id, cx, Bound::Max) {
                return strip_to(args, kept);
            }
        }
        if some == self.min {
            if let Some(kept) = saturates(&args, id, cx, Bound::Min) {
                return strip_to(args, kept);
            }
        }

        // Keep-operand identities (operand kept, so no null/fail gate; gated on the
        // result type already equalling the operand's).
        if some == self.multiply {
            if let Some(kept) = keep_if(&args, id, cx, is_numeric_one, KeepSide::Either) {
                return strip_to(args, kept);
            }
        }
        if some == self.divide {
            if let Some(kept) = keep_if(&args, id, cx, is_numeric_one, KeepSide::Right) {
                return strip_to(args, kept);
            }
        }
        if some == self.add {
            if let Some(kept) = keep_int_if(&args, id, cx, is_integer_zero, KeepSide::Either) {
                return strip_to(args, kept);
            }
        }
        if some == self.subtract {
            if let Some(kept) = keep_int_if(&args, id, cx, is_integer_zero, KeepSide::Right) {
                return strip_to(args, kept);
            }
        }
        if some == self.bit_or || some == self.bit_xor {
            if let Some(kept) = keep_if(&args, id, cx, is_integer_zero, KeepSide::Either) {
                return strip_to(args, kept);
            }
        }
        if some == self.shift_left || some == self.shift_right {
            if let Some(kept) = keep_if(&args, id, cx, is_integer_zero, KeepSide::Right) {
                return strip_to(args, kept);
            }
        }

        // Annihilation (operand dropped → constant zero). Gated on the operand
        // being a non-null, infallible, pure `Int64`.
        if (some == self.multiply || some == self.bit_and)
            && annihilates(&args, cx, is_integer_zero)
        {
            return TypedRewrite::Changed(OptExpr::Const(id, Value::Int64(0)));
        }
        if (some == self.subtract || some == self.bit_xor) && self_inverse(&args, cx) {
            return TypedRewrite::Changed(OptExpr::Const(id, Value::Int64(0)));
        }

        TypedRewrite::Same(OptExpr::Call { id, func, args })
    }

    /// `x && c` / `x || c` with a constant boolean operand. `is_and` selects the
    /// connective. Position matters: the LEFT operand of `&&`/`||` is always
    /// evaluated, the RIGHT is short-circuited.
    fn rewrite_bool(
        &self,
        id: NodeId,
        is_and: bool,
        left: Box<OptExpr>,
        right: Box<OptExpr>,
        cx: &TypedRuleCx,
    ) -> TypedRewrite {
        let left_const = left.as_const().and_then(as_bool);
        let right_const = right.as_const().and_then(as_bool);

        // Unit element (`&& true`, `|| false`): the connective collapses to the
        // OTHER operand, unchanged. `true`/`false` is the unit when it equals
        // `is_and` (`true` for AND, `false` for OR).
        let unit = is_and;
        if right_const == Some(unit) {
            return TypedRewrite::Changed(*left); // left always evaluated → kept
        }
        if left_const == Some(unit) {
            return TypedRewrite::Changed(*right);
        }

        // Absorbing element (`&& false → false`, `|| true → true`): the result is
        // the constant regardless of the other operand. The absorbing value is
        // `!is_and` (`false` for AND, `true` for OR).
        let absorb = !is_and;
        if left_const == Some(absorb) {
            // Left is the constant; the right operand is short-circuited away
            // (never evaluated), so dropping it is free.
            return TypedRewrite::Changed(OptExpr::Const(id, Value::Bool(absorb)));
        }
        if right_const == Some(absorb) && is_droppable(&left, cx) {
            // Right is the constant; the left operand is always evaluated, so it
            // may be dropped only when it is non-null and infallible.
            return TypedRewrite::Changed(OptExpr::Const(id, Value::Bool(absorb)));
        }

        TypedRewrite::Same(rebuild_bool(id, is_and, left, right))
    }
}

/// Which operand of a binary call may be the matched constant.
enum KeepSide {
    /// Either operand (commutative ops: `*`, `+`, `|`, `^`).
    Either,
    /// Only the right operand (non-commutative: `/`, `-`, `<<`, `>>`).
    Right,
}

/// If the call has a constant operand matching `is_match` on the allowed side and
/// the call's resolved data type equals the kept operand's, return the index of
/// the operand to keep.
fn keep_if(
    args: &[OptExpr],
    call_id: NodeId,
    cx: &TypedRuleCx,
    is_match: fn(&Value) -> bool,
    side: KeepSide,
) -> Option<usize> {
    let kept = matched_operand(args, is_match, matches!(side, KeepSide::Either))?;
    types_match(cx, call_id, &args[kept]).then_some(kept)
}

/// Like [`keep_if`] but additionally requires the kept operand to be an
/// integer/`BigInt` (the additive identities exclude floats for the `-0.0`
/// subtlety).
fn keep_int_if(
    args: &[OptExpr],
    call_id: NodeId,
    cx: &TypedRuleCx,
    is_match: fn(&Value) -> bool,
    side: KeepSide,
) -> Option<usize> {
    let kept = keep_if(args, call_id, cx, is_match, side)?;
    let kept_is_integer = cx
        .type_map
        .get(&args[kept].id())
        .is_some_and(|kept_type| is_integer_or_bigint(&kept_type.data_type));
    kept_is_integer.then_some(kept)
}

/// The index of the operand to KEEP when the OTHER operand is a constant matching
/// `is_match`. With `commutative`, the constant may be on either side; otherwise
/// only the right operand may be the constant (so index 0 is kept).
fn matched_operand(
    args: &[OptExpr],
    is_match: fn(&Value) -> bool,
    commutative: bool,
) -> Option<usize> {
    let right_is_const = args[1].as_const().is_some_and(is_match);
    if right_is_const {
        return Some(0);
    }
    if commutative && args[0].as_const().is_some_and(is_match) {
        return Some(1);
    }
    None
}

/// Whether dropping BOTH operands of a commutative annihilating op is sound: a
/// constant zero on either side, and the OTHER operand a droppable (non-null,
/// infallible, pure) `Int64`.
fn annihilates(args: &[OptExpr], cx: &TypedRuleCx, is_zero: fn(&Value) -> bool) -> bool {
    let other = if args[1].as_const().is_some_and(is_zero) {
        &args[0]
    } else if args[0].as_const().is_some_and(is_zero) {
        &args[1]
    } else {
        return false;
    };
    is_int64(cx, other) && is_droppable(other, cx)
}

/// Whether the call is `x ⊕ x` for a droppable `Int64` `x` (so `x - x` / `x ^ x`
/// is a constant zero): both operands structurally identical, non-null, infallible
/// `Int64`.
fn self_inverse(args: &[OptExpr], cx: &TypedRuleCx) -> bool {
    args[0] == args[1] && is_int64(cx, &args[0]) && is_droppable(&args[0], cx)
}

/// Whether the node's resolved type is exactly `Int64`.
fn is_int64(cx: &TypedRuleCx, node: &OptExpr) -> bool {
    cx.type_map
        .get(&node.id())
        .is_some_and(|node_type| node_type.data_type == DataType::Int64)
}

/// Whether the call's resolved data type equals the operand's (type preservation
/// for a keep-operand strip).
fn types_match(cx: &TypedRuleCx, call_id: NodeId, operand: &OptExpr) -> bool {
    match (cx.type_map.get(&call_id), cx.type_map.get(&operand.id())) {
        (Some(call_type), Some(operand_type)) => call_type.data_type == operand_type.data_type,
        _ => false,
    }
}

/// Whether every argument is the same frozen operand (Register/SourceField), so
/// `min`/`max` of them is just that operand.
fn all_same_frozen(args: &[OptExpr]) -> bool {
    let Some(first) = args.first() else {
        return false;
    };
    first.frozen_operand().is_some() && args.iter().all(|arg| arg == first)
}

/// Consume the call's args and return the operand at `kept` as the rewrite.
fn strip_to(args: Vec<OptExpr>, kept: usize) -> TypedRewrite {
    let mut args = args;
    TypedRewrite::Changed(args.swap_remove(kept))
}

/// A fixed-width integer type's saturating boundary for a `min`/`max` collapse.
#[derive(Clone, Copy)]
enum Bound {
    /// The type minimum (`min` saturates here).
    Min,
    /// The type maximum (`max` saturates here).
    Max,
}

/// If a binary `min`/`max` has a constant operand at or beyond the OTHER
/// operand's fixed-width-integer bound, return that constant's index — the
/// saturated result. `Bound::Max`: `max(x, c) → c` when `c ≥ TYPE_MAX(x)`.
/// `Bound::Min`: `min(x, c) → c` when `c ≤ TYPE_MIN(x)`. The result is the
/// constant for every value `x` can take (and for null `x`, which is skipped),
/// so `x`'s evaluation is dropped — it must be infallible. The data-type-match
/// gate keeps the rewrite type-preserving.
fn saturates(args: &[OptExpr], call_id: NodeId, cx: &TypedRuleCx, bound: Bound) -> Option<usize> {
    for [const_index, operand_index] in [[1, 0], [0, 1]] {
        let Some(value) = args[const_index].as_const() else {
            continue;
        };
        let Some(operand_type) = cx.type_map.get(&args[operand_index].id()) else {
            continue;
        };
        let Some(limit) = integer_bound(&operand_type.data_type, bound) else {
            continue;
        };
        let saturated = reaches(value, &limit, bound)
            && types_match(cx, call_id, &args[const_index])
            && !can_fail(&args[operand_index], cx.registry);
        if saturated {
            return Some(const_index);
        }
    }
    None
}

/// Whether `value` reaches `limit` in the saturating direction: at or above for
/// `Bound::Max`, at or below for `Bound::Min`, via the cross-numeric total order.
fn reaches(value: &Value, limit: &Value, bound: Bound) -> bool {
    matches!(
        (compare_values(value, limit), bound),
        (Some(Ordering::Greater | Ordering::Equal), Bound::Max)
            | (Some(Ordering::Less | Ordering::Equal), Bound::Min)
    )
}

/// The saturating boundary value of a fixed-width integer type, or `None` for any
/// other type — `BigInt` is unbounded, and floats carry `NaN`, which `min`/`max`
/// propagate rather than saturate.
fn integer_bound(data_type: &DataType, bound: Bound) -> Option<Value> {
    let (min, max) = match data_type {
        DataType::Int8 => (Value::Int8(i8::MIN), Value::Int8(i8::MAX)),
        DataType::Int16 => (Value::Int16(i16::MIN), Value::Int16(i16::MAX)),
        DataType::Int32 => (Value::Int32(i32::MIN), Value::Int32(i32::MAX)),
        DataType::Int64 => (Value::Int64(i64::MIN), Value::Int64(i64::MAX)),
        DataType::UInt8 => (Value::UInt8(u8::MIN), Value::UInt8(u8::MAX)),
        DataType::UInt16 => (Value::UInt16(u16::MIN), Value::UInt16(u16::MAX)),
        DataType::UInt32 => (Value::UInt32(u32::MIN), Value::UInt32(u32::MAX)),
        DataType::UInt64 => (Value::UInt64(u64::MIN), Value::UInt64(u64::MAX)),
        _ => return None,
    };
    Some(match bound {
        Bound::Min => min,
        Bound::Max => max,
    })
}

/// The boolean value of a constant, if it is one.
fn as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(boolean) => Some(*boolean),
        _ => None,
    }
}

/// Reassemble an unchanged `&&`/`||` node.
fn rebuild_bool(id: NodeId, is_and: bool, left: Box<OptExpr>, right: Box<OptExpr>) -> OptExpr {
    if is_and {
        OptExpr::And { id, left, right }
    } else {
        OptExpr::Or { id, left, right }
    }
}
