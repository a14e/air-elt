//! The compacted, executable program (`CompactProgram` + `OptNode`).
//!
//! This is the optimizer's public output. Nodes live in a single arena laid
//! out in execution (post-)order for cache locality; children are referenced
//! by [`NodeRef`]. Variadic runs of child references (call arguments, `multiIf`
//! branches, interpolation segments) live in a second arena addressed by
//! [`ArgSlice`]. Object keys are interned into a deduplicated string pool
//! addressed by [`KeyId`]; each object holds a contiguous run of those ids in a
//! third arena addressed by [`KeySlice`], so a key name shared across objects
//! (e.g. `"id"`) is stored once. Constants are interned into a flat pool and
//! addressed by [`ConstId`]; variables resolve to register slots addressed by
//! [`RegisterId`]. No node carries an inline heap collection.

use ahash::AHashMap;
use air_elt_commons_arena::{Arena, ArenaRef, ArenaSlice};
use air_elt_expr_funcs::FuncRef;
use air_elt_expr_parse::FieldsSelector;
use air_elt_types::{DataType, Key, Value};

/// Index into a [`CompactProgram`]'s constant pool.
pub type ConstId = u16;

/// Index of a variable's register slot.
pub type RegisterId = u16;

/// Index into a [`CompactProgram`]'s switch-table side list.
pub type SwitchTableId = u16;

/// Index into a [`CompactProgram`]'s deduplicated object-key string pool.
pub type KeyId = u16;

/// A reference to one node in a [`CompactProgram`]'s node arena.
pub type NodeRef = ArenaRef<OptNode>;

/// A reference to a contiguous run in the child-reference arena (each element
/// is itself a [`NodeRef`]).
pub type ArgSlice = ArenaSlice<NodeRef>;

/// A reference to a contiguous run of object keys (each element is a [`KeyId`]
/// into the deduplicated key pool).
pub type KeySlice = ArenaSlice<KeyId>;

/// A coarse type category a [`OptNode::TypeAssert`] checks its operand against.
///
/// Deliberately NOT a full [`DataType`]: Phase 2 has no resolved types, and the
/// category is only ever the **weakest condition sufficient** for the operation
/// that was eliminated (e.g. `contains` needs its operand to be textual — that is
/// `String`, nothing finer). Keeping it minimal matters for schemaless sources /
/// sinks, where the operand's concrete type is resolved per row: an over-strict
/// assert would reject otherwise-valid dynamic data. A variant is added only when
/// a rewrite needs it (no future-proofing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeClass {
    /// Any textual value (`DataType::Text`).
    String,
    /// A boolean (`DataType::Bool`).
    Bool,
    /// Any binary value (`DataType::Bytes`).
    Bytes,
}

impl TypeClass {
    /// Whether `data_type` belongs to this category — the per-row runtime check.
    pub fn accepts(&self, data_type: &DataType) -> bool {
        match self {
            TypeClass::String => matches!(data_type, DataType::Text { .. }),
            TypeClass::Bool => matches!(data_type, DataType::Bool),
            TypeClass::Bytes => matches!(data_type, DataType::Bytes { .. }),
        }
    }

    /// Human-readable category name, for the assertion-failure error.
    pub fn describe(&self) -> &'static str {
        match self {
            TypeClass::String => "String",
            TypeClass::Bool => "Bool",
            TypeClass::Bytes => "Bytes",
        }
    }
}

/// What an [`OptNode::TypeAssert`] yields once its operand is present and of the
/// expected class.
#[derive(Debug, Clone, Copy)]
pub enum CompactYield {
    /// Yield the (already-evaluated) operand unchanged — for round-trip identities.
    Identity,
    /// Yield a fixed constant — for degenerate operations (`contains(x, "")`).
    Const(ConstId),
}

