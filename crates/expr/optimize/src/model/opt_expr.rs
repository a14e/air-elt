//! The heap-form optimization IR (`OptExpr`).
//!
//! This is the representation the optimizer rewrites in place: it owns its
//! children on the heap (`Box`/`Vec`) so rules can move subtrees through one
//! another cheaply. It is deliberately **not** part of the crate's public
//! surface — callers see only the lowering entry point and the compacted
//! [`crate::CompactProgram`] output. Constants are stored inline here (folding
//! produces arbitrary [`Value`]s); interning into a pool happens only in the
//! compaction pass.
//!
//! Every node carries a [`NodeId`] minted at construction from the per-compile
//! [`NodeCounter`](crate::model::node_id::NodeCounter). The id is **excluded from
//! structural equality** (see the hand-written [`PartialEq`] below) so two nodes
//! of the same shape stay equal regardless of identity — the golden structural
//! tests are unaffected. The id is the key of the static type map.

use air_elt_expr_funcs::FuncRef;
use air_elt_expr_parse::FieldsSelector;
use air_elt_types::{Key, Value};

use crate::model::node_id::{NodeCounter, NodeId};
use crate::model::opt_program::OptStatement;
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

/// A single optimization-IR node. Equality is **structural**: the [`NodeId`] each
/// node carries is excluded (see the hand-written [`PartialEq`]), so the golden
/// structural tests (`optimize(lower(a)) == lower(b)`) are unaffected by the
/// per-compile id values.
#[derive(Debug, Clone)]
pub(crate) enum OptExpr {
    /// Literal or const-folded value.
    Const(NodeId, Value),
    /// A variable resolved to a register slot.
    Register(NodeId, u16),
    /// `field(<expr>)` whose argument has not yet folded to a constant column
    /// name. A `Field` that survives optimization is a compile error in the
    /// type-check pass (Phase 3, "non-const field argument").
    Field(NodeId, Box<OptExpr>),
    /// A resolved source column reference (the folded result of `field("x")`
    /// or the backtick shorthand).
    SourceField(NodeId, String),
    /// `fields("*")` / `fields("a,b")` — always typed as an object.
    Fields(NodeId, FieldsSelector),
    /// A regular function or operator call, resolved to a single registry
    /// reference. Conditionals and short-circuit boolean operators are *not*
    /// calls — they have dedicated variants so the optimizer can reason about
    /// branch pruning and laziness.
    Call {
        id: NodeId,
        func: FuncRef,
        args: Vec<OptExpr>,
    },
    /// `if(condition, then, else)`.
    If {
        id: NodeId,
        condition: Box<OptExpr>,
        then_branch: Box<OptExpr>,
        else_branch: Box<OptExpr>,
    },
    /// `multiIf(c1, v1, ..., default)`.
    MultiIf {
        id: NodeId,
        branches: Vec<(OptExpr, OptExpr)>,
        default: Box<OptExpr>,
    },
    /// `ifNull(value, alternative)`.
    IfNull {
        id: NodeId,
        value: Box<OptExpr>,
        alternative: Box<OptExpr>,
    },
    /// `nullIf(value, sentinel)`.
    NullIf {
        id: NodeId,
        value: Box<OptExpr>,
        sentinel: Box<OptExpr>,
    },
    /// `a && b` — short-circuit logical AND (three-valued).
    And {
        id: NodeId,
        left: Box<OptExpr>,
        right: Box<OptExpr>,
    },
    /// `a || b` — short-circuit logical OR (three-valued).
    Or {
        id: NodeId,
        left: Box<OptExpr>,
        right: Box<OptExpr>,
    },
    /// String interpolation: an ordered run of expressions whose rendered
    /// values concatenate. Literal-text segments are lowered to `Const(Text)`,
    /// so every segment is just an expression.
    Interpolation(NodeId, Vec<OptExpr>),
    /// Object literal: ordered `(key, value)` pairs.
    Object(NodeId, Vec<(String, OptExpr)>),
    /// Array literal: an ordered run of element expressions. Mirrors
    /// [`OptExpr::Interpolation`]'s shape (`NodeId` + `Vec<OptExpr>`, no keys);
    /// element types are unified at type-check, the runtime payload is
    /// [`air_elt_types::Value::Array`].
    Array(NodeId, Vec<OptExpr>),
    /// A constant-key dispatch table — the lowered form of a large `multiIf`
    /// whose branches all test 1–2 pure key expressions for equality against
    /// allow-listed constants. `inputs` holds the 1–2 key expressions; `table`
    /// maps each [`Key`] to its branch (first-match order preserved at build
    /// time); `default` is taken on a miss. Produced by
    /// [`switch_lower`](crate::rules) — never by lowering.
    Switch {
        id: NodeId,
        inputs: Vec<OptExpr>,
        table: Vec<(Key, OptExpr)>,
        default: Box<OptExpr>,
    },
    /// Type/null assertion that preserves the error of an eliminated operation.
    /// See [`OptNode::TypeAssert`](crate::model::program::OptNode::TypeAssert).
    TypeAssert {
        id: NodeId,
        inner: Box<OptExpr>,
        expect: TypeClass,
        on_present: AssertYield,
    },
    /// A binding block: evaluate each `statements` entry into its register, then
    /// evaluate `result`. The heap twin of an [`OptProgram`](crate::model::opt_program::OptProgram)
    /// embedded as a sub-expression — it scopes register bindings to a subtree,
    /// so a binding introduced for a common subexpression inside one branch is
    /// evaluated only when that branch runs (enabling branch-local CSE and
    /// pushing a computation down into the branch that uses it). Registers stay
    /// in the program-wide `u16` space; the block only controls *when* they are
    /// written. Blocks are not cloned (their register writes would alias), so
    /// [`reassign_ids`](OptExpr::reassign_ids) restamps only NodeIds, not
    /// registers — a rule that copies one subtree into several surviving
    /// positions must bail out when the subtree contains a `Block` (see
    /// [`contains_block`](crate::util::block_scan::contains_block)).
    ///
    /// Produced by the converter
    /// ([`OptProgramConverter`](crate::engines::OptProgramConverter)) for the
    /// brace-block branch syntax — `if (c) { x = e; …; result } else …` — and
    /// available to the planned CSE / push-down passes as a future producer.
    Block {
        id: NodeId,
        statements: Vec<OptStatement>,
        result: Box<OptExpr>,
    },
}

