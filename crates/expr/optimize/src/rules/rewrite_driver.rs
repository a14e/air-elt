//! The rewrite driver: a bottom-up (post-order) walk that brings every node to
//! a local fixpoint under the rule set.
//!
//! [`RewriteDriver`] owns the rule set and the shared rule context, so the
//! traversal reads as plain recursion instead of threading both through every
//! call. Children are optimized before their parent, so by the time a rule sees
//! a node its operands are already canonical. After a rule fires, the
//! replacement's children are re-optimized — cheap, since they are usually
//! already canonical, but it keeps the invariant total. A hard iteration cap
//! backstops termination even though the registered rules are all
//! size-non-increasing.
//!
//! The walk descends into every child uniformly, including conditional branches
//! and short-circuit operands. Const folding leaves an erroring constant in
//! place (it never fails the build), so optimizing a branch that
//! [`dce`](crate::rules) is about to prune is harmless; the post-fixpoint static
//! [`check`](crate::check) judges only the constants that survive in
//! always-reached positions.

use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_funcs::signature::EvalContext;
use air_elt_types::Value;

use super::{Rewrite, RuleCx, RuleSet};
use crate::model::node_id::NodeCounter;
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::{OptProgram, OptStatement};
use crate::pass::Pass;

const MAX_LOCAL_ITERS: usize = 32;

pub(crate) struct RewriteDriver<'a> {
    rules: &'a RuleSet,
    cx: RuleCx<'a>,
}

impl Pass for RewriteDriver<'_> {
    /// Rewrite every statement value and the result of a program to a fixpoint,
    /// in place.
    fn optimize(&self, program: &mut OptProgram) {
        self.rewrite_program(program, Self::optimize_expr);
    }

    /// Apply the one-shot finalize rules in a single bottom-up sweep over every
    /// statement value and the result, in place. Called once after the fixpoint
    /// has converged (the finalize rules invert a fixpoint canonicalization, so
    /// they must not run inside the loop).
    fn finalize(&self, program: &mut OptProgram) {
        self.rewrite_program(program, Self::finalize_expr);
    }
}

impl<'a> RewriteDriver<'a> {
    pub(crate) fn create(
        rules: &'a RuleSet,
        registry: &'a FunctionRegistry,
        eval_context: &'a EvalContext,
        node_counter: &'a NodeCounter,
    ) -> Self {
        Self {
            rules,
            cx: RuleCx {
                registry,
                eval_context,
                node_counter,
            },
        }
    }

    /// Optimize a subtree to a local fixpoint.
    fn optimize_expr(&self, expr: OptExpr) -> OptExpr {
        let expr = self.map_children(expr, Self::optimize_expr);
        self.local_fixpoint(expr)
    }

    /// Finalize a subtree: finalize the children (post-order), then apply each
    /// finalize rule once. No local fixpoint — a finalize rule fires at most once
    /// per node and yields nodes it does not re-match (e.g. `multiIf` → `if`).
    fn finalize_expr(&self, expr: OptExpr) -> OptExpr {
        let mut expr = self.map_children(expr, Self::finalize_expr);
        for rule in self.rules.finalize_rules() {
            expr = match rule.apply(expr, &self.cx) {
                Rewrite::Changed(rewritten) | Rewrite::Same(rewritten) => rewritten,
            };
        }
        expr
    }

    /// Apply `rewrite` to every statement value and the program result in place.
    fn rewrite_program(&self, program: &mut OptProgram, rewrite: fn(&Self, OptExpr) -> OptExpr) {
        let statements = std::mem::take(&mut program.statements);
        let mut rewritten = Vec::with_capacity(statements.len());
        for statement in statements {
            rewritten.push(OptStatement {
                register: statement.register,
                value: rewrite(self, statement.value),
            });
        }
        program.statements = rewritten;

        let placeholder = OptExpr::Const(self.cx.node_counter.fresh_id(), Value::Null);
        let result = std::mem::replace(&mut program.result, placeholder);
        program.result = rewrite(self, result);
    }

