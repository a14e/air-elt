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

/// Negative case: two `*:body` JSON-pack rules pointing at the same
/// sink column. `expand` runs `check_sink_uniqueness` after pack
/// synthesis and emits `DuplicateSinkField`. Schema introspection runs
/// (`validation.fields = true`) so source/sink schemas are populated;
/// the failure surfaces purely from the mapping shape.
///
/// AIR-70: under the new keyed-table mapping form, two body-pack rules
/// targeting the same sink column collapse to a single map key, so the
/// failure now manifests as a parse-time `duplicate mapping key` error
/// rather than a post-expansion `DuplicateSinkField`. The fixture is
/// kept here for traceability but the test is ignored — the surface is
/// covered by parser-level duplicate-key detection in
/// `crates/core/src/config/model.rs::MappingMapVisitor::visit_map`.
#[ignore = "AIR-70: duplicate body-pack target is now a parse-time duplicate-key error, not a post-expansion DuplicateSinkField"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_rejects_duplicate_json_pack_target() {
    let pg = pg_pool().await;

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
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".sink_t (id BIGINT PRIMARY KEY, body JSONB NOT NULL)"
            )
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
    to: "{src_schema}.sink_t"
    batch-limit: 16

    mapping:
      body: "*"
      body: "*"

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
        .expect_err("validate() must reject duplicate JSON-pack targets");

    let mut found = false;
    let mut cur: &dyn std::error::Error = err.as_ref();
    loop {
        if let Some(ve) = cur.downcast_ref::<ValidationError>()
            && matches!(ve, ValidationError::DuplicateSinkField { field, .. } if field == "body")
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
        "expected ValidationError::DuplicateSinkField {{ field: \"body\", .. }} in chain; got: {err:#}"
    );

    pg.pool.close().await;
}
