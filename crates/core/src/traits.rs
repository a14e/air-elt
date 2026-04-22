use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::RuntimeResult;
use crate::flow::state::CursorState;
use crate::schema::Schema;
use crate::types::value::Value;

/// One logical row read from a source / written to a sink. Ordering matches the
/// flow's mapping order, not the underlying table's column order.
#[derive(Debug, Clone)]
pub struct Row {
    pub values: Vec<Value>,
}

/// A read chunk. `next_cursor` is the state to persist *after* this batch is
/// successfully written downstream; if empty, the source returned nothing new.
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
    /// Column names to select (order is preserved and reflected in the `Row`s).
    pub columns: Vec<String>,
    /// Qualified table identifier in its raw, un-quoted form (e.g. "public.users").
    pub table: String,
    /// Ordered list of cursor columns.
    pub cursor_fields: Vec<String>,
    /// Cursor direction. Propagates from `flow.cursor.order`; both ASC and DESC
    /// must be honoured by the source (emit per-column direction in ORDER BY).
    pub cursor_order: crate::config::model::CursorOrder,
    /// Max rows per batch.
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct WriteSpec {
    /// Column names in the sink, matching `Row` order.
    pub columns: Vec<String>,
    pub table: String,
}

#[async_trait]
pub trait Source: Send + Sync {
    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()>;
    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema>;
    async fn read_batch(
        &self,
        spec: &ReadSpec,
        cursor: Option<&CursorState>,
    ) -> RuntimeResult<Batch>;
}

#[async_trait]
pub trait Sink: Send + Sync {
    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()>;
    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema>;
    async fn write_batch(&self, spec: &WriteSpec, batch: &Batch) -> RuntimeResult<WriteReport>;
}

#[async_trait]
pub trait Storage: Send + Sync {
    async fn validate_access(&self) -> RuntimeResult<()>;
    async fn migrate(&self) -> RuntimeResult<()>;
    async fn load_cursor(&self, flow: &str) -> RuntimeResult<Option<CursorState>>;
    async fn save_cursor(&self, flow: &str, state: &CursorState) -> RuntimeResult<()>;
}
