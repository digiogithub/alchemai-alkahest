//! Signalling an engine's whole process tree.
//!
//! On unix every engine starts in its own process group (pgid = its pid), so signalling the
//! group reaches helper processes the engine forked (vLLM workers, Python children) and the
//! terminal's Ctrl-C does not reach engines behind the server's back. On Windows each engine
//! has its own job object instead (`orphan::windows::Job`), terminated as a whole.

/// Asks the process group led by `pid` to terminate (SIGTERM). Missing groups are fine.
#[cfg(unix)]
pub fn terminate_group(pid: u32) {
    signal_group(pid, rustix::process::Signal::TERM);
}

/// Kills the process group led by `pid` (SIGKILL). Missing groups are fine.
#[cfg(unix)]
pub fn kill_group(pid: u32) {
    signal_group(pid, rustix::process::Signal::KILL);
}

#[cfg(unix)]
fn signal_group(pid: u32, sig: rustix::process::Signal) {
    let Ok(raw) = i32::try_from(pid) else {
        return;
    };
    let Some(pgid) = rustix::process::Pid::from_raw(raw) else {
        return;
    };
    if let Err(e) = rustix::process::kill_process_group(pgid, sig)
        && e != rustix::io::Errno::SRCH
    {
        tracing::debug!(pid, error = %e, "signalling engine process group failed");
    }
}

/// No process groups on this platform: the caller terminates the direct child.
#[cfg(not(unix))]
pub fn terminate_group(_pid: u32) {}

/// No process groups on this platform: the caller kills the direct child.
#[cfg(not(unix))]
pub fn kill_group(_pid: u32) {}
