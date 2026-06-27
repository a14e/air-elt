//! Conversion: parse AST (`Program`/`Expr`) → optimization IR (`OptProgram`).
//!
//! [`OptProgramConverter`] resolves every function name (and arity) to a single
//! registry [`FuncRef`](air_elt_expr_funcs::FuncRef) and every variable to a
//! register slot. It performs no algebraic simplification — that is the
//! optimizer's job. Shadowing (`x = 1; x = x + 1`) allocates a fresh register
//! per binding, so the IR is single-assignment and earlier reads see the
//! earlier value.

use ahash::{AHashMap, AHashSet};
use air_elt_commons_arena::ArenaOverflow;
use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_parse::model::{
    ConditionalExpr, Expr, InterpolationSegment, LiteralValue, Program, Statement,
};
use air_elt_expr_types::limits::MAX_EXPR_DEPTH;
use air_elt_types::Value;

use crate::error::OptimizeError;
use crate::model::node_id::{NodeCounter, NodeId};
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::{OptProgram, OptStatement};

/// Converts a parsed program into the optimization IR against a function registry.
pub(crate) struct OptProgramConverter<'a> {
    registry: &'a FunctionRegistry,
    node_counter: &'a NodeCounter,
    name_to_register: AHashMap<String, u16>,
    next_register: u16,
    depth: usize,
}

impl<'a> OptProgramConverter<'a> {
    pub(crate) fn create(registry: &'a FunctionRegistry, node_counter: &'a NodeCounter) -> Self {
        Self {
            registry,
            node_counter,
            name_to_register: AHashMap::new(),
            next_register: 0,
            depth: 0,
        }
    }

    /// Mint a fresh node id for a node this converter is about to construct.
    fn next_id(&self) -> NodeId {
        self.node_counter.fresh_id()
    }

    /// Convert a whole program: bindings in order, then the result expression.
    pub(crate) fn convert(mut self, program: &Program) -> Result<OptProgram, OptimizeError> {
        let mut statements = Vec::with_capacity(program.statements.len());
        for statement in &program.statements {
            let value = self.lower_expr(&statement.value)?;
            let register = self.allocate_register()?;
            statements.push(OptStatement { register, value });
            self.name_to_register
                .insert(statement.name.clone(), register);
        }

        let result = self.lower_expr(&program.result)?;
        Ok(OptProgram {
            statements,
            result,
            register_count: self.next_register,
        })
    }

    fn allocate_register(&mut self) -> Result<u16, OptimizeError> {
        let register = self.next_register;
        self.next_register = self
            .next_register
            .checked_add(1)
            .ok_or(OptimizeError::Overflow(ArenaOverflow))?;
        Ok(register)
    }

    /// Depth-guarded entry: a directly-constructed `Program` may nest deeper
    /// than the parser permits, so enforce the bound before recursing.
    fn lower_expr(&mut self, expr: &Expr) -> Result<OptExpr, OptimizeError> {
        self.depth += 1;
        if self.depth > MAX_EXPR_DEPTH {
            self.depth -= 1;
            return Err(OptimizeError::NestingTooDeep {
                max: MAX_EXPR_DEPTH,
            });
        }
        let lowered = self.lower_expr_inner(expr);
        self.depth -= 1;
        lowered
    }

    fn lower_expr_inner(&mut self, expr: &Expr) -> Result<OptExpr, OptimizeError> {
        match expr {
            Expr::Literal(literal) => Ok(OptExpr::Const(self.next_id(), literal_to_value(literal))),
            Expr::Variable(name) => self.lower_variable(name),
            Expr::FunctionCall { name, args } => self.lower_call(name, args),
            Expr::Conditional(conditional) => self.lower_conditional(conditional),
            Expr::Interpolation(segments) => self.lower_interpolation(segments),
            Expr::Object(entries) => self.lower_object(entries),
            Expr::Field(inner) => Ok(OptExpr::Field(
                self.next_id(),
                Box::new(self.lower_expr(inner)?),
            )),
            Expr::Fields(selector) => Ok(OptExpr::Fields(self.next_id(), selector.clone())),
            Expr::Block { statements, result } => self.lower_block(statements, result),
        }
    }

    /// Lower a scoped-binding block (`{ name = expr; …; result }` in an
    /// `if`-branch position). Mirrors [`Self::convert`]: each binding lowers
    /// into a fresh SSA register and shadows its name for the remainder of the
    /// block. On exit — success or error — the displaced name entries are
    /// restored in reverse insertion order, so siblings and the outer scope see
    /// the pre-block bindings again.
    fn lower_block(
        &mut self,
        statements: &[Statement],
        result: &Expr,
    ) -> Result<OptExpr, OptimizeError> {
        let id = self.next_id();
        let mut displaced: Vec<(&str, Option<u16>)> = Vec::with_capacity(statements.len());
        let lowered = self.lower_block_scope(statements, result, &mut displaced);

        // Restore the scope map before propagating any error, so a failed block
        // leaves the converter consistent for the caller's remaining siblings.
        // Entries borrow the AST names; only a shadowed restore re-owns one.
        for (name, previous) in displaced.into_iter().rev() {
            match previous {
                Some(register) => {
                    self.name_to_register.insert(name.to_owned(), register);
                }
                None => {
                    self.name_to_register.remove(name);
                }
            }
        }

        let (lowered_statements, lowered_result) = lowered?;
        Ok(OptExpr::Block {
            id,
            statements: lowered_statements,
            result: Box::new(lowered_result),
        })
    }

