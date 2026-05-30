//! Compaction: the heap IR (`OptProgram`) → the executable [`CompactProgram`].
//!
//! A single post-order walk lays every node into the node arena in execution
//! order (children before parents, statements before the result) for cache
//! locality. Variadic child runs (call arguments, `multiIf` branches,
//! interpolation segments) are built in a second arena with `open_slice`/`push`.
//! Object keys are interned into a deduplicated string pool (so a name shared
//! across objects is stored once) and each object holds a contiguous run of the
//! resulting ids in a third arena. Constants are interned into a flat pool in
//! the same pass.
//!
//! Constant dedup is a whole-pool O(1) hash lookup keyed by
//! `(Discriminant<Value>, Key)`. [`Key`] supplies hash + equality, but it
//! canonicalises integer width and compares cross-numerically, so the variant
//! `Discriminant` is paired with it to stay **type-exact**: `Int8(5)` must NOT
//! share a slot with `Int64(5)`, or the reused node's resolved type would
//! silently change. Constants whose `Key` equality is not bit-exact —
//! `Float32`/`Float64` (`-0.0` ≡ `0.0`, NaN payloads), `Decimal` (scale), and
//! the unkeyable `Json`/`Object`/`Null`/`Custom` — are never deduped and always
//! take a fresh slot.

use std::mem::Discriminant;

use ahash::AHashMap;
use air_elt_commons_arena::{Arena, ArenaOverflow};
use air_elt_types::{Key, Value};

use crate::error::OptimizeError;
use crate::model::opt_expr::{AssertYield, OptExpr};
use crate::model::opt_program::OptProgram;
use crate::model::program::{
    ArgSlice, CompactProgram, CompactStatement, CompactYield, ConstId, KeyId, KeySlice, NodeRef,
    OptNode, SwitchTable,
};

/// Lays a fully-optimized [`OptProgram`] into the arena-backed
/// [`CompactProgram`], interning constants and keys as it goes.
pub(crate) struct Compactor {
    nodes: Arena<OptNode>,
    args: Arena<NodeRef>,
    key_runs: Arena<KeyId>,
    key_pool: Vec<String>,
    key_dedup: AHashMap<String, KeyId>,
    consts: Vec<Value>,
    const_dedup: AHashMap<(Discriminant<Value>, Key), ConstId>,
    switch_tables: Vec<SwitchTable>,
}

impl Compactor {
    pub(crate) fn create() -> Self {
        Self {
            nodes: Arena::new(),
            args: Arena::new(),
            key_runs: Arena::new(),
            key_pool: Vec::new(),
            key_dedup: AHashMap::new(),
            consts: Vec::new(),
            const_dedup: AHashMap::new(),
            switch_tables: Vec::new(),
        }
    }

    /// Compact a program into its executable form. Consumes the compactor
    /// since the arenas it fills become the program's storage.
    pub(crate) fn compact(mut self, program: OptProgram) -> Result<CompactProgram, OptimizeError> {
        let mut statements = Vec::with_capacity(program.statements.len());
        for statement in program.statements {
            let value = self.compact_expr(statement.value)?;
            statements.push(CompactStatement {
                register: statement.register,
                value,
            });
        }
        let result = self.compact_expr(program.result)?;

        Ok(CompactProgram::create(
            self.nodes,
            self.args,
            self.key_runs,
            self.key_pool,
            self.consts,
            self.switch_tables,
            program.register_count,
            statements,
            result,
        ))
    }

    fn intern_const(&mut self, value: Value) -> Result<ConstId, OptimizeError> {
        // Whole-pool dedup for exact-equality constants via an O(1) hash lookup.
        // Constants `Key` can't represent bit-exactly (floats, decimals, JSON, …)
        // always take a fresh slot.
        if let Some(dedup_key) = exact_dedup_key(&value) {
            if let Some(&id) = self.const_dedup.get(&dedup_key) {
                return Ok(id);
            }
            let id = self.push_const(value)?;
            self.const_dedup.insert(dedup_key, id);
            return Ok(id);
        }
        self.push_const(value)
    }

