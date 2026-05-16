//! Reverse path: CockroachDB source → PostgreSQL sink + storage.
//!
//! Validates that the Cockroach-flagged source connector can read through
//! the Postgres wire protocol against a real CockroachDB cluster:
//!   * `validate_access` runs the `has_table_privilege` probe,
//!   * `read_batch` flows through `with_serialization_retry` (a no-op
//!     under no contention but compiles in the Cockroach branch),
//!   * cursor algebra (single-column ASC) hands rows over to the PG
//!     sink unchanged.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::cockroach::cockroach_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::types::Value;
use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use sqlx::Executor;
use std::str::FromStr;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cockroachdb_to_pg_smoke() {
    let cockroach = cockroach_pool().await;
    let pg = pg_pool().await;

    let dst_schema = format!("{}_dst", pg.schema);

    // Source exercises the canonical type set the Cockroach source
    // (PG connector behind the `cockroachdb` factory) produces, modulo
    // Cockroach-specific gaps: there is no XML type (rejected upfront
    // by `validate_access`), and Cockroach has no native UInt* (same
    // as PG). Otherwise: Bool, Int2/Int4/Int8, Float4/Float8, fixed
    // and unbounded STRING (Text), fixed and unbounded BYTES, Date,
    // Timestamptz, Uuid, Jsonb, NUMERIC(p, 0) (BigInt) and
    // NUMERIC(p, s) (Decimal).
    cockroach
        .pool
        .execute(
            "CREATE TABLE events (
                id            INT8 PRIMARY KEY,
                payload       STRING NOT NULL,
                is_live       BOOL NOT NULL,
                small_signed  INT2 NOT NULL,
                medium_signed INT4 NOT NULL,
                real_f        FLOAT4 NOT NULL,
                double_f      FLOAT8 NOT NULL,
                big_decimal   NUMERIC(20, 0) NOT NULL,
                fixed_decimal NUMERIC(12, 4) NOT NULL,
                code          VARCHAR(16) NOT NULL,
                long_bio      STRING NOT NULL,
                fixed_blob    BYTES NOT NULL,
                blob_unbounded BYTES NOT NULL,
                born_on       DATE NOT NULL,
                seen_at       TIMESTAMPTZ NOT NULL,
                public_id     UUID NOT NULL,
                payload_json  JSONB NOT NULL,
                big_count     INT8 NOT NULL,
                nickname      STRING
            )",
        )
        .await
        .unwrap();

    let base = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();
    let long_bio = "biography-text-that-definitely-exceeds-eight-bytes";
    let uuids: Vec<Uuid> = (0..4u128)
        .map(|i| Uuid::from_u128(0x3000_0000_0000_4000_8000_0000_0000_0000_u128 + i))
        .collect();
    for i in 1..=4_i64 {
        let row = i;
        let big_decimal = BigDecimal::from(10_000_000_000_000_000_i64 + row);
        let fixed_decimal = BigDecimal::from_str(&format!("{}.{:04}", row * 10, row)).unwrap();
        let born_on = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(row);
        let seen_at = base + chrono::Duration::seconds(row);
        let payload_json = serde_json::json!({ "row": row, "label": format!("label-{row}") });
        // Row 2 (i==2) leaves `nickname` NULL to exercise the nullable
        // source -> NOT NULL sink `default` bridge.
        let nickname: Option<String> = (i != 2).then(|| format!("nick-{row}"));
        sqlx::query(
            "INSERT INTO events (id, payload, is_live, small_signed, medium_signed, \
                                 real_f, double_f, big_decimal, fixed_decimal, \
                                 code, long_bio, fixed_blob, blob_unbounded, \
                                 born_on, seen_at, public_id, payload_json, \
                                 big_count, nickname) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, \
                     $14, $15, $16, $17, $18, $19)",
        )
        .bind(row)
        .bind(format!("payload-{row}"))
        .bind(row % 2 == 0)
        .bind(row as i16 * 100)
        .bind(row as i32 * 1_000)
        .bind(row as f32 * 1.5_f32)
        .bind(row as f64 * 1.25_f64)
        .bind(&big_decimal)
        .bind(&fixed_decimal)
        .bind(format!("code-{row}"))
        .bind(long_bio)
        .bind(vec![row as u8; 4])
        .bind(format!("blob-{row}").into_bytes())
        .bind(born_on)
        .bind(seen_at)
        .bind(uuids[(i - 1) as usize])
        .bind(&payload_json)
        .bind(row * 1_000)
        .bind(nickname)
        .execute(&cockroach.pool)
        .await
        .unwrap();
    }

    pg.pool
        .execute(format!("CREATE SCHEMA \"{dst_schema}\"").as_str())
        .await
        .unwrap();
    // Sink mirrors source widths for identity columns and adds:
    //   * `nickname_safe TEXT NOT NULL`  — nullable_src -> not_null_sink
    //     via `default` on row 2.
    //   * `big_count_narrow INT4 NOT NULL`  — Int64 -> Int32 truncate.
    //   * `long_bio_clipped VARCHAR(8) NOT NULL`  — Text(unbounded) ->
    //     Text(8) truncate.
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema}\".events (
                    id            BIGINT PRIMARY KEY,
                    payload       TEXT NOT NULL,
                    is_live       BOOLEAN NOT NULL,
                    small_signed  SMALLINT NOT NULL,
                    medium_signed INTEGER NOT NULL,
                    real_f        REAL NOT NULL,
                    double_f      DOUBLE PRECISION NOT NULL,
                    big_decimal   NUMERIC(20, 0) NOT NULL,
                    fixed_decimal NUMERIC(12, 4) NOT NULL,
                    code          VARCHAR(16) NOT NULL,
                    long_bio      TEXT NOT NULL,
                    fixed_blob    BYTEA NOT NULL,
                    blob_unbounded BYTEA NOT NULL,
                    born_on       DATE NOT NULL,
                    seen_at       TIMESTAMPTZ NOT NULL,
                    public_id     UUID NOT NULL,
                    payload_json  JSONB NOT NULL,
                    nickname_safe TEXT NOT NULL,
                    big_count_narrow  INTEGER NOT NULL,
                    long_bio_clipped  VARCHAR(8) NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let cockroach_url = cockroach.url_with_database();
    let pg_url = pg.url_with_search_path();

    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "cockroachdb"
