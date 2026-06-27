//! Per-row redis command construction.
//!
//! Each row of a write batch is lowered into a single redis command by
//! [`build_command`], driven by the flow's [`RedisMode`] and the resolved
//! [`ColumnLayout`]. The full redis key is the plain concatenation
//! `{to}{key}` (no separator inserted by the sink — the operator puts any
//! `:` into `to` or the computed `key`). Type contracts are enforced
//! here at build time: `key` must be `Text`, `value` is JSON-encoded via
//! the canonical encoder, `ttl` must be an `Interval`.

use redis::{Cmd, Pipeline};

use air_elt_core::error::{RuntimeError, RuntimeResult, TypeError};
use air_elt_core::model::{Row, RowOp};
use air_elt_core::types::{Value, value_to_json};

use crate::flow_options::{COL_KEY, COL_TTL, COL_VALUE, ColumnLayout, RedisMode};

/// Suffix appended to the flow's key prefix (`to`) to form the
/// write-access probe's sentinel key / channel. Touching the same
/// keyspace the flow writes to lets the probe catch per-prefix ACL rules
/// while staying clear of real data.
const ACCESS_PROBE_SUFFIX: &str = "__air_elt_access_probe__";
/// Throwaway payload written by the probe. Never read back.
const ACCESS_PROBE_PAYLOAD: &str = "air-elt-probe";

/// Build the mode-specific write-access probe. A bare `PING` only proves
/// connectivity and auth; this exercises the actual command the mode
/// issues, so a read-only replica, a missing-write ACL (`NOPERM`), or a
/// disabled command surfaces at validate time rather than on the first
/// live batch. Every probe cleans up after itself — a self-expiring `PX`
/// or an explicit `DEL` — so it leaves no trace in the keyspace. The one
/// unavoidable exception is `pubsub`: a `PUBLISH` can't be taken back, so
/// a client pattern-subscribed across the flow's prefix observes one
/// throwaway message on the `…__air_elt_access_probe__` sentinel channel
/// at validate time (the exact-channel subscribers the flow targets do
/// not).
pub fn build_access_probe(mode: RedisMode, to: &str) -> Pipeline {
    let sentinel = format!("{to}{ACCESS_PROBE_SUFFIX}");
    let mut pipe = redis::pipe();
    match mode {
        RedisMode::Kv => {
            // Self-expiring write — proves SET without needing DEL perm.
            pipe.cmd("SET")
                .arg(&sentinel)
                .arg(ACCESS_PROBE_PAYLOAD)
                .arg("PX")
                .arg(100);
        }
        RedisMode::KvDelete => {
            // The mode's own command; a missing key just returns 0.
            pipe.cmd("DEL").arg(&sentinel);
        }
        RedisMode::List => {
            pipe.cmd("RPUSH").arg(&sentinel).arg(ACCESS_PROBE_PAYLOAD);
            pipe.cmd("DEL").arg(&sentinel);
        }
        RedisMode::Stream => {
            pipe.cmd("XADD")
                .arg(&sentinel)
                .arg("*")
                .arg("data")
                .arg(ACCESS_PROBE_PAYLOAD);
            pipe.cmd("DEL").arg(&sentinel);
        }
        RedisMode::Pubsub => {
            // No persistence, no cleanup; zero receivers is fine.
            pipe.cmd("PUBLISH").arg(&sentinel).arg(ACCESS_PROBE_PAYLOAD);
        }
    }
    pipe
}

/// Outcome of lowering one row: a command to pipeline (tagged by whether
/// it counts as an upsert or a delete) or a skipped row.
pub enum BuiltCommand {
    Upsert(Cmd),
    Delete(Cmd),
    Skipped,
}

