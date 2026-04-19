//! Graceful shutdown coordination for long-running daemons.
//!
//! Installs SIGTERM + SIGINT handlers, broadcasts the signal to any number
//! of subscribers, and lets each server (axum, tonic, custom loop) await
//! the same cancellation token. Keeps tsunagu framework-agnostic — no
//! hard dependency on axum or tonic.
//!
//! # Why this exists
//!
//! Surveying hanabi, hiroba, taimen, kindling, kontena, kenshi, shinka, and
//! seibi showed that none of them install a SIGTERM handler. Under
//! Kubernetes or systemd this means a force-kill after the grace period —
//! in-flight requests dropped, DB connections not closed, nothing logged.
//!
//! # Usage
//!
//! ```no_run
//! use tsunagu::shutdown::ShutdownController;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let shutdown = ShutdownController::install();
//!
//! // Pass a token to each server that needs drain notification.
//! let http_token = shutdown.token();
//! let background_token = shutdown.token();
//!
//! // axum integration:
//! // axum::serve(listener, router)
//! //     .with_graceful_shutdown(http_token.wait())
//! //     .await?;
//!
//! // Custom loop:
//! // let mut tok = background_token;
//! // loop {
//! //     tokio::select! {
//! //         work = do_something() => {},
//! //         () = tok.wait_ref() => break,
//! //     }
//! // }
//!
//! // Block main until shutdown fires and all workers drain.
//! shutdown.token().wait().await;
//! # Ok(())
//! # }
//! ```
//!
//! # Manual shutdown
//!
//! For tests, admin endpoints, or failure-triggered teardown, call
//! [`ShutdownController::shutdown`] — it fires the same channel SIGTERM
//! would, so every waiter drains the same way.

use tokio::sync::watch;
use tracing::info;

/// Coordinator that installs OS signal handlers and hands out drain tokens.
///
/// Created once per process (typically in `main`). Dropping it does not
/// fire shutdown — tokens survive; call [`shutdown`](Self::shutdown)
/// explicitly or wait for a signal.
#[derive(Debug, Clone)]
pub struct ShutdownController {
    tx: watch::Sender<bool>,
}

impl ShutdownController {
    /// Install SIGTERM + SIGINT handlers and return the controller.
    ///
    /// The handlers run on a background tokio task and fire the drain
    /// channel exactly once (subsequent signals are ignored — the
    /// receivers have already been notified).
    ///
    /// # Panics
    ///
    /// Panics at startup if `tokio::signal::unix::signal(SignalKind::terminate())`
    /// fails — this is a programmer error (invalid signal kind) rather than
    /// an operational condition, and propagating the error would force every
    /// daemon to handle a Result at the call site for no benefit.
    #[must_use]
    pub fn install() -> Self {
        let (tx, _rx) = watch::channel(false);
        let tx_sig = tx.clone();

        tokio::spawn(async move {
            wait_for_signal().await;
            // send_replace always updates the stored value — ordinary `send`
            // returns Err when no receivers exist, which can happen if the
            // controller hasn't handed out a token yet.
            tx_sig.send_replace(true);
        });

        Self { tx }
    }

    /// Build a controller that fires only on manual [`shutdown`](Self::shutdown).
    ///
    /// Use in tests or programs that want to exercise the drain path without
    /// installing OS signal handlers. Identical API to [`install`](Self::install).
    #[must_use]
    pub fn manual() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self { tx }
    }

    /// Fire the drain channel manually.
    ///
    /// Idempotent: subsequent calls after the first have no effect since
    /// the channel value is already `true`.
    pub fn shutdown(&self) {
        // send_replace updates the value even when there are no active
        // receivers; plain `send` returns Err in that case and would leave
        // the stored value stale, breaking late subscribers.
        self.tx.send_replace(true);
    }

    /// Hand out a new subscriber token for a server or task.
    ///
    /// Each token is independent — awaiting one does not consume others.
    #[must_use]
    pub fn token(&self) -> Shutdown {
        Shutdown {
            rx: self.tx.subscribe(),
        }
    }

    /// True if shutdown has been triggered.
    ///
    /// Useful for fast-path checks in hot loops that don't want to await.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        *self.tx.borrow()
    }
}

/// A drain-signal receiver held by one server or background task.
///
/// Cheap to clone via [`ShutdownController::token`]. Drop freely — a
/// dropped token does not affect the controller or siblings.
#[derive(Debug, Clone)]
pub struct Shutdown {
    rx: watch::Receiver<bool>,
}

impl Shutdown {
    /// Consume the token and await the drain signal.
    ///
    /// Returns immediately if shutdown has already been triggered. Returns
    /// on signal fire OR on controller drop (to prevent waiters hanging
    /// forever if the coordinator is dropped before firing).
    ///
    /// Designed to pair with `axum::serve(...).with_graceful_shutdown(tok.wait())`.
    pub async fn wait(mut self) {
        self.wait_ref().await;
    }

    /// Await the drain signal through a `&mut` borrow.
    ///
    /// Use this inside `tokio::select!` loops where the token must outlive
    /// each iteration.
    pub async fn wait_ref(&mut self) {
        loop {
            if *self.rx.borrow() {
                return;
            }
            if self.rx.changed().await.is_err() {
                // Controller dropped — treat as shutdown so waiters unblock.
                return;
            }
        }
    }