    /// The scope-mutating part of block lowering: lower each binding, allocate
    /// its register, record what its name displaced into `displaced` (pushed in
    /// insertion order, even on a later error), then lower the result.
    fn lower_block_scope<'s>(
        &mut self,
        statements: &'s [Statement],
        result: &Expr,
        displaced: &mut Vec<(&'s str, Option<u16>)>,
    ) -> Result<(Vec<OptStatement>, OptExpr), OptimizeError> {
        let mut lowered = Vec::with_capacity(statements.len());
        for statement in statements {
            let value = self.lower_expr(&statement.value)?;
            let register = self.allocate_register()?;
            lowered.push(OptStatement { register, value });
            let previous = self
                .name_to_register
                .insert(statement.name.clone(), register);
            displaced.push((&statement.name, previous));
        }
        let result = self.lower_expr(result)?;
        Ok((lowered, result))
    }

    fn lower_variable(&self, name: &str) -> Result<OptExpr, OptimizeError> {
        let register = self.name_to_register.get(name).copied().ok_or_else(|| {
            OptimizeError::UndefinedVariable {
                name: name.to_string(),
            }
        })?;
        Ok(OptExpr::Register(self.next_id(), register))
    }

    fn lower_call(&mut self, name: &str, args: &[Expr]) -> Result<OptExpr, OptimizeError> {
        let func = self.registry.get_ref(name, Some(args.len()))?;
        let lowered = args
            .iter()
            .map(|arg| self.lower_expr(arg))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(OptExpr::Call {
            id: self.next_id(),
            func,
            args: lowered,
        })
    }

    fn lower_interpolation(
        &mut self,
        segments: &[InterpolationSegment],
    ) -> Result<OptExpr, OptimizeError> {
        // A literal-text segment lowers to a constant string node, so every
        // segment is just an expression the evaluator renders and concatenates.
        let lowered = segments
            .iter()
            .map(|segment| match segment {
                InterpolationSegment::Text(text) => {
                    Ok(OptExpr::Const(self.next_id(), Value::Text(text.clone())))
                }
                InterpolationSegment::Expression(expr) => self.lower_expr(expr),
            })
            .collect::<Result<Vec<_>, OptimizeError>>()?;
        Ok(OptExpr::Interpolation(self.next_id(), lowered))
    }

    fn lower_object(&mut self, entries: &[(String, Expr)]) -> Result<OptExpr, OptimizeError> {
        let mut seen: AHashSet<&str> = AHashSet::with_capacity(entries.len());
        for (key, _) in entries {
            if !seen.insert(key.as_str()) {
                return Err(OptimizeError::DuplicateObjectKey { key: key.clone() });
            }
        }
        let lowered = entries
            .iter()
            .map(|(key, value)| Ok((key.clone(), self.lower_expr(value)?)))
            .collect::<Result<Vec<_>, OptimizeError>>()?;
        Ok(OptExpr::Object(self.next_id(), lowered))
    }

    fn lower_conditional(
        &mut self,
        conditional: &ConditionalExpr,
    ) -> Result<OptExpr, OptimizeError> {
        match conditional {
            ConditionalExpr::If {
                condition,
                then_branch,
                else_branch,
            } => Ok(OptExpr::If {
                id: self.next_id(),
                condition: Box::new(self.lower_expr(condition)?),
                then_branch: Box::new(self.lower_expr(then_branch)?),
                else_branch: Box::new(self.lower_expr(else_branch)?),
            }),
            ConditionalExpr::MultiIf { branches, default } => {
                let lowered = branches
                    .iter()
                    .map(|(condition, value)| {
                        Ok((self.lower_expr(condition)?, self.lower_expr(value)?))
                    })
                    .collect::<Result<Vec<_>, OptimizeError>>()?;
                Ok(OptExpr::MultiIf {
                    id: self.next_id(),
                    branches: lowered,
                    default: Box::new(self.lower_expr(default)?),
                })
            }
            ConditionalExpr::IfNull { value, alternative } => Ok(OptExpr::IfNull {
                id: self.next_id(),
                value: Box::new(self.lower_expr(value)?),
                alternative: Box::new(self.lower_expr(alternative)?),
            }),
            ConditionalExpr::NullIf { value, sentinel } => Ok(OptExpr::NullIf {
                id: self.next_id(),
                value: Box::new(self.lower_expr(value)?),
                sentinel: Box::new(self.lower_expr(sentinel)?),
            }),
            ConditionalExpr::And { left, right } => Ok(OptExpr::And {
                id: self.next_id(),
                left: Box::new(self.lower_expr(left)?),
                right: Box::new(self.lower_expr(right)?),
            }),
            ConditionalExpr::Or { left, right } => Ok(OptExpr::Or {
                id: self.next_id(),
                left: Box::new(self.lower_expr(left)?),
                right: Box::new(self.lower_expr(right)?),
            }),
        }
    }
}

/// Convert a parsed literal to its runtime [`Value`]. Mirrors the heap
/// evaluator's `eval_literal` so lowering and evaluation agree.
fn literal_to_value(literal: &LiteralValue) -> Value {
    match literal {
        LiteralValue::Null => Value::Null,
        LiteralValue::Bool(boolean) => Value::Bool(*boolean),
        LiteralValue::Int(integer) => Value::Int64(*integer),
        LiteralValue::Float(float) => Value::Float64(*float),
        LiteralValue::String(string) => Value::Text(string.clone()),
        LiteralValue::Interval(duration) => Value::Interval(*duration),
    }
}
