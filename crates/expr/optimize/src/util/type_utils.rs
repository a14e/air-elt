//! Shared helpers for the typed rules: type lookups, the drop-safety gate, and
//! numeric-constant predicates.

use air_elt_expr_funcs::FunctionRegistry;
use air_elt_types::{DataType, Value};

use crate::model::opt_expr::OptExpr;
use crate::model::program::TypeClass;
use crate::typed::engine::TypedRuleCx;
use crate::util::fallibility::can_fail;

/// Whether `node`'s evaluation can be DROPPED without changing observable
/// behaviour: its type is statically known and non-null (so the original's
/// null-propagation is reproduced by a non-null constant), it cannot raise (so
/// dropping it drops no error), AND it is pure (so its single value is the value
/// the original would have produced — a non-deterministic operand cannot be
/// dropped). Required by every annihilation rewrite that discards an operand
/// (`x * 0 → 0`, `x - x → 0`, `x & 0 → 0`, `x && false → false`, …).
pub(crate) fn is_droppable(node: &OptExpr, cx: &TypedRuleCx) -> bool {
    let non_null = cx
        .type_map
        .get(&node.id())
        .is_some_and(|node_type| !node_type.nullable);
    non_null && !can_fail(node, cx.registry) && is_pure(node, cx.registry)
}

/// Whether evaluating `node` is deterministic (same value every time), so
/// dropping or duplicating it is observation-neutral. Conservative: a `Call` is
/// pure only when the function declares itself pure AND every argument is pure;
/// `now`/`today`/`random*` are impure. Leaves (`Const`/`Register`/`SourceField`/
/// `Fields`) are pure — a register is bound once per row, a field read is total
/// and deterministic per row.
pub(crate) fn is_pure(node: &OptExpr, registry: &FunctionRegistry) -> bool {
    match node {
        OptExpr::Const(..)
        | OptExpr::Register(..)
        | OptExpr::SourceField(..)
        | OptExpr::Fields(..) => true,
        OptExpr::Field(_, inner) => is_pure(inner, registry),
        OptExpr::Call { func, args, .. } => {
            registry.get_by_ref(*func).is_pure() && args.iter().all(|arg| is_pure(arg, registry))
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            is_pure(condition, registry)
                && is_pure(then_branch, registry)
                && is_pure(else_branch, registry)
        }
        OptExpr::MultiIf {
            branches, default, ..
        } => {
            branches
                .iter()
                .all(|(condition, value)| is_pure(condition, registry) && is_pure(value, registry))
                && is_pure(default, registry)
        }
        OptExpr::IfNull {
            value, alternative, ..
        } => is_pure(value, registry) && is_pure(alternative, registry),
        OptExpr::NullIf {
            value, sentinel, ..
        } => is_pure(value, registry) && is_pure(sentinel, registry),
        OptExpr::And { left, right, .. } | OptExpr::Or { left, right, .. } => {
            is_pure(left, registry) && is_pure(right, registry)
        }
        OptExpr::Interpolation(_, segments) => {
            segments.iter().all(|segment| is_pure(segment, registry))
        }
        OptExpr::Object(_, entries) => entries.iter().all(|(_, value)| is_pure(value, registry)),
        OptExpr::Switch {
            inputs,
            table,
            default,
            ..
        } => {
            inputs.iter().all(|input| is_pure(input, registry))
                && table.iter().all(|(_, value)| is_pure(value, registry))
                && is_pure(default, registry)
        }
        OptExpr::TypeAssert { inner, .. } => is_pure(inner, registry),
        OptExpr::Block {
            statements, result, ..
        } => {
            statements
                .iter()
                .all(|statement| is_pure(&statement.value, registry))
                && is_pure(result, registry)
        }
    }
}

/// Whether a [`DataType`] satisfies a [`TypeClass`] (the coarse class a
/// `TypeAssert` requires).
pub(crate) fn satisfies(data_type: &DataType, class: &TypeClass) -> bool {
    match class {
        TypeClass::String => matches!(data_type, DataType::Text { .. }),
        TypeClass::Bool => matches!(data_type, DataType::Bool),
        TypeClass::Bytes => matches!(data_type, DataType::Bytes { .. }),
    }
}

/// A signed/unsigned integer or unbounded-magnitude `BigInt` — the types whose
/// cross-comparison and arithmetic are EXACT (no IEEE rounding, no NaN), so the
/// algebraic identities that are unsound for floats hold.
pub(crate) fn is_integer_or_bigint(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::BigInt { .. }
    )
}

/// An IEEE floating-point type (`Float32`/`Float64`).
pub(crate) fn is_float(data_type: &DataType) -> bool {
    matches!(data_type, DataType::Float32 | DataType::Float64)
}

/// Whether `value` is a fixed-width integer-typed zero (signed or unsigned).
/// Excludes float `0.0` (whose `-0.0`/identity subtleties keep the integer-only
/// identities sound) and `BigInt` (a literal `BigInt` zero would already have
/// const-folded; not worth the `num_bigint` dependency here).
pub(crate) fn is_integer_zero(value: &Value) -> bool {
    match value {
        Value::Int8(v) => *v == 0,
        Value::Int16(v) => *v == 0,
        Value::Int32(v) => *v == 0,
        Value::Int64(v) => *v == 0,
        Value::UInt8(v) => *v == 0,
        Value::UInt16(v) => *v == 0,
        Value::UInt32(v) => *v == 0,
        Value::UInt64(v) => *v == 0,
        _ => false,
    }
}

/// Whether `value` is a numeric one — any fixed-width integer one or float `1.0`.
/// The multiplicative/division identities (`x * 1`, `x / 1`) are value-exact for
/// both integer and float operands (`1` is a true unit, preserving `-0.0`/NaN), so
/// the float `1.0` case is admitted; soundness for the result *type* is enforced
/// separately by the result-type-matches-operand gate.
pub(crate) fn is_numeric_one(value: &Value) -> bool {
    match value {
        Value::Int8(v) => *v == 1,
        Value::Int16(v) => *v == 1,
        Value::Int32(v) => *v == 1,
        Value::Int64(v) => *v == 1,
        Value::UInt8(v) => *v == 1,
        Value::UInt16(v) => *v == 1,
        Value::UInt32(v) => *v == 1,
        Value::UInt64(v) => *v == 1,
        Value::Float32(v) => *v == 1.0,
        Value::Float64(v) => *v == 1.0,
        _ => false,
    }
}