/// One executable node. Mirrors the optimization IR but fully flattened:
/// children are arena references, constants are pool indices, variadic child
/// runs and object keys are arena slices. No variant owns a heap collection.
#[derive(Debug)]
pub enum OptNode {
    /// A constant addressed by pool index.
    Const(ConstId),
    /// A variable's register slot, read by **clone**.
    Register(RegisterId),
    /// A variable's register slot, read by **move**: the
    /// [`MoveAnnotator`](crate::engines) proved this is the register's last read
    /// on every execution path, so the evaluator may take the value out instead
    /// of cloning it. Treating a `RegisterTake` as a plain `Register` (clone) is
    /// always sound — the move is purely an optimization — so an evaluator that
    /// ignores the distinction stays correct. Produced only by the annotator,
    /// never by compaction.
    RegisterTake(RegisterId),
    /// A resolved source column reference.
    SourceField(String),
    /// `field(<expr>)` with a still-dynamic argument (rejected by the
    /// type-check pass; represented so compaction stays total).
    Field(NodeRef),
    /// `fields("*")` / `fields("a,b")`.
    Fields(FieldsSelector),
    /// A function/operator call over an argument run.
    Call { func: FuncRef, args: ArgSlice },
    /// `if(condition, then, else)`.
    If {
        condition: NodeRef,
        then_branch: NodeRef,
        else_branch: NodeRef,
    },
    /// `multiIf` with its branches flattened into a single run of alternating
    /// `condition, value` references (so `branches.len()` is always even).
    MultiIf {
        branches: ArgSlice,
        default: NodeRef,
    },
    /// `ifNull(value, alternative)`.
    IfNull {
        value: NodeRef,
        alternative: NodeRef,
    },
    /// `nullIf(value, sentinel)`.
    NullIf { value: NodeRef, sentinel: NodeRef },
    /// `a && b`.
    And { left: NodeRef, right: NodeRef },
    /// `a || b`.
    Or { left: NodeRef, right: NodeRef },
    /// String interpolation: a run of nodes whose rendered values concatenate.
    /// Literal text segments are lowered to `Const(Text)` nodes, so a segment
    /// is just another expression.
    Interpolation(ArgSlice),
    /// Object literal: parallel runs of keys and value nodes (`keys[i]` pairs
    /// with `values[i]`). Each key is a [`KeyId`] into the deduplicated key
    /// pool; resolve it with [`CompactProgram::key_name`].
    Object { keys: KeySlice, values: ArgSlice },
    /// Constant-key dispatch: evaluate `inputs` (1–2 nodes) into a [`Key`], look
    /// it up in the side table `table`, and run the matched branch or `default`.
    Switch {
        inputs: ArgSlice,
        table: SwitchTableId,
        default: NodeRef,
    },
    /// Assert `inner` is non-null and of `expect`'s class, then yield `on_present`
    /// — the defunctionalized form of "preserve the type/null error of an
    /// eliminated operation". A rewrite that drops a heavy op over a *dynamic*
    /// operand (`contains(x, "")`, `reverse(reverse(x))`) leaves this behind so
    /// the same `Null`-propagation and `TypeMismatch` survive. Evaluates per row,
    /// so it doubles as a runtime guard for schemaless sources/sinks; the Phase-3
    /// type-aware pass discharges it once `inner`'s type is statically known.
    TypeAssert {
        inner: NodeRef,
        expect: TypeClass,
        on_present: CompactYield,
    },
    /// A scoped binding: evaluate `value` into `register`, then evaluate `body`
    /// (the result). The flattened form of a heap
    /// [`Block`](crate::model::opt_expr::OptExpr::Block) — a multi-binding block
    /// lowers to nested `Bind` nodes. Because it is reached only when control
    /// descends to it, the binding is evaluated lazily/branch-locally (a binding
    /// introduced inside one `if` arm is written only when that arm runs),
    /// enabling branch-local CSE and computation push-down. The register is a
    /// slot in the program-wide register file.
    ///
    /// Reached only once a producer of the heap `Block` exists (the planned
    /// CSE / push-down passes). Until then no `Block` is built, so compaction
    /// never emits a `Bind` and the variant is `dead_code` — kept wired so the
    /// evaluator and arena layout are ready when the passes land.
    #[allow(dead_code)]
    Bind {
        register: RegisterId,
        value: NodeRef,
        body: NodeRef,
    },
}

/// A compacted switch dispatch table: an O(1) map from constant [`Key`] to the
/// matched branch node, plus the key arity (1 or 2). Stored in a `Vec`-backed
/// side list rather than the node arena — the keys must not consume the arena's
/// `u16` budget (a generated table can hold thousands of entries), and a hashmap
/// is not an arena shape.
#[derive(Debug)]
pub struct SwitchTable {
    map: AHashMap<Key, NodeRef>,
    key_arity: u8,
}

impl SwitchTable {
    pub(crate) fn create(map: AHashMap<Key, NodeRef>, key_arity: u8) -> Self {
        Self { map, key_arity }
    }

    /// The branch matched by `key`, if any.
    pub fn lookup(&self, key: &Key) -> Option<NodeRef> {
        self.map.get(key).copied()
    }

    /// The node references of every dispatch branch (excluding the default).
    /// Used by the [`MoveAnnotator`](crate::engines) to treat the arms as
    /// mutually-exclusive branches during liveness analysis.
    pub(crate) fn branches(&self) -> impl Iterator<Item = NodeRef> + '_ {
        self.map.values().copied()
    }

    /// Number of key expressions (1 or 2) the dispatch reads.
    pub fn key_arity(&self) -> u8 {
        self.key_arity
    }
}

/// A variable binding: evaluate `value` and store it in `register` before the
/// result (and any later statements) are evaluated. Statements appear in
/// source order.
#[derive(Debug)]
pub struct CompactStatement {
    pub register: RegisterId,
    pub value: NodeRef,
}

