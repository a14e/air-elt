//! Same-vendor: PostgreSQL source → PostgreSQL sink, PostgreSQL storage.
//!
//! Two e2e cases for wildcard + JSON auto-pack:
//!   * `pg_to_pg_wildcard_round_trip` — `mapping = ["*"]` against a 3-col
//!     table; round-trips every column (including a NULL) into a sink
//!     table with the same schema.
//!   * `pg_to_pg_json_auto_pack` — `mapping = ["id", "*:body"]`; sink has
//!     `(id, body JSONB)` and the runner packs every source field into
//!     `body`: `numeric` → JSON string (Decimal),
//!     plain ints stay JSON numbers, text stays a string.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::pg::pg_pool;
use bigdecimal::BigDecimal;
use chrono::{NaiveDate, TimeZone, Utc};
use sqlx::Executor;
use sqlx::Row;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_pg_wildcard_round_trip() {
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

    // Source table exercises every canonical type the PG source produces.
    // Truncate columns deliberately use narrower sink targets:
    //   * `bio` source is `TEXT` (unbounded), sink is `VARCHAR(5)` —
    //     forces the text-truncate path.
    //   * `legacy_big` source is `BIGINT`, sink is `INTEGER` with values
    //     that fit i32; forces the integer-truncate path without
    //     overflowing.
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".people (
                    id            BIGINT NOT NULL,
                    name          TEXT,
                    age           INT NOT NULL,
                    is_active     BOOLEAN NOT NULL,
                    rating_small  SMALLINT NOT NULL,
                    visits        INT,
                    score32       REAL NOT NULL,
                    score64       DOUBLE PRECISION NOT NULL,
                    huge_count    NUMERIC(20, 0) NOT NULL,
                    price         NUMERIC(10, 2) NOT NULL,
                    code          VARCHAR(8) NOT NULL,
                    blob_small    BYTEA NOT NULL,
                    blob_big      BYTEA,
                    born_on       DATE NOT NULL,
                    seen_at       TIMESTAMPTZ NOT NULL,
                    public_id     UUID NOT NULL,
                    payload       JSONB,
                    doc_xml       XML,
                    bio           TEXT,
                    legacy_big    BIGINT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Sink mirrors source for most columns. `bio` and `legacy_big` are
    // intentionally narrower than their source counterparts so the long-form
    // mapping below can opt them into the truncate path.
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema}\".people (
                    id            BIGINT NOT NULL,
                    name          TEXT,
                    age           INT NOT NULL,
                    is_active     BOOLEAN NOT NULL,
                    rating_small  SMALLINT NOT NULL,
                    visits        INT,
                    score32       REAL NOT NULL,
                    score64       DOUBLE PRECISION NOT NULL,
                    huge_count    NUMERIC(20, 0) NOT NULL,
                    price         NUMERIC(10, 2) NOT NULL,
                    code          VARCHAR(8) NOT NULL,
                    blob_small    BYTEA NOT NULL,
                    blob_big      BYTEA,
                    born_on       DATE NOT NULL,
                    seen_at       TIMESTAMPTZ NOT NULL,
                    public_id     UUID NOT NULL,
                    payload       JSONB,
                    doc_xml       XML,
                    bio           VARCHAR(5),
                    legacy_big    INT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let base_ts = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
    let uuid_a = uuid::Uuid::parse_str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11").unwrap();
    let uuid_b = uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
    let uuid_c = uuid::Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap();

    // Inserts use one SQL statement per row to keep typing readable.
    let insert_one = |id: i64,
                      name: Option<&'static str>,
                      age: i32,
                      is_active: bool,
                      rating_small: i16,
                      visits: Option<i32>,
                      score32: f32,
                      score64: f64,
                      huge_count: &'static str,
                      price: &'static str,
                      code: &'static str,
                      blob_small: Vec<u8>,
                      blob_big: Option<Vec<u8>>,
                      born_on: NaiveDate,
                      seen_at: chrono::DateTime<Utc>,
                      public_id: uuid::Uuid,
                      payload: Option<serde_json::Value>,
                      doc_xml: Option<&'static str>,
                      bio: Option<&'static str>,
                      legacy_big: i64| {
        let pool = pg.pool.clone();
        let src_schema = src_schema.clone();
        async move {
            let huge: BigDecimal = huge_count.parse().unwrap();
            let price_dec: BigDecimal = price.parse().unwrap();
            let sql = format!(
                "INSERT INTO \"{src_schema}\".people (
                    id, name, age, is_active, rating_small, visits,
                    score32, score64, huge_count, price, code,
                    blob_small, blob_big, born_on, seen_at, public_id,
                    payload, doc_xml, bio, legacy_big
                 ) VALUES (
                    $1, $2, $3, $4, $5, $6,
                    $7, $8, $9::numeric, $10::numeric, $11,
                    $12, $13, $14, $15, $16,
                    $17, $18::xml, $19, $20
                 )"
            );
            sqlx::query(&sql)
                .bind(id)
                .bind(name)
                .bind(age)
                .bind(is_active)
                .bind(rating_small)
                .bind(visits)
                .bind(score32)
                .bind(score64)
                .bind(huge)
                .bind(price_dec)
                .bind(code)
                .bind(blob_small)
                .bind(blob_big)
                .bind(born_on)
                .bind(seen_at)
                .bind(public_id)
                .bind(payload)
                .bind(doc_xml)
                .bind(bio)
                .bind(legacy_big)
                .execute(&pool)
                .await
                .unwrap();
        }
    };

    // Row 1 — full payload across every column. `bio` is intentionally
    // longer than the 5-char sink width to exercise the truncate path.
    insert_one(
        1,
        Some("alice"),
        30,
        true,
        7_i16,
        Some(100_i32),
        1.5_f32,
        2.75_f64,
        "99999999999999999999",
        "1234.56",
        "abc",
        vec![1_u8, 2, 3, 4],
        Some(b"hello-world-blob".to_vec()),
        NaiveDate::from_ymd_opt(1990, 1, 15).unwrap(),
        base_ts,
        uuid_a,
        Some(serde_json::json!({ "k": "v", "n": 1 })),
        Some("<root><x>1</x></root>"),
        Some("alphabet-soup"),
        10_000_i64,
    )
    .await;

    // Row 2 — every nullable column carries NULL.
    insert_one(
        2,
        None,
        41,
        false,
        -3_i16,
        None,
        -0.5_f32,
        0.125_f64,
        "1",
        "0.00",
        "z",
        vec![0xff_u8],
        None,
        NaiveDate::from_ymd_opt(1985, 6, 20).unwrap(),
        base_ts + chrono::Duration::seconds(60),
        uuid_b,
        None,
        None,
        None,
        -42_i64,
    )
    .await;

    // Row 3 — extremes (i16::MAX, BigInt that overflows i64, large
    // legacy_big near i32::MAX).
    insert_one(
        3,
        Some("carol"),
        27,
        true,
        i16::MAX,
        Some(0_i32),
        std::f32::consts::PI,
        std::f64::consts::E,
        "12345678901234567890",
        "999.99",
        "ALONG-8C",
        vec![],
        Some(vec![0_u8; 16]),
        NaiveDate::from_ymd_opt(1998, 12, 31).unwrap(),
        base_ts + chrono::Duration::seconds(120),
        uuid_c,
        Some(serde_json::json!([1, 2, 3])),
        Some("<root/>"),
        Some("short"),
        2_000_000_000_i64,
    )
    .await;

    let pg_url = pg.url_with_search_path();

    // Mapping uses `"*" = "*"` to round-trip every matching pair, plus two
    // explicit long-form entries that opt the narrowing columns into the
    // truncate path. AIR-70 forbids `default` on NOT NULL sources, so the
    // truncate entry on `legacy_big` (NOT NULL source) deliberately omits
    // `default`. The `bio` source is nullable so `default = "n/a"` is legal
    // and exercises the null-fallback path on row 2.
    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[sinks]]
