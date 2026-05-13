//! Spawn-and-detach helper for the `mongodb` 3.x driver.
//!
//! The driver is not cancellation-safe — dropping a driver future
//! mid-await can leave the connection in an inconsistent state. The
//! flow runner wraps every adapter call in
//! `tokio::time::timeout` + `select!`, which DOES drop the future on
//! timeout / shutdown. To shield the driver from that drop, each
//! mongo trait method spawns its driver work onto the runtime and
//! awaits the resulting `JoinHandle`. Dropping a `JoinHandle` does
//! NOT abort the task: the driver future runs to completion on the
//! runtime even after the outer await is gone. The server-side
//! `maxTimeMS` (set on every operation via `*Options::max_time`)
//! bounds runaway work.
//!
//! Caveat — runtime shutdown: when `app::main` returns, the tokio
//! runtime is dropped, which aborts in-flight spawned tasks. A graceful
//! shutdown that awaits `engine.shutdown()` before returning still
//! ends up dropping pending detached mongo work. The runner reports
//! `Cancelled` regardless and the cursor is not advanced, so on the
//! next process start the work re-runs from the last persisted cursor.
//!
//! This module owns the small wrapper so every mongo connector
//! (source / cdc-source / sink / storage) goes through the same
//! shape and the cancel-safety story stays in one place.
use std::future::Future;

use air_elt_core::error::{RuntimeError, RuntimeResult};

pub async fn detached<F, T>(fut: F) -> RuntimeResult<T>
where
    F: Future<Output = RuntimeResult<T>> + Send + 'static,
    T: Send + 'static,
{
    // `JoinError` shapes: panic, or cancellation via `AbortHandle`.
    // We never abort — the only way to land in the error arm is a
    // panic in the spawned task. Map it to `RuntimeError::backend` so
    // `should_drop_ctx_on` refreshes the ctx on the next tick, matching
    // the SQL connectors' recovery posture.
    match tokio::spawn(fut).await {
        Ok(res) => res,
        Err(e) if e.is_panic() => Err(RuntimeError::backend(std::io::Error::other(format!(
            "mongo spawned task panicked: {e}"
        )))),
        // Cancellation is unreachable in normal operation (no abort
        // handle is exposed); kept as `backend` for parity.
        Err(e) => Err(RuntimeError::backend(std::io::Error::other(format!(
            "mongo spawned task failed: {e}"
        )))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::sync::Notify;

    use super::*;

    #[tokio::test]
    async fn detached_returns_value() {
        let v = detached(async { Ok(42i32) }).await.unwrap();
        assert_eq!(v, 42);
    }

    #[tokio::test]
    async fn detached_propagates_error() {
        let res: RuntimeResult<i32> =
            detached(async { Err(RuntimeError::Other("boom".into())) }).await;
        assert!(matches!(res, Err(RuntimeError::Other(s)) if s == "boom"));
    }

    /// Dropping the outer future awaiting `detached` must NOT abort the
    /// spawned task — the spawned work runs to completion in the
    /// background. This is the load-bearing property: the mongo driver
    /// future is never dropped mid-await.
    ///
    /// The signalling uses `Notify` (not wall-clock sleeps) so the test
    /// asserts the property rather than racing a timer.
    #[tokio::test]
    async fn dropped_outer_does_not_abort_spawned() {
        let started = Arc::new(Notify::new());
        let proceed = Arc::new(Notify::new());
        let finished = Arc::new(AtomicBool::new(false));

        let started2 = started.clone();
        let proceed2 = proceed.clone();
        let finished2 = finished.clone();
        let fut = detached(async move {
            started2.notify_one();
            proceed2.notified().await;
            finished2.store(true, Ordering::SeqCst);
            Ok::<_, RuntimeError>(())
        });
        let mut fut = Box::pin(fut);
        // Drive the outer future until `tokio::spawn` runs and the
        // inner task signals it is alive.
        tokio::select! {
            _ = &mut fut => panic!("detached future returned before signal"),
            _ = started.notified() => {}
        }
        // Drop the outer future. If `detached` were aborting the
        // spawn, the inner `proceed.notified()` would never resolve.
        drop(fut);
        // Release the inner task; it must still be alive to observe
        // the notify.
        proceed.notify_one();
        // Yield long enough for the spawned task to flip the flag.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        for _ in 0..10 {
            if finished.load(Ordering::SeqCst) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("spawned task did not complete after outer drop");
    }

    #[tokio::test]
    async fn detached_reports_panic_as_backend() {
        let res: RuntimeResult<i32> = detached(async {
            panic!("inner panic");
        })
        .await;
        let err = res.unwrap_err();
        assert!(
            matches!(err, RuntimeError::Backend(_)),
            "expected Backend variant for panics, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("panicked"), "unexpected error message: {msg}");
    }
}