/// Lower a single row into a redis command for `mode`. `to` is the
/// flow's key prefix (`spec.table`).
pub fn build_command(
    mode: RedisMode,
    layout: &ColumnLayout,
    to: &str,
    row: &Row,
) -> RuntimeResult<BuiltCommand> {
    // `kv-delete` deletes regardless of `RowOp` — the mode *is* the
    // delete. A pull source emits `Upsert` rows; mapping them in
    // kv-delete mode issues a `DEL` per row.
    if mode == RedisMode::KvDelete {
        let key = full_key(layout, row, to, true)?;
        let mut cmd = redis::cmd("DEL");
        cmd.arg(key);
        return Ok(BuiltCommand::Delete(cmd));
    }

    // Write modes carry no per-row delete semantics. A `Delete` row is
    // only ever produced by a CDC source — which redis rejects via the
    // conflict-block guard — so this branch is defensive: drop the row
    // and count it as skipped.
    if row.op == RowOp::Delete {
        return Ok(BuiltCommand::Skipped);
    }

    let cmd = match mode {
        RedisMode::Kv => build_set(layout, row, to)?,
        RedisMode::List => build_rpush(layout, row, to)?,
        RedisMode::Stream => build_xadd(layout, row, to)?,
        RedisMode::Pubsub => build_publish(layout, row, to)?,
        RedisMode::KvDelete => unreachable!("kv-delete handled above"),
    };
    Ok(BuiltCommand::Upsert(cmd))
}

/// `SET {to}{key} {json} [PX ttl_ms]`.
fn build_set(layout: &ColumnLayout, row: &Row, to: &str) -> RuntimeResult<Cmd> {
    let key = full_key(layout, row, to, true)?;
    let json = value_json(layout, row)?;
    let mut cmd = redis::cmd("SET");
    cmd.arg(key).arg(json);
    if let Some(ttl_ms) = ttl_millis(layout, row)? {
        cmd.arg("PX").arg(ttl_ms);
    }
    Ok(cmd)
}

/// `RPUSH {to}{key?} {json}` — keyless flows push onto the list named `{to}`.
fn build_rpush(layout: &ColumnLayout, row: &Row, to: &str) -> RuntimeResult<Cmd> {
    let key = full_key(layout, row, to, false)?;
    let json = value_json(layout, row)?;
    let mut cmd = redis::cmd("RPUSH");
    cmd.arg(key).arg(json);
    Ok(cmd)
}

/// `XADD {to}{key} * data {json}` — server-assigned id, JSON under field `data`.
fn build_xadd(layout: &ColumnLayout, row: &Row, to: &str) -> RuntimeResult<Cmd> {
    let key = full_key(layout, row, to, true)?;
    let json = value_json(layout, row)?;
    let mut cmd = redis::cmd("XADD");
    cmd.arg(key).arg("*").arg("data").arg(json);
    Ok(cmd)
}

/// `PUBLISH {to}{key?} {json}` — keyless flows publish to channel `{to}`.
fn build_publish(layout: &ColumnLayout, row: &Row, to: &str) -> RuntimeResult<Cmd> {
    let channel = full_key(layout, row, to, false)?;
    let json = value_json(layout, row)?;
    let mut cmd = redis::cmd("PUBLISH");
    cmd.arg(channel).arg(json);
    Ok(cmd)
}

/// Resolve the full redis key `{to}{key}`. `key_required` toggles
/// handling of a missing / null key: required-key modes (kv, kv-delete,
/// stream) reject a null key; optional-key modes (list, pubsub) fall
/// back to the bare prefix `{to}`.
fn full_key(
    layout: &ColumnLayout,
    row: &Row,
    to: &str,
    key_required: bool,
) -> RuntimeResult<String> {
    let Some(idx) = layout.key_idx else {
        if key_required {
            return Err(RuntimeError::DerivedPlanInvariant {
                detail: format!("redis required-key mode missing resolved `{COL_KEY}` column"),
            });
        }
        return Ok(to.to_string());
    };
    match &row.values[idx] {
        Value::Text(s) => Ok(format!("{to}{s}")),
        Value::Null => {
            if key_required {
                Err(TypeError::SinkValueUnsupported {
                    column: COL_KEY.to_string(),
                    expected: "Text (non-null key)".to_string(),
                    got_kind: "Null".to_string(),
                }
                .into())
            } else {
                Ok(to.to_string())
            }
        }
        other => Err(TypeError::SinkValueUnsupported {
            column: COL_KEY.to_string(),
            expected: "Text".to_string(),
            got_kind: other.variant_name(),
        }
        .into()),
    }
}

