//! Test-only helper: provision a sandbox MongoDB database for e2e
//! tests. Mirrors the API of `pg_pool` / `mysql_pool`.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_MONGO_URL` / `AIR_ELT_TEST_MONGO_RS_URL` is set,
//!    connect there and create a unique sandbox database per test.
//!    Note: `AIR_ELT_TEST_MONGO_URL` is now expected to point at an
//!    RS-capable Mongo (single-node `replSet rs0` is fine), since
//!    `mongo_pool` shares one container with `mongo_rs_pool` and tests
//!    may exercise change-stream-shaped operations on either handle.
//!    Pointing it at a plain standalone is the operator's choice and
//!    is unsupported.
//! 2. Otherwise launch a MongoDB container via testcontainers in
//!    `ReuseDirective::Always` mode — labelled with the current ryuk
//!    session and `air-elt.kind=mongo-rs` (or `mongo-legacy`), so the
//!    container is shared across every test process of one cargo
//!    invocation and reaped automatically when the last process exits.
//!
//! Two `OnceCell`s cache the resolved base URL per variant:
//!
//! * `MONGO_RS_INFRA` — `mongo:8 --replSet rs0` single-node, backing
//!   both `mongo_pool()` and `mongo_rs_pool()`. The base URL carries
//!   `?replicaSet=rs0&directConnection=true` so plain CRUD callers
//!   inherit the same client semantics as CDC ones.
//! * `MONGO_LEGACY_INFRA` — `mongo:7` standalone, used only by
//!   `mongo_pool_legacy()` for the bulk-write fallback / standalone
//!   smoke tests.
//!
//! Per-test sandbox databases are created and dropped on a fresh
//! `mongodb::Client` (the driver spawns background tasks tied to the
//! tokio runtime that built it; `#[tokio::test]` constructs a fresh
//! runtime per test, so caching a `Client` would break across tests).

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use mongodb::bson::doc;
use mongodb::{Client, options::ClientOptions};
use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use testcontainers::core::{ContainerPort, Mount};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt, ReuseDirective};
use testcontainers_modules::mongo::Mongo as MongoImage;
use tokio::sync::OnceCell;
use tracing::info;

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

const URL_VAR: &str = "AIR_ELT_TEST_MONGO_URL";
/// Optional URL pointing at a *legacy* (pre-8.0) MongoDB. The mongo
/// sink branches on server version: ≥8.0 uses `Client::bulk_write`,
/// older falls back to a `replace_one` loop. Tests that need to
/// exercise the fallback set this to a 7.x server.
const LEGACY_URL_VAR: &str = "AIR_ELT_TEST_MONGO_LEGACY_URL";
const RS_URL_VAR: &str = "AIR_ELT_TEST_MONGO_RS_URL";
const KIND_LABEL_KEY: &str = "air-elt.kind";

/// Cached connection URL only. We deliberately do **not** cache a
/// `mongodb::Client` here: the driver spawns background tasks on the
/// tokio runtime that constructed it, and `#[tokio::test]` builds a
/// fresh runtime per test. A client cached on test 1's runtime starts
/// failing on test 2 with "A Tokio 1.x context was found, but it is
/// being shutdown.". Cheap to reconstruct per test.
struct MongoInfra {
    base_url: String,
}

static MONGO_LEGACY_INFRA: OnceCell<MongoInfra> = OnceCell::const_new();
static MONGO_RS_INFRA: OnceCell<MongoInfra> = OnceCell::const_new();

pub struct MongoTestHandle {
    pub client: Client,
    /// Base URL without database segment (e.g. `mongodb://host:27017`).
    pub url: String,
    /// Sandbox database name. Unique per handle.
    pub database: String,
}

impl MongoTestHandle {
    pub fn url_with_database(&self) -> String {
        format!("{}/{}", self.url, self.database)
    }
}

/// Provisions the legacy (pre-8.0) standalone MongoDB container. Used
/// only by `mongo_pool_legacy()` for the bulk-write fallback /
/// standalone-semantics smoke path. The modern (≥ 8.0) handle now
/// shares the RS container with `mongo_rs_pool()` — see
/// `mongo_rs_infra()`.
async fn mongo_legacy_infra() -> &'static MongoInfra {
    MONGO_LEGACY_INFRA
        .get_or_init(|| async move {
            if let Ok(external) = std::env::var(LEGACY_URL_VAR) {
                let (base_url, _existing_db) = strip_db(&external);
                return MongoInfra { base_url };
            }
            let backend = detect_with_timeout(LEGACY_URL_VAR)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            let socket = match backend {
                TestBackend::ExternalUrl => unreachable!("handled above"),
                TestBackend::Container { socket } => socket,
            };
            prepare_container_env(&socket);
            ryuk::ensure_session(&socket).await;
            let (sk, sv) = ryuk::session_label();
            let start_lock = crate::filelock::acquire_lock("mongo-legacy");
            let kind_value = "mongo-legacy";
            info!(
                tag = "7.0",
                kind = kind_value,
                "ensuring shared mongo-legacy standalone container (reuse=Always, ryuk-managed)"
            );
            // tmpfs on /data/db: container is reaped at session end, so
            // on-disk state has no value.
            let container = MongoImage::default()
                .with_tag("7.0")
                .with_container_name(format!("air-elt-{kind_value}-{sv}"))
                .with_label(sk, sv)
                .with_label(KIND_LABEL_KEY, kind_value)
                .with_mount(Mount::tmpfs_mount("/data/db"))
                .with_reuse(ReuseDirective::Always)
                .start()
                .await
                .expect("start mongo-legacy container failed");
            drop(start_lock);
            let host = container.get_host().await.expect("container host");
            let port = container
                .get_host_port_ipv4(27017)
                .await
                .expect("container port");
            let base_url = format!("mongodb://{host}:{port}");
            drop(container);
            MongoInfra { base_url }
        })
        .await
}

