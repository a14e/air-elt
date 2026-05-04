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

    /// Append a stable byte fingerprint of this row's `conflict.key`
    /// values to `buf`. Used by CDC dedup to bucket rows by their key
    /// tuple without relying on `Hash`/`Eq` for `Value` (the float /
    /// decimal / Json variants don't combine cleanly with a derived
    /// `Eq`/`Hash`).
    ///
    /// `key_indices` are positions into `self.values` that select the
    /// key columns. Pre-computed once per flow and exposed via
    /// `FlowState::dedup_key_indices` — call sites should never re-derive
    /// them per row.
    ///
    /// `buf` is *not* cleared — bytes are appended. The current dedup
    /// caller passes a fresh `Vec` per row (so the buffer can be moved
    /// into the dedup hashset without an extra clone); a future caller
    /// that hashes incrementally could pass a long-lived buffer and
    /// `clear()` between rows. Each value is tag-prefixed and
    /// terminated with `0xFF` (max byte — see `core::types::raw_key`)
    /// so cross-variant collisions are impossible. Non-NaN floats use
    /// their bit pattern; every NaN canonicalises to a single quiet-NaN
    /// pattern so distinct in-memory NaNs still bucket together.
    pub fn raw_key(&self, key_indices: &[usize], buf: &mut Vec<u8>) {
        for &i in key_indices {
            let v = self.values.get(i).unwrap_or(&Value::Null);
            crate::types::raw_key::write_value_key(buf, v);
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn key(values: Vec<Value>, indices: &[usize]) -> Vec<u8> {
        let row = Row::upsert(values);
        let mut buf = Vec::new();
        row.raw_key(indices, &mut buf);
        buf
    }

    #[test]
    fn compound_key_order_matters() {
        // (a, b) and (b, a) at conflict.key positions [0, 1] are
        // distinct fingerprints — column order is part of the key.
        let ab = key(
            vec![Value::Int64(1), Value::Int64(2), Value::Text("x".into())],
            &[0, 1],
        );
        let ba = key(
            vec![Value::Int64(2), Value::Int64(1), Value::Text("x".into())],
            &[0, 1],
        );
        assert_ne!(ab, ba);
    }

    #[test]
    fn key_uses_only_indexed_columns() {
        // Non-key columns must not affect the fingerprint — two rows
        // that agree on conflict.key but differ on payload columns
        // must bucket together (CDC dedup compacts by key, not by
        // full row).
        let with_x = key(vec![Value::Int64(7), Value::Text("payload-A".into())], &[0]);
        let with_y = key(vec![Value::Int64(7), Value::Text("payload-B".into())], &[0]);
        assert_eq!(with_x, with_y);
    }

    #[test]
    fn missing_index_treated_as_null() {
        // `Row::raw_key` substitutes Value::Null for an out-of-range
        // index instead of panicking — defensive against schema/spec
        // drift. The result must equal an explicit-Null encoding.
        let short = key(vec![Value::Int64(1)], &[0, 1]);
        let with_null = key(vec![Value::Int64(1), Value::Null], &[0, 1]);
        assert_eq!(short, with_null);
    }

    #[test]
    fn separator_prevents_text_concatenation_collision() {
        // ("ab", "c") at indices [0, 1] must not collide with
        // ("a", "bc") — the separator + length prefix make each
        // value self-delimiting.
        let ab_c = key(
            vec![Value::Text("ab".into()), Value::Text("c".into())],
            &[0, 1],
        );
        let a_bc = key(
            vec![Value::Text("a".into()), Value::Text("bc".into())],
            &[0, 1],
        );
        assert_ne!(ab_c, a_bc);
    }

    #[test]
    fn buf_is_appended_not_cleared() {
        // Caller invariant: `raw_key` *appends* to the existing buf
        // contents. We rely on this in dedup_cdc_batch which reuses a
        // single buffer + clear() between rows.
        let mut buf = vec![0xAA, 0xBB];
        Row::upsert(vec![Value::Int64(1)]).raw_key(&[0], &mut buf);
        assert_eq!(&buf[0..2], &[0xAA, 0xBB], "preexisting bytes preserved");
        assert!(buf.len() > 2, "raw_key actually wrote something");
    }
}
