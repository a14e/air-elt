//! Panic-safe RAII cleanup for sandbox extensions.
//!
//! `pg_pool` / `mysql_pool` / `mariadb_pool` / `mongo_pool` each own a
//! single sandbox schema/database that is dropped when their handle
//! drops. Cross-vendor tests routinely create *additional* sibling
//! schemas/databases (`{sandbox}_dst`, `{sandbox}_state`, …) — those
//! live outside the handle's scope and would leak past test panics
//! without an explicit guard.
//!
//! Each guard runs its `DROP …` in a fresh current-thread runtime
//! inside a dedicated thread, so:
//!   * `Drop` is panic-safe (a panic in the cleanup thread is caught
//!     by `join` and logged, not propagated — propagation during
//!     unwind aborts the process),
//!   * the drop is bounded by a 5s timeout so a hung server can't
//!     wedge the test process,
//!   * `mem::take` makes the guard idempotent across double-drop.

#![allow(dead_code)]

use std::time::Duration;

use mongodb::Client as MongoClient;
use sqlx::{Executor, MySqlPool, PgPool};

const DROP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct MysqlDbGuard {
    pool: MySqlPool,
    dbs: Vec<String>,
}

impl MysqlDbGuard {
    pub fn new(pool: MySqlPool, dbs: Vec<String>) -> Self {
        Self { pool, dbs }
    }
}

impl Drop for MysqlDbGuard {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let dbs = std::mem::take(&mut self.dbs);
        run_cleanup(move || async move {
            for db in dbs {
                let stmt = format!("DROP DATABASE IF EXISTS `{db}`");
                let _ = tokio::time::timeout(DROP_TIMEOUT, pool.execute(stmt.as_str())).await;
            }
        });
    }
}

pub struct PgSchemaGuard {
    pool: PgPool,
    schemas: Vec<String>,
}

impl PgSchemaGuard {
    pub fn new(pool: PgPool, schemas: Vec<String>) -> Self {
        Self { pool, schemas }
    }
}

impl Drop for PgSchemaGuard {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let schemas = std::mem::take(&mut self.schemas);
        run_cleanup(move || async move {
            for schema in schemas {
                let stmt = format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE");
                let _ = tokio::time::timeout(DROP_TIMEOUT, pool.execute(stmt.as_str())).await;
            }
        });
    }
}

pub struct CockroachDbGuard {
    pool: PgPool,
    dbs: Vec<String>,
}

impl CockroachDbGuard {
    pub fn new(pool: PgPool, dbs: Vec<String>) -> Self {
        Self { pool, dbs }
    }
}

impl Drop for CockroachDbGuard {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let dbs = std::mem::take(&mut self.dbs);
        run_cleanup(move || async move {
            for db in dbs {
                let stmt = format!("DROP DATABASE IF EXISTS \"{db}\" CASCADE");
                let _ = tokio::time::timeout(DROP_TIMEOUT, pool.execute(stmt.as_str())).await;
            }
        });
    }
}

pub struct MongoDbGuard {
    client: MongoClient,
    dbs: Vec<String>,
}

impl MongoDbGuard {
    pub fn new(client: MongoClient, dbs: Vec<String>) -> Self {
        Self { client, dbs }
    }
}

impl Drop for MongoDbGuard {
    fn drop(&mut self) {
        let client = self.client.clone();
        let dbs = std::mem::take(&mut self.dbs);
        run_cleanup(move || async move {
            for db in dbs {
                let database = client.database(&db);
                let _ = tokio::time::timeout(DROP_TIMEOUT, database.drop()).await;
            }
        });
    }
}

/// Spawn a dedicated thread, build a current-thread tokio runtime in
/// it, and `block_on` the cleanup future. Any panic in the cleanup
/// task stays inside the thread (`join` catches it) — propagating from
/// `Drop` during unwind would abort the process.
fn run_cleanup<F, Fut>(f: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    let join = std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(_) => return,
        };
        rt.block_on(f());
    });
    let _ = join.join();
}