    /// True if shutdown has been triggered. Does not block.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        *self.rx.borrow()
    }
}

/// Wait for SIGTERM or SIGINT (whichever arrives first). Logs the signal.
///
/// On non-unix targets, only SIGINT (`ctrl-c`) is wired — SIGTERM doesn't
/// exist. This is only relevant for Windows daemons, which already have
/// different lifecycle semantics.
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => info!(signal = "SIGTERM", "shutdown signal received, draining"),
            _ = sigint.recv() => info!(signal = "SIGINT", "shutdown signal received, draining"),
        }
    }

    #[cfg(not(unix))]
    {
        // Windows — only SIGINT equivalent is available.
        if let Ok(()) = tokio::signal::ctrl_c().await {
            info!(signal = "ctrl-c", "shutdown signal received, draining");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::oneshot;
    use tokio::time::timeout;

    #[tokio::test]
    async fn manual_shutdown_fires_token() {
        let ctrl = ShutdownController::manual();
        let tok = ctrl.token();

        ctrl.shutdown();

        timeout(Duration::from_millis(200), tok.wait())
            .await
            .expect("wait should complete after manual shutdown");
    }

    #[tokio::test]
    async fn multiple_tokens_all_fire() {
        let ctrl = ShutdownController::manual();
        let t1 = ctrl.token();
        let t2 = ctrl.token();
        let t3 = ctrl.token();

        ctrl.shutdown();

        let handle = tokio::spawn(async move {
            tokio::join!(t1.wait(), t2.wait(), t3.wait());
        });

        timeout(Duration::from_millis(500), handle)
            .await
            .expect("all three tokens should fire")
            .expect("join handle");
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let ctrl = ShutdownController::manual();
        let tok = ctrl.token();

        ctrl.shutdown();
        ctrl.shutdown();
        ctrl.shutdown();

        assert!(ctrl.is_triggered());
        assert!(tok.is_triggered());
        tok.wait().await;
    }

    #[tokio::test]
    async fn token_fires_when_subscribed_after_shutdown() {
        // Late subscribers still see the drain signal — the initial value
        // of the watch channel is the current state.
        let ctrl = ShutdownController::manual();
        ctrl.shutdown();

        let late_tok = ctrl.token();
        assert!(late_tok.is_triggered());

        timeout(Duration::from_millis(200), late_tok.wait())
            .await
            .expect("late subscriber wait should return immediately");
    }

    #[tokio::test]
    async fn wait_ref_can_be_polled_in_select() {
        let ctrl = ShutdownController::manual();
        let mut tok = ctrl.token();
        let ctrl_clone = ctrl.clone();

        let (work_tx, work_rx) = oneshot::channel::<()>();
        let triggered_after_work = Arc::new(AtomicBool::new(false));
        let t_flag = triggered_after_work.clone();

        let worker = tokio::spawn(async move {
            tokio::select! {
                _ = work_rx => {
                    // Work done — check shutdown state afterwards.
                    t_flag.store(tok.is_triggered(), Ordering::SeqCst);
                }
                () = tok.wait_ref() => {
                    t_flag.store(true, Ordering::SeqCst);
                }
            }
        });

        // Trigger shutdown while worker is in select.
        tokio::time::sleep(Duration::from_millis(20)).await;
        ctrl_clone.shutdown();

        timeout(Duration::from_millis(500), worker)
            .await
            .expect("worker should complete")
            .expect("join");

        assert!(triggered_after_work.load(Ordering::SeqCst));
        // work_tx never sent — drop it to avoid unused warning.
        drop(work_tx);
    }

    #[tokio::test]
    async fn is_triggered_reflects_state() {
        let ctrl = ShutdownController::manual();
        let tok = ctrl.token();

        assert!(!ctrl.is_triggered());
        assert!(!tok.is_triggered());

        ctrl.shutdown();

        assert!(ctrl.is_triggered());
        assert!(tok.is_triggered());
    }

    #[tokio::test]
    async fn controller_drop_unblocks_waiters() {
        let ctrl = ShutdownController::manual();
        let tok = ctrl.token();

        // Drop the controller (and its tx) — waiters should resolve via the
        // Err path, not hang.
        drop(ctrl);

        timeout(Duration::from_millis(200), tok.wait())
            .await
            .expect("wait should unblock when controller is dropped");
    }

    #[tokio::test]
    async fn cloned_controllers_share_state() {
        let ctrl1 = ShutdownController::manual();
        let ctrl2 = ctrl1.clone();
        let tok = ctrl2.token();

        ctrl1.shutdown();

        assert!(ctrl2.is_triggered());
        tok.wait().await;
    }

    #[tokio::test]
    async fn install_signal_handlers_does_not_fire_without_signal() {
        let ctrl = ShutdownController::install();
        assert!(!ctrl.is_triggered());

        // Wait briefly; handlers should be silent without a signal.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!ctrl.is_triggered());

        // Clean up by manually triggering.
        ctrl.shutdown();
    }
}
