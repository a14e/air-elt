//! Downward guard propagation: substitute a frozen operand with the value its
//! guard pins it to, then let the bottom-up const fold finish the job.
//!
//! Unlike the bottom-up [`RewriteDriver`](crate::rules::RewriteDriver), this walk
//! is context-sensitive: a branch is rewritten knowing what its guard already
//! proved about an operand. The pass does exactly ONE thing — **substitution**:
//! inside a branch reached only when `x == c` (or `x` is null), every read of the
//! frozen operand `x` (a `Register`/`SourceField`, bound once per row) is replaced
//! with `Const(c)`. Everything else — folding the now-constant subtree, and
//! *stopping at an error* — is left to the existing const fold, which already
//! leaves an erroring call in place for the static [`check`](crate::check) to
//! judge. So the pass never evaluates or reasons about errors itself; it
//! substitutes, and const folding does the rest.
//!
//! It is not rule-based (it does not plug into the
//! [`RuleSet`](crate::rules::RuleSet)), so it lives here among the standalone
//! engines rather than with the rules.
//!
//! **Why substitution is safe here.** A substitution site is always a *lazy*
//! position (an `if`/`multiIf` branch, an `&&` right operand), so a constant that
//! turns out to error lands where the static check defers it to runtime — exactly
//! where the original dynamic expression would have raised. The branch is reached
//! only when its guard holds, so the substituted value is the value `x` actually
//! has there.
//!
//! **What it pins.** Only guards that prove a single, *representation-stable*
//! value are used: `isNull(x)` / `x == null` (the operand is `Null`), and
//! `x == c` for a single-representation constant (`String`/`Bool`). A numeric
//! equality pins the value but not its width, so substituting it could diverge
//! `typeof`/width-sensitive consumers — those are left for the typed Phase-3 pass.
//! Equality and the null tests are TOTAL (never null), so threading a true-fact
//! into an `&&` right operand is sound (the right runs only when the left is
//! true). An `ifNull(value, alternative)` is itself a guard: its `alternative`
//! runs only when `value` is null, so when `value` is a frozen operand that fact
//! (`operand` is `Null`) threads into the `alternative` — the same shape as the
//! `then` branch of `if(isNull(x), …)`.
//!
//! The pass is size-non-increasing (an operand and a constant are both one node),
//! so it runs inside the optimizer's program fixpoint, where the substitution it
//! exposes is folded by the bottom-up rules in the same iteration.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::Value;

use crate::model::node_id::NodeCounter;
use crate::model::opt_expr::{FrozenOperand, OptExpr};
use crate::model::opt_program::{OptProgram, OptStatement};
use crate::pass::Pass;

/// The facts known on the current path: each frozen operand mapped to the
/// constant value its guard pins it to. Small (branch-local), so a vector with
/// linear scan beats a map, and extending clones cheaply.
#[derive(Clone, Default)]
struct FactEnv {
    facts: Vec<(FrozenOperand, Value)>,
}

impl FactEnv {
    /// A copy of this environment with one more pinned value (innermost wins).
    fn with(&self, operand: FrozenOperand, value: Value) -> FactEnv {
        let mut facts = self.facts.clone();
        facts.push((operand, value));
        FactEnv { facts }
    }

    /// The value the environment pins the operand to, if any. The most recently
    /// added (innermost guard) fact wins.
    fn value_of(&self, operand: &FrozenOperand) -> Option<&Value> {
        self.facts
            .iter()
            .rev()
            .find_map(|(tracked, value)| (tracked == operand).then_some(value))
    }
}

/// Constants whose single value-representation makes inlining them safe — null,
/// and the `String`/`Bool` literals of the grammar. Numeric constants are
/// excluded: cross-numeric equality pins the value but not the width, so
/// substituting one would diverge a width/encoding-sensitive consumer.
fn is_substitutable(value: &Value) -> bool {
    matches!(value, Value::Null | Value::Text(_) | Value::Bool(_))
}

pub(crate) struct GuardPropagation<'a> {
    is_null: Option<FuncRef>,
    is_not_null: Option<FuncRef>,
    equals: Option<FuncRef>,
    not_equals: Option<FuncRef>,
    node_counter: &'a NodeCounter,
}

