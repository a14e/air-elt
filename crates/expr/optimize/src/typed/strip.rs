//! Stripping the type guards Phase 2 parked: redundant `TypeAssert`s and
//! redundant casts, both discharged when the static type map proves the operand
//! already has the required type.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::DataType;

use super::engine::{TypedRewrite, TypedRule, TypedRuleCx};
use crate::model::node_id::NodeId;
use crate::model::opt_expr::{AssertYield, OptExpr};
use crate::model::program::TypeClass;
use crate::util::type_utils::{is_droppable, satisfies};

/// Strip a `TypeAssert` whose operand the type map already proves is of the
/// asserted class.
///
/// A parked [`TypeAssert`](OptExpr::TypeAssert) checks at runtime that its operand
/// is of the expected [`TypeClass`] (raising `TypeMismatch` otherwise). When the
/// map proves the operand's data type is already that class, the "wrong type"
/// branch is dead:
/// - `Identity` yields the operand when present and null when null — both equal
///   the operand, so the assert collapses to its inner expression (sound for any
///   nullability).
/// - `Const(v)` yields `v` when present, null when null — equal to `v` only when
///   the operand is non-null and infallible (so dropping its evaluation drops no
///   error). Otherwise the assert stays.
pub(crate) struct AssertStrip;

impl TypedRule for AssertStrip {
    fn apply(&self, node: OptExpr, cx: &TypedRuleCx) -> TypedRewrite {
        let OptExpr::TypeAssert {
            id,
            inner,
            expect,
            on_present,
        } = node
        else {
            return TypedRewrite::Same(node);
        };
        let proven = cx
            .type_map
            .get(&inner.id())
            .is_some_and(|inner_type| satisfies(&inner_type.data_type, &expect));
        if !proven {
            return TypedRewrite::Same(rebuild_assert(id, inner, expect, on_present));
        }
        match on_present {
            AssertYield::Identity => TypedRewrite::Changed(*inner),
            AssertYield::Const(value) => {
                if is_droppable(&inner, cx) {
                    TypedRewrite::Changed(OptExpr::Const(id, value))
                } else {
                    TypedRewrite::Same(rebuild_assert(id, inner, expect, AssertYield::Const(value)))
                }
            }
        }
    }
}

/// Reassemble an unchanged `TypeAssert` from its parts.
fn rebuild_assert(
    id: NodeId,
    inner: Box<OptExpr>,
    expect: TypeClass,
    on_present: AssertYield,
) -> OptExpr {
    OptExpr::TypeAssert {
        id,
        inner,
        expect,
        on_present,
    }
}

/// Strip a redundant cast `cast(x) → x` when the type map proves `x` already has
/// the cast's target type.
///
/// Every cast resolves to a fixed target [`DataType`] and propagates operand
/// nullability, so `cast(x) → x` preserves the program's resolved type **only**
/// when `x`'s data type already equals the target *exactly* — a widening
/// (`toInt64(x: Int8)`) would change the resolved type and is not stripped. On an
/// already-correct-typed value every cast is the identity (value unchanged, null →
/// null) and cannot fail, so the strip drops no error.
///
/// `toStringCast`/`toString` match any `Text` (size-agnostic). `toDecimal` is
/// deliberately excluded: it re-scales its operand (not an identity even on a
/// `Decimal`) and can overflow.
pub(crate) struct CastStrip {
    /// Each strippable single-argument cast's `FuncRef` paired with how its target
    /// type is matched.
    casts: Vec<(FuncRef, TargetMatch)>,
}

/// How a cast's target type is matched against an operand's resolved type.
#[derive(Clone)]
enum TargetMatch {
    /// Exactly this data type.
    Exact(DataType),
    /// Any `Text { size }` (size-agnostic).
    AnyText,
    /// An unbounded `BigInt { width: None }` — the only form `toBigInt` yields and
    /// the form a `Value::BigInt` reports.
    UnboundedBigInt,
}

impl TargetMatch {
    fn matches(&self, data_type: &DataType) -> bool {
        match self {
            TargetMatch::Exact(target) => data_type == target,
            TargetMatch::AnyText => matches!(data_type, DataType::Text { .. }),
            TargetMatch::UnboundedBigInt => matches!(data_type, DataType::BigInt { width: None }),
        }
    }
}

impl CastStrip {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        let table = [
            ("toStringCast", TargetMatch::AnyText),
            ("toString", TargetMatch::AnyText),
            ("toInt8", TargetMatch::Exact(DataType::Int8)),
            ("toInt16", TargetMatch::Exact(DataType::Int16)),
            ("toInt32", TargetMatch::Exact(DataType::Int32)),
            ("toInt64", TargetMatch::Exact(DataType::Int64)),
            ("toUInt8", TargetMatch::Exact(DataType::UInt8)),
            ("toUInt16", TargetMatch::Exact(DataType::UInt16)),
            ("toUInt32", TargetMatch::Exact(DataType::UInt32)),
            ("toUInt64", TargetMatch::Exact(DataType::UInt64)),
            ("toFloat32", TargetMatch::Exact(DataType::Float32)),
            ("toFloat64", TargetMatch::Exact(DataType::Float64)),
            ("toBool", TargetMatch::Exact(DataType::Bool)),
            ("toDate", TargetMatch::Exact(DataType::Date)),
            ("toTimestamp", TargetMatch::Exact(DataType::Timestamp)),
            ("toUuid", TargetMatch::Exact(DataType::Uuid)),
            ("toBigInt", TargetMatch::UnboundedBigInt),
        ];
        let casts = table
            .into_iter()
            .filter_map(|(name, target)| {
                registry
                    .get_ref(name, Some(1))
                    .ok()
                    .map(|func| (func, target))
            })
            .collect();
        Self { casts }
    }

    /// The target matcher for a cast `FuncRef`, if it is one of the strippable
    /// single-argument casts.
    fn target_for(&self, func: FuncRef) -> Option<&TargetMatch> {
        self.casts
            .iter()
            .find_map(|(cast, target)| (*cast == func).then_some(target))
    }
}

impl TypedRule for CastStrip {
    fn apply(&self, node: OptExpr, cx: &TypedRuleCx) -> TypedRewrite {
        let OptExpr::Call { id, func, args } = node else {
            return TypedRewrite::Same(node);
        };
        if args.len() != 1 {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        }
        let Some(target) = self.target_for(func) else {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        };
        let strippable = cx
            .type_map
            .get(&args[0].id())
            .is_some_and(|arg_type| target.matches(&arg_type.data_type));
        if !strippable {
            return TypedRewrite::Same(OptExpr::Call { id, func, args });
        }
        let arg = args
            .into_iter()
            .next()
            .expect("a single-argument call has its argument");
        TypedRewrite::Changed(arg)
    }
}
