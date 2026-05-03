//! Test-only helper: provision a sandbox MongoDB database for e2e
//! tests. Mirrors the API of `pg_pool` / `mysql_pool`.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_MONGO_URL` is set, connect there and create a
//!    unique sandbox database per test. The database is dropped when
//!    the handle is dropped. CI uses this mode.
//! 2. Otherwise launch a fresh MongoDB container via testcontainers.

use mongodb::Client;
use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::mongo::Mongo as MongoImage;
use tracing::{info, warn};

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};

const URL_VAR: &str = "AIR_ELT_TEST_MONGO_URL";
/// Optional URL pointing at a *legacy* (pre-8.0) MongoDB. The mongo
/// sink branches on server version: ≥8.0 uses `Client::bulk_write`,
/// older falls back to a `replace_one` loop. Tests that need to
/// exercise the fallback set this to a 7.x server.
const LEGACY_URL_VAR: &str = "AIR_ELT_TEST_MONGO_LEGACY_URL";

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

enum CleanupGuard {
    External {
        client: Client,
        database: String,
    },
    Container {
        _container: Box<ContainerAsync<MongoImage>>,
    },
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        match self {
            CleanupGuard::External { client, database } => {
                let client = client.clone();
                let database = database.clone();
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
                        match tokio::time::timeout(std::time::Duration::from_secs(5), drop_fut)
                            .await
                        {
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
            CleanupGuard::Container { .. } => {}
        }
    }
}

pub async fn mongo_pool() -> MongoTestHandle {
    if let Ok(external) = std::env::var(URL_VAR) {
        return external_with_sandbox(&external).await;
    }
    let backend = detect_with_timeout(URL_VAR)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    match backend {
        TestBackend::ExternalUrl => unreachable!("handled above"),
        TestBackend::Container { socket } => {
            prepare_container_env(&socket);
            spawn_container("8").await
        }
    }
}

/// Sandbox handle pointing at a legacy (pre-8.0) MongoDB. Used only by
/// the bulk-write versioning e2e tests; everything else should use
/// `mongo_pool`. Honours `AIR_ELT_TEST_MONGO_LEGACY_URL`; falls back to
/// a `mongo:7.0` container.
pub async fn mongo_pool_legacy() -> MongoTestHandle {
    if let Ok(external) = std::env::var(LEGACY_URL_VAR) {
        return external_with_sandbox(&external).await;
    }
    let backend = detect_with_timeout(LEGACY_URL_VAR)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    match backend {
        TestBackend::ExternalUrl => unreachable!("handled above"),
        TestBackend::Container { socket } => {
            prepare_container_env(&socket);
            spawn_container("7.0").await
        }
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

async fn external_with_sandbox(url: &str) -> MongoTestHandle {
    let (base_url, _existing_db) = strip_db(url);
    let database = random_db();
    info!(db = %database, "creating sandbox database on external mongo");
    let client = Client::with_uri_str(&base_url)
        .await
        .expect("connect to AIR_ELT_TEST_MONGO_URL failed");
    // Touching a collection materialises the database; nothing to do
    // explicitly — Mongo databases are implicit.
    let cleanup = CleanupGuard::External {
        client: client.clone(),
        database: database.clone(),
    };
    MongoTestHandle {
        client,
        url: base_url,
        database,
        _cleanup: cleanup,
    }
}

async fn spawn_container(tag: &str) -> MongoTestHandle {
    info!(tag, "starting ephemeral mongo container");
    let container = MongoImage::default()
        .with_tag(tag)
        .start()
        .await
        .expect("start mongo container failed");
    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(27017)
        .await
        .expect("container port");
    let base_url = format!("mongodb://{host}:{port}");
    let client = Client::with_uri_str(&base_url)
        .await
        .expect("connect to containerised mongo failed");
    let database = random_db();
    MongoTestHandle {
        client,
        url: base_url,
        database,
        _cleanup: CleanupGuard::Container {
            _container: Box::new(container),
        },
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
