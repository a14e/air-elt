//! Bounded retry wrapper for transient backend I/O.
//!
//! The validation pipeline wraps each access probe in [`retry_transient`]
//! so a single TCP listen-backlog overshoot or a remote pool restart
//! doesn't fail an entire flow's validation. Authoritative errors
//! (type-matrix violation, missing table, privilege denied, …) bypass
//! the retry and surface immediately.
//!
//! The retry budget is intentionally tiny — the real backpressure
//! primitive is the per-component semaphore in [`crate::util::concurrency`].
//! This helper only catches residual transient noise the cap cannot
//! prevent on its own.

use std::time::Duration;

use crate::error::RuntimeError;

/// Maximum total attempts (the first attempt + retries). Three is
/// enough to ride out a brief reconnect cycle but short enough not to
/// turn an unhealthy backend into a long startup hang.
pub const ACCESS_PROBE_RETRY_ATTEMPTS: u32 = 3;

/// Initial backoff between attempts. `50 ms → 250 ms → 1 250 ms` with
/// the factor below; the last delay is unused because the third
/// failure short-circuits to a hard error.
pub const ACCESS_PROBE_RETRY_BASE: Duration = Duration::from_millis(50);

/// Multiplier applied to the backoff between attempts.
pub const ACCESS_PROBE_RETRY_FACTOR: u32 = 5;

/// Run `op` up to [`ACCESS_PROBE_RETRY_ATTEMPTS`] times, returning the
/// first `Ok` or the final `Err`. Only [`RuntimeError::Backend`] is
/// retried — every other variant is authoritative and fails
/// immediately. Backoff grows by [`ACCESS_PROBE_RETRY_FACTOR`].
pub async fn retry_transient<F, Fut, T>(mut op: F) -> Result<T, RuntimeError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, RuntimeError>>,
{
    let mut delay = ACCESS_PROBE_RETRY_BASE;
    // Run the first N-1 attempts in a loop; the Nth attempt's result
    // becomes the function's return value. Structuring it this way
    // eliminates the trailing `unreachable!` marker — the type system
    // sees that every code path produces a `Result`.
    for attempt in 1..ACCESS_PROBE_RETRY_ATTEMPTS {
        match op().await {
            Ok(v) => return Ok(v),
            Err(RuntimeError::Backend(inner)) => {
                tracing::warn!(
                    attempt,
                    max_attempts = ACCESS_PROBE_RETRY_ATTEMPTS,
                    delay_ms = delay.as_millis() as u64,
                    error = %inner,
                    "transient backend error during validation probe — retrying"
                );
                tokio::time::sleep(delay).await;
                delay *= ACCESS_PROBE_RETRY_FACTOR;
            }
            Err(other) => return Err(other),
        }
    }
    // Final attempt — its result (Ok or Err of any variant) is what
    // the caller sees. No more retries left, no more backoff.
    op().await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A `Backend` failure on the first call followed by `Ok` on the
    /// retry must surface as a successful overall result.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn retry_transient_succeeds_after_one_backend_failure() {
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_seen = attempts.clone();
        let result: Result<u32, RuntimeError> = retry_transient(|| {
            let attempts = attempts_seen.clone();
            async move {
                let n = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 1 {
                    Err(RuntimeError::backend(std::io::Error::other("flaky")))
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(result.expect("eventual success"), 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// Three consecutive `Backend` failures must exhaust the budget and
    /// return the final error verbatim.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn retry_transient_exhausts_budget_on_persistent_backend_error() {
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_seen = attempts.clone();
        let result: Result<u32, RuntimeError> = retry_transient(|| {
            let attempts = attempts_seen.clone();
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(RuntimeError::backend(std::io::Error::other("persistent")))
            }
        })
        .await;
        assert!(matches!(result, Err(RuntimeError::Backend(_))));
        assert_eq!(attempts.load(Ordering::SeqCst), ACCESS_PROBE_RETRY_ATTEMPTS);
    }

    /// Non-`Backend` errors are authoritative — a single failure must
    /// short-circuit without consuming further retries.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn retry_transient_does_not_retry_non_backend_errors() {
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_seen = attempts.clone();
        let result: Result<u32, RuntimeError> = retry_transient(|| {
            let attempts = attempts_seen.clone();
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(RuntimeError::Other("authoritative".into()))
            }
        })
        .await;
        assert!(matches!(result, Err(RuntimeError::Other(_))));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "non-Backend error must not be retried"
        );
    }
}
