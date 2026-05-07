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
