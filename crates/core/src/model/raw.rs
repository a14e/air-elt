//! Source-layer batch types.
//!
//! Sources produce [`RawBatch`] from `read_batch`. The Transform layer
//! ([`crate::transform::Transform::apply`]) consumes a `RawBatch` and
//! produces a [`Batch`](crate::model::Batch) for sinks. [`crate::model::Row`]
//! carries only sink-bound state (`values`, `op`); [`RawRow`] additionally
//! carries an opaque body [`Value`] when the flow has body targets.

use crate::model::{CursorState, Row, RowOp};
use crate::types::Value;

/// Source-layer batch. Produced by `Source::read_batch`, consumed by
/// `Transform::apply`.
#[derive(Debug, Default)]
pub struct RawBatch {
    pub rows: Vec<RawRow>,
    pub next_cursor: Option<CursorState>,
}

impl RawBatch {
    /// Drop bodies and forward each `RawRow.values` straight into a
    /// [`Batch`](crate::model::Batch). Convenience for tests that
    /// exercise `Source::read_batch` end-to-end without running a
    /// Transform program — production code goes through `Transform::apply`.
    pub fn into_batch(self) -> crate::model::Batch {
        let RawBatch { rows, next_cursor } = self;
        crate::model::Batch {
            rows: rows.into_iter().map(Row::from).collect(),
            next_cursor,
        }
    }
}

/// Per-row source payload before the Transform program runs.
#[derive(Debug, Default)]
pub struct RawRow {
    /// One canonical `Value` per `ReadSpec.columns` slot. Source decodes
    /// vendor types to canonical `Value`. Order matches `ReadSpec.columns`.
    pub values: Vec<Value>,
    /// Optional "full document" body. Attached only when the flow has
    /// at least one body target (i.e. `ReadSpec.needs_body` is `true`).
    /// Relational sources push `Value::Json(...)`; mongo sources push
    /// `Value::Custom(BsonObjectValue(doc))`. The Transform interpreter
    /// `Body` op consumes it via `take()` (last reference) or
    /// `clone()` (earlier references).
    pub body: Option<Value>,
    /// Per-row directive — pulled-source rows are `Upsert`, CDC tombstone
    /// rows are `Delete`.
    pub op: RowOp,
}

impl RawRow {
    /// Build a regular upsert row from per-column values. Shorthand for
    /// the common source-side construction.
    pub fn upsert(values: Vec<Value>) -> Self {
        Self {
            values,
            body: None,
            op: RowOp::Upsert,
        }
    }

    /// Build a delete row (CDC tombstone). Body is `None` by default;
    /// callers attach one via [`Self::with_body`] when the flow needs
    /// body folding.
    pub fn delete(values: Vec<Value>) -> Self {
        Self {
            values,
            body: None,
            op: RowOp::Delete,
        }
    }

    /// Builder that attaches a body when the flow has body targets
    /// (`ReadSpec.needs_body == true`). `None` is a no-op so call sites
    /// can pass the cost-guarded `Option<Value>` straight in.
    pub fn with_body(mut self, body: Option<Value>) -> Self {
        self.body = body;
        self
    }
}

// Adapters preserved for the few call sites that still straddle the
// boundary: identity-only flows where the source emitted a `RawBatch`
// and the runner forwards it as a `Batch` after a noop
// `Transform::apply`. Drops `body`.
impl From<Row> for RawRow {
    fn from(row: Row) -> Self {
        Self {
            values: row.values,
            body: None,
            op: row.op,
        }
    }
}

impl From<RawRow> for Row {
    /// Drops `body`.
    fn from(raw: RawRow) -> Self {
        Row {
            values: raw.values,
            op: raw.op,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn row_to_raw_row_round_trip_preserves_values_and_op() {
        let original = Row::delete(vec![Value::Int32(1), Value::Text("x".into())]);
        let original_values = original.values.clone();
        let original_op = original.op;

        let raw: RawRow = original.into();
        assert!(raw.body.is_none());
        assert_eq!(raw.values, original_values);
        assert_eq!(raw.op, original_op);

        let back: Row = raw.into();
        assert_eq!(back.values, original_values);
        assert_eq!(back.op, original_op);
    }

    #[test]
    fn with_body_attaches_value() {
        let payload = serde_json::json!({"a": 1, "b": "two"});
        let raw = RawRow::upsert(Vec::new()).with_body(Some(Value::Json(payload.clone())));
        assert_eq!(raw.body, Some(Value::Json(payload)));
    }
}