/// JSON-encode the `value` column. The canonical encoder handles both
/// `Value::Object` (the object-literal shape the mapping produces) and
/// `Value::Json`, and errors on variants with no JSON form (e.g. `Interval`).
fn value_json(layout: &ColumnLayout, row: &Row) -> RuntimeResult<String> {
    let idx = layout
        .value_idx
        .ok_or_else(|| RuntimeError::DerivedPlanInvariant {
            detail: format!("redis value mode missing resolved `{COL_VALUE}` column"),
        })?;
    let json = value_to_json(&row.values[idx])?;
    let text = serde_json::to_string(&json)?;
    Ok(text)
}

/// Resolve the optional `ttl` column into milliseconds for `PX`. Absent
/// column or null value → no expiry.
fn ttl_millis(layout: &ColumnLayout, row: &Row) -> RuntimeResult<Option<u64>> {
    let Some(idx) = layout.ttl_idx else {
        return Ok(None);
    };
    match &row.values[idx] {
        Value::Interval(d) => {
            let ms = u64::try_from(d.as_millis()).map_err(|_| TypeError::SinkValueUnsupported {
                column: COL_TTL.to_string(),
                expected: "Interval within u64 milliseconds".to_string(),
                got_kind: format!("Interval({}ms)", d.as_millis()),
            })?;
            Ok(Some(ms))
        }
        Value::Null => Ok(None),
        other => Err(TypeError::SinkValueUnsupported {
            column: COL_TTL.to_string(),
            expected: "Interval".to_string(),
            got_kind: other.variant_name(),
        }
        .into()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn layout(key: Option<usize>, value: Option<usize>, ttl: Option<usize>) -> ColumnLayout {
        ColumnLayout {
            key_idx: key,
            value_idx: value,
            ttl_idx: ttl,
        }
    }

    fn obj(pairs: &[(&str, Value)]) -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn full_key_concatenates_prefix_and_key() {
        let row = Row::upsert(vec![Value::Text("42".into())]);
        let key = full_key(&layout(Some(0), None, None), &row, "users:", true).unwrap();
        assert_eq!(key, "users:42");
    }

    #[test]
    fn full_key_keyless_optional_uses_bare_prefix() {
        let row = Row::upsert(vec![Value::Json(serde_json::json!({"a": 1}))]);
        // value at 0, no key column.
        let key = full_key(&layout(None, Some(0), None), &row, "events", false).unwrap();
        assert_eq!(key, "events");
    }

    #[test]
    fn full_key_null_optional_uses_bare_prefix() {
        let row = Row::upsert(vec![Value::Null, Value::Json(serde_json::json!({}))]);
        let key = full_key(&layout(Some(0), Some(1), None), &row, "ch", false).unwrap();
        assert_eq!(key, "ch");
    }

    #[test]
    fn full_key_null_required_errors() {
        let row = Row::upsert(vec![Value::Null]);
        let err = full_key(&layout(Some(0), None, None), &row, "k", true).unwrap_err();
        assert!(matches!(
            err,
            RuntimeError::Type(TypeError::SinkValueUnsupported { .. })
        ));
    }

    #[test]
    fn full_key_wrong_type_errors() {
        let row = Row::upsert(vec![Value::Int64(7)]);
        let err = full_key(&layout(Some(0), None, None), &row, "k", true).unwrap_err();
        match err {
            RuntimeError::Type(TypeError::SinkValueUnsupported { got_kind, .. }) => {
                assert_eq!(got_kind, "Int64");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn value_json_encodes_object_literal() {
        let row = Row::upsert(vec![obj(&[
            ("name", Value::Text("ann".into())),
            ("age", Value::Int64(30)),
        ])]);
        let json = value_json(&layout(None, Some(0), None), &row).unwrap();
        // Object preserves insertion order.
        assert_eq!(json, r#"{"name":"ann","age":30}"#);
    }

    #[test]
    fn value_json_encodes_json_passthrough() {
        let row = Row::upsert(vec![Value::Json(serde_json::json!({"x": [1, 2]}))]);
        let json = value_json(&layout(None, Some(0), None), &row).unwrap();
        assert_eq!(json, r#"{"x":[1,2]}"#);
    }

    #[test]
    fn ttl_millis_reads_interval() {
        let row = Row::upsert(vec![Value::Interval(Duration::from_millis(1500))]);
        let ms = ttl_millis(&layout(None, None, Some(0)), &row).unwrap();
        assert_eq!(ms, Some(1500));
    }

    #[test]
    fn ttl_millis_absent_column_is_none() {
        let row = Row::upsert(vec![Value::Json(serde_json::json!(1))]);
        assert_eq!(
            ttl_millis(&layout(None, Some(0), None), &row).unwrap(),
            None
        );
    }

    #[test]
    fn ttl_millis_null_is_none() {
        let row = Row::upsert(vec![Value::Null]);
        assert_eq!(
            ttl_millis(&layout(None, None, Some(0)), &row).unwrap(),
            None
        );
    }

    #[test]
    fn ttl_millis_wrong_type_errors() {
        let row = Row::upsert(vec![Value::Int64(5)]);
        let err = ttl_millis(&layout(None, None, Some(0)), &row).unwrap_err();
        assert!(matches!(
            err,
            RuntimeError::Type(TypeError::SinkValueUnsupported { .. })
        ));
    }

    #[test]
    fn ttl_millis_overflowing_u64_errors() {
        // A duration whose millisecond count exceeds u64::MAX must hit the
        // `try_from` guard and return a typed error, not panic.
        let row = Row::upsert(vec![Value::Interval(Duration::from_secs(u64::MAX))]);
        let err = ttl_millis(&layout(None, None, Some(0)), &row).unwrap_err();
        match err {
            RuntimeError::Type(TypeError::SinkValueUnsupported { column, .. }) => {
                assert_eq!(column, COL_TTL);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn kv_delete_row_builds_delete_regardless_of_op() {
        let row = Row::upsert(vec![Value::Text("k1".into())]);
        let built = build_command(
            RedisMode::KvDelete,
            &layout(Some(0), None, None),
            "p:",
            &row,
        )
        .unwrap();
        assert!(matches!(built, BuiltCommand::Delete(_)));
    }

    #[test]
    fn write_mode_delete_row_is_skipped() {
        let row = Row::delete(vec![
            Value::Text("k".into()),
            Value::Json(serde_json::json!(1)),
        ]);
        let built =
            build_command(RedisMode::Kv, &layout(Some(0), Some(1), None), "p:", &row).unwrap();
        assert!(matches!(built, BuiltCommand::Skipped));
    }

    #[test]
    fn write_mode_upsert_row_builds_upsert() {
        let row = Row::upsert(vec![
            Value::Text("k".into()),
            Value::Json(serde_json::json!(1)),
        ]);
        let built =
            build_command(RedisMode::Kv, &layout(Some(0), Some(1), None), "p:", &row).unwrap();
        assert!(matches!(built, BuiltCommand::Upsert(_)));
    }

    #[test]
    fn value_json_rejects_interval() {
        // An interval nested in the value column has no JSON form.
        let row = Row::upsert(vec![Value::Interval(Duration::from_secs(1))]);
        let err = value_json(&layout(None, Some(0), None), &row).unwrap_err();
        assert!(matches!(err, RuntimeError::JsonEncode(_)));
    }

    #[test]
    fn access_probe_command_count_per_mode() {
        // Self-cleaning modes pipeline their write + an explicit DEL;
        // self-expiring / fire-and-forget modes need a single command.
        assert_eq!(build_access_probe(RedisMode::Kv, "p:").len(), 1);
        assert_eq!(build_access_probe(RedisMode::KvDelete, "p:").len(), 1);
        assert_eq!(build_access_probe(RedisMode::List, "p:").len(), 2);
        assert_eq!(build_access_probe(RedisMode::Stream, "p:").len(), 2);
        assert_eq!(build_access_probe(RedisMode::Pubsub, "p:").len(), 1);
    }
}
