//! Live end-to-end tests for the redis sink against a real Valkey
//! container — one per write mode. Each test drives the sink directly
//! (build_context → write_batch) and reads the result back through an
//! independent admin connection to prove the wire behaviour.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use futures::StreamExt;

use air_elt_commons_redis::{RedisPool, RedisPoolConfig};
use air_elt_commons_testing::valkey::valkey_handle;
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::error::{ConfigError, RuntimeError};
use air_elt_core::model::{Batch, Row, WriteReport, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_redis::RedisSink;

fn write_spec(columns: &[&str], to: &str, mode: &str) -> WriteSpec {
    let mut sink_options = toml::Table::new();
    sink_options.insert("mode".to_string(), toml::Value::String(mode.to_string()));
    WriteSpec {
        columns: columns.iter().map(|c| c.to_string()).collect(),
        table: to.to_string(),
        conflict: None,
        sink_options,
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

async fn sink_for(url: &str) -> RedisSink {
    let pool = RedisPool::create(url, &RedisPoolConfig::default())
        .await
        .unwrap();
    RedisSink::new(pool)
}

async fn admin(url: &str) -> redis::aio::MultiplexedConnection {
    redis::Client::open(url)
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap()
}

async fn build_and_write(sink: &RedisSink, spec: &WriteSpec, rows: Vec<Row>) -> WriteReport {
    let ctx = sink.build_context(spec).await.unwrap();
    sink.write_batch(
        spec,
        &ctx,
        Batch {
            rows,
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kv_sets_value_and_ttl() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let to = handle.key("kv:");
    let spec = write_spec(&["key", "value", "ttl"], &to, "kv");

    let row = Row::upsert(vec![
        Value::Text("u1".into()),
        obj(&[
            ("name", Value::Text("ann".into())),
            ("age", Value::Int64(30)),
        ]),
        Value::Interval(Duration::from_secs(60)),
    ]);
    let report = build_and_write(&sink, &spec, vec![row]).await;
    assert_eq!(report.upserts, 1);

    let mut conn = admin(&handle.url).await;
    let full = format!("{to}u1");
    let got: String = redis::cmd("GET")
        .arg(&full)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(got, r#"{"name":"ann","age":30}"#);
    let pttl: i64 = redis::cmd("PTTL")
        .arg(&full)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert!(pttl > 0 && pttl <= 60_000, "ttl should be set, got {pttl}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kv_without_ttl_has_no_expiry() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let to = handle.key("kvnottl:");
    let spec = write_spec(&["key", "value"], &to, "kv");

    let row = Row::upsert(vec![
        Value::Text("k".into()),
        Value::Json(serde_json::json!({"v": 1})),
    ]);
    build_and_write(&sink, &spec, vec![row]).await;

    let mut conn = admin(&handle.url).await;
    // PTTL returns -1 for a key with no expiry.
    let pttl: i64 = redis::cmd("PTTL")
        .arg(format!("{to}k"))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(pttl, -1, "no ttl column → key must never expire");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kv_delete_removes_key() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let to = handle.key("del:");

    let mut conn = admin(&handle.url).await;
    let full = format!("{to}gone");
    let _: () = redis::cmd("SET")
        .arg(&full)
        .arg("x")
        .query_async(&mut conn)
        .await
        .unwrap();

    let spec = write_spec(&["key"], &to, "kv-delete");
    let report = build_and_write(
        &sink,
        &spec,
        vec![Row::upsert(vec![Value::Text("gone".into())])],
    )
    .await;
    assert_eq!(report.deletes, 1);

    let exists: i64 = redis::cmd("EXISTS")
        .arg(&full)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(exists, 0, "kv-delete must remove the key");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_rpush_preserves_order() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    // Keyless list mode: the list name is exactly the prefix.
    let to = handle.key("list");
    let spec = write_spec(&["value"], &to, "list");

    let rows = vec![
        Row::upsert(vec![Value::Json(serde_json::json!(1))]),
        Row::upsert(vec![Value::Json(serde_json::json!(2))]),
        Row::upsert(vec![Value::Json(serde_json::json!(3))]),
    ];
    let report = build_and_write(&sink, &spec, rows).await;
    assert_eq!(report.upserts, 3);

    let mut conn = admin(&handle.url).await;
    let items: Vec<String> = redis::cmd("LRANGE")
        .arg(&to)
        .arg(0)
        .arg(-1)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        items,
        vec!["1".to_string(), "2".to_string(), "3".to_string()]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_xadd_appends_entry() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let to = handle.key("stream:");
    let spec = write_spec(&["key", "value"], &to, "stream");

    let row = Row::upsert(vec![
        Value::Text("s1".into()),
        obj(&[("event", Value::Text("login".into()))]),
    ]);
    build_and_write(&sink, &spec, vec![row]).await;

    let mut conn = admin(&handle.url).await;
    let full = format!("{to}s1");
    let len: i64 = redis::cmd("XLEN")
        .arg(&full)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(len, 1, "one stream entry");
    // XRANGE returns [[id, [field, value, ...]]]; the JSON lands under `data`.
    let entries: Vec<(String, Vec<String>)> = redis::cmd("XRANGE")
        .arg(&full)
        .arg("-")
        .arg("+")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    let fields = &entries[0].1;
    assert_eq!(fields[0], "data");
    assert_eq!(fields[1], r#"{"event":"login"}"#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pubsub_publishes_to_subscriber() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let to = handle.key("ps:");
    let spec = write_spec(&["value", "key"], &to, "pubsub");
    let channel = format!("{to}c1");

    // Subscribe BEFORE publishing so the message is delivered.
    let client = redis::Client::open(handle.url.as_str()).unwrap();
    let mut pubsub = client.get_async_pubsub().await.unwrap();
    pubsub.subscribe(&channel).await.unwrap();

    let row = Row::upsert(vec![
        Value::Json(serde_json::json!({"hello": "world"})),
        Value::Text("c1".into()),
    ]);
    let report = build_and_write(&sink, &spec, vec![row]).await;
    assert_eq!(report.upserts, 1);

    let mut messages = pubsub.on_message();
    let msg = tokio::time::timeout(Duration::from_secs(5), messages.next())
        .await
        .expect("message within 5s")
        .expect("a message");
    let payload: String = msg.get_payload().unwrap();
    assert_eq!(payload, r#"{"hello":"world"}"#);
    assert_eq!(msg.get_channel_name(), channel);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_batch_is_noop() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let to = handle.key("empty:");
    let spec = write_spec(&["key", "value"], &to, "kv");
    let ctx = sink.build_context(&spec).await.unwrap();
    let report = sink
        .write_batch(&spec, &ctx, Batch::default(), false)
        .await
        .unwrap();
    assert_eq!(report.upserts, 0);
    assert_eq!(report.deletes, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conflict_block_rejected_at_build_context() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let mut spec = write_spec(&["key", "value"], &handle.key("conf:"), "kv");
    spec.conflict = Some(ConflictConfig {
        key: vec!["key".to_string()],
        strategy: ConflictStrategy::Overwrite,
    });
    // `.err()` drops the Ok arm (Arc<dyn SinkCtx> isn't Debug, so
    // `expect_err` won't compile).
    let err = sink
        .build_context(&spec)
        .await
        .err()
        .expect("conflict must be rejected");
    match err {
        RuntimeError::Config(ConfigError::ConflictNotSupported { sink, .. }) => {
            assert_eq!(sink, "redis");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_context_rejects_unexpected_and_missing_columns() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let to = handle.key("contract:");
    // Unexpected column for kv.
    let extra = write_spec(&["key", "value", "channel"], &to, "kv");
    assert!(
        sink.build_context(&extra).await.is_err(),
        "extra column must reject"
    );
    // Missing required `value` for kv.
    let missing = write_spec(&["key"], &to, "kv");
    assert!(
        sink.build_context(&missing).await.is_err(),
        "missing value must reject"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_does_not_write() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let to = handle.key("dry:");
    let spec = write_spec(&["key", "value"], &to, "kv");
    let ctx = sink.build_context(&spec).await.unwrap();
    let row = Row::upsert(vec![
        Value::Text("k".into()),
        Value::Json(serde_json::json!({"v": 1})),
    ]);
    let report = sink
        .write_batch(
            &spec,
            &ctx,
            Batch {
                rows: vec![row],
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    // Default report and nothing on the wire.
    assert_eq!(report.upserts, 0);
    let mut conn = admin(&handle.url).await;
    let exists: i64 = redis::cmd("EXISTS")
        .arg(format!("{to}k"))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(exists, 0, "dry-run must not write");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_mode_skips_delete_rows() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let to = handle.key("skip:");
    let spec = write_spec(&["key", "value"], &to, "kv");
    let rows = vec![
        Row::upsert(vec![
            Value::Text("live".into()),
            Value::Json(serde_json::json!(1)),
        ]),
        // A Delete row in a write mode is dropped (counted skipped), not DEL'd.
        Row::delete(vec![
            Value::Text("ghost".into()),
            Value::Json(serde_json::json!(2)),
        ]),
    ];
    let report = build_and_write(&sink, &spec, rows).await;
    assert_eq!(report.upserts, 1);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.deletes, 0);

    let mut conn = admin(&handle.url).await;
    let live: i64 = redis::cmd("EXISTS")
        .arg(format!("{to}live"))
        .query_async(&mut conn)
        .await
        .unwrap();
    let ghost: i64 = redis::cmd("EXISTS")
        .arg(format!("{to}ghost"))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(live, 1, "upsert row written");
    assert_eq!(ghost, 0, "delete row neither written nor DEL'd");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_access_probes_write_and_self_cleans() {
    // The sentinel suffix the sink appends to the flow prefix. Mirrors
    // `commands::ACCESS_PROBE_SUFFIX` (private); kept in sync by contract.
    const PROBE_SUFFIX: &str = "__air_elt_access_probe__";

    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let mut conn = admin(&handle.url).await;

    // (mode, valid columns for that mode). validate_access runs
    // resolve_layout first, so columns must satisfy the contract.
    let cases: &[(&str, &[&str])] = &[
        ("kv", &["key", "value"]),
        ("kv-delete", &["key"]),
        ("list", &["value"]),
        ("stream", &["key", "value"]),
        ("pubsub", &["value"]),
    ];

    for (mode, columns) in cases {
        let to = handle.key(&format!("probe-{mode}:"));
        let spec = write_spec(columns, &to, mode);
        // A real write probe must succeed against a writable Valkey.
        sink.validate_access(&spec)
            .await
            .unwrap_or_else(|e| panic!("validate_access failed for {mode}: {e:?}"));

        // The sentinel must leave no lasting trace: cleanup modes DEL it
        // (or never create a key); kv writes it self-expiring (PX 100).
        let sentinel = format!("{to}{PROBE_SUFFIX}");
        let pttl: i64 = redis::cmd("PTTL")
            .arg(&sentinel)
            .query_async(&mut conn)
            .await
            .unwrap();
        if *mode == "kv" {
            // -2 = already expired/gone, or a short bounded TTL still
            // counting down. Never -1 (that would be a permanent leak).
            assert!(
                pttl == -2 || (1..=100).contains(&pttl),
                "kv probe must self-expire, got pttl={pttl}"
            );
        } else {
            // -2 = no such key. The probe cleaned up after itself.
            assert_eq!(pttl, -2, "{mode} probe must leave no key, got pttl={pttl}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kv_multi_row_distinct_keys() {
    let handle = valkey_handle().await.unwrap();
    let sink = sink_for(&handle.url).await;
    let to = handle.key("multi:");
    let spec = write_spec(&["key", "value"], &to, "kv");
    let rows = (0..3u32)
        .map(|i| {
            Row::upsert(vec![
                Value::Text(format!("k{i}")),
                Value::Json(serde_json::json!({ "i": i })),
            ])
        })
        .collect();
    let report = build_and_write(&sink, &spec, rows).await;
    assert_eq!(report.upserts, 3);

    let mut conn = admin(&handle.url).await;
    for i in 0..3u32 {
        let got: String = redis::cmd("GET")
            .arg(format!("{to}k{i}"))
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(got, format!(r#"{{"i":{i}}}"#));
    }
}
