//! The heap-form optimization IR (`OptExpr`).
//!
//! This is the representation the optimizer rewrites in place: it owns its
//! children on the heap (`Box`/`Vec`) so rules can move subtrees through one
//! another cheaply. It is deliberately **not** part of the crate's public
//! surface — callers see only the lowering entry point and the compacted
//! [`crate::CompactProgram`] output. Constants are stored inline here (folding
//! produces arbitrary [`Value`]s); interning into a pool happens only in the
//! compaction pass.

use air_elt_expr_funcs::FuncRef;
use air_elt_expr_parse::FieldsSelector;
use air_elt_types::{Key, Value};

use crate::model::program::TypeClass;

/// What a [`OptExpr::TypeAssert`] yields when its operand is present and of the
/// expected class — the heap twin of
/// [`CompactYield`](crate::model::program::CompactYield) (carrying an inline
/// [`Value`] rather than a pool index).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AssertYield {
    /// Yield the operand unchanged — round-trip identities (`reverse(reverse(x))`).
    Identity,
    /// Yield a fixed constant — degenerate operations (`contains(x, "")`).
    Const(Value),
}

/// A single optimization-IR node. `#[derive(PartialEq)]` powers the golden
/// structural tests (`optimize(lower(a)) == lower(b)`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OptExpr {
    /// Literal or const-folded value.
    Const(Value),
    /// A variable resolved to a register slot.
    Register(u16),
    /// `field(<expr>)` whose argument has not yet folded to a constant column
    /// name. A `Field` that survives optimization is a compile error in the
    /// type-check pass (Phase 3, "non-const field argument").
    Field(Box<OptExpr>),
    /// A resolved source column reference (the folded result of `field("x")`
    /// or the backtick shorthand).
    SourceField(String),
    /// `fields("*")` / `fields("a,b")` — always typed as an object.
    Fields(FieldsSelector),
    /// A regular function or operator call, resolved to a single registry
    /// reference. Conditionals and short-circuit boolean operators are *not*
    /// calls — they have dedicated variants so the optimizer can reason about
    /// branch pruning and laziness.
    Call { func: FuncRef, args: Vec<OptExpr> },
    /// `if(condition, then, else)`.
    If {
        condition: Box<OptExpr>,
        then_branch: Box<OptExpr>,
        else_branch: Box<OptExpr>,
    },
    /// `multiIf(c1, v1, ..., default)`.
    MultiIf {
        branches: Vec<(OptExpr, OptExpr)>,
        default: Box<OptExpr>,
    },
    /// `ifNull(value, alternative)`.
    IfNull {
        value: Box<OptExpr>,
        alternative: Box<OptExpr>,
    },
    /// `nullIf(value, sentinel)`.
    NullIf {
        value: Box<OptExpr>,
        sentinel: Box<OptExpr>,
    },
    /// `a && b` — short-circuit logical AND (three-valued).
    And {
        left: Box<OptExpr>,
        right: Box<OptExpr>,
    },
    /// `a || b` — short-circuit logical OR (three-valued).
    Or {
        left: Box<OptExpr>,
        right: Box<OptExpr>,
    },
    /// String interpolation: an ordered run of expressions whose rendered
    /// values concatenate. Literal-text segments are lowered to `Const(Text)`,
    /// so every segment is just an expression.
    Interpolation(Vec<OptExpr>),
    /// Object literal: ordered `(key, value)` pairs.
    Object(Vec<(String, OptExpr)>),
    /// A constant-key dispatch table — the lowered form of a large `multiIf`
    /// whose branches all test 1–2 pure key expressions for equality against
    /// allow-listed constants. `inputs` holds the 1–2 key expressions; `table`
    /// maps each [`Key`] to its branch (first-match order preserved at build
    /// time); `default` is taken on a miss. Produced by
    /// [`switch_lower`](crate::rules) — never by lowering.
    Switch {
        inputs: Vec<OptExpr>,
        table: Vec<(Key, OptExpr)>,
        default: Box<OptExpr>,
    },
    /// Type/null assertion that preserves the error of an eliminated operation.
    /// See [`OptNode::TypeAssert`](crate::model::program::OptNode::TypeAssert).
    TypeAssert {
        inner: Box<OptExpr>,
        expect: TypeClass,
        on_present: AssertYield,
    },
}

/// A frozen, once-per-row operand — the kind of expression a path fact or a
/// type assertion can be keyed on. A register is bound once; a `SourceField`
/// read is total and deterministic per row, so a fact proved about either still
/// holds wherever it is read again. Shared by guard propagation and the
/// conjunction-infeasibility check, which both reason over the same operands.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum FrozenOperand {
    Register(u16),
    SourceField(String),
}

impl OptExpr {
    /// Borrow the constant value if this node is one.
    pub(crate) fn as_const(&self) -> Option<&Value> {
        match self {
            OptExpr::Const(value) => Some(value),
            _ => None,
        }
    }

    /// The frozen operand this node reads, if it is one.
    pub(crate) fn frozen_operand(&self) -> Option<FrozenOperand> {
        match self {
            OptExpr::Register(register) => Some(FrozenOperand::Register(*register)),
            OptExpr::SourceField(name) => Some(FrozenOperand::SourceField(name.clone())),
            _ => None,
        }
    }

    /// Total node count of the subtree (the primary term of the termination
    /// measure: size-reducing rules strictly shrink it).
    pub(crate) fn node_count(&self) -> usize {
        1 + match self {
            OptExpr::Const(_)
            | OptExpr::Register(_)
            | OptExpr::SourceField(_)
            | OptExpr::Fields(_) => 0,
            OptExpr::Field(inner) => inner.node_count(),
            OptExpr::Call { args, .. } => args.iter().map(OptExpr::node_count).sum(),
            OptExpr::If {
                condition,
                then_branch,
                else_branch,
            } => condition.node_count() + then_branch.node_count() + else_branch.node_count(),
            OptExpr::MultiIf { branches, default } => {
                branches
                    .iter()
                    .map(|(c, v)| c.node_count() + v.node_count())
                    .sum::<usize>()
                    + default.node_count()
            }
            OptExpr::IfNull { value, alternative } => value.node_count() + alternative.node_count(),
            OptExpr::NullIf { value, sentinel } => value.node_count() + sentinel.node_count(),
            OptExpr::And { left, right } | OptExpr::Or { left, right } => {
                left.node_count() + right.node_count()
            }
            OptExpr::Interpolation(segments) => segments.iter().map(OptExpr::node_count).sum(),
            OptExpr::Object(entries) => entries.iter().map(|(_, v)| v.node_count()).sum(),
            OptExpr::Switch {
                inputs,
                table,
                default,
            } => {
                inputs.iter().map(OptExpr::node_count).sum::<usize>()
                    + table.iter().map(|(_, v)| v.node_count()).sum::<usize>()
                    + default.node_count()
            }
            OptExpr::TypeAssert { inner, .. } => inner.node_count(),
        }
    }
}
