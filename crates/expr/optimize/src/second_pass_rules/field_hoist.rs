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

use std::ops::ControlFlow;

use ahash::AHashMap;
use air_elt_expr_funcs::FunctionRegistry;

use super::engine::ProgramPass;
use crate::model::node_id::NodeCounter;
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::{OptProgram, OptStatement};
use crate::util::visit::{for_each_recursive, for_each_recursive_mut};

pub(crate) struct FieldHoister;

impl ProgramPass for FieldHoister {
    /// Hoist every source field read more than once into a fresh register
    /// binding, rewriting the reads to register references. Size-increasing, so
    /// it runs as a one-shot finalization pass — never inside the fixpoint.
    fn run(
        &self,
        program: &mut OptProgram,
        _registry: &FunctionRegistry,
        node_counter: &NodeCounter,
    ) {
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
            rewrite_fields(&mut statement.value, &registers, node_counter);
        }
        rewrite_fields(&mut program.result, &registers, node_counter);

        // Prepend one binding per hoisted field; the reads now reference them.
        let mut statements = Vec::with_capacity(targets.len() + program.statements.len());
        for name in &targets {
            statements.push(OptStatement {
                register: registers[name],
                value: OptExpr::SourceField(node_counter.fresh_id(), name.clone()),
            });
        }
        statements.append(&mut program.statements);
        program.statements = statements;
        program.register_count = base + targets.len() as u16;
    }
}

/// Tally each [`OptExpr::SourceField`] read, recording first-appearance order.
fn count_fields(expr: &OptExpr, counts: &mut AHashMap<String, usize>, order: &mut Vec<String>) {
    let _: ControlFlow<()> = for_each_recursive(expr, &mut |node| {
        if let OptExpr::SourceField(_, name) = node {
            // Clone the name only on first appearance — repeated reads (the
            // common case this pass targets) just bump the count.
            match counts.get_mut(name) {
                Some(count) => *count += 1,
                None => {
                    counts.insert(name.clone(), 1);
                    order.push(name.clone());
                }
            }
        }
        ControlFlow::Continue(())
    });
}

/// Replace every hoisted [`OptExpr::SourceField`] with its register reference.
fn rewrite_fields(
    expr: &mut OptExpr,
    registers: &AHashMap<String, u16>,
    node_counter: &NodeCounter,
) {
    let _: ControlFlow<()> = for_each_recursive_mut(expr, &mut |node| {
        if let OptExpr::SourceField(_, name) = node {
            if let Some(register) = registers.get(name) {
                *node = OptExpr::Register(node_counter.fresh_id(), *register);
            }
        }
        ControlFlow::Continue(())
    });
}
