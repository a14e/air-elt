//! [`MoveAnnotator`] — clone-elision by marking each register's last read.
//!
//! After compaction the program is a node arena in execution order. A variable's
//! register is read by cloning ([`OptNode::Register`]); but a read that is the
//! register's *last* use on every path can instead **move** the value out
//! ([`OptNode::RegisterTake`]) — saving a clone of a heap `Value`
//! (`Text`/`Bytes`/`Json`/`Object`). This pass finds those last reads and flips
//! them.
//!
//! The analysis is a backward, branch-aware **liveness** over the node tree. A
//! register read at node `N` can move iff the register is dead after `N` on ALL
//! paths, so we use *may*-liveness (union over paths): a register is live-after a
//! node if some path from there reads it again. Concretely:
//! - **Sequential** children (call arguments, interpolation/object values,
//!   `nullIf` operands) thread the live set through in reverse execution order.
//! - **Mutually-exclusive** branches (`if` arms, `multiIf` values, `switch`
//!   arms + default) each see the *parent's* live-after — not the sibling's
//!   reads — so a register whose last use is in two arms is takeable in both; the
//!   controlling input then sees the union of the arms' live-before sets.
//! - **Short-circuit** operands (`&&`/`||` right, `ifNull` alternative) fold the
//!   maybe-run side into the always-run side's live-after (union), so a read in
//!   the always-run operand will not move if the maybe-run operand reads the same
//!   register. Sound, mildly conservative on the short-circuited path.
//!
//! At program level the result is processed first (live-after `∅`), then the
//! statements in reverse; a statement `r = value` is the sole definition of `r`
//! (SSA), so `r` is dropped from the live set before the preceding statements.
//!
//! **Soundness.** A read moves iff the register is dead after it on every path.
//! This holds because (a) execution order is a subsequence of arena order (a DFS
//! that skips dead branches), (b) each node runs at most once (a tree, no loops,
//! no CSE), and (c) registers are SSA (one definition, all reads after it).
//! Treating a `RegisterTake` as a clone is always correct, so a missed or
//! over-eager *non-*take only costs a clone — never correctness; the only thing
//! we must never do is move a value that is still read later, which the liveness
//! check forbids.
//!
//! The pass runs last in [`Optimizer::compile`](crate::optimizer): after the
//! fixpoint, the finalizers (incl. field hoisting, which *creates* registers),
//! and the static check, so every register's last-use position is final.

use crate::model::program::{CompactProgram, NodeRef, OptNode, RegisterId};

/// Marks last-use register reads as moves over a compacted program.
pub(crate) struct MoveAnnotator;

impl MoveAnnotator {
    pub(crate) fn create() -> Self {
        Self
    }

    /// Annotate every register's last read as a [`OptNode::RegisterTake`], in
    /// place. A program with no registers has nothing to do.
    pub(crate) fn annotate(&self, program: &mut CompactProgram) {
        let register_count = program.register_count();
        if register_count == 0 {
            return;
        }

        // Phase 1: backward liveness collects the node refs to flip. The result
        // runs last (nothing live after it), then the statements in reverse.
        let mut to_take = Vec::new();
        let mut live = self.analyze(
            program,
            program.result(),
            LiveSet::new(register_count),
            &mut to_take,
        );
        for statement in program.statements().iter().rev() {
            live = self.analyze(program, statement.value, live, &mut to_take);
            // The statement is `r`'s only definition (SSA): `r` is not live
            // before it, regardless of whether later code read it.
            live.remove(statement.register);
        }

        // Phase 2: flip the marked reads. (A clean two-phase split because phase
        // 1 holds shared borrows of the arena while phase 2 mutates it.)
        for node_ref in to_take {
            if let OptNode::Register(register) = program.node(node_ref) {
                let register = *register;
                *program.node_mut(node_ref) = OptNode::RegisterTake(register);
            }
        }
    }

    /// Liveness of `node`, given the registers live *after* it. Returns the
    /// registers live *before* it and records every last-use register read in
    /// `to_take`.
    fn analyze(
        &self,
        program: &CompactProgram,
        node: NodeRef,
        mut live: LiveSet,
        to_take: &mut Vec<NodeRef>,
    ) -> LiveSet {
        match program.node(node) {
            OptNode::Register(register) => {
                // Last read of the register on this path ⇒ takeable.
                if !live.contains(*register) {
                    to_take.push(node);
                }
                live.insert(*register);
                live
            }
            // Already a move (not produced by compaction, but kept total): it
            // reads the register; never re-mark.
            OptNode::RegisterTake(register) => {
                live.insert(*register);
                live
            }
            OptNode::Const(_) | OptNode::SourceField(_) | OptNode::Fields(_) => live,
            OptNode::Field(inner) => self.analyze(program, *inner, live, to_take),
            OptNode::TypeAssert { inner, .. } => self.analyze(program, *inner, live, to_take),
            OptNode::Call { args, .. } => self.analyze_sequential(program, *args, live, to_take),
            OptNode::Interpolation(segments) => {
                self.analyze_sequential(program, *segments, live, to_take)
            }
            OptNode::Object { values, .. } => {
                self.analyze_sequential(program, *values, live, to_take)
            }
            OptNode::NullIf { value, sentinel } => {
                // Both always run, value then sentinel ⇒ reverse: sentinel first.
                let live = self.analyze(program, *sentinel, live, to_take);
                self.analyze(program, *value, live, to_take)
            }
            OptNode::And { left, right } | OptNode::Or { left, right } => {
                // The right runs only when the left short-circuits through; fold
                // it into the left's live-after (union), so an always-run left
                // read won't move if the maybe-run right reads the same register.
                let after_left = self.analyze(program, *right, live, to_take);
                self.analyze(program, *left, after_left, to_take)
            }
            OptNode::IfNull { value, alternative } => {
                // The alternative runs only when value is null — same shape as
                // `&&`: fold it into value's live-after.
                let after_value = self.analyze(program, *alternative, live, to_take);
                self.analyze(program, *value, after_value, to_take)
            }
            OptNode::If {
                condition,
                then_branch,
                else_branch,
            } => {
                // The arms are mutually exclusive: each sees the parent's
                // live-after, so a register's last use in both is takeable in
                // both. The condition then sees the union (either may follow).
                let after_then = self.analyze(program, *then_branch, live.clone(), to_take);
                let after_else = self.analyze(program, *else_branch, live, to_take);
                let mut after_condition = after_then;
                after_condition.union_with(&after_else);
                self.analyze(program, *condition, after_condition, to_take)
            }
            OptNode::MultiIf { branches, default } => {
                self.analyze_multi_if(program, *branches, *default, live, to_take)
            }
            OptNode::Switch {
                inputs,
                table,
                default,
            } => self.analyze_switch(program, *inputs, *table, *default, live, to_take),
        }
    }

