//! Keeping engine processes from outliving the server (ALK-US-0092, ALK-US-0094).
//!
//! A graceful stop of `alkahest serve` stops every engine, but a SIGKILL (or a crash, or the
//! OOM killer) gives the server no chance to. Per platform:
//!
//! - Linux: every engine is spawned with `PR_SET_PDEATHSIG = SIGTERM`, so the kernel signals
//!   it the moment its parent dies. The kernel ties the parent-death signal to the *thread*
//!   that forked the child, not to the process, so engines are forked from one dedicated
//!   spawner thread that lives as long as the process; forking from a tokio worker or a
//!   blocking-pool thread would kill the engine whenever that thread retires.
//! - Windows: every engine is assigned to its own job object with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The server holds the only handle; when the server
//!   dies, for whatever reason, the kernel closes it and kills the engine's whole process
//!   tree. A stop that outlasts its grace terminates the job (the tree), not just the child.
//! - macOS and other Unix systems (no parent-death signal): a watchdog per engine, a tiny
//!   `/bin/sh` loop started as a child of the server. Once a second it checks that its own
//!   parent is still the server (a killed server reparents it, which also covers a server
//!   that is a zombie or whose pid was reused) and that the engine still runs; when the server
//!   is gone it sends SIGTERM to the engine's process group, then SIGKILL after five seconds.
//!   The supervisor kills the watchdog when it reaps the engine. A shell loop instead of a
//!   kqueue helper: no extra binary to ship or find, and `sh`, `ps` and `kill` exist on every
//!   such system. `ALKAHEST_ENGINE_WATCHDOG=1` selects the watchdog on Linux too (tests, and
//!   sandboxes where `prctl` is filtered).
//! - Every platform: the [`ledger`](super::ledger) records each engine (pid, start time,
//!   executable) and the next `alkahest serve` stops the ones whose server is gone.

use std::io;

use tokio::process::{Child, Command};

/// Environment switch forcing the portable watchdog on Linux.
pub const WATCHDOG_ENV: &str = "ALKAHEST_ENGINE_WATCHDOG";

/// What keeps an engine tied to the server; dropped once the engine is reaped. Dropping it
/// also cleans up what is left of the engine's tree (Windows: closing the job kills it).
#[derive(Debug, Default)]
pub(crate) struct Tie {
    #[cfg(unix)]
    watchdog: Option<std::process::Child>,
    #[cfg(windows)]
    job: Option<windows::Job>,
}

impl Tie {
    /// Kills the engine's whole process tree at once (Windows job); a no-op elsewhere, where
    /// the caller signals the process group.
    pub(crate) fn terminate_tree(&self) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
    }
}

#[cfg(unix)]
impl Drop for Tie {
    fn drop(&mut self) {
        if let Some(mut w) = self.watchdog.take() {
            let _ = w.kill();
            let _ = w.wait();
        }
    }
}

/// Spawns `cmd` tied to this process (see the module docs). Must run inside a tokio runtime
/// (the child's pipes and reaper register with it).
pub(crate) fn spawn_tied_to_parent(mut cmd: Command) -> io::Result<(Child, Tie)> {
    #[cfg(windows)]
    {
        windows::prepare(&mut cmd);
        let child = cmd.spawn()?;
        let job = windows::Job::for_child(&child);
        if let Err(e) = &job {
            tracing::warn!(error = %e, "cannot put the engine in a job object; a killed \
                           server would leave it running until the next start");
        }
        Ok((child, Tie { job: job.ok() }))
    }
    #[cfg(unix)]
    {
        let force_watchdog = std::env::var_os(WATCHDOG_ENV).is_some_and(|v| v == "1");
        #[cfg(target_os = "linux")]
        let child = if force_watchdog {
            cmd.spawn()?
        } else {
            linux::spawn(cmd)?
        };
        #[cfg(not(target_os = "linux"))]
        let child = cmd.spawn()?;
        let mut tie = Tie::default();
        if !cfg!(target_os = "linux") || force_watchdog {
            match child.id().map(watchdog::start) {
                Some(Ok(w)) => tie.watchdog = Some(w),
                Some(Err(e)) => tracing::warn!(error = %e, "cannot start the engine watchdog; \
                    a killed server would leave the engine running until the next start"),
                None => {}
            }
        }
        Ok((child, tie))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok((cmd.spawn()?, Tie::default()))
    }
}

/// The portable watchdog (macOS and other non-Linux Unix systems).
#[cfg(unix)]
pub(crate) mod watchdog {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    /// The loop: `$1` = server pid, `$2` = engine pid (leader of its process group).
    pub(crate) const SCRIPT: &str = r#"s=$1; e=$2
while [ "$(ps -o ppid= -p $$ | tr -d ' ')" = "$s" ]; do
  kill -0 "$e" 2>/dev/null || exit 0
  sleep 1
done
kill -TERM -"$e" 2>/dev/null || exit 0
i=0
while [ $i -lt 5 ] && kill -0 "$e" 2>/dev/null; do sleep 1; i=$((i+1)); done
kill -KILL -"$e" 2>/dev/null
exit 0"#;