/// Structural equality — **excludes the [`NodeId`]**. Two nodes of the same shape
/// are equal regardless of their per-compile identity, so golden structural tests
/// (`optimize(lower(a)) == lower(b)`) compare shape, not identity.
impl PartialEq for OptExpr {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (OptExpr::Const(_, a), OptExpr::Const(_, b)) => a == b,
            (OptExpr::Register(_, a), OptExpr::Register(_, b)) => a == b,
            (OptExpr::Field(_, a), OptExpr::Field(_, b)) => a == b,
            (OptExpr::SourceField(_, a), OptExpr::SourceField(_, b)) => a == b,
            (OptExpr::Fields(_, a), OptExpr::Fields(_, b)) => a == b,
            (
                OptExpr::Call {
                    func: fa, args: aa, ..
                },
                OptExpr::Call {
                    func: fb, args: ab, ..
                },
            ) => fa == fb && aa == ab,
            (
                OptExpr::If {
                    condition: ca,
                    then_branch: ta,
                    else_branch: ea,
                    ..
                },
                OptExpr::If {
                    condition: cb,
                    then_branch: tb,
                    else_branch: eb,
                    ..
                },
            ) => ca == cb && ta == tb && ea == eb,
            (
                OptExpr::MultiIf {
                    branches: ba,
                    default: da,
                    ..
                },
                OptExpr::MultiIf {
                    branches: bb,
                    default: db,
                    ..
                },
            ) => ba == bb && da == db,
            (
                OptExpr::IfNull {
                    value: va,
                    alternative: aa,
                    ..
                },
                OptExpr::IfNull {
                    value: vb,
                    alternative: ab,
                    ..
                },
            ) => va == vb && aa == ab,
            (
                OptExpr::NullIf {
                    value: va,
                    sentinel: sa,
                    ..
                },
                OptExpr::NullIf {
                    value: vb,
                    sentinel: sb,
                    ..
                },
            ) => va == vb && sa == sb,
            (
                OptExpr::And {
                    left: la,
                    right: ra,
                    ..
                },
                OptExpr::And {
                    left: lb,
                    right: rb,
                    ..
                },
            ) => la == lb && ra == rb,
            (
                OptExpr::Or {
                    left: la,
                    right: ra,
                    ..
                },
                OptExpr::Or {
                    left: lb,
                    right: rb,
                    ..
                },
            ) => la == lb && ra == rb,
            (OptExpr::Interpolation(_, a), OptExpr::Interpolation(_, b)) => a == b,
            (OptExpr::Object(_, a), OptExpr::Object(_, b)) => a == b,
            (OptExpr::Array(_, a), OptExpr::Array(_, b)) => a == b,
            (
                OptExpr::Switch {
                    inputs: ia,
                    table: ta,
                    default: da,
                    ..
                },
                OptExpr::Switch {
                    inputs: ib,
                    table: tb,
                    default: db,
                    ..
                },
            ) => ia == ib && ta == tb && da == db,
            (
                OptExpr::TypeAssert {
                    inner: ia,
                    expect: ea,
                    on_present: pa,
                    ..
                },
                OptExpr::TypeAssert {
                    inner: ib,
                    expect: eb,
                    on_present: pb,
                    ..
                },
            ) => ia == ib && ea == eb && pa == pb,
            (
                OptExpr::Block {
                    statements: sa,
                    result: ra,
                    ..
                },
                OptExpr::Block {
                    statements: sb,
                    result: rb,
                    ..
                },
            ) => {
                sa.len() == sb.len()
                    && sa
                        .iter()
                        .zip(sb)
                        .all(|(x, y)| x.register == y.register && x.value == y.value)
                    && ra == rb
            }
            _ => false,
        }
    }
}