    fn local_fixpoint(&self, mut expr: OptExpr) -> OptExpr {
        for _ in 0..MAX_LOCAL_ITERS {
            let mut changed = false;
            for rule in self.rules.fixpoint_rules() {
                expr = match rule.apply(expr, &self.cx) {
                    Rewrite::Changed(rewritten) => {
                        changed = true;
                        self.map_children(rewritten, Self::optimize_expr)
                    }
                    Rewrite::Same(unchanged) => unchanged,
                };
            }
            if !changed {
                break;
            }
        }
        expr
    }

    /// Rebuild a node with each child passed through `recurse`. Shared by the
    /// fixpoint walk (`recurse = optimize`) and the finalize walk
    /// (`recurse = finalize`) so the context-free traversal is defined once. The
    /// context-threading walks deliberately keep their own copies — the static
    /// [`check`](crate::check) threads an eager/lazy bit, and
    /// [`guard_propagation`](crate::engines::guard_propagation) threads a fact environment
    /// that varies per branch — neither of which this `fn`-pointer can carry.
    fn map_children(&self, expr: OptExpr, recurse: fn(&Self, OptExpr) -> OptExpr) -> OptExpr {
        // Re-optimizing children preserves the node's identity — the same logical
        // node with canonicalized operands — so every arm carries `id` forward.
        match expr {
            OptExpr::Const(..)
            | OptExpr::Register(..)
            | OptExpr::SourceField(..)
            | OptExpr::Fields(..) => expr,
            OptExpr::Field(id, inner) => OptExpr::Field(id, Box::new(recurse(self, *inner))),
            OptExpr::Call { id, func, args } => OptExpr::Call {
                id,
                func,
                args: args.into_iter().map(|arg| recurse(self, arg)).collect(),
            },
            OptExpr::If {
                id,
                condition,
                then_branch,
                else_branch,
            } => OptExpr::If {
                id,
                condition: Box::new(recurse(self, *condition)),
                then_branch: Box::new(recurse(self, *then_branch)),
                else_branch: Box::new(recurse(self, *else_branch)),
            },
            OptExpr::MultiIf {
                id,
                branches,
                default,
            } => OptExpr::MultiIf {
                id,
                branches: branches
                    .into_iter()
                    .map(|(condition, value)| (recurse(self, condition), recurse(self, value)))
                    .collect(),
                default: Box::new(recurse(self, *default)),
            },
            OptExpr::IfNull {
                id,
                value,
                alternative,
            } => OptExpr::IfNull {
                id,
                value: Box::new(recurse(self, *value)),
                alternative: Box::new(recurse(self, *alternative)),
            },
            OptExpr::NullIf {
                id,
                value,
                sentinel,
            } => OptExpr::NullIf {
                id,
                value: Box::new(recurse(self, *value)),
                sentinel: Box::new(recurse(self, *sentinel)),
            },
            OptExpr::And { id, left, right } => OptExpr::And {
                id,
                left: Box::new(recurse(self, *left)),
                right: Box::new(recurse(self, *right)),
            },
            OptExpr::Or { id, left, right } => OptExpr::Or {
                id,
                left: Box::new(recurse(self, *left)),
                right: Box::new(recurse(self, *right)),
            },
            OptExpr::Interpolation(id, segments) => {
                OptExpr::Interpolation(id, segments.into_iter().map(|s| recurse(self, s)).collect())
            }
            OptExpr::Object(id, entries) => OptExpr::Object(
                id,
                entries
                    .into_iter()
                    .map(|(key, value)| (key, recurse(self, value)))
                    .collect(),
            ),
            OptExpr::Switch {
                id,
                inputs,
                table,
                default,
            } => OptExpr::Switch {
                id,
                inputs: inputs.into_iter().map(|i| recurse(self, i)).collect(),
                table: table
                    .into_iter()
                    .map(|(key, value)| (key, recurse(self, value)))
                    .collect(),
                default: Box::new(recurse(self, *default)),
            },
            OptExpr::TypeAssert {
                id,
                inner,
                expect,
                on_present,
            } => OptExpr::TypeAssert {
                id,
                inner: Box::new(recurse(self, *inner)),
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
                        value: recurse(self, statement.value),
                    })
                    .collect(),
                result: Box::new(recurse(self, *result)),
            },
        }
    }
}
