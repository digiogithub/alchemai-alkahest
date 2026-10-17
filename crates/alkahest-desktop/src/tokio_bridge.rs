//! Bridge between GPUI's executors and a dedicated tokio runtime.
//!
//! GPUI polls futures on its own foreground and background executors, which are not tokio:
//! anything that needs a tokio reactor (reqwest inside `alkahest-client`, tokio timers, child
//! processes) panics with "no reactor running" when polled there. [`init`] creates a small
//! multi-thread runtime and stores it as a GPUI global; [`spawn`] runs a future on it and hands
//! back a GPUI [`Task`] that aborts the tokio task when dropped, so a view that goes away
//! cancels its requests.
//!
//! Same pattern as the Bitacora desktop app (`bitacora-app/src/tokio_bridge.rs`, Digio, MIT),
//! itself an original implementation of the idea behind Zed's Apache-2.0 `gpui_tokio` crate;
//! no code was copied from Zed.

use std::future::Future;
use std::time::Duration;

use tokio::runtime::{Builder, Handle, Runtime};
use tokio::task::{AbortHandle, JoinError};

use crate::ui::{App, AppContext as _, Global, Task};

/// How long the runtime may take to wind down when the app quits.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// The runtime kept as a GPUI global.
struct TokioGlobal {
    /// Taken (and shut down) on app quit.
    runtime: Option<Runtime>,
    handle: Handle,
}

impl Global for TokioGlobal {}

/// Worker threads: the app only does HTTP and a few timers, so two are plenty.
const WORKERS: usize = 2;

/// Creates the runtime and stores it as a global; shuts it down when the app quits.
pub fn init(cx: &mut App) -> std::io::Result<()> {
    let runtime = Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .thread_name("alkahest-desktop-tokio")
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();
    cx.set_global(TokioGlobal {
        runtime: Some(runtime),
        handle,
    });
    cx.on_app_quit(|cx| {
        let runtime = cx
            .try_global::<TokioGlobal>()
            .is_some()
            .then(|| cx.global_mut::<TokioGlobal>().runtime.take())
            .flatten();
        // Shutting down blocks until the workers stop (bounded by the timeout), so do it off
        // the main thread.
        cx.background_spawn(async move {
            if let Some(runtime) = runtime {
                runtime.shutdown_timeout(SHUTDOWN_TIMEOUT);
            }
        })
    })
    .detach();
    Ok(())
}

/// A handle to the runtime, for code that must start long-lived tokio tasks itself.
///
/// # Panics
///
/// Panics if [`init`] was not called, a programming error at app start.
pub fn handle(cx: &App) -> Handle {
    cx.global::<TokioGlobal>().handle.clone()
}

/// Aborts the tokio task when dropped.
#[allow(dead_code)]
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs `fut` on the tokio runtime.
///
/// The returned GPUI task resolves with the future's output, or a [`JoinError`] if it panicked
/// or was cancelled. Dropping the GPUI task aborts the tokio task.
#[allow(dead_code)]
pub fn spawn<Fut, R>(cx: &App, fut: Fut) -> Task<Result<R, JoinError>>
where
    Fut: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    let join = cx.global::<TokioGlobal>().handle.spawn(fut);
    let guard = AbortOnDrop(join.abort_handle());
    cx.background_spawn(async move {
        let result = join.await;
        drop(guard);
        result
    })
}

// GPUI test contexts need gpui-kit's `test-support`, enabled by the `gui-test` feature (the
// desktop CI job), so the default workspace test run never compiles GPUI.
#[cfg(all(test, feature = "gui-test"))]
mod tests {
    use super::*;
    use crate::ui::testing::{TestAppContext, gpui_test};

    #[gpui_test]
    async fn spawned_tokio_future_returns_its_value(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(init).unwrap();
        let task = cx.update(|cx| {
            spawn(cx, async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                42_u32
            })
        });
        assert_eq!(task.await.unwrap(), 42);
    }
}
