//! Validation-pipeline concurrency cap: with `max-connections = 2` on
//! a single shared pg source/sink/storage and several flows referencing
//! it, the per-component `tokio::sync::Semaphore` must serialise the
//! access probes / schema introspection / sampling so the total
//! concurrent I/O never exceeds the cap — and the run must complete
//! cleanly without deadlocking or surfacing a backend error.
//!
//! The test is intentionally driven through `App::validate()` — the
//! same path the `validate` CLI subcommand runs — so a regression in
//! either `assemble` (semaphore construction) or `validate_flow`
//! (canonical-order acquire) would surface as a real-world failure for
//! an operator.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::pg::pg_pool;
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn validate_under_max_connections_cap_completes_cleanly() {
    let pg = pg_pool().await;

    // One src + one sink schema, two trivial tables per flow. The
    // exact schema doesn't matter — the test exercises probe + schema
    // introspection concurrency, not type matrix logic.
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

    // Six flow pairs — well above the `max-connections = 2` cap. If
    // the semaphore were missing or acquired in non-canonical order,
    // either a backend connection-exhaustion error or a deadlock would
    // surface here. With canonical acquire + retry-transient guarding
    // residual flakes, the run completes deterministically.
    let flow_count = 6;
    for i in 0..flow_count {
        pg.pool
            .execute(
                format!(
                    "CREATE TABLE \"{src_schema}\".t{i} (id BIGINT NOT NULL, name TEXT NOT NULL)"
                )
                .as_str(),
            )
            .await
            .unwrap();
        pg.pool
            .execute(
                format!(
                    "CREATE TABLE \"{dst_schema}\".t{i} (id BIGINT NOT NULL, name TEXT NOT NULL)"
                )
                .as_str(),
            )
            .await
            .unwrap();
    }

    let pg_url = pg.url_with_search_path();

    // `max-connections = 2` on every component, six flows referencing
    // each one — the semaphore is the only thing keeping the validation
    // I/O within budget. If `assemble` skipped semaphore construction
    // or `validate_flow` didn't acquire permits before probing, this
    // would race the pg pool's own connection cap.
    let mut yaml = format!(
        r#"
sources:
  - name: src
    type: postgres
    config:
      url: "{pg_url}"
      max-connections: 2

sinks:
  - name: snk
    type: postgres
    config:
      url: "{pg_url}"
      max-connections: 2

storages:
  - name: st
    type: postgres
    config:
      url: "{pg_url}"
      max-connections: 2

flow:
"#
    );
    for i in 0..flow_count {
        yaml.push_str(&format!(
            r#"  f{i}:
    source: src
    sink: snk
    storage: st
    from: "{src_schema}.t{i}"
    to: "{dst_schema}.t{i}"
    batch-limit: 16
    mapping:
      id: id
      name: name
    cursor:
      fields: [id]
      order: asc
      interval: "1s"
"#
        ));
    }

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &yaml).unwrap();

    // Storage migrations must run before validation can probe the
    // cursor table — the validate CLI assumes the operator has already
    // run `air-elt migrate` once. App exposes the same hook.
    let app = App::from_path(&config_path).expect("App::from_path");
    app.migrate().await.expect("migrate succeeds");
    let app = App::from_path(&config_path).expect("App::from_path");

    // The contract: validation completes within a generous overall
    // budget and returns Ok. A semaphore regression would either
    // deadlock (test times out) or surface a connection-exhaustion
    // error from the pg pool.
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(60), app.validate()).await;
    let result = outcome.expect("validate must not deadlock under the connection cap");
    result.expect("validate must succeed against a real pg pool with max-connections=2");

    pg.pool.close().await;
}