/// A frozen, once-per-row operand — the kind of expression a path fact or a
/// type assertion can be keyed on. A register is bound once; a `SourceField`
/// read is total and deterministic per row, so a fact proved about either still
/// holds wherever it is read again. Shared by guard propagation and the
/// conjunction-infeasibility check, which both reason over the same operands.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum FrozenOperand {
    Register(u16),
    SourceField(String),
}

impl OptExpr {
    /// This node's stable identity (the key of the static type map).
    pub(crate) fn id(&self) -> NodeId {
        match self {
            OptExpr::Const(id, _)
            | OptExpr::Register(id, _)
            | OptExpr::Field(id, _)
            | OptExpr::SourceField(id, _)
            | OptExpr::Fields(id, _)
            | OptExpr::Interpolation(id, _)
            | OptExpr::Object(id, _)
            | OptExpr::Array(id, _)
            | OptExpr::Call { id, .. }
            | OptExpr::If { id, .. }
            | OptExpr::MultiIf { id, .. }
            | OptExpr::IfNull { id, .. }
            | OptExpr::NullIf { id, .. }
            | OptExpr::And { id, .. }
            | OptExpr::Or { id, .. }
            | OptExpr::Switch { id, .. }
            | OptExpr::TypeAssert { id, .. }
            | OptExpr::Block { id, .. } => *id,
        }
    }