impl Pass for GuardPropagation<'_> {
    /// Substitute through every statement value and the program result, in place,
    /// starting from an empty environment.
    fn optimize(&self, program: &mut OptProgram) {
        let statements = std::mem::take(&mut program.statements);
        program.statements = statements
            .into_iter()
            .map(|statement| OptStatement {
                register: statement.register,
                value: self.propagate(statement.value, &FactEnv::default()),
            })
            .collect();

        let placeholder = OptExpr::Const(self.node_counter.fresh_id(), Value::Null);
        let result = std::mem::replace(&mut program.result, placeholder);
        program.result = self.propagate(result, &FactEnv::default());
    }
}

impl<'a> GuardPropagation<'a> {
    pub(crate) fn create(registry: &FunctionRegistry, node_counter: &'a NodeCounter) -> Self {
        Self {
            is_null: registry.get_ref("isNull", Some(1)).ok(),
            is_not_null: registry.get_ref("isNotNull", Some(1)).ok(),
            equals: registry.get_ref("equals", Some(2)).ok(),
            not_equals: registry.get_ref("notEquals", Some(2)).ok(),
            node_counter,
        }
    }

    fn propagate(&self, expr: OptExpr, env: &FactEnv) -> OptExpr {
        // A frozen operand the environment pins to a value folds to that value —
        // a genuinely new `Const` node, so it gets a fresh id.
        if let Some(operand) = expr.frozen_operand() {
            if let Some(value) = env.value_of(&operand) {
                return OptExpr::Const(self.node_counter.fresh_id(), value.clone());
            }
            return expr;
        }

        // Substitution preserves a node's identity, so every structural arm
        // carries `id` forward.
        match expr {
            OptExpr::If {
                id,
                condition,
                then_branch,
                else_branch,
            } => {
                let condition = self.propagate(*condition, env);
                let then_env = self.extend(env, &condition, true);
                let else_env = self.extend(env, &condition, false);
                OptExpr::If {
                    id,
                    condition: Box::new(condition),
                    then_branch: Box::new(self.propagate(*then_branch, &then_env)),
                    else_branch: Box::new(self.propagate(*else_branch, &else_env)),
                }
            }
            OptExpr::MultiIf {
                id,
                branches,
                default,
            } => {
                let mut reached = env.clone();
                let mut rewritten = Vec::with_capacity(branches.len());
                for (condition, value) in branches {
                    let condition = self.propagate(condition, &reached);
                    let value_env = self.extend(&reached, &condition, true);
                    let value = self.propagate(value, &value_env);
                    // Later branches and the default are reached only on a miss.
                    reached = self.extend(&reached, &condition, false);
                    rewritten.push((condition, value));
                }
                OptExpr::MultiIf {
                    id,
                    branches: rewritten,
                    default: Box::new(self.propagate(*default, &reached)),
                }
            }
            OptExpr::And { id, left, right } => {
                let left = self.propagate(*left, env);
                // The fact-bearing guards are all total, so the left is never
                // null; the right runs only when the left is true, where its
                // true-fact holds.
                let right_env = self.extend(env, &left, true);
                let right = self.propagate(*right, &right_env);
                OptExpr::And {
                    id,
                    left: Box::new(left),
                    right: Box::new(right),
                }
            }
            OptExpr::Or { id, left, right } => OptExpr::Or {
                id,
                left: Box::new(self.propagate(*left, env)),
                right: Box::new(self.propagate(*right, env)),
            },
            OptExpr::Field(id, inner) => OptExpr::Field(id, Box::new(self.propagate(*inner, env))),
            OptExpr::Call { id, func, args } => OptExpr::Call {
                id,
                func,
                args: args
                    .into_iter()
                    .map(|arg| self.propagate(arg, env))
                    .collect(),
            },
            OptExpr::IfNull {
                id,
                value,
                alternative,
            } => {
                let value = self.propagate(*value, env);
                // `ifNull` evaluates `value` once; the alternative runs only on the
                // null path. So if `value` is a frozen operand, it is null
                // throughout the alternative — thread that fact in, exactly like the
                // `then` branch of `if(isNull(x), …)`.
                let alternative_env = match value.frozen_operand() {
                    Some(operand) => env.with(operand, Value::Null),
                    None => env.clone(),
                };
                OptExpr::IfNull {
                    id,
                    value: Box::new(value),
                    alternative: Box::new(self.propagate(*alternative, &alternative_env)),
                }
            }
            OptExpr::NullIf {
                id,
                value,
                sentinel,
            } => OptExpr::NullIf {
                id,
                value: Box::new(self.propagate(*value, env)),
                sentinel: Box::new(self.propagate(*sentinel, env)),
            },
            OptExpr::Interpolation(id, segments) => OptExpr::Interpolation(
                id,
                segments
                    .into_iter()
                    .map(|segment| self.propagate(segment, env))
                    .collect(),
            ),
            OptExpr::Array(id, elements) => OptExpr::Array(
                id,
                elements
                    .into_iter()
                    .map(|element| self.propagate(element, env))
                    .collect(),
            ),
            OptExpr::Object(id, entries) => OptExpr::Object(
                id,
                entries
                    .into_iter()
                    .map(|(key, value)| (key, self.propagate(value, env)))
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
                    .map(|input| self.propagate(input, env))
                    .collect(),
                table: table
                    .into_iter()
                    .map(|(key, value)| (key, self.propagate(value, env)))
                    .collect(),
                default: Box::new(self.propagate(*default, env)),
            },
            OptExpr::TypeAssert {
                id,
                inner,
                expect,
                on_present,
            } => OptExpr::TypeAssert {
                id,
                inner: Box::new(self.propagate(*inner, env)),
                expect,
                on_present,
            },
            // A block's bindings stay in the outer fact environment (the facts
            // proved above still hold inside); we do not yet derive facts about
            // the block's own bound registers, so threading `env` unchanged is the
            // conservative, sound choice.
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
                        value: self.propagate(statement.value, env),
                    })
                    .collect(),
                result: Box::new(self.propagate(*result, env)),
            },
            leaf @ (OptExpr::Const(..)
            | OptExpr::Register(..)
            | OptExpr::SourceField(..)
            | OptExpr::Fields(..)) => leaf,
        }
    }

    /// Extend the environment with the fact a guard proves when it takes the
    /// given truth value, if it pins a substitutable operand value.
    fn extend(&self, env: &FactEnv, condition: &OptExpr, truth: bool) -> FactEnv {
        match self.pinned_value(condition, truth) {
            Some((operand, value)) => env.with(operand, value),
            None => env.clone(),
        }
    }

    /// The `(operand, value)` a guard pins when it evaluates to `truth`, when that
    /// value is a single, substitutable constant. `None` otherwise (e.g. a
    /// non-null fact carries no concrete value, a numeric equality is not
    /// representation-stable).
    fn pinned_value(&self, condition: &OptExpr, truth: bool) -> Option<(FrozenOperand, Value)> {
        let OptExpr::Call { func, args, .. } = condition else {
            return None;
        };
        let func = Some(*func);

        // `isNull(x)` true / `isNotNull(x)` false ⇒ `x` is null.
        let null_when = if func == self.is_null && args.len() == 1 {
            Some(truth)
        } else if func == self.is_not_null && args.len() == 1 {
            Some(!truth)
        } else {
            None
        };
        if let Some(is_null) = null_when {
            return if is_null {
                args[0]
                    .frozen_operand()
                    .map(|operand| (operand, Value::Null))
            } else {
                None
            };
        }

        // `x == c` true / `x != c` false ⇒ `x == c` (total equality, so this also
        // covers `x == null` ⇒ `x` is null).
        let proves_equality = (func == self.equals && truth) || (func == self.not_equals && !truth);
        if proves_equality {
            let (operand, constant) = operand_and_const(args)?;
            if is_substitutable(&constant) {
                return Some((operand, constant));
            }
        }
        None
    }
}

/// Extract the `(operand, constant)` pair from a binary call's arguments, in
/// either order.
fn operand_and_const(args: &[OptExpr]) -> Option<(FrozenOperand, Value)> {
    let [first, second] = args else {
        return None;
    };
    if let (Some(operand), OptExpr::Const(_, constant)) = (first.frozen_operand(), second) {
        return Some((operand, constant.clone()));
    }
    if let (OptExpr::Const(_, constant), Some(operand)) = (first, second.frozen_operand()) {
        return Some((operand, constant.clone()));
    }
    None
}