/// The optimizer's executable output. Holds the node arena, the child-reference
/// arena, the object-key run arena, the deduplicated key pool, the constant
/// pool, the register count, the ordered statements, and the result node.
#[derive(Debug)]
pub struct CompactProgram {
    nodes: Arena<OptNode>,
    args: Arena<NodeRef>,
    key_runs: Arena<KeyId>,
    key_pool: Vec<String>,
    consts: Vec<Value>,
    switch_tables: Vec<SwitchTable>,
    register_count: u16,
    statements: Vec<CompactStatement>,
    result: NodeRef,
}

impl CompactProgram {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create(
        nodes: Arena<OptNode>,
        args: Arena<NodeRef>,
        key_runs: Arena<KeyId>,
        key_pool: Vec<String>,
        consts: Vec<Value>,
        switch_tables: Vec<SwitchTable>,
        register_count: u16,
        statements: Vec<CompactStatement>,
        result: NodeRef,
    ) -> Self {
        Self {
            nodes,
            args,
            key_runs,
            key_pool,
            consts,
            switch_tables,
            register_count,
            statements,
            result,
        }
    }

    /// Borrow the node at `node_ref`.
    pub fn node(&self, node_ref: NodeRef) -> &OptNode {
        self.nodes.get(node_ref)
    }

    /// Mutably borrow the node at `node_ref`. Used by the post-compaction
    /// [`MoveAnnotator`](crate::engines) to rewrite a `Register` read into a
    /// `RegisterTake` in place.
    pub(crate) fn node_mut(&mut self, node_ref: NodeRef) -> &mut OptNode {
        self.nodes.get_mut(node_ref)
    }

    /// Borrow the switch table at `id`.
    pub fn switch_table(&self, id: SwitchTableId) -> &SwitchTable {
        &self.switch_tables[id as usize]
    }

    /// Borrow the child-reference run described by `slice`.
    pub fn args(&self, slice: ArgSlice) -> &[NodeRef] {
        self.args.slice(slice)
    }

    /// Borrow the run of object-key ids described by `slice`. Resolve each id
    /// to its name with [`Self::key_name`].
    pub fn keys(&self, slice: KeySlice) -> &[KeyId] {
        self.key_runs.slice(slice)
    }

    /// Resolve an interned object-key id to its string name.
    pub fn key_name(&self, id: KeyId) -> &str {
        &self.key_pool[id as usize]
    }

    /// Borrow the constant at `id`.
    pub fn constant(&self, id: ConstId) -> &Value {
        &self.consts[id as usize]
    }

    /// Number of register slots the program needs.
    pub fn register_count(&self) -> u16 {
        self.register_count
    }

    /// The ordered variable bindings, evaluated before the result.
    pub fn statements(&self) -> &[CompactStatement] {
        &self.statements
    }

    /// The program's result node.
    pub fn result(&self) -> NodeRef {
        self.result
    }

    /// Every named source-column reference in the program, in node-arena
    /// (execution) order. Covers both `field(<name>)` (a single
    /// [`OptNode::SourceField`]) and the named form `fields("a,b")` (an
    /// [`OptNode::Fields`] carrying [`FieldsSelector::Named`]) — the latter
    /// reads each listed column, so those names must be projected too.
    /// Names may repeat — the caller deduplicates. The wildcard
    /// `fields("*")` carries no names; query it via [`Self::reads_all_fields`].
    /// Used by the runtime layer to compute the set of source columns a
    /// compute script reads.
    pub fn source_fields(&self) -> impl Iterator<Item = &str> {
        self.nodes.iter().flat_map(|node| {
            let names: &[String] = match node {
                OptNode::SourceField(name) => std::slice::from_ref(name),
                OptNode::Fields(FieldsSelector::Named(names)) => names.as_slice(),
                _ => &[],
            };
            names.iter().map(String::as_str)
        })
    }

    /// `true` when the program reads the whole row via `fields("*")`
    /// ([`FieldsSelector::All`]). Such a program needs every source column
    /// projected, not just the ones named through `field`/`fields("named")`,
    /// so the Transform compiler unions the full source schema into the read
    /// projection.
    pub fn reads_all_fields(&self) -> bool {
        self.nodes
            .iter()
            .any(|node| matches!(node, OptNode::Fields(FieldsSelector::All)))
    }

    /// Total number of nodes in the program (for diagnostics and tests).
    pub fn node_len(&self) -> usize {
        self.nodes.len()
    }

    /// Size of the constant pool (for diagnostics and tests).
    pub fn const_len(&self) -> usize {
        self.consts.len()
    }

    /// Number of distinct interned object keys (for diagnostics and tests).
    pub fn key_pool_len(&self) -> usize {
        self.key_pool.len()
    }
}
