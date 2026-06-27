//! The [`Check`] trait, the shared [`CheckCx`], and the [`StaticCheckEngine`]
//! driver that walks the program once, position-aware, running every check at
//! every node. See the [module docs](super) for the eager-vs-everywhere split.

use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_funcs::signature::EvalContext;

use super::conjunction::ConjunctionInfeasibility;
use super::const_args::ConstArgsValidation;
use super::eager_const::EagerConstEval;
use super::field_arg::FieldArgCheck;
use crate::error::OptimizeError;
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::OptProgram;

/// Shared context handed to every [`Check`].
pub(crate) struct CheckCx<'a> {
    pub(crate) registry: &'a FunctionRegistry,
    pub(crate) eval_context: &'a EvalContext,
}

/// A single static check applied to one node. `eager` is `true` when the node
/// occupies an always-evaluated position.
pub(crate) trait Check {
    fn check(&self, node: &OptExpr, eager: bool, cx: &CheckCx) -> Result<(), OptimizeError>;
}

/// Runs the registered static checks over a program with a single position-aware
/// traversal.
pub(crate) struct StaticCheckEngine<'a> {
    cx: CheckCx<'a>,
    checks: Vec<Box<dyn Check>>,
}

impl<'a> StaticCheckEngine<'a> {
    pub(crate) fn create(registry: &'a FunctionRegistry, eval_context: &'a EvalContext) -> Self {
        let checks: Vec<Box<dyn Check>> = vec![
            Box::new(EagerConstEval),
            Box::new(ConstArgsValidation),
            Box::new(FieldArgCheck),
            Box::new(ConjunctionInfeasibility),
        ];
        Self {
            cx: CheckCx {
                registry,
                eval_context,
            },
            checks,
        }
    }

    /// Check every statement value and the result. Each is an eager root: the
    /// runtime evaluates all statement bindings in order (an unread binding that
    /// can fail is deliberately kept), then the result.
    pub(crate) fn check(&self, program: &OptProgram) -> Result<(), OptimizeError> {
        for statement in &program.statements {
            self.walk(&statement.value, true)?;
        }
        self.walk(&program.result, true)
    }

    fn walk(&self, node: &OptExpr, eager: bool) -> Result<(), OptimizeError> {
        for check in &self.checks {
            check.check(node, eager, &self.cx)?;
        }
        self.walk_children(node, eager)
    }

    /// Recurse into children, marking each child eager or lazy. A child is eager
    /// only if the parent is eager AND the position is always evaluated.
    fn walk_children(&self, node: &OptExpr, eager: bool) -> Result<(), OptimizeError> {
        match node {
            OptExpr::Const(..)
            | OptExpr::Register(..)
            | OptExpr::SourceField(..)
            | OptExpr::Fields(..) => Ok(()),
            // The field name is always evaluated.
            OptExpr::Field(_, inner) => self.walk(inner, eager),
            // Every argument of a call is evaluated iff the call is.
            OptExpr::Call { args, .. } => self.walk_all(args, eager),
            // The condition is always evaluated; both branches are lazy.
            OptExpr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                self.walk(condition, eager)?;
                self.walk(then_branch, false)?;
                self.walk(else_branch, false)
            }
            // Only the first condition is unconditionally evaluated; later
            // conditions and all values/default are reached only on a prior miss.
            OptExpr::MultiIf {
                branches, default, ..
            } => {
                for (index, (condition, value)) in branches.iter().enumerate() {
                    self.walk(condition, eager && index == 0)?;
                    self.walk(value, false)?;
                }
                self.walk(default, false)
            }
            // The value is always evaluated; the alternative only when it is null.
            OptExpr::IfNull {
                value, alternative, ..
            } => {
                self.walk(value, eager)?;
                self.walk(alternative, false)
            }
            // `nullIf` evaluates both operands unconditionally.
            OptExpr::NullIf {
                value, sentinel, ..
            } => {
                self.walk(value, eager)?;
                self.walk(sentinel, eager)
            }
            // Short-circuit: only the left operand is always evaluated.
            OptExpr::And { left, right, .. } | OptExpr::Or { left, right, .. } => {
                self.walk(left, eager)?;
                self.walk(right, false)
            }
            // Every interpolation segment is rendered.
            OptExpr::Interpolation(_, segments) => self.walk_all(segments, eager),
            // Every array element is evaluated.
            OptExpr::Array(_, elements) => self.walk_all(elements, eager),
            // Every object value is evaluated.
            OptExpr::Object(_, entries) => {
                for (_, value) in entries {
                    self.walk(value, eager)?;
                }
                Ok(())
            }
            // The key inputs are always evaluated; the arms and default are
            // selected lazily by the dispatch.
            OptExpr::Switch {
                inputs,
                table,
                default,
                ..
            } => {
                self.walk_all(inputs, eager)?;
                for (_, value) in table {
                    self.walk(value, false)?;
                }
                self.walk(default, false)
            }
            // The asserted operand is always evaluated when the assert is.
            OptExpr::TypeAssert { inner, .. } => self.walk(inner, eager),
            // A block's bindings and result are evaluated (in order) exactly when
            // the block is reached, so they inherit the block's eager bit.
            OptExpr::Block {
                statements, result, ..
            } => {
                for statement in statements {
                    self.walk(&statement.value, eager)?;
                }
                self.walk(result, eager)
            }
        }
    }

    fn walk_all(&self, nodes: &[OptExpr], eager: bool) -> Result<(), OptimizeError> {
        for node in nodes {
            self.walk(node, eager)?;
        }
        Ok(())
    }
}
