//! Stable node identity for the heap optimization IR.
//!
//! Every [`OptExpr`](crate::model::opt_expr::OptExpr) node carries a [`NodeId`]
//! minted at construction from a single per-compile [`NodeCounter`]. The id is a
//! *pure identity*: it is excluded from structural equality (two nodes of the
//! same shape are equal regardless of id, so the golden structural tests are
//! unaffected) and carries no provenance.
//!
//! The id is the key of the static type map the type-check pass derives
//! (`AHashMap<NodeId, Type>`): a read-only walk records each node's output type
//! under its id, and the typed-discharge pass looks the type back up by id. The
//! counter is threaded only through the node-*constructing* stages (the
//! converter, the rewrite rules via [`RuleCx`](crate::rules::RuleCx), the second
//! pass, and guard propagation); the type-check pass mints nothing, and discharge
//! carries a consumed node's id forward onto its replacement.
//!
//! Ids do not survive compaction — they live on the heap IR only, exactly as the
//! type map does. The counter lives on the compile pipeline, never on the runtime
//! `EvalContext` (which is cloned per batch on the hot path).

use std::cell::Cell;

/// A stable, per-compile-unique identity for an [`OptExpr`](crate::model::opt_expr::OptExpr)
/// node. Used as the key of the static type map. `Copy` and cheap to hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct NodeId(u64);

/// Mints monotonically increasing [`NodeId`]s for one compilation. Interior
/// mutability (`Cell`) so it threads through the passes behind a shared
/// reference, matching how the registry and eval context are shared.
pub(crate) struct NodeCounter {
    next: Cell<u64>,
}

impl NodeCounter {
    /// A fresh counter starting at zero. One per `Optimizer::optimize` call, so
    /// ids are unique within a compile and deterministic across runs (the pass
    /// order is fixed).
    pub(crate) fn create() -> Self {
        Self { next: Cell::new(0) }
    }

    /// Mint the next unique id. Globally unique within this compile by
    /// construction (monotonic, never reused).
    pub(crate) fn fresh_id(&self) -> NodeId {
        let id = self.next.get();
        self.next.set(id + 1);
        NodeId(id)
    }
}

#[cfg(test)]
impl NodeId {
    /// A fixed placeholder id for tests that hand-build an expected `OptExpr` tree
    /// to compare structurally. Structural equality excludes the id, so the value
    /// is irrelevant — these trees are never fed back through the type map.
    pub(crate) const PLACEHOLDER: NodeId = NodeId(u64::MAX);
}
