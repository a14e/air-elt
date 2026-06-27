//! Cross-engine: PostgreSQL source → Redis/Valkey sink, PostgreSQL storage.
//!
//! Drives the full pipeline through a single `App::run_once` that fans out
//! to **all five redis modes** at once — one flow per mode (kv / kv-delete
//! / list / stream / pubsub), each reading its own PG table and writing to
//! the shared redis sink. Proves the per-flow `mode` plumbing, the
//! object-literal → JSON and duration-literal → `Interval` paths, and the
//! direct `Json → value` mapping all line up through a real run.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use air_elt_app::App;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_commons_testing::valkey::valkey_handle;
use futures::StreamExt;
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_redis_all_modes() {
    let pg = pg_pool().await;
    let valkey = valkey_handle().await.expect("valkey handle");
    let schema = format!("{}_redis_all", pg.schema);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .unwrap();
    // Each table's TEXT primary key is both the cursor field and the redis
    // key suffix (key must resolve to Text at write time).
    for ddl in [
        format!(
            "CREATE TABLE \"{schema}\".kv_users (uid TEXT PRIMARY KEY, name TEXT NOT NULL, age INT NOT NULL)"
        ),
        format!("CREATE TABLE \"{schema}\".del_keys (uid TEXT PRIMARY KEY)"),
        format!(
            "CREATE TABLE \"{schema}\".list_items (seq TEXT PRIMARY KEY, label TEXT NOT NULL, qty INT NOT NULL)"
        ),
        format!(
            "CREATE TABLE \"{schema}\".stream_events (eid TEXT PRIMARY KEY, payload JSONB NOT NULL)"
        ),
        format!(
            "CREATE TABLE \"{schema}\".ps_events (pid TEXT PRIMARY KEY, payload JSONB NOT NULL)"
        ),
    ] {
        pg.pool.execute(ddl.as_str()).await.unwrap();
    }
    for (uid, name, age) in [("u1", "ann", 30), ("u2", "bob", 41)] {
        sqlx::query(&format!(
            "INSERT INTO \"{schema}\".kv_users(uid, name, age) VALUES ($1,$2,$3)"
        ))
        .bind(uid)
        .bind(name)
        .bind(age)
        .execute(&pg.pool)
        .await
        .unwrap();
    }
    for uid in ["d1", "d2"] {
        sqlx::query(&format!(
            "INSERT INTO \"{schema}\".del_keys(uid) VALUES ($1)"
        ))
        .bind(uid)
        .execute(&pg.pool)
        .await
        .unwrap();
    }
    for (seq, label, qty) in [("s1", "a", 1), ("s2", "b", 2)] {
        sqlx::query(&format!(
            "INSERT INTO \"{schema}\".list_items(seq, label, qty) VALUES ($1,$2,$3)"
        ))
        .bind(seq)
        .bind(label)
        .bind(qty)
        .execute(&pg.pool)
        .await
        .unwrap();
    }
    sqlx::query(&format!(
        "INSERT INTO \"{schema}\".stream_events(eid, payload) VALUES ($1,$2)"
    ))
    .bind("e1")
    .bind(serde_json::json!({ "ev": "login" }))
    .execute(&pg.pool)
    .await
    .unwrap();
    for (pid, m) in [("p1", 1), ("p2", 2)] {
        sqlx::query(&format!(
            "INSERT INTO \"{schema}\".ps_events(pid, payload) VALUES ($1,$2)"
        ))
        .bind(pid)
        .bind(serde_json::json!({ "m": m }))
        .execute(&pg.pool)
        .await
        .unwrap();
    }

    let pg_url = pg.url_with_search_path();
    let redis_url = valkey.url.clone();
    let kv_to = valkey.key("kv:");
    let del_to = valkey.key("del:");
    let list_to = valkey.key("list:");
    let stream_to = valkey.key("stream:");
    let ps_to = valkey.key("ps:");

    let client = redis::Client::open(redis_url.as_str()).unwrap();
    let mut admin = client.get_multiplexed_async_connection().await.unwrap();
    // Pre-seed the keys the kv-delete flow must remove.
    for uid in ["d1", "d2"] {
        let _: () = redis::cmd("SET")
            .arg(format!("{del_to}{uid}"))
            .arg("seeded")
            .query_async(&mut admin)
            .await
            .unwrap();
    }

    // Subscribe to the pubsub channels BEFORE the run so the published
    // messages are delivered to a live subscriber. The collector runs in a
    // task and stops once it has both messages (or times out).
    let mut pubsub = client.get_async_pubsub().await.unwrap();
    // Subscribe to `p*` (the real channels are `…p1` / `…p2`), not `*`: the
    // sink's pubsub access-probe PUBLISHes one throwaway message to the
    // `…__air_elt_access_probe__` sentinel channel at validate time, which a
    // catch-all `*` pattern would also capture.
    pubsub.psubscribe(format!("{ps_to}p*")).await.unwrap();
    let collector = tokio::spawn(async move {
        let mut got: Vec<(String, String)> = Vec::new();
        let mut stream = pubsub.into_on_message();
        while got.len() < 2 {
            match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
                Ok(Some(msg)) => got.push((
                    msg.get_channel_name().to_string(),
                    msg.get_payload::<String>().unwrap(),
                )),
                _ => break,
            }
        }
        got
    });

    let config_yaml = format!(
        r#"
sources:
  - name: src
    type: postgres
    config: {{ url: "{pg_url}" }}

sinks:
  - name: snk
    type: redis
    config: {{ url: "{redis_url}" }}

storages:
  - name: st
    type: postgres
    config: {{ url: "{pg_url}" }}

flow:
  kv:
    source: src
    sink: {{ name: snk, mode: kv }}
    storage: st
    from: "{schema}.kv_users"
    to: "{kv_to}"
    batch-limit: 100
    mapping: {{ key: uid }}
    compute-mapping:
      value: '{{ "name" = `name`, "age" = `age` }}'
      ttl: "1h"
    cursor: {{ fields: [uid], order: asc, interval: "100ms" }}

  del:
    source: src
    sink: {{ name: snk, mode: kv-delete }}
    storage: st
    from: "{schema}.del_keys"
    to: "{del_to}"
    batch-limit: 100
    mapping: {{ key: uid }}
    cursor: {{ fields: [uid], order: asc, interval: "100ms" }}

  list:
    source: src
    sink: {{ name: snk, mode: list }}
    storage: st
    from: "{schema}.list_items"
    to: "{list_to}"
    batch-limit: 100
    mapping: {{ key: seq }}
    compute-mapping:
      value: '{{ "label" = `label`, "qty" = `qty` }}'
    cursor: {{ fields: [seq], order: asc, interval: "100ms" }}

  stream:
    source: src
    sink: {{ name: snk, mode: stream }}
    storage: st
    from: "{schema}.stream_events"
    to: "{stream_to}"
    batch-limit: 100
    mapping: {{ key: eid, value: payload }}
    cursor: {{ fields: [eid], order: asc, interval: "100ms" }}

  pubsub:
    source: src
    sink: {{ name: snk, mode: pubsub }}
    storage: st
    from: "{schema}.ps_events"
    to: "{ps_to}"
    batch-limit: 100
    mapping: {{ key: pid, value: payload }}
    cursor: {{ fields: [pid], order: asc, interval: "100ms" }}
"#
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // kv — object-literal value + 1h ttl.
    for (uid, name, age) in [("u1", "ann", 30), ("u2", "bob", 41)] {
        let got: String = redis::cmd("GET")
            .arg(format!("{kv_to}{uid}"))
            .query_async(&mut admin)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&got).unwrap(),
            serde_json::json!({ "name": name, "age": age })
        );
        let pttl: i64 = redis::cmd("PTTL")
            .arg(format!("{kv_to}{uid}"))
            .query_async(&mut admin)
            .await
            .unwrap();
        assert!(
            pttl > 0 && pttl <= 3_600_000,
            "kv ttl set for {uid}, got {pttl}"
        );
    }

    // kv-delete — the seeded keys are gone.
    for uid in ["d1", "d2"] {
        let exists: i64 = redis::cmd("EXISTS")
            .arg(format!("{del_to}{uid}"))
            .query_async(&mut admin)
            .await
            .unwrap();
        assert_eq!(exists, 0, "kv-delete removed {uid}");
    }

    // list — RPUSH of an object built in-script from typed columns via the
    // object-literal + backtick form (`{ "k" = `col` }`), exactly the shape
    // the task spec shows. One keyed list per row.
    for (seq, label, qty) in [("s1", "a", 1), ("s2", "b", 2)] {
        let items: Vec<String> = redis::cmd("LRANGE")
            .arg(format!("{list_to}{seq}"))
            .arg(0)
            .arg(-1)
            .query_async(&mut admin)
            .await
            .unwrap();
        assert_eq!(items, vec![format!(r#"{{"label":"{label}","qty":{qty}}}"#)]);
    }

    // stream — XADD landed one entry with the JSON under field `data`.
    let len: i64 = redis::cmd("XLEN")
        .arg(format!("{stream_to}e1"))
        .query_async(&mut admin)
        .await
        .unwrap();
    assert_eq!(len, 1);
    let entries: Vec<(String, Vec<String>)> = redis::cmd("XRANGE")
        .arg(format!("{stream_to}e1"))
        .arg("-")
        .arg("+")
        .query_async(&mut admin)
        .await
        .unwrap();
    assert_eq!(entries[0].1[0], "data");
    assert_eq!(entries[0].1[1], r#"{"ev":"login"}"#);

    // pubsub — both messages reached the live subscriber.
    let mut messages = collector.await.unwrap();
    messages.sort();
    assert_eq!(
        messages,
        vec![
            (format!("{ps_to}p1"), r#"{"m":1}"#.to_string()),
            (format!("{ps_to}p2"), r#"{"m":2}"#.to_string()),
        ]
    );

    // Every flow persisted its cursor.
    let cursors: Vec<(String,)> = sqlx::query_as("SELECT flow FROM air_elt_cursors ORDER BY flow")
        .fetch_all(&pg.pool)
        .await
        .unwrap();
    let flows: Vec<String> = cursors.into_iter().map(|c| c.0).collect();
    assert_eq!(flows, vec!["del", "kv", "list", "pubsub", "stream"]);

    pg.pool.close().await;
}