    /// Starts the watchdog of engine `engine` for this process.
    pub(crate) fn start(engine: u32) -> io::Result<std::process::Child> {
        start_for(std::process::id(), engine)
    }

    /// Starts a watchdog that guards `engine` while `server` (its parent) lives.
    pub(crate) fn start_for(server: u32, engine: u32) -> io::Result<std::process::Child> {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(SCRIPT)
            .arg("alkahest-engine-watchdog")
            .arg(server.to_string())
            .arg(engine.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Its own group: a terminal Ctrl-C aimed at the server must not kill the watchdog
            // and leave the engines unguarded.
            .process_group(0)
            .spawn()
    }
}

#[cfg(target_os = "linux")]
mod linux {
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

/// Job objects and console control events (ALK-T-0221).
#[cfg(windows)]
pub(crate) mod windows {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use tokio::process::{Child, Command};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

    /// Starts the engine as the leader of a new console process group, so CTRL_BREAK can be
    /// sent to it alone and the server's console Ctrl-C does not reach it.
    pub(crate) fn prepare(cmd: &mut Command) {
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    /// A job object that kills its processes when its last handle closes.
    #[derive(Debug)]
    pub(crate) struct Job {
        handle: OwnedHandle,
    }

    impl Job {
        /// Creates the job and assigns `child` to it. Processes the engine starts later
        /// inherit the job.
        #[allow(unsafe_code)]
        pub(crate) fn for_child(child: &Child) -> io::Result<Self> {
            let process = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("the engine already exited"))?;
            // SAFETY: CreateJobObjectW with null attributes and name creates an anonymous job
            // and returns an owned handle or null; ownership moves into `OwnedHandle`, which
            // closes it exactly once.
            let handle = unsafe {
                let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if raw.is_null() {
                    return Err(io::Error::last_os_error());
                }
                OwnedHandle::from_raw_handle(raw)
            };
            // SAFETY: zero is a valid bit pattern for this plain C struct.
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = u32::try_from(std::mem::size_of_val(&info)).map_err(io::Error::other)?;
            // SAFETY: `handle` is a live job handle; `info` is a properly sized, initialised
            // JOBOBJECT_EXTENDED_LIMIT_INFORMATION matching the information class.
            let ok = unsafe {
                SetInformationJobObject(
                    handle.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    size,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: both handles are live: the job is owned above, the process handle
            // belongs to `child`, which outlives this call.
            let ok = unsafe { AssignProcessToJobObject(handle.as_raw_handle(), process) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { handle })
        }

        /// Kills every process in the job (the engine and its helpers).
        #[allow(unsafe_code)]
        pub(crate) fn terminate(&self) {
            // SAFETY: the handle is a live job handle owned by `self`.
            let ok = unsafe { TerminateJobObject(self.handle.as_raw_handle(), 1) };
            if ok == 0 {
                tracing::debug!(error = %io::Error::last_os_error(), "terminating the engine job failed");
            }
        }
    }

    /// Asks the console process group led by `pid` to stop (CTRL_BREAK). Fails when the server
    /// has no console (running as a service); the caller then terminates the job.
    #[allow(unsafe_code)]
    pub(crate) fn ctrl_break(pid: u32) -> bool {
        use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
        // SAFETY: plain FFI call with integer arguments; no memory is shared.
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) != 0 }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::watchdog;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn alive(pid: u32) -> bool {
        // Zombies count as dead: they have exited and only wait to be reaped.
        Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .is_ok_and(|o| {
                let stat = String::from_utf8_lossy(&o.stdout);
                o.status.success() && !stat.trim().is_empty() && !stat.contains('Z')
            })
    }

    fn wait_dead(pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// A shell "server" starts the watchdog script (exactly what the supervisor runs) for an
    /// "engine" in its own process group, then is SIGKILLed.
    #[test]
    fn watchdog_kills_the_engine_group_when_the_server_dies() {
        use std::os::unix::process::CommandExt;
        let mut engine = Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        let e = engine.id();
        let server_script = format!(
            "/bin/sh -c '{}' wd $$ {e} >/dev/null 2>&1 & sleep 60",
            watchdog::SCRIPT.replace('\'', r"'\''")
        );
        let mut server = Command::new("/bin/sh")
            .arg("-c")
            .arg(server_script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            alive(e),
            "the watchdog must not touch a live server's engine"
        );
        server.kill().unwrap();
        server.wait().unwrap();
        let died = wait_dead(e, Duration::from_secs(10));
        let _ = engine.kill();
        let _ = engine.wait();
        assert!(died, "engine {e} survived its server");
    }

    #[test]
    fn watchdog_exits_when_the_engine_is_gone() {
        let mut engine = Command::new("sleep").arg("60").spawn().unwrap();
        let mut w = watchdog::start(engine.id()).unwrap();
        engine.kill().unwrap();
        engine.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while Instant::now() < deadline && !exited {
            exited = w.try_wait().unwrap().is_some();
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = w.kill();
        let _ = w.wait();
        assert!(exited, "the watchdog outlived its engine");
    }
}
