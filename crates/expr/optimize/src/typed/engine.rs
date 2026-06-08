//! The typed rewrite engine: the [`TypedRule`] trait, its outcome, the shared
//! context, the registered rule set, and the bottom-up driver.
//!
//! It mirrors the untyped [`rules` engine](crate::rules) — a [`TypedRule`] takes
//! ownership of one node and returns it rewritten or unchanged — but every typed
//! rule additionally consults the static [`TypeMap`]: it fires only where the map
//! proves the type that makes the rewrite sound (a redundant cast over an
//! already-typed operand, a flatten whose operand types exclude the lossy compare
//! arm, an identity/annihilation whose null/overflow hazards the type rules out).
//!
//! Like [`SecondPassDriver`](crate::second_pass_rules) the trait, the rule set,
//! and the driver live in one file (the typed pass is a single small engine). The
//! [`TypedRewriteDriver`] walks bottom-up and applies the set at each node:
//! children are rewritten before their parent, so when a rule inspects an
//! operand's type the operand is already final and its map entry current. At each
//! node it tries the rules in order and keeps the **first** that fires (one
//! rewrite per node — cascades across levels are caught by the bottom-up order). A
//! single sweep suffices: the typed rewrites are size-non-increasing and only ever
//! replace a node with one of its own children or a fresh, type-preserving
//! subtree built from the node's own id.
//!
//! **Map maintenance.** A fired rule yields a type-preserving replacement, so the
//! new root carries the original node's recorded type; the driver records
//! `map[new_root] = old_type`, keeping the map a valid oracle for the ancestors
//! that will read this node's type. The map is therefore mutable in the driver,
//! unlike the read-only lookups inside the rules.

use std::cell::Cell;

use air_elt_expr_funcs::FunctionRegistry;

use super::algebraic_identities::AlgebraicIdentities;
use super::flatten::TypedFlatten;
use super::power_reduce::PowerReduce;
use super::self_compare::SelfCompare;
use super::strip::{AssertStrip, CastStrip};
use super::unary_reduce::UnaryReduce;
use crate::engines::type_check::TypeMap;
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::{OptProgram, OptStatement};

/// The outcome of applying a typed rule to a node.
pub(crate) enum TypedRewrite {
    /// The rule fired; the node was replaced. The replacement is type-preserving
    /// (its output type equals the original's), so the driver carries the
    /// original's recorded type onto the new root id.
    Changed(OptExpr),
    /// The rule did not apply; the node is returned unchanged.
    Same(OptExpr),
}

/// Shared context a typed rule consults: the registry (function properties,
/// `FuncRef` resolution, `can_fail`, purity) and the static type map (the
/// read-only type oracle).
pub(crate) struct TypedRuleCx<'a> {
    pub(crate) registry: &'a FunctionRegistry,
    pub(crate) type_map: &'a TypeMap,
}

/// A single type-gated rewrite. Takes ownership of one node and returns it
/// rewritten ([`TypedRewrite::Changed`]) or untouched ([`TypedRewrite::Same`]).
/// A rule must fire only when the [`TypeMap`] proves the rewrite sound.
pub(crate) trait TypedRule {
    fn apply(&self, node: OptExpr, cx: &TypedRuleCx) -> TypedRewrite;
}

/// The registered typed rules. The driver tries them in order at each node and
/// keeps the first that fires (one rewrite per node per sweep), so the order is
/// only a tie-break — the rules match disjoint node shapes.
pub(crate) struct TypedRuleSet {
    rules: Vec<Box<dyn TypedRule>>,
}

impl TypedRuleSet {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        let rules: Vec<Box<dyn TypedRule>> = vec![
            // Strip the guards Phase 2 parked (cheapest, most common first).
            Box::new(AssertStrip),
            Box::new(CastStrip::create(registry)),
            // Type-gated normalization and algebraic simplification.
            Box::new(TypedFlatten::create(registry)),
            Box::new(AlgebraicIdentities::create(registry)),
            Box::new(PowerReduce::create(registry)),
            Box::new(SelfCompare::create(registry)),
            Box::new(UnaryReduce::create(registry)),
        ];
        Self { rules }
    }

    fn rules(&self) -> &[Box<dyn TypedRule>] {
        &self.rules
    }
}

/// Runs the typed rule set bottom-up over a program, mutating the type map as
/// nodes are replaced.
pub(crate) struct TypedRewriteDriver<'a> {
    rules: &'a TypedRuleSet,
    registry: &'a FunctionRegistry,
}

impl<'a> TypedRewriteDriver<'a> {
    pub(crate) fn create(rules: &'a TypedRuleSet, registry: &'a FunctionRegistry) -> Self {
        Self { rules, registry }
    }

    /// Rewrite every statement value and the program result, mutating the type map
    /// as nodes are replaced. Consumes and returns the program (each value is moved
    /// through `rewrite`, so no placeholder is needed) plus whether any rule fired
    /// — the interleaved fixpoint loop uses that to keep iterating even when a
    /// rewrite is size-neutral (e.g. `add → concat`).
    pub(crate) fn run(&self, program: OptProgram, map: &mut TypeMap) -> (OptProgram, bool) {
        let changed = Cell::new(false);
        let register_count = program.register_count;
        let statements = program
            .statements
            .into_iter()
            .map(|statement| OptStatement {
                register: statement.register,
                value: self.rewrite(statement.value, map, &changed),
            })
            .collect();
        let result = self.rewrite(program.result, map, &changed);
        let rewritten = OptProgram {
            statements,
            result,
            register_count,
        };
        (rewritten, changed.get())
    }

