//! Retry wrapper for CockroachDB serialization failures (`40001`).
//!
//! CockroachDB defaults to SERIALIZABLE isolation; under contention it returns
//! `40001 RETRY_SERIALIZABLE` instead of blocking. The standard handling is to
//! re-execute the whole statement (or transaction). For the Postgres dialect
//! the wrapper is a pass-through — there is no behaviour change for any
//! existing call site.

use std::future::Future;
use std::time::Duration;

use air_elt_core::error::{RuntimeError, RuntimeResult};
use tracing::{debug, warn};

use crate::dialect::Dialect;

/// Maximum number of attempts (including the first) before giving up and
/// surfacing the last `40001` error to the caller. Ten gives enough headroom
/// to ride out longer contention spikes (multi-row UPSERT batches under a
/// busy hot key) without amplifying load on a struggling cluster — combined
/// with the exponential backoff capped at [`MAX_BACKOFF`], the worst-case
/// total wait is bounded.
pub const MAX_ATTEMPTS: u32 = 10;
/// Base for the exponential backoff between retries.
pub const BASE_BACKOFF: Duration = Duration::from_millis(50);
/// Cap so a long-running retry doesn't sleep forever.
pub const MAX_BACKOFF: Duration = Duration::from_secs(2);

/// Run `op`. On `Postgres` dialect the closure is invoked exactly once. On
/// `Cockroach` dialect a `40001` error from the underlying database triggers
/// an exponential-backoff retry up to [`MAX_ATTEMPTS`] times.
///
/// `op` must be idempotent across attempts. Naturally-idempotent call sites
/// in the codebase: the sink's `INSERT … ON CONFLICT` / `UPSERT` paths, the
/// storage's `UPSERT_CURSOR` / `UPSERT_RESUME_TOKEN`, and any pure-read SELECT
/// (source `read_batch`, `load_cursor`, `load_resume_token`). The
/// `Storage::migrate` path is *not* wrapped — it runs once at deploy time
/// outside the contention window, and `sqlx::Migrator` already serialises
/// itself per migration step.
pub async fn with_serialization_retry<F, Fut, T>(dialect: Dialect, mut op: F) -> RuntimeResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = RuntimeResult<T>>,
{
    if !dialect.is_cockroach() {
        return op().await;
    }
    let mut attempt: u32 = 0;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(err) if is_serialization_failure(&err) && attempt + 1 < MAX_ATTEMPTS => {
                let delay = backoff_for(attempt);
                warn!(
                    attempt = attempt + 1,
                    max = MAX_ATTEMPTS,
                    backoff_ms = delay.as_millis() as u64,
                    "cockroach 40001 serialization failure; retrying"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(err) => {
                if is_serialization_failure(&err) {
                    debug!(attempt = attempt + 1, "40001 retries exhausted");
                }
                return Err(err);
            }
        }
    }
}

/// Detects a `40001 RETRY_SERIALIZABLE` from CockroachDB inside our
/// `RuntimeError::Backend` shell.
pub fn is_serialization_failure(err: &RuntimeError) -> bool {
    let RuntimeError::Backend(boxed) = err else {
        return false;
    };
    let Some(sqlx_err) = boxed.downcast_ref::<sqlx::Error>() else {
        return false;
    };
    let sqlx::Error::Database(db_err) = sqlx_err else {
        return false;
    };
    db_err.code().as_deref() == Some("40001")
}

fn backoff_for(attempt: u32) -> Duration {
    let factor = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    let nanos = (BASE_BACKOFF.as_nanos() as u64).saturating_mul(factor);
    Duration::from_nanos(nanos).min(MAX_BACKOFF)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[tokio::test]
    async fn postgres_dialect_is_pure_pass_through() {
        let calls = Cell::new(0u32);
        let out: i32 = with_serialization_retry(Dialect::Postgres, || async {
            calls.set(calls.get() + 1);
            Ok::<_, RuntimeError>(7)
        })
        .await
        .unwrap();
        assert_eq!(out, 7);
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn postgres_dialect_does_not_retry_even_on_serialization_like_error() {
        // A 40001-shaped RuntimeError isn't constructible without a real db;
        // for Postgres we just confirm any error short-circuits immediately.
        let calls = Cell::new(0u32);
        let res: RuntimeResult<()> = with_serialization_retry(Dialect::Postgres, || async {
            calls.set(calls.get() + 1);
            Err(RuntimeError::Other("boom".into()))
        })
        .await;
        assert!(res.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn cockroach_dialect_does_not_retry_on_non_serialization_error() {
        // The real 40001 path is exercised in e2e (sink_retry_e2e).
        // Here we verify that *other* errors are surfaced immediately
        // without retries.
        let calls = Cell::new(0u32);
        let res: RuntimeResult<()> = with_serialization_retry(Dialect::Cockroach, || async {
            calls.set(calls.get() + 1);
            Err(RuntimeError::Other("not-40001".into()))
        })
        .await;
        assert!(res.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn cockroach_dialect_returns_ok_without_retry_on_success() {
        let calls = Cell::new(0u32);
        let out: i32 = with_serialization_retry(Dialect::Cockroach, || async {
            calls.set(calls.get() + 1);
            Ok::<_, RuntimeError>(42)
        })
        .await
        .unwrap();
        assert_eq!(out, 42);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff_for(0), BASE_BACKOFF);
        assert_eq!(backoff_for(1), BASE_BACKOFF * 2);
        assert_eq!(backoff_for(2), BASE_BACKOFF * 4);
        // Very large attempt indices saturate at the cap.
        assert_eq!(backoff_for(40), MAX_BACKOFF);
    }

    #[test]
    fn other_errors_are_not_serialization_failures() {
        assert!(!is_serialization_failure(&RuntimeError::Other("x".into())));
        assert!(!is_serialization_failure(&RuntimeError::FlowAborted {
            flow: "f".into(),
            reason: "r".into(),
        }));
    }
}