config = {{ url = "{cockroach_url}" }}

[[sinks]]
name = "snk"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[storages]]
name = "st"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.events]
source = "src"
sink = "snk"
storage = "st"
from = "public.events"
to = "{dst_schema}.events"
batch-limit = 2

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}

[flow.events.mapping]
id = "id"
payload = "payload"
is_live = "is_live"
small_signed = "small_signed"
medium_signed = "medium_signed"
real_f = "real_f"
double_f = "double_f"
big_decimal = "big_decimal"
fixed_decimal = "fixed_decimal"
code = "code"
long_bio = "long_bio"
fixed_blob = "fixed_blob"
blob_unbounded = "blob_unbounded"
born_on = "born_on"
seen_at = "seen_at"
public_id = "public_id"
payload_json = "payload_json"
# Nullable source -> NOT NULL sink, bridged by `default`.
nickname_safe = {{ from = "nickname", default = "anonymous" }}
# truncate=true: Int64 -> Int32 (values fit i32).
big_count_narrow = {{ from = "big_count", truncate = true }}
# truncate=true: STRING (unbounded) -> VARCHAR(8) clips to 8 chars.
long_bio_clipped = {{ from = "long_bio", truncate = true }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // The result row has more than 16 columns — sqlx tuple `FromRow`
    // tops out at arity 16, so fetch via `sqlx::Row::try_get`.
    use sqlx::Row as _;
    let rows = sqlx::query(&format!(
        "SELECT id, payload, is_live, small_signed, medium_signed, \
                real_f, double_f, big_decimal, fixed_decimal, \
                code, long_bio, fixed_blob, blob_unbounded, \
                born_on, seen_at, public_id, payload_json, \
                nickname_safe, big_count_narrow, long_bio_clipped \
         FROM \"{dst_schema}\".events ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 4);
    for (i, r) in rows.iter().enumerate() {
        let row = (i + 1) as i64;
        let id: i64 = r.try_get("id").unwrap();
        let payload: String = r.try_get("payload").unwrap();
        let is_live: bool = r.try_get("is_live").unwrap();
        let small_signed: i16 = r.try_get("small_signed").unwrap();
        let medium_signed: i32 = r.try_get("medium_signed").unwrap();
        let real_f: f32 = r.try_get("real_f").unwrap();
        let double_f: f64 = r.try_get("double_f").unwrap();
        let big_decimal: BigDecimal = r.try_get("big_decimal").unwrap();
        let fixed_decimal: BigDecimal = r.try_get("fixed_decimal").unwrap();
        let code: String = r.try_get("code").unwrap();
        let long_bio_out: String = r.try_get("long_bio").unwrap();
        let fixed_blob: Vec<u8> = r.try_get("fixed_blob").unwrap();
        let blob_unbounded: Vec<u8> = r.try_get("blob_unbounded").unwrap();
        let born_on: NaiveDate = r.try_get("born_on").unwrap();
        let seen_at: DateTime<Utc> = r.try_get("seen_at").unwrap();
        let public_id: Uuid = r.try_get("public_id").unwrap();
        let payload_json: serde_json::Value = r.try_get("payload_json").unwrap();
        let nickname_safe: String = r.try_get("nickname_safe").unwrap();
        let big_count_narrow: i32 = r.try_get("big_count_narrow").unwrap();
        let long_bio_clipped: String = r.try_get("long_bio_clipped").unwrap();

        assert_eq!(id, row);
        assert_eq!(payload, format!("payload-{row}"));
        assert_eq!(is_live, row % 2 == 0);
        assert_eq!(small_signed, row as i16 * 100);
        assert_eq!(medium_signed, row as i32 * 1_000);
        assert_eq!(real_f, row as f32 * 1.5_f32);
        assert_eq!(double_f, row as f64 * 1.25_f64);
        assert_eq!(
            big_decimal,
            BigDecimal::from(10_000_000_000_000_000_i64 + row),
            "big_decimal (NUMERIC(20,0) → BigInt) round-trips"
        );
        assert_eq!(
            fixed_decimal,
            BigDecimal::from_str(&format!("{}.{:04}", row * 10, row)).unwrap(),
            "fixed_decimal (NUMERIC(12,4)) round-trips"
        );
        assert_eq!(code, format!("code-{row}"));
        assert_eq!(long_bio_out, long_bio);
        assert_eq!(fixed_blob, vec![row as u8; 4]);
        assert_eq!(blob_unbounded, format!("blob-{row}").into_bytes());
        assert_eq!(
            born_on,
            NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(row),
        );
        assert_eq!(seen_at, base + chrono::Duration::seconds(row));
        assert_eq!(public_id, uuids[i]);
        assert_eq!(
            payload_json,
            serde_json::json!({ "row": row, "label": format!("label-{row}") }),
        );

        // Nullable -> NOT NULL bridge.
        if i == 1 {
            assert_eq!(
                nickname_safe, "anonymous",
                "default kicks in when source is NULL"
            );
        } else {
            assert_eq!(nickname_safe, format!("nick-{row}"));
        }
        // Truncate: Int64 -> Int32 with in-range source value.
        assert_eq!(
            big_count_narrow,
            (row * 1_000) as i32,
            "Int64 -> Int32 truncate (value fits) preserves value"
        );
        // Truncate: STRING (unbounded) -> VARCHAR(8).
        assert_eq!(
            long_bio_clipped.chars().count(),
            8,
            "Text(unbounded) -> Text(8) truncates to declared width"
        );
        assert!(long_bio.starts_with(long_bio_clipped.as_str()));
    }

    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&pg.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "events");
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(parsed.fields[0].value, Value::Int64(4));

    cockroach.pool.close().await;
    pg.pool.close().await;
}
