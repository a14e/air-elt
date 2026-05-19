//! HTTP client wrapper for ClickHouse.
//!
//! ClickHouse exposes a SQL-over-HTTP interface on port 8123. Every
//! request is a `POST /` with the SQL in the `query` URL parameter and
//! the row payload (for `INSERT … FORMAT RowBinary`) in the request
//! body. We use [`reqwest`] directly rather than the `clickhouse` 0.13
//! crate — that crate's typed `Client::insert::<T: Row>(...)` API is
//! built around `serde::Serialize` rows, which doesn't fit our dynamic
//! `Vec<Value>` batches.
//!
//! Auth: HTTP `X-ClickHouse-User` / `X-ClickHouse-Key` headers. Works
//! with all CH auth backends; query-parameter auth is forbidden in
//! CH 22.8+ for security.

use std::io::Write;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use thiserror::Error;
use tracing::debug;

use air_elt_commons::pool_settings::PoolSettings;

/// Compression algorithm applied to the INSERT body. ClickHouse decodes
/// it server-side based on the `Content-Encoding` request header — the
/// CH HTTP handler natively supports `lz4` as the standard
/// [LZ4 frame format][1].
///
/// [1]: https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChCompression {
    /// Send the body uncompressed.
    None,
    /// Frame-encoded LZ4. Default — small CPU cost, ~3-5× wire-size
    /// reduction on typical row-binary numeric payloads.
    #[default]
    Lz4,
}

/// Connection configuration. The sink crate's `ChSinkConfig` builds
/// this directly.
#[derive(Debug, Clone)]
pub struct ChClientConfig {
    /// Base URL of the ClickHouse HTTP endpoint (no path, no query
    /// string). Example: `http://localhost:8123`.
    pub url: String,
    pub database: String,
    /// CH user. Required — never `Option`. For authless CH instances
    /// (default user, no password) set `user = "default"` and
    /// `password = ""`; the empty `X-ClickHouse-Key` header still goes
    /// over the wire and CH's auth accepts the empty match.
    pub user: String,
    /// CH password matching `user`. Required — never `Option`. Use
    /// `""` for authless setups; see the comment on `user`.
    pub password: String,
    pub pool: PoolSettings,
    /// INSERT body compression. Defaults to LZ4.
    pub compression: ChCompression,
}

#[derive(Debug, Error)]
pub enum ChClientError {
    #[error("reqwest build failure: {0}")]
    Build(#[from] reqwest::Error),
    #[error("http error {status}: {body}")]
    Http { status: u16, body: String },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("invalid header: {0}")]
    Header(String),
    #[error("body compression failed: {0}")]
    Compression(String),
}

/// Thin wrapper around a configured [`reqwest::Client`] plus the
/// resolved CH endpoint URL and default database.
#[derive(Debug, Clone)]
pub struct ChClient {
    http: reqwest::Client,
    base_url: String,
    database: String,
    compression: ChCompression,
}

impl ChClient {
    pub fn connect(config: ChClientConfig) -> Result<Self, ChClientError> {
        let mut headers = HeaderMap::new();
        // Always emit both auth headers — empty strings are a valid
        // wire shape for authless CH (CH's auth accepts the empty-
        // password match). Branching on emptiness would silently
        // re-introduce the bug where a missing user header caused
        // CH 24.3+ to reject the connection with a cryptic 403.
        insert_header(&mut headers, "X-ClickHouse-User", &config.user)?;
        insert_header(&mut headers, "X-ClickHouse-Key", &config.password)?;
        insert_header(&mut headers, "X-ClickHouse-Database", &config.database)?;
        // CH HTTP has no separate per-statement timeout — the request
        // timeout doubles as the unified call cap.
        let request_cap: Duration = config.pool.statement;
        // `max_connections == 0` would disable keep-alive and force a new
        // TCP connection per request; clamp to at least 1 to keep the pool
        // usable on a degenerate config.
        let pool_max = usize::try_from(config.pool.max_connections)
            .unwrap_or(5)
            .max(1);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(config.pool.connect)
            .timeout(request_cap)
            .pool_idle_timeout(config.pool.idle)
            .pool_max_idle_per_host(pool_max)
            .build()?;
        debug!(
            url = %config.url,
            database = %config.database,
            compression = ?config.compression,
            "clickhouse http client built"
        );
        Ok(Self {
            http,
            base_url: trim_trailing_slash(&config.url),
            database: config.database,
            compression: config.compression,
        })
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `SELECT 1` — server liveness probe.
    pub async fn ping(&self) -> Result<(), ChClientError> {
        self.query_text("SELECT 1").await.map(|_| ())
    }

    /// Execute an arbitrary SQL string (DDL or small read). Returns
    /// the raw response body. The SQL is sent in the request body.
    pub async fn query_text(&self, sql: &str) -> Result<String, ChClientError> {
        let resp = self
            .http
            .post(&self.base_url)
            .query(&[("database", self.database.as_str())])
            .body(sql.to_string())
            .send()
            .await
            .map_err(|e| ChClientError::Transport(e.to_string()))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| ChClientError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(ChClientError::Http {
                status: status.as_u16(),
                body,
            });
        }
        Ok(body)
    }

