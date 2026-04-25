use tokio::signal;
use tokio::sync::watch;
use tracing::{error, info};

pub async fn wait_for_shutdown(tx: &watch::Sender<bool>) {
    let ctrl_c = async {
        // Why: signal registration can fail in restrictive seccomp profiles.
        // Logging + exit(1) is more operator-friendly than an `expect`-panic
        // mid-shutdown-hotpath: the process fails early with a clear reason
        // instead of dumping a stack trace.
        signal::ctrl_c().await.unwrap_or_else(|e| {
            error!(error = ?e, "ctrl_c handler install failed");
            std::process::exit(1);
        });
    };

    #[cfg(unix)]
    let term = async {
        let mut stream = signal::unix::signal(signal::unix::SignalKind::terminate())
            .unwrap_or_else(|e| {
                error!(error = ?e, "SIGTERM handler install failed");
                std::process::exit(1);
            });
        stream.recv().await;
    };

    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received ctrl_c"),
        _ = term => info!("received SIGTERM"),
    }
    // Why: send fails only if every receiver was dropped, which is the
    // expected state if all flows already stopped on their own. Debug-log and
    // move on — operator doesn't need to see this at info.
    if tx.send(true).is_err() {
        tracing::debug!("shutdown channel already closed — flows likely completed on their own");
    }
}
