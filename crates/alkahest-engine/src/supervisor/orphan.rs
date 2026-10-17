//! Keeping engine processes from outliving the server (ALK-US-0092).
//!
//! A graceful stop of `alkahest serve` stops every engine, but a SIGKILL (or a crash, or the
//! OOM killer) gives the server no chance to. Two mechanisms cover that:
//!
//! - Linux: every engine is spawned with `PR_SET_PDEATHSIG = SIGTERM`, so the kernel signals
//!   it the moment its parent dies. The kernel ties the parent-death signal to the *thread*
//!   that forked the child, not to the process, so engines are forked from one dedicated
//!   spawner thread that lives as long as the process; forking from a tokio worker or a
//!   blocking-pool thread would kill the engine whenever that thread retires.
//! - Every platform: the [`ledger`](super::ledger) records each engine (pid, start time,
//!   executable) and the next `alkahest serve` stops the ones whose server is gone.
//!
//! macOS and Windows have no parent-death signal; there the ledger sweep at the next start
//! is the only cleanup (Windows job objects are a follow-up).

use std::io;

use tokio::process::{Child, Command};

/// Spawns `cmd` so that the child gets SIGTERM when this process dies (Linux), or spawns it
/// plainly elsewhere.
///
/// Must run inside a tokio runtime (the child's pipes and reaper register with it).
pub(crate) fn spawn_tied_to_parent(cmd: Command) -> io::Result<Child> {
    imp::spawn(cmd)
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io;
    use std::sync::OnceLock;
    use std::sync::mpsc;

    use tokio::process::{Child, Command};

    /// One spawn request for the spawner thread.
    struct Job {
        cmd: Command,
        runtime: tokio::runtime::Handle,
        reply: mpsc::SyncSender<io::Result<Child>>,
    }

    /// Queue of the process-lifetime spawner thread; `None` when the thread could not be
    /// started (the caller then spawns in place).
    static SPAWNER: OnceLock<Option<mpsc::Sender<Job>>> = OnceLock::new();

    fn spawner() -> Option<&'static mpsc::Sender<Job>> {
        SPAWNER
            .get_or_init(|| {
                let (tx, rx) = mpsc::channel::<Job>();
                let started = std::thread::Builder::new()
                    .name("alkahest-engine-spawner".into())
                    .spawn(move || {
                        // Never returns while the sender lives in the static: the thread must
                        // outlive every engine it forks (see the module docs).
                        while let Ok(job) = rx.recv() {
                            let _ctx = job.runtime.enter();
                            let mut cmd = job.cmd;
                            let _ = job.reply.send(cmd.spawn());
                        }
                    });
                match started {
                    Ok(_) => Some(tx),
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot start the engine spawner thread");
                        None
                    }
                }
            })
            .as_ref()
    }

    pub(super) fn spawn(mut cmd: Command) -> io::Result<Child> {
        arm_parent_death_signal(&mut cmd);
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let Some(queue) = spawner() else {
            // Degraded: the signal is tied to this thread, which may be a pool thread.
            return cmd.spawn();
        };
        let (reply, answer) = mpsc::sync_channel(1);
        queue
            .send(Job {
                cmd,
                runtime,
                reply,
            })
            .map_err(|_| io::Error::other("the engine spawner thread is gone"))?;
        // fork + exec takes milliseconds; blocking the caller that long is what
        // `Command::spawn` does anyway.
        answer
            .recv()
            .map_err(|_| io::Error::other("the engine spawner thread is gone"))?
    }

    /// Makes the child ask for SIGTERM when its parent thread dies, and exit at once when the
    /// parent already died between fork and the `prctl` call (the classic race).
    #[allow(unsafe_code)]
    fn arm_parent_death_signal(cmd: &mut Command) {
        let parent = rustix::process::getpid();
        // SAFETY: the closure runs in the forked child before exec, where only
        // async-signal-safe work is allowed. It makes two raw syscalls through rustix
        // (prctl, getppid), allocates nothing, takes no locks and touches no shared state;
        // the error values are `io::Error::from(Errno)`, which does not allocate.
        unsafe {
            cmd.pre_exec(move || {
                rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::TERM,
                ))
                .map_err(io::Error::from)?;
                if rustix::process::getppid() != Some(parent) {
                    // Reparented already: the server died before the signal was armed.
                    return Err(io::Error::from(rustix::io::Errno::SRCH));
                }
                Ok(())
            });
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::io;

    use tokio::process::{Child, Command};

    pub(super) fn spawn(mut cmd: Command) -> io::Result<Child> {
        cmd.spawn()
    }
}