    /// Liveness of a run of children all evaluated in order: thread the live set
    /// through them in reverse execution order.
    fn analyze_sequential(
        &self,
        program: &CompactProgram,
        run: crate::model::ArgSlice,
        mut live: LiveSet,
        to_take: &mut Vec<NodeRef>,
    ) -> LiveSet {
        for child in program.args(run).iter().rev() {
            live = self.analyze(program, *child, live, to_take);
        }
        live
    }

    /// Liveness of a `multiIf`. Values and the default are mutually exclusive
    /// (each sees the parent's live-after); the conditions run in sequence, each
    /// followed by either its value (match) or the rest of the chain (miss), so
    /// the chain is folded from the back.
    fn analyze_multi_if(
        &self,
        program: &CompactProgram,
        branches: crate::model::ArgSlice,
        default: NodeRef,
        live: LiveSet,
        to_take: &mut Vec<NodeRef>,
    ) -> LiveSet {
        let branch_refs = program.args(branches).to_vec();
        // Live after a condition that misses, starting from the default.
        let mut carry = self.analyze(program, default, live.clone(), to_take);
        // `branch_refs` is the flattened `[c0, v0, c1, v1, …]` run.
        for pair in (0..branch_refs.len() / 2).rev() {
            let condition = branch_refs[2 * pair];
            let value = branch_refs[2 * pair + 1];
            let after_value = self.analyze(program, value, live.clone(), to_take);
            let mut after_condition = after_value;
            after_condition.union_with(&carry);
            carry = self.analyze(program, condition, after_condition, to_take);
        }
        carry
    }

    /// Liveness of a `switch`. The arms (table branches + default) are mutually
    /// exclusive; the inputs are always evaluated first (to form the key), so
    /// they thread the union of the arms' live-before sets in reverse.
    fn analyze_switch(
        &self,
        program: &CompactProgram,
        inputs: crate::model::ArgSlice,
        table: crate::model::SwitchTableId,
        default: NodeRef,
        live: LiveSet,
        to_take: &mut Vec<NodeRef>,
    ) -> LiveSet {
        let branches: Vec<NodeRef> = program.switch_table(table).branches().collect();
        let mut arms_union = self.analyze(program, default, live.clone(), to_take);
        for branch in branches {
            let after_branch = self.analyze(program, branch, live.clone(), to_take);
            arms_union.union_with(&after_branch);
        }
        let mut live = arms_union;
        for input in program.args(inputs).iter().rev() {
            live = self.analyze(program, *input, live, to_take);
        }
        live
    }
}

/// A set of live register slots, backed by a `u64` bitset. Cloned at each
/// branch, so a compact word layout matters; register counts are small in
/// practice (one bit per variable).
#[derive(Clone)]
struct LiveSet {
    words: Vec<u64>,
}

impl LiveSet {
    fn new(register_count: u16) -> Self {
        Self {
            words: vec![0; (register_count as usize).div_ceil(64)],
        }
    }

    fn contains(&self, register: RegisterId) -> bool {
        let (word, bit) = Self::position(register);
        self.words[word] & (1u64 << bit) != 0
    }

    fn insert(&mut self, register: RegisterId) {
        let (word, bit) = Self::position(register);
        self.words[word] |= 1u64 << bit;
    }

    fn remove(&mut self, register: RegisterId) {
        let (word, bit) = Self::position(register);
        self.words[word] &= !(1u64 << bit);
    }

    fn union_with(&mut self, other: &LiveSet) {
        for (slot, incoming) in self.words.iter_mut().zip(&other.words) {
            *slot |= *incoming;
        }
    }

    fn position(register: RegisterId) -> (usize, usize) {
        let index = register as usize;
        (index / 64, index % 64)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::LiveSet;

    #[test]
    fn insert_contains_remove_round_trip() {
        let mut set = LiveSet::new(130);
        assert!(!set.contains(0));
        assert!(!set.contains(65));
        assert!(!set.contains(129));

        set.insert(0);
        set.insert(65);
        set.insert(129);
        assert!(set.contains(0));
        assert!(set.contains(65));
        assert!(set.contains(129));
        assert!(!set.contains(1));

        set.remove(65);
        assert!(!set.contains(65));
        assert!(set.contains(0));
        assert!(set.contains(129));
    }

    #[test]
    fn union_with_merges_words() {
        let mut a = LiveSet::new(70);
        let mut b = LiveSet::new(70);
        a.insert(1);
        b.insert(64);
        a.union_with(&b);
        assert!(a.contains(1));
        assert!(a.contains(64));
    }
}