/// Sandbox handle pointing at the shared modern (≥ 8.0) MongoDB. Now
/// backed by the same `mongo:8 --replSet rs0` single-node container as
/// `mongo_rs_pool()` — there is no longer a separate standalone
/// `mongo:8` container, since maintaining two near-identical Mongo 8
/// containers per test session bought no extra coverage and cost
/// startup time.
///
/// Honours `AIR_ELT_TEST_MONGO_URL` for external-Mongo mode. The
/// supplied URL is expected to point at an RS-capable Mongo (since
/// callers may now do CDC-shaped operations on this handle); a plain
/// standalone is the operator's deployment choice and unsupported.
pub fn mongo_pool() -> Pin<Box<dyn Future<Output = MongoTestHandle> + Send + 'static>> {
    Box::pin(async move {
        // Honour the modern external override first; otherwise fall
        // through to the shared RS container (which also honours
        // `AIR_ELT_TEST_MONGO_RS_URL`).
        if std::env::var(URL_VAR).is_ok() {
            let infra = mongo_external_infra().await;
            return handle_for(infra).await;
        }
        let infra = mongo_rs_infra().await;
        handle_for(infra).await
    })
}

/// Cached external-mongo infra honouring `AIR_ELT_TEST_MONGO_URL`. Kept
/// distinct from `MONGO_RS_INFRA` so the two env vars don't fight over
/// a single cell when both are set.
static MONGO_EXTERNAL_INFRA: OnceCell<MongoInfra> = OnceCell::const_new();

async fn mongo_external_infra() -> &'static MongoInfra {
    MONGO_EXTERNAL_INFRA
        .get_or_init(|| async move {
            let external = std::env::var(URL_VAR)
                .expect("mongo_external_infra called without AIR_ELT_TEST_MONGO_URL set");
            let (base_url, _existing_db) = strip_db(&external);
            MongoInfra { base_url }
        })
        .await
}

/// Sandbox handle pointing at a legacy (pre-8.0) MongoDB. Used only by
/// the bulk-write versioning e2e tests; everything else should use
/// `mongo_pool`. Honours `AIR_ELT_TEST_MONGO_LEGACY_URL`; falls back to
/// a `mongo:7.0` container.
pub fn mongo_pool_legacy() -> Pin<Box<dyn Future<Output = MongoTestHandle> + Send + 'static>> {
    Box::pin(async move {
        let infra = mongo_legacy_infra().await;
        handle_for(infra).await
    })
}

async fn handle_for(infra: &MongoInfra) -> MongoTestHandle {
    let database = random_db();
    info!(db = %database, "allocating sandbox mongo database");
    let client = Client::with_uri_str(&infra.base_url)
        .await
        .expect("connect to mongo failed");

    MongoTestHandle {
        client,
        url: infra.base_url.clone(),
        database,
    }
}

fn strip_db(url: &str) -> (String, Option<String>) {
    if let Some(qmark) = url.find('?') {
        let (head, tail) = url.split_at(qmark);
        let (base, db) = strip_db_from_path(head);
        (format!("{base}{tail}"), db)
    } else {
        strip_db_from_path(url)
    }
}

fn strip_db_from_path(head: &str) -> (String, Option<String>) {
    let scheme_split = head.find("://").map(|i| i + 3).unwrap_or(0);
    let (scheme, rest) = head.split_at(scheme_split);
    if let Some(slash) = rest.find('/') {
        let (auth_host, slash_db) = rest.split_at(slash);
        let db = slash_db.trim_start_matches('/');
        let db = if db.is_empty() {
            None
        } else {
            Some(db.to_string())
        };
        (format!("{scheme}{auth_host}"), db)
    } else {
        (head.to_string(), None)
    }
}