    /// Send `INSERT INTO db.t (...) FORMAT RowBinary` with a binary
    /// body. The SQL goes in the `query` URL parameter; the body is the
    /// row-binary payload, optionally LZ4-frame-compressed per the
    /// client's `compression` setting (CH decodes `Content-Encoding: lz4`
    /// natively).
    pub async fn insert_row_binary(&self, sql: &str, body: Vec<u8>) -> Result<(), ChClientError> {
        let (payload, encoding) = match self.compression {
            ChCompression::None => (body, None),
            ChCompression::Lz4 => {
                let uncompressed_len = body.len();
                let compressed = compress_lz4_frame(&body)?;
                debug!(
                    uncompressed_len,
                    compressed_len = compressed.len(),
                    "clickhouse lz4 frame compression"
                );
                (compressed, Some("lz4"))
            }
        };
        let mut req = self
            .http
            .post(&self.base_url)
            .query(&[("database", self.database.as_str()), ("query", sql)])
            .body(payload);
        if let Some(enc) = encoding {
            req = req.header("Content-Encoding", enc);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ChClientError::Transport(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(ChClientError::Http {
                status: status.as_u16(),
                body,
            });
        }
        Ok(())
    }
}

/// Compress `body` into a standard [LZ4 frame format][1] block. CH's
/// HTTP handler recognises this via `Content-Encoding: lz4`.
///
/// [1]: https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md
fn compress_lz4_frame(body: &[u8]) -> Result<Vec<u8>, ChClientError> {
    let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::with_capacity(body.len() / 2));
    encoder
        .write_all(body)
        .map_err(|e| ChClientError::Compression(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| ChClientError::Compression(e.to_string()))
}

fn insert_header(map: &mut HeaderMap, name: &str, value: &str) -> Result<(), ChClientError> {
    let n: HeaderName = name
        .parse()
        .map_err(|e: reqwest::header::InvalidHeaderName| ChClientError::Header(e.to_string()))?;
    let v = HeaderValue::from_str(value).map_err(|e| ChClientError::Header(e.to_string()))?;
    map.insert(n, v);
    Ok(())
}

fn trim_trailing_slash(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Single-shot HTTP stub. Binds to 127.0.0.1:0, returns its URL, and
    /// spawns a task that accepts exactly one connection, drains the
    /// request, and writes the configured response. Errors on the
    /// listener side surface via the JoinHandle.
    async fn stub_once(status: u16, reason: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Drain request — just enough to unblock the client. We don't
            // need to parse it for these tests.
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
                len = body.len(),
            );
            sock.write_all(response.as_bytes()).await.unwrap();
            sock.shutdown().await.unwrap();
        });
        format!("http://{addr}")
    }

    fn client_for(url: String) -> ChClient {
        ChClient::connect(ChClientConfig {
            url,
            database: "default".into(),
            user: "default".into(),
            password: String::new(),
            pool: PoolSettings {
                connect: Duration::from_secs(1),
                statement: Duration::from_secs(2),
                idle: Duration::from_secs(10),
                ..PoolSettings::defaults()
            },
            compression: ChCompression::None,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn query_text_surfaces_4xx_status_and_body() {
        let url = stub_once(400, "Bad Request", "Code: 62. DB::Exception: Syntax error").await;
        let client = client_for(url);
        let err = client.query_text("BOGUS").await.unwrap_err();
        match err {
            ChClientError::Http { status, body } => {
                assert_eq!(status, 400);
                assert!(body.contains("Syntax error"), "body was {body:?}");
            }
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn insert_row_binary_surfaces_5xx_status_and_body() {
        let url = stub_once(
            500,
            "Internal Server Error",
            "Code: 36. DB::Exception: Bad cast",
        )
        .await;
        let client = client_for(url);
        let err = client
            .insert_row_binary("INSERT INTO t (x) FORMAT RowBinary", vec![1, 2, 3])
            .await
            .unwrap_err();
        match err {
            ChClientError::Http { status, body } => {
                assert_eq!(status, 500);
                assert!(body.contains("Bad cast"), "body was {body:?}");
            }
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn query_text_surfaces_transport_error_on_closed_port() {
        // Bind then immediately drop to free the port — connect will fail.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let client = client_for(format!("http://{addr}"));
        let err = client.query_text("SELECT 1").await.unwrap_err();
        assert!(
            matches!(err, ChClientError::Transport(_)),
            "expected Transport error, got {err:?}"
        );
    }
}