    /// Stamp this subtree with fresh ids from `counter`, recursively. Used when a
    /// rule clones a subtree into several surviving positions (e.g. one branch
    /// value shared across several switch keys): without re-ids the clones would
    /// share their source's ids, breaking the per-node uniqueness the type map and
    /// the per-rewrite soundness diff rely on.
    pub(crate) fn reassign_ids(&mut self, counter: &NodeCounter) {
        match self {
            OptExpr::Const(id, _)
            | OptExpr::Register(id, _)
            | OptExpr::SourceField(id, _)
            | OptExpr::Fields(id, _) => *id = counter.fresh_id(),
            OptExpr::Field(id, inner) => {
                *id = counter.fresh_id();
                inner.reassign_ids(counter);
            }
            OptExpr::Call { id, args, .. } => {
                *id = counter.fresh_id();
                for arg in args {
                    arg.reassign_ids(counter);
                }
            }
            OptExpr::If {
                id,
                condition,
                then_branch,
                else_branch,
            } => {
                *id = counter.fresh_id();
                condition.reassign_ids(counter);
                then_branch.reassign_ids(counter);
                else_branch.reassign_ids(counter);
            }
            OptExpr::MultiIf {
                id,
                branches,
                default,
            } => {
                *id = counter.fresh_id();
                for (condition, value) in branches {
                    condition.reassign_ids(counter);
                    value.reassign_ids(counter);
                }
                default.reassign_ids(counter);
            }
            OptExpr::IfNull {
                id,
                value,
                alternative,
            } => {
                *id = counter.fresh_id();
                value.reassign_ids(counter);
                alternative.reassign_ids(counter);
            }
            OptExpr::NullIf {
                id,
                value,
                sentinel,
            } => {
                *id = counter.fresh_id();
                value.reassign_ids(counter);
                sentinel.reassign_ids(counter);
            }
            OptExpr::And { id, left, right } | OptExpr::Or { id, left, right } => {
                *id = counter.fresh_id();
                left.reassign_ids(counter);
                right.reassign_ids(counter);
            }
            OptExpr::Interpolation(id, segments) => {
                *id = counter.fresh_id();
                for segment in segments {
                    segment.reassign_ids(counter);
                }
            }
            OptExpr::Object(id, entries) => {
                *id = counter.fresh_id();
                for (_key, value) in entries {
                    value.reassign_ids(counter);
                }
            }
            OptExpr::Array(id, elements) => {
                *id = counter.fresh_id();
                for element in elements {
                    element.reassign_ids(counter);
                }
            }
            OptExpr::Switch {
                id,
                inputs,
                table,
                default,
            } => {
                *id = counter.fresh_id();
                for input in inputs {
                    input.reassign_ids(counter);
                }
                for (_key, value) in table {
                    value.reassign_ids(counter);
                }
                default.reassign_ids(counter);
            }
            OptExpr::TypeAssert { id, inner, .. } => {
                *id = counter.fresh_id();
                inner.reassign_ids(counter);
            }
            OptExpr::Block {
                id,
                statements,
                result,
            } => {
                *id = counter.fresh_id();
                for statement in statements {
                    statement.value.reassign_ids(counter);
                }
                result.reassign_ids(counter);
            }
        }
    }

    /// Borrow the constant value if this node is one.
    pub(crate) fn as_const(&self) -> Option<&Value> {
        match self {
            OptExpr::Const(_, value) => Some(value),
            _ => None,
        }
    }

    /// The frozen operand this node reads, if it is one.
    pub(crate) fn frozen_operand(&self) -> Option<FrozenOperand> {
        match self {
            OptExpr::Register(_, register) => Some(FrozenOperand::Register(*register)),
            OptExpr::SourceField(_, name) => Some(FrozenOperand::SourceField(name.clone())),
            _ => None,
        }
    }

    /// Total node count of the subtree (the primary term of the termination
    /// measure: size-reducing rules strictly shrink it).
    pub(crate) fn node_count(&self) -> usize {
        1 + match self {
            OptExpr::Const(..)
            | OptExpr::Register(..)
            | OptExpr::SourceField(..)
            | OptExpr::Fields(..) => 0,
            OptExpr::Field(_, inner) => inner.node_count(),
            OptExpr::Call { args, .. } => args.iter().map(OptExpr::node_count).sum(),
            OptExpr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => condition.node_count() + then_branch.node_count() + else_branch.node_count(),
            OptExpr::MultiIf {
                branches, default, ..
            } => {
                branches
                    .iter()
                    .map(|(c, v)| c.node_count() + v.node_count())
                    .sum::<usize>()
                    + default.node_count()
            }
            OptExpr::IfNull {
                value, alternative, ..
            } => value.node_count() + alternative.node_count(),
            OptExpr::NullIf {
                value, sentinel, ..
            } => value.node_count() + sentinel.node_count(),
            OptExpr::And { left, right, .. } | OptExpr::Or { left, right, .. } => {
                left.node_count() + right.node_count()
            }
            OptExpr::Interpolation(_, segments) => segments.iter().map(OptExpr::node_count).sum(),
            OptExpr::Object(_, entries) => entries.iter().map(|(_, v)| v.node_count()).sum(),
            OptExpr::Array(_, elements) => elements.iter().map(OptExpr::node_count).sum(),
            OptExpr::Switch {
                inputs,
                table,
                default,
                ..
            } => {
                inputs.iter().map(OptExpr::node_count).sum::<usize>()
                    + table.iter().map(|(_, v)| v.node_count()).sum::<usize>()
                    + default.node_count()
            }
            OptExpr::TypeAssert { inner, .. } => inner.node_count(),
            OptExpr::Block {
                statements, result, ..
            } => {
                statements
                    .iter()
                    .map(|statement| statement.value.node_count())
                    .sum::<usize>()
                    + result.node_count()
            }
        }
    }
}
