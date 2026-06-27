//! CLI-surface negative validation tests.
//!
//! Drives `App::validate()` (the same code path the `validate` CLI
//! subcommand calls) against configs known to trip a specific
//! `ValidationError`. The contract under test is that the failure
//! actually reaches the operator — i.e. the result is `Err`, the error's
//! `Display` text matches the `thiserror` `#[error(...)]` rendering, and
//! the error type is the expected variant.
//!
//! Today's coverage:
//!   * `WildcardWithoutSchema` — flow with `mapping = ["*"]` against a
//!     pg→pg pair where neither side is schemaless and
//!     `validation.fields = false` keeps the pipeline from introspecting
//!     either side. `validation::pipeline::validate_flow` runs `expand`
//!     before the access probes, so this combination reaches `expand`
//!     with `(src_schema=None, dst_schema=None, src_schemaless=false,
//!     dst_schemaless=false)` — the exact trigger condition in
//!     `mapping/expand.rs`.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::error::ValidationError;
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_wildcard_without_schema() {
    let pg = pg_pool().await;

    // Both source and sink point at real pg URLs so `assemble` (which
    // connects per declared component) succeeds. Validation then trips
    // on `expand` before any I/O probe runs — `validation.fields =
    // false` is what suppresses schema introspection and forces the
    // wildcard-without-schema path.
    let src_schema = format!("{}_src", pg.schema);
    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!("CREATE TABLE \"{src_schema}\".users (id BIGINT PRIMARY KEY, name TEXT)")
                .as_str(),
        )
        .await
        .unwrap();

    let pg_url = pg.url_with_search_path();

    let config_yaml = format!(
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

flow:
  bad:
    source: src
    sink: snk
    storage: st
    from: "{src_schema}.users"
    to: "{src_schema}.users"
    batch-limit: 16

    mapping:
      "*": "*"

    cursor:
      fields: [id]
      order: asc
      interval: "1s"

    # Disable schema introspection so the pipeline reaches `expand`
    # with both schemas = None. Neither pg source nor pg sink is
    # schemaless, so the wildcard-without-schema branch fires.
    validation:
      fields: false
      access: false
      inserts: false
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();

    let app = App::from_path(&config_path).expect("App::from_path");
    let err = app
        .validate()
        .await
        .expect_err("validate() must reject wildcard-without-schema");

    // `App::validate` wraps `ValidationError` into `anyhow::Error`. The
    // CLI prints the error chain via `Debug`, so we assert against the
    // displayed message — which is what an operator running
    // `air-elt validate` actually sees on stderr (exit code != 0 maps
    // 1:1 to `Result::Err` from `App::validate` in `main.rs`).
    let displayed = format!("{err:#}");
    assert!(
        displayed.contains("wildcard mapping ('*' / '*:*') requires a schema"),
        "expected WildcardWithoutSchema text, got: {displayed}"
    );
    assert!(
        displayed.contains("\"bad\""),
        "expected flow name in error, got: {displayed}"
    );

    // Type-level check: walk the source chain to find the underlying
    // `ValidationError::WildcardWithoutSchema` variant. This guards
    // against the message changing accidentally without anyone noticing.
    let mut found = false;
    let mut cur: &dyn std::error::Error = err.as_ref();
    loop {
        if let Some(ve) = cur.downcast_ref::<ValidationError>()
            && matches!(ve, ValidationError::WildcardWithoutSchema { flow } if flow == "bad")
        {
            found = true;
            break;
        }
        match cur.source() {
            Some(s) => cur = s,
            None => break,
        }
    }
    assert!(
        found,
        "expected a ValidationError::WildcardWithoutSchema in the error chain"
    );

    pg.pool.close().await;
}

/// Negative case (AIR-5): the developed per-flow sink form
/// `sink = { name, mode }` is only meaningful for sinks that consume
/// per-flow options (today: redis). On any other sink kind `assemble`
/// must reject it loudly rather than silently dropping the option.
///
/// We point a flow's `sink` at a postgres sink carrying an extra `mode`
/// key and assert `validate()` fails inside `assemble` — before any
/// schema probe — with the kind-aware error. `assemble` connects each
/// component pool (hence the real pg URL), but the rejection fires in
/// the per-flow loop ahead of mapping expansion, so the `from`/`to`
/// tables need not exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_sink_options_on_non_redis_sink() {
    let pg = pg_pool().await;
    let pg_url = pg.url_with_search_path();

    let config_yaml = format!(
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

flow:
  bad:
    source: src
    sink: {{ name: snk, mode: kv }}
    storage: st
    from: "public.users"
    to: "public.users"
    batch-limit: 16

    mapping:
      id: id

    cursor:
      fields: [id]
      order: asc
      interval: "1s"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();

    let app = App::from_path(&config_path).expect("App::from_path");
    let err = app
        .validate()
        .await
        .expect_err("validate() must reject per-flow sink options on a non-redis sink");

    let displayed = format!("{err:#}");
    assert!(
        displayed.contains("does not accept per-flow sink options"),
        "expected the sink-options rejection text, got: {displayed}"
    );
    assert!(
        displayed.contains("\"bad\""),
        "expected the flow name in error, got: {displayed}"
    );
    assert!(
        displayed.contains("redis"),
        "expected the error to name the only supporting sink kind, got: {displayed}"
    );
    // Kind-aware: the message names the offending sink kind, not just the
    // permitted one — so an operator sees *which* sink rejected the option.
    assert!(
        displayed.contains("postgres"),
        "expected the error to name the offending sink kind, got: {displayed}"
    );

    pg.pool.close().await;
}