    fn push_const(&mut self, value: Value) -> Result<ConstId, OptimizeError> {
        let id = self.consts.len();
        if id > u16::MAX as usize {
            return Err(OptimizeError::Overflow(ArenaOverflow));
        }
        self.consts.push(value);
        Ok(id as ConstId)
    }

    fn compact_expr(&mut self, expr: OptExpr) -> Result<NodeRef, OptimizeError> {
        match expr {
            OptExpr::Const(value) => {
                let id = self.intern_const(value)?;
                Ok(self.nodes.alloc(OptNode::Const(id))?)
            }
            OptExpr::Register(register) => Ok(self.nodes.alloc(OptNode::Register(register))?),
            OptExpr::SourceField(name) => Ok(self.nodes.alloc(OptNode::SourceField(name))?),
            OptExpr::Field(inner) => {
                let inner = self.compact_expr(*inner)?;
                Ok(self.nodes.alloc(OptNode::Field(inner))?)
            }
            OptExpr::Fields(selector) => Ok(self.nodes.alloc(OptNode::Fields(selector))?),
            OptExpr::Call { func, args } => {
                let args = self.compact_run(args)?;
                Ok(self.nodes.alloc(OptNode::Call { func, args })?)
            }
            OptExpr::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition = self.compact_expr(*condition)?;
                let then_branch = self.compact_expr(*then_branch)?;
                let else_branch = self.compact_expr(*else_branch)?;
                Ok(self.nodes.alloc(OptNode::If {
                    condition,
                    then_branch,
                    else_branch,
                })?)
            }
            OptExpr::MultiIf { branches, default } => {
                // Flatten `(cond, val)` pairs into one alternating run.
                let mut flattened = Vec::with_capacity(branches.len() * 2);
                for (condition, value) in branches {
                    flattened.push(condition);
                    flattened.push(value);
                }
                let branches = self.compact_run(flattened)?;
                let default = self.compact_expr(*default)?;
                Ok(self.nodes.alloc(OptNode::MultiIf { branches, default })?)
            }
            OptExpr::IfNull { value, alternative } => {
                let value = self.compact_expr(*value)?;
                let alternative = self.compact_expr(*alternative)?;
                Ok(self.nodes.alloc(OptNode::IfNull { value, alternative })?)
            }
            OptExpr::NullIf { value, sentinel } => {
                let value = self.compact_expr(*value)?;
                let sentinel = self.compact_expr(*sentinel)?;
                Ok(self.nodes.alloc(OptNode::NullIf { value, sentinel })?)
            }
            OptExpr::And { left, right } => {
                let left = self.compact_expr(*left)?;
                let right = self.compact_expr(*right)?;
                Ok(self.nodes.alloc(OptNode::And { left, right })?)
            }
            OptExpr::Or { left, right } => {
                let left = self.compact_expr(*left)?;
                let right = self.compact_expr(*right)?;
                Ok(self.nodes.alloc(OptNode::Or { left, right })?)
            }
            OptExpr::Interpolation(segments) => {
                let segments = self.compact_run(segments)?;
                Ok(self.nodes.alloc(OptNode::Interpolation(segments))?)
            }
            OptExpr::Object(entries) => {
                let mut keys = Vec::with_capacity(entries.len());
                let mut values = Vec::with_capacity(entries.len());
                for (key, value) in entries {
                    keys.push(key);
                    values.push(value);
                }
                let values = self.compact_run(values)?;
                let keys = self.intern_keys(keys)?;
                Ok(self.nodes.alloc(OptNode::Object { keys, values })?)
            }
            OptExpr::Switch {
                inputs,
                table,
                default,
            } => {
                let key_arity = inputs.len() as u8;
                // Branch nodes first (the table maps keys to their refs); keys
                // are distinct (the rule dedups, first-match) so insertion order
                // is irrelevant.
                let mut map = AHashMap::with_capacity(table.len());
                for (key, branch) in table {
                    let branch_ref = self.compact_expr(branch)?;
                    map.entry(key).or_insert(branch_ref);
                }
                let default = self.compact_expr(*default)?;
                let inputs = self.compact_run(inputs)?;

                // `table_id` indexes the side list (the COUNT of switches), not
                // the table's entries — those live in the `usize`-keyed map and
                // never touch the arena, which is the whole point of option A.
                // The count is bounded by the number of `Switch` nodes, each of
                // which is itself a `u16` arena slot, so this cannot realistically
                // trip before the node arena overflows; it is defensive only.
                let table_id = self.switch_tables.len();
                if table_id > u16::MAX as usize {
                    return Err(OptimizeError::Overflow(ArenaOverflow));
                }
                self.switch_tables.push(SwitchTable::create(map, key_arity));
                Ok(self.nodes.alloc(OptNode::Switch {
                    inputs,
                    table: table_id as u16,
                    default,
                })?)
            }
            OptExpr::TypeAssert {
                inner,
                expect,
                on_present,
            } => {
                let inner = self.compact_expr(*inner)?;
                let on_present = match on_present {
                    AssertYield::Identity => CompactYield::Identity,
                    AssertYield::Const(value) => CompactYield::Const(self.intern_const(value)?),
                };
                Ok(self.nodes.alloc(OptNode::TypeAssert {
                    inner,
                    expect,
                    on_present,
                })?)
            }
        }
    }

    /// Compact a run of expressions and lay their root references into the
    /// child-reference arena as one contiguous slice. All children are compacted
    /// first, then the slice is filled at the (now-stable) arena tail.
    fn compact_run(&mut self, items: Vec<OptExpr>) -> Result<ArgSlice, OptimizeError> {
        let mut refs = Vec::with_capacity(items.len());
        for item in items {
            refs.push(self.compact_expr(item)?);
        }
        let mut slice = self.args.open_slice();
        for node_ref in refs {
            slice.push(&mut self.args, node_ref)?;
        }
        Ok(slice)
    }

    /// Intern a run of object keys into the deduplicated key pool and lay their
    /// ids into the key-run arena as one contiguous slice. All keys are interned
    /// first (which mutates the pool), then the ids are filled at the now-stable
    /// run-arena tail — mirroring [`Self::compact_run`].
    fn intern_keys(&mut self, keys: Vec<String>) -> Result<KeySlice, OptimizeError> {
        let mut ids = Vec::with_capacity(keys.len());
        for key in keys {
            ids.push(self.intern_key(key)?);
        }
        let mut slice = self.key_runs.open_slice();
        for id in ids {
            slice.push(&mut self.key_runs, id)?;
        }
        Ok(slice)
    }

    /// Map an object key name to its pool id, inserting it on first sight. A key
    /// shared across objects (e.g. `"id"`) collapses to a single pool entry.
    fn intern_key(&mut self, key: String) -> Result<KeyId, OptimizeError> {
        if let Some(&id) = self.key_dedup.get(&key) {
            return Ok(id);
        }
        let next = self.key_pool.len();
        if next > u16::MAX as usize {
            return Err(OptimizeError::Overflow(ArenaOverflow));
        }
        let id = next as KeyId;
        self.key_pool.push(key.clone());
        self.key_dedup.insert(key, id);
        Ok(id)
    }
}

/// The dedup identity for a constant, or `None` when the constant must always
/// get a fresh slot.
///
/// `Float32`/`Float64`/`Decimal` are screened out **before** projecting through
/// [`Key`]: `Key` accepts them but conflates `-0.0` with `0.0` (sign loss),
/// every `NaN` payload, and `Decimal` scale (`1.0` vs `1.00`), so deduping them
/// could drop sign / precision. Everything else relies on `Key` directly: a
/// **cursor-compatible** `Custom` deduplicates through its `DynValue`
/// equality/hash, while `Null`/`Json`/`Object`/non-cursor `Custom` have no
/// projection so [`Key::from_value`] yields `None` (the cursor-compatibility
/// gate lives inside it). The value's `Discriminant` is paired with the `Key`
/// so that `Key`'s width-canonicalised, cross-numeric equality (`Int8(5)` ≡
/// `Int64(5)` as keys) still keeps type-distinct constants apart.
fn exact_dedup_key(value: &Value) -> Option<(Discriminant<Value>, Key)> {
    if matches!(
        value,
        Value::Float32(_) | Value::Float64(_) | Value::Decimal(_)
    ) {
        return None;
    }
    Key::from_value(value).map(|key| (std::mem::discriminant(value), key))
}
