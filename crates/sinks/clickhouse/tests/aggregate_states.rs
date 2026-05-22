//! Round-trip every aggregate-state shape we care about (TDigest,
//! DDSketch, HLL-based uniq, plain uniq, uniqCombined) through the
//! sink's RowBinary path.
//!
//! The states are *opaque* — we cannot construct them in user space.
//! Instead we use ClickHouse itself to build a real state via
//! `*State(...)` aggregate-function combinators, ferry the bytes out
//! via `hex(state)`, push them back through the sink as
//! [`ChAggregateStateValue`], then ask CH to merge the inserted state
//! and prove the result is identical to the source aggregate computed
//! directly. If the byte payload were corrupted at any point on the
//! sink path the merged result would diverge or CH would error out.

use air_elt_commons_clickhouse::types::aggregate_state::ChAggregateStateValue;
use air_elt_commons_testing::clickhouse::clickhouse_handle;
use air_elt_core::model::{Batch, Row, RowOp, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_clickhouse::{ChSink, ChSinkConfig};

/// Driver for one aggregate kind. Each scenario:
/// * declares a CH `AggregateFunction(fn, args)` column type,
/// * populates a *source* staging table with one row whose state is
///   produced by `<fn>State(...)` on a deterministic numbers expression,
/// * extracts the state as hex via `SELECT hex(state) …`,
/// * inserts the same hex-decoded bytes into a *sink* table via
///   `ChSink::write_batch`,
/// * asserts that `<fn>Merge(state)` on the sink table equals
///   `<fn>(numbers_expr)` computed directly on the same population.
struct Scenario {
    label: &'static str,
    /// Air-Elt-side `fn_name` for `ChAggregateStateValue`. Echoes the
    /// CH side; not consumed by the encoder beyond labelling.
    fn_name: &'static str,
    /// SQL fragment for the column type, e.g.
    /// `AggregateFunction(quantilesTDigest, Float64)`.
    column_type: &'static str,
    /// SQL projection that builds one row of state, e.g.
    /// `quantilesTDigestState(0.5)(toFloat64(number))`.
    state_expr: &'static str,
    /// SQL fragment that population the input to the state expression.
    /// Same numbers must be used in both the staging state build and
    /// the direct-aggregate sanity expression.
    from_expr: &'static str,
    /// SQL projection that, given the merged state on the sink side,
    /// returns a scalar comparable to `direct_expr`.
    merge_expr: &'static str,
    /// SQL projection that computes the same aggregate **directly**
    /// (no state, no merge) on the same `from_expr` population.
    direct_expr: &'static str,
    /// `true` if the result is a floating point quantile / array — we
    /// then compare with a small absolute tolerance because TDigest /
    /// DDSketch are inherently approximate. `false` for exact integer
    /// aggregates (uniq, count).
    approximate: bool,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        label: "quantilesTDigest",
        fn_name: "quantilesTDigest",
        column_type: "AggregateFunction(quantilesTDigest(0.5), Float64)",
        state_expr: "quantilesTDigestState(0.5)(toFloat64(number))",
        from_expr: "numbers(1000)",
        merge_expr: "quantilesTDigestMerge(0.5)(state)[1]",
        direct_expr: "quantilesTDigest(0.5)(toFloat64(number))[1]",
        approximate: true,
    },
    Scenario {
        label: "quantilesDD",
        fn_name: "quantilesDD",
        // quantilesDD takes a relative-error parameter; 0.01 = 1% rel
        // error which matches the default in ClickHouse docs examples.
        column_type: "AggregateFunction(quantilesDD(0.01, 0.5), Float64)",
        state_expr: "quantilesDDState(0.01, 0.5)(toFloat64(number))",
        from_expr: "numbers(1000)",
        merge_expr: "quantilesDDMerge(0.01, 0.5)(state)[1]",
        direct_expr: "quantilesDD(0.01, 0.5)(toFloat64(number))[1]",
        approximate: true,
    },
    Scenario {
        label: "uniqHLL12",
        fn_name: "uniqHLL12",
        // HyperLogLog with 2^12 buckets — CH's stock HLL flavour.
        column_type: "AggregateFunction(uniqHLL12, UInt64)",
        state_expr: "uniqHLL12State(number)",
        from_expr: "numbers(1000)",
        merge_expr: "uniqHLL12Merge(state)",
        direct_expr: "uniqHLL12(number)",
        // HLL is approximate but for a tight 1000-element domain the
        // result is exactly the same as the direct aggregate — both go
        // through identical code paths. Compare exactly.
        approximate: false,
    },
    Scenario {
        label: "uniq",
        fn_name: "uniq",
        column_type: "AggregateFunction(uniq, UInt64)",
        state_expr: "uniqState(number)",
        from_expr: "numbers(1000)",
        merge_expr: "uniqMerge(state)",
        direct_expr: "uniq(number)",
        approximate: false,
    },
    Scenario {
        label: "uniqCombined",
        fn_name: "uniqCombined",
        column_type: "AggregateFunction(uniqCombined, UInt64)",
        state_expr: "uniqCombinedState(number)",
        from_expr: "numbers(1000)",
        merge_expr: "uniqCombinedMerge(state)",
        direct_expr: "uniqCombined(number)",
        approximate: false,
    },
];

