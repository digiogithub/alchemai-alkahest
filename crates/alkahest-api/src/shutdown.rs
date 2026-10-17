//! Stop signals and shutdown hooks (ALK-SP-0009.R4).
//!
//! On stop the server stops accepting connections, drains in-flight requests up to a
//! timeout ([`crate::server::serve_with`]), then runs the registered [`ShutdownHooks`] in
//! reverse registration order. The scheduler registers engine teardown there, so no engine
//! process outlives the server; the Windows service wrapper (ALK-US-0045) passes its own
//! stop future instead of [`stop_signal`].

use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

type HookFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type Hook = Box<dyn FnOnce() -> HookFuture + Send>;

/// Async callbacks run once, after the server has drained.
#[derive(Clone, Default)]
pub struct ShutdownHooks {
    hooks: Arc<Mutex<Vec<(String, Hook)>>>,
}

impl fmt::Debug for ShutdownHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self.hooks.lock().iter().map(|(n, _)| n.clone()).collect();
        f.debug_struct("ShutdownHooks")
            .field("hooks", &names)
            .finish()
    }
}

/// How one hook ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// Finished in time.
    Completed,
    /// Did not finish within the hook timeout and was abandoned.
    TimedOut,
}

impl ShutdownHooks {
    /// Registers `hook` under `name` (for logs). Later registrations run first, so a
    /// subsystem that depends on another is torn down before it.
    pub fn register<F, Fut>(&self, name: impl Into<String>, hook: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let boxed: Hook = Box::new(move || Box::pin(hook()) as HookFuture);
        self.hooks.lock().push((name.into(), boxed));
    }

    /// Number of hooks not yet run.
    pub fn len(&self) -> usize {
        self.hooks.lock().len()
    }

    /// True when no hook is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Runs every registered hook once, newest first, each bounded by `timeout`.
    pub async fn run(&self, timeout: Duration) -> Vec<(String, HookOutcome)> {
        let hooks: Vec<(String, Hook)> = std::mem::take(&mut *self.hooks.lock());
        let mut outcomes = Vec::with_capacity(hooks.len());
        for (name, hook) in hooks.into_iter().rev() {
            let outcome = match tokio::time::timeout(timeout, hook()).await {
                Ok(()) => HookOutcome::Completed,
                Err(_) => {
                    tracing::warn!(hook = %name, ?timeout, "shutdown hook timed out");
                    HookOutcome::TimedOut
                }
            };
            tracing::debug!(hook = %name, ?outcome, "shutdown hook finished");
            outcomes.push((name, outcome));
        }
        outcomes
    }
}

/// Installs the stop-signal handlers now and returns a future that resolves with the
/// signal's name when the first one arrives.
///
/// Unix: SIGINT and SIGTERM. Windows: ctrl-c, ctrl-break, console close and system
/// shutdown. Handlers are installed eagerly so a signal that arrives while the server is
/// still starting is not lost.
pub fn stop_signal() -> io::Result<impl Future<Output = &'static str> + Send + 'static> {
    imp::stop_signal()
}

#[cfg(unix)]
mod imp {
    use std::future::Future;
    use std::io;

    use tokio::signal::unix::{SignalKind, signal};

    pub(super) fn stop_signal() -> io::Result<impl Future<Output = &'static str> + Send + 'static> {
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        Ok(async move {
            tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = int.recv() => "SIGINT",
            }
        })
    }
}

#[cfg(windows)]
mod imp {
    use std::future::Future;
    use std::io;

    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_shutdown};

    pub(super) fn stop_signal() -> io::Result<impl Future<Output = &'static str> + Send + 'static> {
        let mut c = ctrl_c()?;
        let mut brk = ctrl_break()?;
        let mut close = ctrl_close()?;
        let mut shutdown = ctrl_shutdown()?;
        Ok(async move {
            tokio::select! {
                _ = c.recv() => "ctrl-c",
                _ = brk.recv() => "ctrl-break",
                _ = close.recv() => "console close",
                _ = shutdown.recv() => "system shutdown",
            }
        })
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::future::Future;
    use std::io;

    pub(super) fn stop_signal() -> io::Result<impl Future<Output = &'static str> + Send + 'static> {
        Ok(async {
            let _ = tokio::signal::ctrl_c().await;
            "ctrl-c"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn hooks_run_newest_first_once_and_time_out() {
        let hooks = ShutdownHooks::default();
        let order = Arc::new(Mutex::new(Vec::new()));
        for name in ["engines", "scheduler"] {
            let order = order.clone();
            hooks.register(name, move || async move { order.lock().push(name) });
        }
        hooks.register("stuck", || async {
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        assert_eq!(hooks.len(), 3);
        let outcomes = hooks.run(Duration::from_millis(50)).await;
        assert_eq!(
            outcomes,
            vec![
                ("stuck".to_string(), HookOutcome::TimedOut),
                ("scheduler".to_string(), HookOutcome::Completed),
                ("engines".to_string(), HookOutcome::Completed),
            ]
        );
        assert_eq!(*order.lock(), vec!["scheduler", "engines"]);
        assert!(hooks.is_empty());
        assert!(hooks.run(Duration::from_millis(50)).await.is_empty());
    }

    #[tokio::test]
    async fn clones_share_the_registry() {
        let hooks = ShutdownHooks::default();
        let runs = Arc::new(AtomicUsize::new(0));
        let r = runs.clone();
        hooks.clone().register("x", move || async move {
            r.fetch_add(1, Ordering::SeqCst);
        });
        hooks.run(Duration::from_secs(1)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stop_signal_installs() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async { assert!(stop_signal().is_ok()) });
    }
}
