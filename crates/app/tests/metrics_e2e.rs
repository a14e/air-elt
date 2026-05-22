//! App-level e2e for the `/metrics` endpoint. Runs a one-flow pg→pg
//! pipeline through `App::run_once`, then asserts the live `/metrics`
//! scrape contains non-zero counters that match the work done.
//!
//! Closes the loop the validators flagged: monitoring's intra-crate
//! tests exercise the manager directly, but only this test proves the
//! recorder wiring survives end-to-end (assemble → validate → run →
//! scrape).

#![allow(clippy::unwrap_used)]

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use air_elt_app::App;
use air_elt_commons_testing::pg::pg_pool;
use sqlx::Executor;
use tokio::net::TcpListener;
use tokio::sync::watch;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_endpoint_reflects_flow_activity() {
    let pg = pg_pool().await;

    let src_schema = format!("{}_src", pg.schema);
    let dst_schema = format!("{}_dst", pg.schema);
    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(format!("CREATE SCHEMA \"{dst_schema}\"").as_str())
        .await
        .unwrap();

    pg.pool
        .execute(
            format!("CREATE TABLE \"{src_schema}\".t (id BIGINT NOT NULL, name TEXT NOT NULL)")
                .as_str(),
        )
        .await
        .unwrap();
    pg.pool
        .execute(
            format!("CREATE TABLE \"{dst_schema}\".t (id BIGINT NOT NULL, name TEXT NOT NULL)")
                .as_str(),
        )
        .await
        .unwrap();
    for i in 1..=5 {
        pg.pool
            .execute(format!("INSERT INTO \"{src_schema}\".t VALUES ({i}, 'row-{i}')").as_str())
            .await
            .unwrap();
    }

    let pg_url = pg.url_with_search_path();

    let yaml = format!(
        r#"
sources:
  - name: src
    type: postgres
    config:
      url: "{pg_url}"

sinks:
  - name: snk
    type: postgres
    config:
      url: "{pg_url}"

storages:
  - name: st
    type: postgres
    config:
      url: "{pg_url}"

metrics:
  prometheus:
    enabled: true
    port: 18090

flow:
  f1:
    source: src
    sink: snk
    storage: st
    from: "{src_schema}.t"
    to: "{dst_schema}.t"
    mapping:
      id: id
      name: name
    cursor:
      fields: [id]
      order: asc
      interval: "100ms"
    batch-limit: 10
"#
    );

    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.yml");
    std::fs::write(&cfg_path, yaml).unwrap();
    let app = App::from_path(&cfg_path).expect("app loaded");

    // Run a single drain through the production pipeline so the
    // recorder sees real work. `run_once` triggers assemble → migrate
    // → validate → engine, mirroring the daemon path minus the
    // shutdown loop.
    app.run_once().await.expect("run_once");

    // Bind an ephemeral loopback port for the scrape — port-from-config
    // is irrelevant here because we go straight through
    // `serve_on_listener`, sidestepping `App::spawn_metrics`.
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let local_addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let scraper = app.take_scraper();
    let server_task = tokio::spawn(async move {
        air_elt_monitoring::server::serve_on_listener(scraper, listener, shutdown_rx)
            .await
            .expect("server task");
    });

    let client = reqwest::Client::new();
    let url = format!("http://{local_addr}/metrics");
    let response = client.get(&url).send().await.expect("GET /metrics");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().await.expect("body");

    // The 5 source rows must surface in
    // `air_elt_rows_total{stage=read, op=upsert}` AND
    // `air_elt_rows_total{stage=written, op=upsert}` — assemble→engine
    // wiring is what's being tested here; intra-crate tests already
    // cover the per-recorder math.
    let rows_read_upsert = sum_metric(
        &body,
        "air_elt_rows_total",
        &[("stage", "read"), ("op", "upsert")],
    );
    let rows_written_upsert = sum_metric(
        &body,
        "air_elt_rows_total",
        &[("stage", "written"), ("op", "upsert")],
    );
    assert!(
        rows_read_upsert >= 5,
        "expected rows_read_total{{op=upsert}} >= 5, got {rows_read_upsert}\n{body}"
    );
    assert!(
        rows_written_upsert >= 5,
        "expected rows_written_total{{op=upsert}} >= 5, got {rows_written_upsert}\n{body}"
    );

    // Locks must show non-zero integrals: every read/write/save_cursor
    // call holds a permit briefly, integrated time > 0. The source
    // (`src`) is exercised by every `read_batch` call, so its lock
    // integral must be > 0 — a stronger bound than the name-only
    // `contains(...)` check, which trivially passes on zero values.
    let lock_active_source = sum_metric_f64(
        &body,
        "air_elt_lock_active_seconds_integral",
        &[("kind", "source"), ("component", "src")],
    );
    assert!(
        lock_active_source > 0.0,
        "lock_active_seconds_integral{{kind=source, component=src}} must be > 0 after run_once; got {lock_active_source}\n{body}"
    );
    assert!(
        body.contains("air_elt_lock_max{"),
        "lock_max family missing from body"
    );

    // Driver pool stats must show up for every component. Each factory
    // calls `monitoring.register_pool_stats(...)` with the bounds and a
    // backend-specific `PoolStatsReader`; the collector pulls counts
    // from sqlx on every scrape. By the time `run_once` returns and we hit `/metrics`,
    // the pool is back to fully idle (`active = 0`), but sqlx keeps
    // at least one idle conn around after use, so the always-non-zero
    // `idle` plain gauge is a safe assertion alongside the
    // configuration gauges max/min, which prove the stats-reader
    // wiring reached the collector.
    let pool_idle_source = sum_metric_f64(
        &body,
        "air_elt_pool_connections_idle",
        &[("kind", "source"), ("component", "src")],
    );
    assert!(
        pool_idle_source > 0.0,
        "pool_connections_idle{{kind=source, component=src}} must be > 0 after run_once (sqlx keeps idle conns); got {pool_idle_source}\n{body}"
    );
    assert!(
        body.contains("air_elt_pool_connections_max{"),
        "pool_connections_max family missing from body"
    );
    // Pin max to the default `PoolSettings::resolve_bounds` value (5).
    // This catches a regression where the factory routes the wrong
    // bounds to `monitoring.register_pool_stats(..., max, min, ...)` —
    // the family name alone passes for any non-zero value.
    let pool_max = sum_metric(&body, "air_elt_pool_connections_max", &[("kind", "source")]);
    assert_eq!(
        pool_max, 5,
        "pool_connections_max{{kind=source}} must equal default max_connections (5); got {pool_max}\n{body}"
    );

    // Process metrics must always emit.
    assert!(body.contains("process_cpu_seconds_total"));
    assert!(body.contains("memory_used_bytes_seconds_integral"));

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), server_task).await;
    pg.pool.close().await;
}

fn sum_metric(body: &str, name: &str, label_filter: &[(&str, &str)]) -> u64 {
    sum_metric_f64(body, name, label_filter) as u64
}

fn sum_metric_f64(body: &str, name: &str, label_filter: &[(&str, &str)]) -> f64 {
    let mut total = 0.0_f64;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (head, value) = match line.rsplit_once(' ') {
            Some((h, v)) => (h, v),
            None => continue,
        };
        let metric_name = match head.find('{') {
            Some(idx) => &head[..idx],
            None => head,
        };
        if metric_name != name {
            continue;
        }
        if !label_filter
            .iter()
            .all(|(k, v)| head.contains(&format!("{k}=\"{v}\"")))
        {
            continue;
        }
        if let Ok(parsed) = value.parse::<f64>() {
            total += parsed;
        }
    }
    total
}