    /// Bottom-up: rewrite children first, then apply the rule set at this node.
    fn rewrite(&self, expr: OptExpr, map: &mut TypeMap, changed: &Cell<bool>) -> OptExpr {
        let expr = self.rewrite_children(expr, map, changed);
        let old_type = map.get(&expr.id()).cloned();
        let mut current = expr;
        for rule in self.rules.rules() {
            let cx = TypedRuleCx {
                registry: self.registry,
                type_map: map,
            };
            match rule.apply(current, &cx) {
                TypedRewrite::Changed(rewritten) => {
                    // The replacement preserves the node's type; record it for the
                    // new root so ancestors still read a valid entry.
                    if let Some(node_type) = &old_type {
                        map.insert(rewritten.id(), node_type.clone());
                    }
                    changed.set(true);
                    return rewritten;
                }
                TypedRewrite::Same(unchanged) => current = unchanged,
            }
        }
        current
    }

    /// Rebuild a node with each child rewritten, carrying the node's id forward
    /// (rewriting children preserves the node's identity).
    fn rewrite_children(&self, expr: OptExpr, map: &mut TypeMap, changed: &Cell<bool>) -> OptExpr {
        match expr {
            OptExpr::Const(..)
            | OptExpr::Register(..)
            | OptExpr::SourceField(..)
            | OptExpr::Fields(..) => expr,
            OptExpr::Field(id, inner) => {
                OptExpr::Field(id, Box::new(self.rewrite(*inner, map, changed)))
            }
            OptExpr::Call { id, func, args } => OptExpr::Call {
                id,
                func,
                args: args
                    .into_iter()
                    .map(|arg| self.rewrite(arg, map, changed))
                    .collect(),
            },
            OptExpr::If {
                id,
                condition,
                then_branch,
                else_branch,
            } => OptExpr::If {
                id,
                condition: Box::new(self.rewrite(*condition, map, changed)),
                then_branch: Box::new(self.rewrite(*then_branch, map, changed)),
                else_branch: Box::new(self.rewrite(*else_branch, map, changed)),
            },
            OptExpr::MultiIf {
                id,
                branches,
                default,
            } => OptExpr::MultiIf {
                id,
                branches: branches
                    .into_iter()
                    .map(|(condition, value)| {
                        (
                            self.rewrite(condition, map, changed),
                            self.rewrite(value, map, changed),
                        )
                    })
                    .collect(),
                default: Box::new(self.rewrite(*default, map, changed)),
            },
            OptExpr::IfNull {
                id,
                value,
                alternative,
            } => OptExpr::IfNull {
                id,
                value: Box::new(self.rewrite(*value, map, changed)),
                alternative: Box::new(self.rewrite(*alternative, map, changed)),
            },
            OptExpr::NullIf {
                id,
                value,
                sentinel,
            } => OptExpr::NullIf {
                id,
                value: Box::new(self.rewrite(*value, map, changed)),
                sentinel: Box::new(self.rewrite(*sentinel, map, changed)),
            },
            OptExpr::And { id, left, right } => OptExpr::And {
                id,
                left: Box::new(self.rewrite(*left, map, changed)),
                right: Box::new(self.rewrite(*right, map, changed)),
            },
            OptExpr::Or { id, left, right } => OptExpr::Or {
                id,
                left: Box::new(self.rewrite(*left, map, changed)),
                right: Box::new(self.rewrite(*right, map, changed)),
            },
            OptExpr::Interpolation(id, segments) => OptExpr::Interpolation(
                id,
                segments
                    .into_iter()
                    .map(|s| self.rewrite(s, map, changed))
                    .collect(),
            ),
            OptExpr::Object(id, entries) => OptExpr::Object(
                id,
                entries
                    .into_iter()
                    .map(|(key, value)| (key, self.rewrite(value, map, changed)))
                    .collect(),
            ),
            OptExpr::Switch {
                id,
                inputs,
                table,
                default,
            } => OptExpr::Switch {
                id,
                inputs: inputs
                    .into_iter()
                    .map(|input| self.rewrite(input, map, changed))
                    .collect(),
                table: table
                    .into_iter()
                    .map(|(key, value)| (key, self.rewrite(value, map, changed)))
                    .collect(),
                default: Box::new(self.rewrite(*default, map, changed)),
            },
            OptExpr::TypeAssert {
                id,
                inner,
                expect,
                on_present,
            } => OptExpr::TypeAssert {
                id,
                inner: Box::new(self.rewrite(*inner, map, changed)),
                expect,
                on_present,
            },
            OptExpr::Block {
                id,
                statements,
                result,
            } => OptExpr::Block {
                id,
                statements: statements
                    .into_iter()
                    .map(|statement| OptStatement {
                        register: statement.register,
                        value: self.rewrite(statement.value, map, changed),
                    })
                    .collect(),
                result: Box::new(self.rewrite(*result, map, changed)),
            },
        }
    }
}
