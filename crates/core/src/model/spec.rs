use serde::{Deserialize, Serialize};

use crate::types::value::Value;

use super::cursor::CursorState;

#[derive(Debug, Clone)]
pub struct Row {
    pub values: Vec<Value>,
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
}

#[derive(Debug, Clone)]
pub struct WriteSpec {
    pub columns: Vec<String>,
    pub table: String,
    /// Optional upsert directive. Absent → plain INSERT / insertMany.
    /// Present → sink upserts on `conflict.key` using `strategy`.
    pub conflict: Option<crate::config::conflict::ConflictConfig>,
}