/// Sandbox handle backed by a Mongo deployment with change-streams
/// enabled (i.e. a replica set). Required by every e2e test for the
/// `mongo-cdc` source, since Change Streams cannot run on a
/// standalone mongod.
///
/// Two modes, mirroring `mongo_pool`:
///
/// 1. If `AIR_ELT_TEST_MONGO_RS_URL` is set (CI workflow does this),
///    connect there.
/// 2. Otherwise launch a `mongo:8 --replSet rs0` container via
///    testcontainers, run `replSetInitiate` once, and reuse it across
///    every test process of the cargo invocation (ryuk-managed).
///
/// Clients always connect with `directConnection=true` so the driver
/// doesn't try to follow the in-container topology entry to a host:port
/// it can't reach.
async fn mongo_rs_infra() -> &'static MongoInfra {
    MONGO_RS_INFRA
        .get_or_init(|| async move {
            if let Ok(external) = std::env::var(RS_URL_VAR) {
                let (base_url, _existing_db) = strip_db(&external);
                return MongoInfra { base_url };
            }
            let backend = detect_with_timeout(RS_URL_VAR)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            let socket = match backend {
                TestBackend::ExternalUrl => unreachable!("handled above"),
                TestBackend::Container { socket } => socket,
            };
            prepare_container_env(&socket);
            ryuk::ensure_session(&socket).await;
            let (sk, sv) = ryuk::session_label();
            let start_lock = crate::filelock::acquire_lock("mongo-rs");
            info!(
                kind = "mongo-rs",
                "ensuring shared mongo-rs container (reuse=Always, ryuk-managed) — \
                 backs both mongo_pool() and mongo_rs_pool()"
            );
            // GenericImage rather than testcontainers_modules::Mongo —
            // we need to override the entrypoint command with
            // `--replSet rs0 --bind_ip_all`. tmpfs on /data/db: container
            // is reaped at session end, so on-disk state has no value.
            let image = GenericImage::new("mongo", "8")
                .with_exposed_port(ContainerPort::Tcp(27017))
                .with_cmd(["--replSet", "rs0", "--bind_ip_all"])
                .with_container_name(format!("air-elt-mongo-rs-{sv}"))
                .with_label(sk, sv)
                .with_label(KIND_LABEL_KEY, "mongo-rs")
                .with_mount(Mount::tmpfs_mount("/data/db"))
                .with_reuse(ReuseDirective::Always);
            let container = image.start().await.expect("start mongo-rs container");
            drop(start_lock);
            let host = container.get_host().await.expect("container host");
            let port = container
                .get_host_port_ipv4(27017)
                .await
                .expect("container port");
            let base_url = format!("mongodb://{host}:{port}/?replicaSet=rs0&directConnection=true");
            init_replica_set(&host.to_string(), port).await;
            drop(container);
            MongoInfra { base_url }
        })
        .await
}

/// Idempotent one-node `rs.initiate` + wait-for-primary. Tolerates a
/// reused container that's already initialised (replSetInitiate returns
/// `AlreadyInitialized` — we ignore the error and fall through to the
/// readiness probe).
async fn init_replica_set(host: &str, port: u16) {
    let admin_url = format!("mongodb://{host}:{port}/?directConnection=true");
    // Short server-selection timeout — pre-rs.initiate the server has
    // no primary yet, so the default 30s wait makes the bootstrap slow.
    let mut opts = ClientOptions::parse(&admin_url)
        .await
        .expect("parse admin mongo url");
    opts.server_selection_timeout = Some(Duration::from_secs(2));
    opts.direct_connection = Some(true);
    let client = Client::with_options(opts).expect("admin mongo client");
    let admin = client.database("admin");

    let _ = admin
        .run_command(doc! {
            "replSetInitiate": {
                "_id": "rs0",
                "members": [{ "_id": 0i32, "host": "localhost:27017" }],
            }
        })
        .await;

    for _ in 0..120 {
        if let Ok(resp) = admin.run_command(doc! { "hello": 1 }).await
            && resp.get_bool("isWritablePrimary").unwrap_or(false)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("mongo replica set did not become primary within 60s");
}

pub fn mongo_rs_pool() -> Pin<Box<dyn Future<Output = MongoTestHandle> + Send + 'static>> {
    Box::pin(async move {
        let infra = mongo_rs_infra().await;
        handle_for(infra).await
    })
}

fn random_db() -> String {
    let suffix = Alphanumeric
        .sample_string(&mut rng(), 8)
        .to_ascii_lowercase();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("test_{now}_{suffix}")
}

#[cfg(test)]
mod tests {
    use super::strip_db;

    #[test]
    fn strip_db_basic() {
        assert_eq!(
            strip_db("mongodb://localhost:27017/myapp"),
            ("mongodb://localhost:27017".into(), Some("myapp".into()))
        );
    }

    #[test]
    fn strip_db_no_db() {
        assert_eq!(
            strip_db("mongodb://localhost:27017"),
            ("mongodb://localhost:27017".into(), None)
        );
    }
}
