use serde::{Deserialize, Serialize};

use crate::types::value::Value;

use super::cursor::CursorState;

/// Per-row directive that tells the sink how to apply this row.
///
/// `Upsert` covers insert + update + replace from CDC and any plain
/// row from a pull-based source. Without a `[flow.<x>.conflict]`
/// block the sink falls back to a plain `INSERT` for `Upsert` rows.
///
/// `Delete` requires a configured `conflict.key` — the sink uses the
/// key columns of the row to issue a DELETE / `deleteMany`. Sources
/// emit `Delete` only when they observe a real removal (mongo-cdc).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RowOp {
    #[default]
    Upsert,
    Delete,
}

/// A sink-bound row produced by the Transform interpreter.
///
/// `values` carries one canonical [`Value`] per `WriteSpec.columns`
/// slot — the order of values matches the order of declared sink
/// columns. The schemaless raw-passthrough flow (mongo→mongo `["*"]`)
/// lowers to a Transform with a single `Body` op writing one
/// `Value::Custom(BsonObjectValue)`; mongo sink recognises that shape
/// and writes the BSON document at root.
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub values: Vec<Value>,
    /// Default `Upsert`. CDC sources set `Delete` for tombstone events.
    pub op: RowOp,
}

impl Row {
    pub fn upsert(values: Vec<Value>) -> Self {
        Self {
            values,
            op: RowOp::Upsert,
        }
    }
    pub fn delete(values: Vec<Value>) -> Self {
        Self {
            values,
            op: RowOp::Delete,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Batch {
    pub rows: Vec<Row>,
    pub next_cursor: Option<CursorState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WriteReport {
    pub rows_written: u64,
}

#[derive(Debug, Clone)]
pub struct ReadSpec {
    pub columns: Vec<String>,
    pub table: String,
    pub cursor_fields: Vec<String>,
    pub cursor_order: crate::config::model::CursorOrder,
    pub limit: usize,
    /// Per-flow source-specific options coming from the developed
    /// form `source = { name = "...", <opts...> }` in the flow block.
    /// Bare `source = "name"` produces an empty table. Sources that
    /// don't recognise these options ignore them; sources that do
    /// (e.g. mongo-cdc's `mode`) deserialize this into their own
    /// typed struct in `build_context`.
    pub source_options: toml::Table,
    /// `true` when the flow has at least one body target — sources
    /// populate `body: Option<Value>` on each
    /// [`crate::model::raw::RawRow`] so the Transform interpreter can
    /// fold body columns at native fidelity.
    /// Defaults to `false`; flipped on by
    /// `flow_state::build_derived_plans_from_expanded` when expansion
    /// produces a `body` block.
    pub needs_body: bool,
}

impl Default for ReadSpec {
    fn default() -> Self {
        Self {
            columns: Vec::new(),
            table: String::new(),
            cursor_fields: Vec::new(),
            cursor_order: crate::config::model::CursorOrder::Asc,
            limit: 0,
            source_options: toml::Table::new(),
            needs_body: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriteSpec {
    pub columns: Vec<String>,
    pub table: String,
    /// Optional upsert directive. Absent → plain INSERT / insertMany.
    /// Present → sink upserts on `conflict.key` using `strategy`. Also
    /// gates `Delete` row support: sinks reject Delete rows when this
    /// is `None` (no key to target).
    pub conflict: Option<crate::config::conflict::ConflictConfig>,
}

/// Schema-independent half of [`ReadSpec`]. Carries everything that's
/// final at config-assembly time — table, cursor, batch limit, per-
/// source options. `columns` and `needs_body` are deliberately absent
/// because they depend on schema introspection and live on the
/// post-expansion [`ReadSpec`] inside `DerivedPlans`.
#[derive(Debug, Clone)]
pub struct ConfigReadSpec {
    pub table: String,
    pub cursor_fields: Vec<String>,
    pub cursor_order: crate::config::model::CursorOrder,
    pub limit: usize,
    pub source_options: toml::Table,
}

/// Schema-independent half of [`WriteSpec`]. `columns` is absent — it
/// depends on expansion and lives on the post-expansion [`WriteSpec`]
/// inside `DerivedPlans`.
#[derive(Debug, Clone)]
pub struct ConfigWriteSpec {
    pub table: String,
    pub conflict: Option<crate::config::conflict::ConflictConfig>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn read_spec_default_needs_body_is_false() {
        let spec = ReadSpec::default();
        assert!(!spec.needs_body);
    }

    #[test]
    fn upsert_constructor_sets_op() {
        let row = Row::upsert(vec![Value::Int32(1), Value::Int32(2), Value::Int32(3)]);
        assert_eq!(row.op, RowOp::Upsert);
        assert_eq!(row.values.len(), 3);
    }

    #[test]
    fn delete_constructor_sets_op() {
        let row = Row::delete(vec![Value::Int32(7)]);
        assert_eq!(row.op, RowOp::Delete);
        assert_eq!(row.values, vec![Value::Int32(7)]);
    }
}
