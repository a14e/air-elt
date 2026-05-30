//! Field-read CSE: hoist each source field read more than once into a register.
//!
//! `field("x")` resolves to [`OptExpr::SourceField`]. When the same field is
//! read several times, the field is materialized once into a register binding
//! and every read is replaced with a register reference. This deduplicates the
//! (per-row, potentially expensive) field extraction so the runtime copies a
//! cheap register slot instead of re-reading the row. It deliberately grows the
//! program slightly — one binding per shared field — in exchange for fewer row
//! lookups and copies.
//!
//! [`FieldHoister`] runs **once, after the optimization fixpoint** (it is
//! size-increasing, so it cannot take part in the size-non-increasing
//! second-pass loop) and therefore after dead-code elimination — a field read
//! only by a pruned binding is already gone and is not hoisted.
//!
//! Hoisting is sound because a source-field read is pure and total: it has no
//! side effects and (schema-validated, or absent → null for schemaless
//! sources) cannot error, so reading it once up front is equivalent to reading
//! it at each original site.

use ahash::AHashMap;
use air_elt_expr_funcs::FunctionRegistry;

use super::engine::ProgramPass;
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::{OptProgram, OptStatement};

pub(crate) struct FieldHoister;

impl ProgramPass for FieldHoister {
    /// Hoist every source field read more than once into a fresh register
    /// binding, rewriting the reads to register references. Size-increasing, so
    /// it runs as a one-shot finalization pass — never inside the fixpoint.
    fn run(&self, program: &mut OptProgram, _registry: &FunctionRegistry) {
        let mut counts: AHashMap<String, usize> = AHashMap::new();
        let mut order: Vec<String> = Vec::new();
        for statement in &program.statements {
            count_fields(&statement.value, &mut counts, &mut order);
        }
        count_fields(&program.result, &mut counts, &mut order);

        // Hoist fields read at least twice, in first-appearance order so the
        // emitted bindings are deterministic.
        let base = program.register_count;
        let capacity = (u16::MAX as usize).saturating_sub(base as usize);
        let mut targets: Vec<String> = order
            .into_iter()
            .filter(|name| counts.get(name).copied().unwrap_or(0) >= 2)
            .collect();
        // Defensive: never allocate past the u16 register space (unreachable in
        // practice — the field count is bounded by the AST-node cap).
        targets.truncate(capacity);
        if targets.is_empty() {
            return;
        }

        let mut registers: AHashMap<String, u16> = AHashMap::with_capacity(targets.len());
        for (offset, name) in targets.iter().enumerate() {
            registers.insert(name.clone(), base + offset as u16);
        }

        for statement in &mut program.statements {
            rewrite_fields(&mut statement.value, &registers);
        }
        rewrite_fields(&mut program.result, &registers);

        // Prepend one binding per hoisted field; the reads now reference them.
        let mut statements = Vec::with_capacity(targets.len() + program.statements.len());
        for name in &targets {
            statements.push(OptStatement {
                register: registers[name],
                value: OptExpr::SourceField(name.clone()),
            });
        }
        statements.append(&mut program.statements);
        program.statements = statements;
        program.register_count = base + targets.len() as u16;
    }
}

/// Tally each [`OptExpr::SourceField`] read, recording first-appearance order.
fn count_fields(expr: &OptExpr, counts: &mut AHashMap<String, usize>, order: &mut Vec<String>) {
    match expr {
        OptExpr::SourceField(name) => {
            let entry = counts.entry(name.clone()).or_insert(0);
            if *entry == 0 {
                order.push(name.clone());
            }
            *entry += 1;
        }
        OptExpr::Const(_) | OptExpr::Register(_) | OptExpr::Fields(_) => {}
        OptExpr::Field(inner) => count_fields(inner, counts, order),
        OptExpr::Call { args, .. } => {
            for arg in args {
                count_fields(arg, counts, order);
            }
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            count_fields(condition, counts, order);
            count_fields(then_branch, counts, order);
            count_fields(else_branch, counts, order);
        }
        OptExpr::MultiIf { branches, default } => {
            for (condition, value) in branches {
                count_fields(condition, counts, order);
                count_fields(value, counts, order);
            }
            count_fields(default, counts, order);
        }
        OptExpr::IfNull { value, alternative } => {
            count_fields(value, counts, order);
            count_fields(alternative, counts, order);
        }
        OptExpr::NullIf { value, sentinel } => {
            count_fields(value, counts, order);
            count_fields(sentinel, counts, order);
        }
        OptExpr::And { left, right } | OptExpr::Or { left, right } => {
            count_fields(left, counts, order);
            count_fields(right, counts, order);
        }
        OptExpr::Interpolation(segments) => {
            for segment in segments {
                count_fields(segment, counts, order);
            }
        }
        OptExpr::Object(entries) => {
            for (_, value) in entries {
                count_fields(value, counts, order);
            }
        }
        OptExpr::Switch {
            inputs,
            table,
            default,
        } => {
            for input in inputs {
                count_fields(input, counts, order);
            }
            for (_, value) in table {
                count_fields(value, counts, order);
            }
            count_fields(default, counts, order);
        }
        OptExpr::TypeAssert { inner, .. } => count_fields(inner, counts, order),
    }
}

/// Replace every hoisted [`OptExpr::SourceField`] with its register reference.
fn rewrite_fields(expr: &mut OptExpr, registers: &AHashMap<String, u16>) {
    match expr {
        OptExpr::SourceField(name) => {
            if let Some(register) = registers.get(name) {
                *expr = OptExpr::Register(*register);
            }
        }
        OptExpr::Const(_) | OptExpr::Register(_) | OptExpr::Fields(_) => {}
        OptExpr::Field(inner) => rewrite_fields(inner, registers),
        OptExpr::Call { args, .. } => {
            for arg in args {
                rewrite_fields(arg, registers);
            }
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            rewrite_fields(condition, registers);
            rewrite_fields(then_branch, registers);
            rewrite_fields(else_branch, registers);
        }
        OptExpr::MultiIf { branches, default } => {
            for (condition, value) in branches {
                rewrite_fields(condition, registers);
                rewrite_fields(value, registers);
            }
            rewrite_fields(default, registers);
        }
        OptExpr::IfNull { value, alternative } => {
            rewrite_fields(value, registers);
            rewrite_fields(alternative, registers);
        }
        OptExpr::NullIf { value, sentinel } => {
            rewrite_fields(value, registers);
            rewrite_fields(sentinel, registers);
        }
        OptExpr::And { left, right } | OptExpr::Or { left, right } => {
            rewrite_fields(left, registers);
            rewrite_fields(right, registers);
        }
        OptExpr::Interpolation(segments) => {
            for segment in segments {
                rewrite_fields(segment, registers);
            }
        }
        OptExpr::Object(entries) => {
            for (_, value) in entries {
                rewrite_fields(value, registers);
            }
        }
        OptExpr::Switch {
            inputs,
            table,
            default,
        } => {
            for input in inputs {
                rewrite_fields(input, registers);
            }
            for (_, value) in table {
                rewrite_fields(value, registers);
            }
            rewrite_fields(default, registers);
        }
        OptExpr::TypeAssert { inner, .. } => rewrite_fields(inner, registers),
    }
}