#[tokio::test]
async fn round_trip_aggregate_states_through_sink() {
    let h = clickhouse_handle().await;

    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");

    for (idx, sc) in SCENARIOS.iter().enumerate() {
        let staging = format!("agg_src_{idx}");
        let sink_table = format!("agg_dst_{idx}");

        // Source: one row of CH-built aggregate state. Older CH builds
        // (the testcontainers default lags CI) may lack newer
        // aggregate-function combinators (`quantilesDD`, etc.). If the
        // CREATE fails, skip the scenario rather than failing — the
        // remaining scenarios still prove the opaque-bytes path.
        if let Err(e) = h
            .exec(&format!(
                "CREATE TABLE {staging} (state {ct}) ENGINE = MergeTree() ORDER BY tuple()",
                ct = sc.column_type
            ))
            .await
        {
            eprintln!(
                "[{}] skip — CREATE staging failed (likely unsupported on this CH version): {e}",
                sc.label
            );
            continue;
        }
        h.exec(&format!(
            "INSERT INTO {staging} SELECT {se} FROM {fe}",
            se = sc.state_expr,
            fe = sc.from_expr
        ))
        .await
        .unwrap_or_else(|e| panic!("[{}] INSERT state: {e}", sc.label));

        // Pull the state bytes out as hex. TabSeparated returns one
        // unquoted hex string per row — no escaping ambiguity.
        let hex_body = h
            .exec(&format!(
                "SELECT hex(state) FROM {staging} FORMAT TabSeparated"
            ))
            .await
            .unwrap_or_else(|e| panic!("[{}] SELECT hex: {e}", sc.label));
        let hex_str = hex_body.trim();
        assert!(
            !hex_str.is_empty(),
            "[{}] CH returned empty state hex",
            sc.label
        );
        let bytes = hex::decode(hex_str)
            .unwrap_or_else(|e| panic!("[{}] hex decode failed: {e}", sc.label));

        // Sink: independent table that we drive through our sink.
        h.exec(&format!(
            "CREATE TABLE {sink_table} (state {ct}) ENGINE = MergeTree() ORDER BY tuple()",
            ct = sc.column_type
        ))
        .await
        .unwrap_or_else(|e| panic!("[{}] CREATE sink table: {e}", sc.label));

        let spec = WriteSpec {
            table: sink_table.clone(),
            columns: vec!["state".into()],
            conflict: None,
        };
        sink.validate_access(&spec)
            .await
            .unwrap_or_else(|e| panic!("[{}] validate_access: {e}", sc.label));
        let ctx = sink
            .build_context(&spec)
            .await
            .unwrap_or_else(|e| panic!("[{}] build_context: {e}", sc.label));

        let batch = Batch {
            rows: vec![Row {
                values: vec![Value::Custom(Box::new(ChAggregateStateValue {
                    bytes: bytes.clone(),
                    fn_name: sc.fn_name.to_string(),
                }))],
                body: None,
                op: RowOp::Upsert,
            }],
            next_cursor: None,
        };
        let report = sink
            .write_batch(&spec, &ctx, batch, false)
            .await
            .unwrap_or_else(|e| panic!("[{}] write_batch: {e}", sc.label));
        assert_eq!(report.rows_written(), 1, "[{}] rows_written", sc.label);

        // Merge on the sink side vs. direct aggregate on the source
        // population — both expressed as a single Float64 (CH coerces
        // integer aggregates to Float64 in mixed projections; we cast
        // both sides explicitly to be unambiguous).
        let merged_body = h
            .exec(&format!(
                "SELECT toFloat64({me}) FROM {sink_table} FORMAT TabSeparated",
                me = sc.merge_expr
            ))
            .await
            .unwrap_or_else(|e| panic!("[{}] SELECT merge: {e}", sc.label));
        let direct_body = h
            .exec(&format!(
                "SELECT toFloat64({de}) FROM {fe} FORMAT TabSeparated",
                de = sc.direct_expr,
                fe = sc.from_expr
            ))
            .await
            .unwrap_or_else(|e| panic!("[{}] SELECT direct: {e}", sc.label));

        let merged: f64 = merged_body
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("[{}] parse merged {merged_body:?}: {e}", sc.label));
        let direct: f64 = direct_body
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("[{}] parse direct {direct_body:?}: {e}", sc.label));

        if sc.approximate {
            // 5% tolerance on quantile/sketch approximations against
            // direct computation. Both go through the same algorithm
            // on identical input, so this is generous in practice.
            let tolerance = direct.abs() * 0.05 + 1.0;
            assert!(
                (merged - direct).abs() <= tolerance,
                "[{}] merged={merged} vs direct={direct} (tolerance {tolerance})",
                sc.label
            );
        } else {
            assert_eq!(merged, direct, "[{}] merged != direct", sc.label);
        }
    }
}
