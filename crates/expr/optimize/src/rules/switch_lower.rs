//! Switch lowering: a large `multiIf` of equality tests becomes an O(1)
//! constant-key dispatch ([`OptExpr::Switch`]).
//!
//! Fires only when EVERY branch condition is a disjunction (`||`) of clauses,
//! each clause a conjunction (`&&`) of `equals(K, const)` tests over the SAME
//! one or two key expressions `K`. Guards keep the rewrite sound:
//! * **threshold > 5 branches** — below that a linear `multiIf` beats the
//!   hashmap's build/lookup overhead;
//! * **allow-listed constants** (`Int*`/`UInt*`/`BigInt`/`Text`/`Bool`/`Uuid`,
//!   non-null) — excludes `Float`/`Decimal`/… so key hashing is well-defined
//!   (no NaN, no float-equality surprises);
//! * **pure key expressions** — the switch evaluates `K` once, a `multiIf`
//!   re-evaluates the condition per branch, so `K` must be deterministic;
//! * **all-or-nothing** — any non-conforming branch leaves the `multiIf` intact.
//!
//! An `or` of clauses expands to several table entries pointing at one branch;
//! duplicate keys keep the first (preserving `multiIf` first-match order).

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::{Key, Value};

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::OptExpr;

/// Minimum branch count for the lookup table to pay off (strictly `> 5`).
const MIN_SWITCH_BRANCHES: usize = 6;

/// One conjunctive clause: `(key expression, constant)` equality tests.
type Clause = Vec<(OptExpr, Value)>;

/// The 1–2 key expressions a switch reads, in canonical order.
type KeyExprs = Vec<OptExpr>;

/// The dispatch table: each constant [`Key`] mapped to its branch expression.
type SwitchEntries = Vec<(Key, OptExpr)>;

pub(crate) struct SwitchLower {
    equals: Option<FuncRef>,
}

impl SwitchLower {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        Self {
            equals: registry.get_ref("equals", Some(2)).ok(),
        }
    }
}

impl Rule for SwitchLower {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let OptExpr::MultiIf { branches, default } = node else {
            return Rewrite::Same(node);
        };
        let Some(equals) = self.equals else {
            return Rewrite::Same(OptExpr::MultiIf { branches, default });
        };
        if branches.len() < MIN_SWITCH_BRANCHES {
            return Rewrite::Same(OptExpr::MultiIf { branches, default });
        }

        match try_lower(&branches, equals, cx.registry) {
            Some((inputs, table)) => Rewrite::Changed(OptExpr::Switch {
                inputs,
                table,
                default,
            }),
            None => Rewrite::Same(OptExpr::MultiIf { branches, default }),
        }
    }
}

/// Attempt to read the branches as a constant-key dispatch. Returns the key
/// expressions and the `(Key, branch)` table on success.
fn try_lower(
    branches: &[(OptExpr, OptExpr)],
    equals: FuncRef,
    registry: &FunctionRegistry,
) -> Option<(KeyExprs, SwitchEntries)> {
    let mut key_exprs: Option<KeyExprs> = None;
    let mut table: SwitchEntries = Vec::new();

    for (condition, value) in branches {
        let clauses = parse_condition(condition, equals)?;
        if clauses.is_empty() {
            return None;
        }
        for clause in clauses {
            let key = clause_to_key(clause, &mut key_exprs)?;
            // First-match wins: a key already in the table keeps its branch.
            if table.iter().any(|(existing, _)| existing == &key) {
                continue;
            }
            table.push((key, value.clone()));
        }
    }

    let key_exprs = key_exprs?;
    if table.is_empty() {
        return None;
    }
    // The switch evaluates each key expression exactly once.
    if !key_exprs.iter().all(|expr| is_pure(expr, registry)) {
        return None;
    }
    Some((key_exprs, table))
}

/// A condition is a disjunction (`||`) of conjunctive clauses.
fn parse_condition(condition: &OptExpr, equals: FuncRef) -> Option<Vec<Clause>> {
    match condition {
        OptExpr::Or { left, right } => {
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
        OptExpr::And { left, right } => {
            let mut clause = parse_clause(left, equals)?;
            clause.extend(parse_clause(right, equals)?);
            Some(clause)
        }
        OptExpr::Call { func, args } if *func == equals && args.len() == 2 => {
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

/// Resolve a clause to a [`Key`], establishing the canonical key-expression
/// order from the first clause and requiring every later clause to match it.
fn clause_to_key(clause: Clause, key_exprs: &mut Option<Vec<OptExpr>>) -> Option<Key> {
    if clause.is_empty() || clause.len() > 2 {
        return None;
    }

    match key_exprs {
        None => {
            let exprs: Vec<OptExpr> = clause.iter().map(|(expr, _)| expr.clone()).collect();
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
fn is_switchable_const(value: &Value) -> bool {
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

/// Whether an expression is deterministic (no impure function), so the switch
/// may evaluate it once instead of per branch.
fn is_pure(expr: &OptExpr, registry: &FunctionRegistry) -> bool {
    match expr {
        OptExpr::Const(_) | OptExpr::Register(_) | OptExpr::SourceField(_) | OptExpr::Fields(_) => {
            true
        }
        OptExpr::Field(inner) => is_pure(inner, registry),
        OptExpr::Call { func, args } => {
            registry.get_by_ref(*func).is_pure() && args.iter().all(|arg| is_pure(arg, registry))
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            is_pure(condition, registry)
                && is_pure(then_branch, registry)
                && is_pure(else_branch, registry)
        }
        OptExpr::MultiIf { branches, default } => {
            branches
                .iter()
                .all(|(condition, value)| is_pure(condition, registry) && is_pure(value, registry))
                && is_pure(default, registry)
        }
        OptExpr::IfNull { value, alternative } => {
            is_pure(value, registry) && is_pure(alternative, registry)
        }
        OptExpr::NullIf { value, sentinel } => {
            is_pure(value, registry) && is_pure(sentinel, registry)
        }
        OptExpr::And { left, right } | OptExpr::Or { left, right } => {
            is_pure(left, registry) && is_pure(right, registry)
        }
        OptExpr::Interpolation(segments) => {
            segments.iter().all(|segment| is_pure(segment, registry))
        }
        OptExpr::Object(entries) => entries.iter().all(|(_, value)| is_pure(value, registry)),
        OptExpr::Switch {
            inputs,
            table,
            default,
        } => {
            inputs.iter().all(|input| is_pure(input, registry))
                && table.iter().all(|(_, value)| is_pure(value, registry))
                && is_pure(default, registry)
        }
        // The assert itself is a pure type/null check; purity follows the operand.
        OptExpr::TypeAssert { inner, .. } => is_pure(inner, registry),
    }
}