/// Negative case (AIR-70): two mapping rules pointing at the same sink
/// column. Before AIR-70 this surfaced post-expansion as
/// `ValidationError::DuplicateSinkField` from `check_sink_uniqueness`;
/// under the inverted keyed-table mapping form the duplicate is
/// caught earlier — by `MappingMapVisitor::visit_map` in
/// `crates/core/src/config/model.rs:194-198`, which dedupes via an
/// AHashSet of keys it has already seen and emits a `serde::Error::custom`
/// with the message `"duplicate mapping key {key:?} — sink column
/// names must be unique"`.
///
/// `App::from_path` now fails at parse time; the operator never even
/// reaches `validate()`. We assert on the displayed error containing
/// our visitor's signature string so the contract is pinned regardless
/// of which `ConfigError` variant transports it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_path_rejects_duplicate_mapping_key() {
    // No DB is needed: the failure is at parse time, before any access
    // probe runs. `App::from_path` reads the file, deserialises through
    // our visitor, and returns the error synchronously.
    let config_yaml = r#"
sources:
  - name: src
    type: postgres
    config:
      url: "postgres://nobody@localhost/nope"

sinks:
  - name: snk
    type: postgres
    config:
      url: "postgres://nobody@localhost/nope"

storages:
  - name: st
    type: postgres
    config:
      url: "postgres://nobody@localhost/nope"

flow:
  bad:
    source: src
    sink: snk
    storage: st
    from: "public.users"
    to: "public.sink_t"
    batch-limit: 16

    mapping:
      body: "*"
      body: "*"

    cursor:
      fields: [id]
      order: asc
      interval: "1s"
"#;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let err = App::from_path(&config_path)
        .err()
        .expect("App::from_path must reject duplicate mapping key at parse time");
    let displayed = format!("{err:#}");
    assert!(
        displayed.contains("duplicate mapping key"),
        "expected the visitor's duplicate-key signature, got: {displayed}"
    );
    assert!(
        displayed.contains("\"body\""),
        "expected the offending key name in the error, got: {displayed}"
    );
}

/// Negative case (AIR-5): a computed `Interval` (a duration-literal, the
/// `ttl` shape) routed into a non-redis sink column it cannot occupy must
/// be rejected at validate time, not silently coerced at runtime. The
/// redis sink has an `Interval`-typed `ttl` column (identity-compatible),
/// so this is the producer that finally makes the Stage-1-deferred check
/// reachable: a postgres `BIGINT` column has no `Interval` conversion, so
/// the const-folded compute fails `ensure_sink_compatible` during plan
/// build and surfaces as `ValidationError::ComputeCompile`.
///
/// Targeting a `BIGINT` (not `TEXT`) column is deliberate: every
/// `* → Text` conversion routes through the permissive stringify arm, so
/// `Interval → Text` is intentionally accepted; only a target with no
/// conversion arm (here `Int64`) rejects.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_interval_compute_into_non_redis_sink() {
    let pg = pg_pool().await;

    // Real tables so `assemble` connects and the validate stage can
    // introspect both schemas (compute compile needs the sink column type
    // to type-check the produced `Interval`).
    let schema = format!("{}_iv", pg.schema);
    pg.pool
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(format!("CREATE TABLE \"{schema}\".src (id BIGINT PRIMARY KEY)").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!("CREATE TABLE \"{schema}\".dst (id BIGINT PRIMARY KEY, n BIGINT)").as_str(),
        )
        .await
        .unwrap();

    let pg_url = pg.url_with_search_path();

    let config_yaml = format!(
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

flow:
  bad:
    source: src
    sink: snk
    storage: st
    from: "{schema}.src"
    to: "{schema}.dst"
    batch-limit: 16

    mapping:
      id: id

    compute-mapping:
      n: "1h"

    cursor:
      fields: [id]
      order: asc
      interval: "1s"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();

    let app = App::from_path(&config_path).expect("App::from_path");
    let err = app
        .validate()
        .await
        .expect_err("validate() must reject an Interval compute into a BIGINT sink column");

    let displayed = format!("{err:#}");
    assert!(
        displayed.contains("\"bad\""),
        "expected the flow name in error, got: {displayed}"
    );
    assert!(
        displayed.contains("\"n\""),
        "expected the offending compute column in error, got: {displayed}"
    );
    assert!(
        displayed.to_lowercase().contains("interval"),
        "expected the Interval-type reject reason, got: {displayed}"
    );

    // Type-level guard against message drift: a `ComputeCompile` for the
    // `n` column of flow `bad` must be in the error chain.
    let mut found = false;
    let mut cur: &dyn std::error::Error = err.as_ref();
    loop {
        if let Some(ve) = cur.downcast_ref::<ValidationError>()
            && matches!(
                ve,
                ValidationError::ComputeCompile { flow, column, .. }
                    if flow == "bad" && column == "n"
            )
        {
            found = true;
            break;
        }
        match cur.source() {
            Some(s) => cur = s,
            None => break,
        }
    }
    assert!(
        found,
        "expected a ValidationError::ComputeCompile in the error chain"
    );

    pg.pool.close().await;
}
