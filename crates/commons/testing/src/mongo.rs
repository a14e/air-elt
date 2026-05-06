//! Test-only helper: provision a sandbox MongoDB database for e2e
//! tests. Mirrors the API of `pg_pool` / `mysql_pool`.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_MONGO_URL` is set, connect there and create a
//!    unique sandbox database per test.
//! 2. Otherwise launch a MongoDB container via testcontainers in
//!    `ReuseDirective::Always` mode — labelled with the current ryuk
//!    session and `air-elt.kind=mongo` (or `mongo-legacy`), so the
//!    container is shared across every test process of one cargo
//!    invocation and reaped automatically when the last process exits.
//!
//! Two `OnceCell`s cache the resolved `(base_url, mongodb::Client)` per
//! variant — modern (≥ 8.0) and legacy (7.0, used by the bulk-write
//! versioning tests). Per-test sandbox databases are created and dropped
//! on the cached client.

use mongodb::Client;
use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ImageExt, ReuseDirective};
use testcontainers_modules::mongo::Mongo as MongoImage;
use tokio::sync::OnceCell;
use tracing::{info, warn};

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

static MONGO_INFRA: OnceCell<MongoInfra> = OnceCell::const_new();
static MONGO_LEGACY_INFRA: OnceCell<MongoInfra> = OnceCell::const_new();

pub struct MongoTestHandle {
    pub client: Client,
    /// Base URL without database segment (e.g. `mongodb://host:27017`).
    pub url: String,
    /// Sandbox database name. Unique per handle.
    pub database: String,
    _cleanup: CleanupGuard,
}

impl MongoTestHandle {
    pub fn url_with_database(&self) -> String {
        format!("{}/{}", self.url, self.database)
    }
}

struct CleanupGuard {
    client: Client,
    database: String,
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        let client = self.client.clone();
        let database = std::mem::take(&mut self.database);
        let join = std::thread::spawn(move || {
            // Panic-safe: a `Drop` that panics during unwind aborts
            // the process. If we cannot build a runtime here, log and
            // give up — the sandbox database is already disposable
            // (each run gets a fresh randomised name).
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    warn!(error = %e, db = %database, "could not build cleanup runtime; skipping db drop");
                    return;
                }
            };
            rt.block_on(async move {
                let db = client.database(&database);
                let drop_fut = db.drop();
                match tokio::time::timeout(std::time::Duration::from_secs(5), drop_fut).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        warn!(error = %e, db = %database, "failed to drop test database");
                    }
                    Err(_) => {
                        warn!(db = %database, "drop database timed out");
                    }
                }
            });
        });
        if let Err(e) = join.join() {
            warn!(?e, "cleanup thread panicked");
        }
    }
}

async fn mongo_infra(env_var: &str, container_tag: &str) -> &'static MongoInfra {
    let cell = if env_var == LEGACY_URL_VAR {
        &MONGO_LEGACY_INFRA
    } else {
        &MONGO_INFRA
    };
    cell.get_or_init(|| async move {
        if let Ok(external) = std::env::var(env_var) {
            let (base_url, _existing_db) = strip_db(&external);
            return MongoInfra { base_url };
        }
        let backend = detect_with_timeout(env_var)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let socket = match backend {
            TestBackend::ExternalUrl => unreachable!("handled above"),
            TestBackend::Container { socket } => socket,
        };
        prepare_container_env(&socket);
        ryuk::ensure_session(&socket).await;
        let (sk, sv) = ryuk::session_label();
        // Differentiate modern-vs-legacy mongo by tag, since reuse-mode
        // matches by labels: each variant gets its own kind value.
        let kind_value = if env_var == LEGACY_URL_VAR {
            "mongo-legacy"
        } else {
            "mongo"
        };
        info!(
            tag = container_tag,
            "ensuring shared mongo container (reuse=Always, ryuk-managed)"
        );
        let container = MongoImage::default()
            .with_tag(container_tag)
            .with_container_name(format!("air-elt-{kind_value}-{sv}"))
            .with_label(sk, sv)
            .with_label(KIND_LABEL_KEY, kind_value)
            .with_reuse(ReuseDirective::Always)
            .start()
            .await
            .expect("start mongo container failed");
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

pub async fn mongo_pool() -> MongoTestHandle {
    let infra = mongo_infra(URL_VAR, "8").await;
    handle_for(infra).await
}

/// Sandbox handle pointing at a legacy (pre-8.0) MongoDB. Used only by
/// the bulk-write versioning e2e tests; everything else should use
/// `mongo_pool`. Honours `AIR_ELT_TEST_MONGO_LEGACY_URL`; falls back to
/// a `mongo:7.0` container.
pub async fn mongo_pool_legacy() -> MongoTestHandle {
    let infra = mongo_infra(LEGACY_URL_VAR, "7.0").await;
    handle_for(infra).await
}

async fn handle_for(infra: &MongoInfra) -> MongoTestHandle {
    let database = random_db();
    info!(db = %database, "allocating sandbox mongo database");
    let client = Client::with_uri_str(&infra.base_url)
        .await
        .expect("connect to mongo failed");
    MongoTestHandle {
        client: client.clone(),
        url: infra.base_url.clone(),
        database: database.clone(),
        _cleanup: CleanupGuard { client, database },
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
/// Operator must set `AIR_ELT_TEST_MONGO_RS_URL` to a URL pointing
/// at a running RS-mongo. CI does this in the workflow (a
/// `mongo:8 --replSet rs0` service initiated to a one-node RS).
/// Local devs can replicate via:
///
/// ```text
/// docker run -d --rm -p 27017:27017 mongo:8 --replSet rs0 --bind_ip_all
/// docker exec <id> mongosh --eval 'rs.initiate({_id:"rs0",members:[{_id:0,host:"localhost:27017"}]})'
/// export AIR_ELT_TEST_MONGO_RS_URL="mongodb://localhost:27017/?replicaSet=rs0&directConnection=true"
/// ```
///
/// Tests that call this without the env var set are expected to
/// `#[ignore]`-skip themselves at runtime via
/// `mongo_rs_url_or_skip()`.
pub fn mongo_rs_url_or_skip() -> Option<String> {
    std::env::var(RS_URL_VAR).ok()
}

pub async fn mongo_rs_pool() -> MongoTestHandle {
    let url = std::env::var(RS_URL_VAR).unwrap_or_else(|_| {
        panic!(
            "{RS_URL_VAR} not set — mongo-cdc e2e tests need a replica-set mongo. \
             See `mongo_rs_url_or_skip` docstring for setup."
        )
    });
    let (base_url, _existing_db) = strip_db(&url);
    let client = Client::with_uri_str(&base_url)
        .await
        .expect("connect to RS mongo failed");
    let database = random_db();
    MongoTestHandle {
        client: client.clone(),
        url: base_url,
        database: database.clone(),
        _cleanup: CleanupGuard { client, database },
    }
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
