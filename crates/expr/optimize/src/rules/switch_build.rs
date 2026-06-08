//! Shared extraction for the two switch-lowering rules.
//!
//! Both [`switch_lower`](super::switch_lower) (a `multiIf` of equality branches)
//! and [`or_membership`](super::or_membership) (a bare `||`-chain of equality
//! tests) read the SAME shape: a disjunction (`||`) of conjunctive (`&&`)
//! `equals(K, const)` clauses over one or two pure key expressions `K`. This
//! module owns that shape's parser, the constant allow-list, the [`Key`] builder,
//! and the purity walk so the two rules agree on exactly what is switchable.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::{Key, Value};

use crate::model::opt_expr::OptExpr;

/// One conjunctive clause: a list of `(key expression, constant)` equality tests
/// joined by `&&`.
pub(super) type Clause = Vec<(OptExpr, Value)>;

/// The 1–2 key expressions a switch reads, in canonical order.
pub(super) type KeyExprs = Vec<OptExpr>;

/// The dispatch table: each constant [`Key`] mapped to its branch expression.
pub(super) type SwitchEntries = Vec<(Key, OptExpr)>;

/// A condition is a disjunction (`||`) of conjunctive clauses.
pub(super) fn parse_condition(condition: &OptExpr, equals: FuncRef) -> Option<Vec<Clause>> {
    match condition {
        OptExpr::Or { left, right, .. } => {
            let mut clauses = parse_condition(left, equals)?;
            clauses.extend(parse_condition(right, equals)?);
            Some(clauses)
        }
        other => Some(vec![parse_clause(other, equals)?]),
    }
}

/// A clause is a conjunction (`&&`) of `equals(K, const)` tests.
fn parse_clause(condition: &OptExpr, equals: FuncRef) -> Option<Clause> {
    match condition {
        OptExpr::And { left, right, .. } => {
            let mut clause = parse_clause(left, equals)?;
            clause.extend(parse_clause(right, equals)?);
            Some(clause)
        }
        OptExpr::Call { func, args, .. } if *func == equals && args.len() == 2 => {
            match (args[0].as_const(), args[1].as_const()) {
                (Some(constant), None) if is_switchable_const(constant) => {
                    Some(vec![(args[1].clone(), constant.clone())])
                }
                (None, Some(constant)) if is_switchable_const(constant) => {
                    Some(vec![(args[0].clone(), constant.clone())])
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Resolve a clause to a [`Key`], establishing the canonical key-expression order
/// from the first clause and requiring every later clause to match it.
pub(super) fn clause_to_key(clause: Clause, key_exprs: &mut Option<KeyExprs>) -> Option<Key> {
    if clause.is_empty() || clause.len() > 2 {
        return None;
    }

    match key_exprs {
        None => {
            let exprs: KeyExprs = clause.iter().map(|(expr, _)| expr.clone()).collect();
            // Composite keys must read two *distinct* expressions.
            if exprs.len() == 2 && exprs[0] == exprs[1] {
                return None;
            }
            let constants: Vec<Value> = clause.into_iter().map(|(_, value)| value).collect();
            *key_exprs = Some(exprs);
            make_key(constants)
        }
        Some(exprs) => {
            if clause.len() != exprs.len() {
                return None;
            }
            // Reorder this clause's constants into the canonical key order.
            let mut constants = Vec::with_capacity(exprs.len());
            for expr in exprs.iter() {
                let (_, value) = clause.iter().find(|(clause_expr, _)| clause_expr == expr)?;
                constants.push(value.clone());
            }
            make_key(constants)
        }
    }
}

fn make_key(constants: Vec<Value>) -> Option<Key> {
    if constants.len() == 1 {
        let mut constants = constants;
        Key::from_value(&constants.remove(0))
    } else {
        Key::composite(constants).ok()
    }
}

/// Constants the switch table may key on. Excludes `Float`/`Decimal`/temporal/
/// binary/composite values so key equality and hashing are well-defined.
pub(super) fn is_switchable_const(value: &Value) -> bool {
    matches!(
        value,
        Value::Int8(_)
            | Value::Int16(_)
            | Value::Int32(_)
            | Value::Int64(_)
            | Value::UInt8(_)
            | Value::UInt16(_)
            | Value::UInt32(_)
            | Value::UInt64(_)
            | Value::BigInt(_)
            | Value::Text(_)
            | Value::Bool(_)
            | Value::Uuid(_)
    )
}

/// Whether an expression is deterministic (no impure function), so the switch may
/// evaluate it once instead of per branch.
pub(super) fn is_pure(expr: &OptExpr, registry: &FunctionRegistry) -> bool {
    match expr {
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
        // The assert itself is a pure type/null check; purity follows the operand.
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