name = "snk"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[storages]]
name = "st"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.people]
source = "src"
sink = "snk"
storage = "st"
from = "{src_schema}.people"
to = "{dst_schema}.people"
batch-limit = 8

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}

[flow.people.mapping]
"*" = "*"
bio = {{ from = "bio", truncate = true, default = "n/a" }}
legacy_big = {{ from = "legacy_big", truncate = true }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let rows = sqlx::query(&format!(
        "SELECT id, name, age, is_active, rating_small, visits, \
                score32, score64, huge_count::text AS huge_count, \
                price::text AS price, code, blob_small, blob_big, \
                born_on, seen_at, public_id, payload, doc_xml::text AS doc_xml, \
                bio, legacy_big \
         FROM \"{dst_schema}\".people ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 3, "all source rows must reach the sink");

    // Row 1 — full payload.
    let r1 = &rows[0];
    assert_eq!(r1.get::<i64, _>("id"), 1);
    assert_eq!(
        r1.get::<Option<String>, _>("name").as_deref(),
        Some("alice")
    );
    assert_eq!(r1.get::<i32, _>("age"), 30);
    assert!(r1.get::<bool, _>("is_active"));
    assert_eq!(r1.get::<i16, _>("rating_small"), 7);
    assert_eq!(r1.get::<Option<i32>, _>("visits"), Some(100));
    assert!((r1.get::<f32, _>("score32") - 1.5_f32).abs() < f32::EPSILON);
    assert!((r1.get::<f64, _>("score64") - 2.75_f64).abs() < f64::EPSILON);
    assert_eq!(r1.get::<String, _>("huge_count"), "99999999999999999999");
    assert_eq!(r1.get::<String, _>("price"), "1234.56");
    assert_eq!(r1.get::<String, _>("code"), "abc");
    assert_eq!(r1.get::<Vec<u8>, _>("blob_small"), vec![1, 2, 3, 4]);
    assert_eq!(
        r1.get::<Option<Vec<u8>>, _>("blob_big"),
        Some(b"hello-world-blob".to_vec())
    );
    assert_eq!(
        r1.get::<NaiveDate, _>("born_on"),
        NaiveDate::from_ymd_opt(1990, 1, 15).unwrap()
    );
    assert_eq!(r1.get::<chrono::DateTime<Utc>, _>("seen_at"), base_ts);
    assert_eq!(r1.get::<uuid::Uuid, _>("public_id"), uuid_a);
    assert_eq!(
        r1.get::<Option<serde_json::Value>, _>("payload"),
        Some(serde_json::json!({ "k": "v", "n": 1 }))
    );
    assert_eq!(
        r1.get::<Option<String>, _>("doc_xml").as_deref(),
        Some("<root><x>1</x></root>")
    );
    // Truncate path: source "alphabet-soup" (13 chars) → varchar(5).
    assert_eq!(
        r1.get::<Option<String>, _>("bio").as_deref(),
        Some("alpha"),
        "TEXT unbounded → VARCHAR(5) must truncate to 5 chars"
    );
    assert_eq!(r1.get::<i32, _>("legacy_big"), 10_000);

    // Row 2 — every nullable column carries NULL.
    let r2 = &rows[1];
    assert_eq!(r2.get::<i64, _>("id"), 2);
    assert_eq!(r2.get::<Option<String>, _>("name"), None);
    assert_eq!(r2.get::<i32, _>("age"), 41);
    assert!(!r2.get::<bool, _>("is_active"));
    assert_eq!(r2.get::<i16, _>("rating_small"), -3);
    assert_eq!(r2.get::<Option<i32>, _>("visits"), None);
    assert_eq!(r2.get::<Option<Vec<u8>>, _>("blob_big"), None);
    assert_eq!(r2.get::<Option<serde_json::Value>, _>("payload"), None);
    assert_eq!(r2.get::<Option<String>, _>("doc_xml"), None);
    // Source bio is NULL → default = "n/a" kicks in.
    assert_eq!(
        r2.get::<Option<String>, _>("bio").as_deref(),
        Some("n/a"),
        "NULL source must fall back to default literal"
    );
    assert_eq!(r2.get::<i32, _>("legacy_big"), -42);

    // Row 3 — extremes.
    let r3 = &rows[2];
    assert_eq!(r3.get::<i64, _>("id"), 3);
    assert_eq!(r3.get::<i16, _>("rating_small"), i16::MAX);
    assert_eq!(r3.get::<String, _>("huge_count"), "12345678901234567890");
    assert_eq!(r3.get::<String, _>("price"), "999.99");
    assert_eq!(r3.get::<Vec<u8>, _>("blob_small"), Vec::<u8>::new());
    assert_eq!(
        r3.get::<Option<Vec<u8>>, _>("blob_big"),
        Some(vec![0_u8; 16])
    );
    assert_eq!(
        r3.get::<Option<serde_json::Value>, _>("payload"),
        Some(serde_json::json!([1, 2, 3]))
    );
    assert_eq!(r3.get::<i32, _>("legacy_big"), 2_000_000_000);
    assert_eq!(
        r3.get::<Option<String>, _>("bio").as_deref(),
        Some("short"),
        "short input must pass through untouched under truncate"
    );

    pg.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_pg_json_auto_pack() {
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
            format!(
                "CREATE TABLE \"{src_schema}\".events (
                    id    BIGINT NOT NULL,
                    name  TEXT NOT NULL,
                    score NUMERIC(10,4) NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema}\".events (
                    id   BIGINT,
                    body JSONB
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Three rows with distinct shapes.
    let inserts = [
        (1_i64, "alpha", "12.3400"),
        (2, "beta", "0.5000"),
        (3, "gamma", "999.9999"),
    ];
    let insert = format!(
        "INSERT INTO \"{src_schema}\".events (id, name, score) \
         VALUES ($1, $2, $3::numeric)"
    );
    for (id, name, score) in inserts {
        sqlx::query(&insert)
            .bind(id)
            .bind(name)
            .bind(score)
            .execute(&pg.pool)
            .await
            .unwrap();
    }

    let pg_url = pg.url_with_search_path();

    // Mapping: `id` direct + `*:body` packs every source field (id, name,
    // score) into `body`. Cursor on `id` (an explicit Direct entry — must
    // exist post-expansion).
    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "postgres"
config = {{ url = "{pg_url}" }}

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
from = "{src_schema}.events"
to = "{dst_schema}.events"
batch-limit = 8

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}

[flow.events.mapping]
id = "id"
body = "*"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let rows: Vec<(i64, serde_json::Value)> = sqlx::query_as(&format!(
        "SELECT id, body FROM \"{dst_schema}\".events ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 3, "all rows must land in the sink");

    // JSON encoding rules:
    //   - integers stay JSON numbers,
    //   - text stays a JSON string,
    //   - Decimal serialises as a JSON string (lossless),
    //   - the packed object includes every source column under its own name.
    let expected: [(i64, serde_json::Value); 3] = [
        (
            1,
            serde_json::json!({ "id": 1, "name": "alpha", "score": "12.3400" }),
        ),
        (
            2,
            serde_json::json!({ "id": 2, "name": "beta", "score": "0.5000" }),
        ),
        (
            3,
            serde_json::json!({ "id": 3, "name": "gamma", "score": "999.9999" }),
        ),
    ];

    for (i, (id, body)) in rows.iter().enumerate() {
        let (expected_id, ref expected_body) = expected[i];
        assert_eq!(*id, expected_id, "row {i}: id column");
        assert_eq!(
            body, expected_body,
            "row {i}: packed body must contain every source field"
        );
    }

    pg.pool.close().await;
}

/// AIR-70 `switch` expression with boolean keys, struct-to-struct
/// (pg → pg): the source has a `BOOLEAN NOT NULL` column and the
/// mapping translates `true`/`false` into a `TEXT` sink column.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_pg_switch_bool_keys() {
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
            format!(
                "CREATE TABLE \"{src_schema}\".users (
                    id     BIGINT NOT NULL,
                    active BOOLEAN NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema}\".users_labelled (
                    id    BIGINT,
                    label TEXT
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let insert = format!("INSERT INTO \"{src_schema}\".users (id, active) VALUES ($1, $2)");
    let fixtures: [(i64, bool); 3] = [(1, true), (2, false), (3, true)];
    for (id, active) in fixtures {
        sqlx::query(&insert)
            .bind(id)
            .bind(active)
            .execute(&pg.pool)
            .await
            .unwrap();
    }

    let pg_url = pg.url_with_search_path();

    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[sinks]]
name = "snk"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[storages]]
name = "st"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.users]
source = "src"
sink = "snk"
storage = "st"
from = "{src_schema}.users"
to = "{dst_schema}.users_labelled"
batch-limit = 8

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}

[flow.users.mapping]
id = "id"
label = {{ from = "active", switch = {{ true = "yes", false = "no" }} }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let rows: Vec<(i64, Option<String>)> = sqlx::query_as(&format!(
        "SELECT id, label FROM \"{dst_schema}\".users_labelled ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], (1, Some("yes".to_string())));
    assert_eq!(rows[1], (2, Some("no".to_string())));
    assert_eq!(rows[2], (3, Some("yes".to_string())));

    pg.pool.close().await;
}
