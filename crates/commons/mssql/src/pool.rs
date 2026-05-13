//! MS SQL connection pool via tiberius + bb8.
//!
//! sqlx 0.8 does not have an MSSQL backend (removed after 0.6, pending rewrite).
//! We use tiberius (the TDS driver maintained by Prisma) with bb8 for pooling.

use std::future::Future;
use std::pin::Pin;

use bb8::{CustomizeConnection, Pool};
use bb8_tiberius::ConnectionManager;
use tiberius::{AuthMethod, Config};

pub use air_elt_commons::pool_settings::PoolSettings;
use air_elt_core::error::{RuntimeError, RuntimeResult};

/// Convert a URL of the form `mssql://user:password@host:port/database`
/// (also accepting `sqlserver://…`) into a `tiberius::Config`.
///
/// tiberius itself only parses ADO (`Server=…;User Id=…;`) and JDBC
/// (`jdbc:sqlserver://…`) strings. We accept the more familiar URL form
/// across the project for parity with pg/mysql connectors.
pub fn config_from_url(url: &str) -> RuntimeResult<Config> {
    let stripped = url
        .strip_prefix("mssql://")
        .or_else(|| url.strip_prefix("sqlserver://"))
        .ok_or_else(|| {
            RuntimeError::Other(format!(
                "mssql url must start with mssql:// or sqlserver://: {url}"
            ))
        })?;

    // Split off optional `?query` (we ignore query parameters today —
    // none are part of the project's connection contract).
    let (head, _query) = match stripped.find('?') {
        Some(i) => (&stripped[..i], Some(&stripped[i + 1..])),
        None => (stripped, None),
    };

    // Split off optional `/database`.
    let (auth_host, database) = match head.find('/') {
        Some(i) => (&head[..i], Some(&head[i + 1..])),
        None => (head, None),
    };

    // Split `user:password@host:port` into auth + host. Tolerate
    // `host:port` without credentials (integrated auth not supported —
    // we use SQL auth across the project).
    let (auth, host_port) = match auth_host.rfind('@') {
        Some(i) => (Some(&auth_host[..i]), &auth_host[i + 1..]),
        None => (None, auth_host),
    };
    let (host, port) = match host_port.rfind(':') {
        Some(i) => {
            let h = &host_port[..i];
            let p: u16 = host_port[i + 1..]
                .parse()
                .map_err(|e| RuntimeError::Other(format!("mssql url: bad port: {e}")))?;
            (h, p)
        }
        None => (host_port, 1433u16),
    };

    let mut config = Config::new();
    config.host(host);
    config.port(port);
    if let Some(db) = database
        && !db.is_empty()
    {
        config.database(db);
    }
    if let Some(auth) = auth {
        let (user, password) = match auth.find(':') {
            Some(i) => (&auth[..i], &auth[i + 1..]),
            None => (auth, ""),
        };
        config.authentication(AuthMethod::sql_server(user, password));
    }
    // Self-signed certs are typical for SQL Server containers and local
    // dev — same posture as the PG `?sslmode=disable` in CI.
    config.trust_cert();
    Ok(config)
}

/// Connection-level session initialiser. Runs on every connection right
/// after it is opened (via `ManageConnection::connect`), so `SET
/// QUOTED_IDENTIFIER ON` / `SET ANSI_NULLS ON` apply uniformly across the
/// pool. Without this, sessions inheriting non-default options can mis-quote
/// identifiers or mis-handle NULL comparisons.
#[derive(Debug, Clone, Copy)]
struct MssqlSessionInit;

impl CustomizeConnection<bb8_tiberius::rt::Client, bb8_tiberius::Error> for MssqlSessionInit {
    fn on_acquire<'a>(
        &'a self,
        conn: &'a mut bb8_tiberius::rt::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), bb8_tiberius::Error>> + Send + 'a>> {
        Box::pin(async move {
            conn.simple_query("SET QUOTED_IDENTIFIER ON; SET ANSI_NULLS ON;")
                .await
                .map_err(bb8_tiberius::Error::from)?;
            Ok(())
        })
    }
}

/// Open a bb8 pool backed by tiberius connections.
///
/// The connection URL should use ADO format:
/// `mssql://user:pass@host:port/database`
///
/// Every connection acquired from the pool runs `SET QUOTED_IDENTIFIER ON;
/// SET ANSI_NULLS ON;` via the `CustomizeConnection` hook.
pub async fn connect(url: &str, timeouts: PoolSettings) -> RuntimeResult<Pool<ConnectionManager>> {
    let config = config_from_url(url)?;
    let manager = ConnectionManager::new(config);

    let pool = Pool::builder()
        .max_size(timeouts.max_connections)
        .min_idle(Some(timeouts.min_connections))
        .connection_timeout(timeouts.connect)
        .idle_timeout(Some(timeouts.idle))
        .max_lifetime(Some(timeouts.max_lifetime))
        .connection_customizer(Box::new(MssqlSessionInit))
        .build(manager)
        .await
        .map_err(|e| RuntimeError::Other(format!("mssql pool build: {e}")))?;

    Ok(pool)
}
