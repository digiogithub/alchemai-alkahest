//! Starting a local server from the desktop app (ALK-SP-0008.R2, "offer to start a server").
//!
//! [`LocalLauncher`] prefers the systemd user unit when `alkahest service install --user`
//! installed one (so the server keeps running under the service manager), and otherwise
//! spawns `alkahest serve` as a detached child: its own process group, no inherited stdin or
//! stdout, reaped by a background thread. Closing the app leaves that server running, as a
//! service would; it writes its own log files.
//!
//! A server that exits while it starts (it refuses a network bind without API keys, its port
//! is taken, its configuration does not load) explains why on stderr. The detached child's
//! stderr therefore goes to a file ([`LocalLauncher::stderr_log`], truncated on every launch)
//! and [`ServerLauncher::startup_failure`] reads its last error lines once the reaper saw the
//! child exit (ALK-US-0131). For the systemd unit the journal plays that part.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Unit name written by `alkahest service install --user`.
pub const USER_UNIT: &str = "alkahest.service";

/// How a server was started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchMethod {
    /// `systemctl --user start alkahest.service`.
    SystemdUser,
    /// A detached `alkahest serve` child.
    Process {
        /// Its process id.
        pid: u32,
    },
}

impl LaunchMethod {
    /// One line for the service card.
    pub fn describe(&self) -> String {
        match self {
            Self::SystemdUser => format!("systemd \u{b7} {USER_UNIT}"),
            Self::Process { pid } => format!("alkahest serve \u{b7} pid {pid}"),
        }
    }
}

/// Why a server could not be started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    /// The process could not be spawned.
    #[error("cannot run `alkahest serve`: {0}")]
    Spawn(String),
    /// `systemctl --user start` failed.
    #[error("cannot start the systemd user service: {0}")]
    Systemd(String),
    /// The server could not be stopped (no service unit and no process id, or the signal
    /// failed).
    #[error("cannot stop the server: {0}")]
    Stop(String),
}

/// How a stop request was carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMethod {
    /// `systemctl --user stop` (or `restart`) ran to completion.
    SystemdUser,
    /// The process was asked to stop (SIGTERM / `taskkill`); it shuts down gracefully, so
    /// the caller polls until the server stops answering.
    Signalled,
}

/// Starts a server. Implemented by [`LocalLauncher`]; tests use a fake.
pub trait ServerLauncher: Send + Sync + 'static {
    /// Starts a server and returns how. Must return quickly: readiness is polled by the
    /// connection manager.
    fn launch(&self) -> Result<LaunchMethod, LaunchError>;

    /// Name of the installed service unit, if any (shown in the service card).
    fn service_unit(&self) -> Option<String>;

    /// Asks the local server to stop: through the service unit when one is installed, else by
    /// signalling `pid` (the server's own process id from `GET /alkahest/v1/info`). Must
    /// return quickly; the caller polls until the server stops answering.
    fn stop(&self, pid: Option<u32>) -> Result<StopMethod, LaunchError> {
        let _ = pid;
        Err(LaunchError::Stop(
            "this launcher cannot stop servers".into(),
        ))
    }

    /// Called while a server started with `method` has not answered yet: why it already
    /// gave up (the process exited, the unit failed), with the server's own error text when
    /// it can be found. `None` while it may still come up. Must return quickly.
    fn startup_failure(&self, method: &LaunchMethod) -> Option<String> {
        let _ = method;
        None
    }
}

/// The last detached child: its pid and, once the reaper saw it exit, its exit status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Spawned {
    pid: u32,
    exit: Option<String>,
}

/// The real launcher.
#[derive(Debug, Clone)]
pub struct LocalLauncher {
    /// The `alkahest` executable (normally the running binary).
    pub program: PathBuf,
    /// Arguments before `serve` is appended (`--config <file>` when the user gave one).
    pub args: Vec<OsString>,
    /// The systemd user unit file, when installed.
    pub user_unit: Option<PathBuf>,
    /// Where a detached server's stderr goes (`<log_dir>/desktop-serve.stderr.log`); `None`
    /// discards it, and an early exit is then reported without the server's own words.
    pub stderr_log: Option<PathBuf>,
    /// The last detached child, shared with its reaper thread.
    spawned: Arc<Mutex<Option<Spawned>>>,
}

impl LocalLauncher {
    /// A launcher running `program [args] serve`, using the systemd user unit when its file
    /// exists.
    pub fn new(program: PathBuf, args: Vec<OsString>) -> Self {
        Self::with_unit(
            program,
            args,
            user_unit_file(&|k| std::env::var_os(k)).filter(|p| p.is_file()),
        )
    }

    /// A launcher with an explicit unit file (`None` = always spawn) and no stderr log.
    pub fn with_unit(program: PathBuf, args: Vec<OsString>, user_unit: Option<PathBuf>) -> Self {
        Self {
            program,
            args,
            user_unit,
            stderr_log: None,
            spawned: Arc::default(),
        }
    }

    /// Sends a detached server's stderr to `path` (created, truncated on every launch).
    #[must_use]
    pub fn with_stderr_log(mut self, path: Option<PathBuf>) -> Self {
        self.stderr_log = path;
        self
    }

    /// The stderr target: the log file when it can be opened, else nothing (a launch never
    /// fails because of its diagnostics).
    fn stderr_target(&self) -> Stdio {
        let Some(path) = &self.stderr_log else {
            return Stdio::null();
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::File::create(path) {
            Ok(f) => Stdio::from(f),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot open the serve stderr log");
                Stdio::null()
            }
        }
    }

    fn spawn_detached(&self) -> Result<LaunchMethod, LaunchError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(self.stderr_target());
        detach(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| LaunchError::Spawn(e.to_string()))?;
        let pid = child.id();
        if let Ok(mut s) = self.spawned.lock() {
            *s = Some(Spawned { pid, exit: None });
        }
        let spawned = Arc::clone(&self.spawned);
        // Reap the child when it exits so it does not linger as a zombie while the app runs,
        // and remember that it exited so an early exit can be reported.
        std::thread::Builder::new()
            .name("alkahest-serve-reaper".into())
            .spawn(move || {
                let status = child
                    .wait()
                    .map_or_else(|e| format!("unknown status ({e})"), |s| s.to_string());
                if let Ok(mut s) = spawned.lock()
                    && let Some(s) = s.as_mut().filter(|s| s.pid == pid)
                {
                    s.exit = Some(status);
                }
            })
            .map_err(|e| LaunchError::Spawn(e.to_string()))?;
        Ok(LaunchMethod::Process { pid })
    }

    /// Why detached child `pid` already exited, if it did.
    fn process_failure(&self, pid: u32) -> Option<String> {
        let status = self
            .spawned
            .lock()
            .ok()?
            .as_ref()
            .filter(|s| s.pid == pid)?
            .exit
            .clone()?;
        let stderr = self
            .stderr_log
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        Some(match startup_error_summary(&stderr) {
            Some(why) => format!("the server exited while starting: {why}"),
            None => format!("the server exited while starting ({status}); see its log"),
        })
    }

    /// Whether the user unit failed, with the end of its journal when it can be read.
    fn unit_failure(&self) -> Option<String> {
        let state = Command::new("systemctl")
            .args(["--user", "is-failed", USER_UNIT])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if String::from_utf8_lossy(&state.stdout).trim() != "failed" {
            return None;
        }
        let journal = Command::new("journalctl")
            .args([
                "--user",
                "-u",
                USER_UNIT,
                "-n",
                "20",
                "-o",
                "cat",
                "--no-pager",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        Some(unit_failure_message(&journal))
    }
}

/// The message for a failed systemd user unit, from the tail of its journal.
pub fn unit_failure_message(journal: &str) -> String {
    match startup_error_summary(journal) {
        Some(why) => format!("the {USER_UNIT} unit failed: {why}"),
        None => format!("the {USER_UNIT} unit failed; see `journalctl --user -u {USER_UNIT}`"),
    }
}

/// Longest summary shown in the connection state.
const SUMMARY_MAX_CHARS: usize = 600;

/// The server's own explanation from the end of its stderr: the last `error:` line (what
/// `alkahest` prints before exiting non-zero) with up to two lines after it, else the last
/// non-empty line. Terminal colour codes are removed. `None` when there is no text at all.
pub fn startup_error_summary(stderr: &str) -> Option<String> {
    let lines: Vec<String> = stderr
        .lines()
        .map(strip_ansi)
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines
        .iter()
        .rposition(|l| l.starts_with("error:") || l.starts_with("Error:"))
        .unwrap_or(lines.len().saturating_sub(1));
    let picked: Vec<String> = lines.get(start..)?.iter().take(3).cloned().collect();
    if picked.is_empty() {
        return None;
    }
    let mut text = picked.join(" ");
    if let Some(rest) = text
        .strip_prefix("error:")
        .or_else(|| text.strip_prefix("Error:"))
    {
        text = rest.trim_start().to_string();
    }
    if text.chars().count() > SUMMARY_MAX_CHARS {
        text = text.chars().take(SUMMARY_MAX_CHARS).collect::<String>() + "\u{2026}";
    }
    Some(text)
}

/// `line` without ANSI escape sequences (`ESC [ parameters letter`).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            // Skip `[` and the parameters up to the final letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

impl ServerLauncher for LocalLauncher {
    fn launch(&self) -> Result<LaunchMethod, LaunchError> {
        if self.user_unit.is_some() {
            let status = Command::new("systemctl")
                .args(["--user", "start", USER_UNIT])
                .stdin(Stdio::null())
                .status();
            match status {
                Ok(s) if s.success() => return Ok(LaunchMethod::SystemdUser),
                Ok(s) => {
                    tracing::warn!(status = %s, "systemctl --user start failed; spawning instead");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "systemctl not available; spawning instead");
                }
            }
        }
        self.spawn_detached()
    }

    fn service_unit(&self) -> Option<String> {
        self.user_unit.as_ref().map(|_| USER_UNIT.to_string())
    }

    fn stop(&self, pid: Option<u32>) -> Result<StopMethod, LaunchError> {
        if self.user_unit.is_some() {
            let status = Command::new("systemctl")
                .args(["--user", "stop", USER_UNIT])
                .stdin(Stdio::null())
                .status();
            if status.is_ok_and(|s| s.success()) {
                return Ok(StopMethod::SystemdUser);
            }
        }
        let pid = pid.ok_or_else(|| {
            LaunchError::Stop("the server's process id is unknown (no admin access?)".into())
        })?;
        signal_stop(pid)?;
        Ok(StopMethod::Signalled)
    }

    fn startup_failure(&self, method: &LaunchMethod) -> Option<String> {
        match method {
            LaunchMethod::Process { pid } => self.process_failure(*pid),
            LaunchMethod::SystemdUser => self.unit_failure(),
        }
    }
}

/// Asks process `pid` to shut down gracefully: SIGTERM on Unix (the server drains requests
/// and stops its engines), `taskkill` without `/F` on Windows.
fn signal_stop(pid: u32) -> Result<(), LaunchError> {
    if pid == 0 || pid == std::process::id() {
        return Err(LaunchError::Stop(format!("refusing to signal pid {pid}")));
    }
    let status = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(LaunchError::Stop(format!(
            "signalling pid {pid} failed ({s})"
        ))),
        Err(e) => Err(LaunchError::Stop(e.to_string())),
    }
}

/// Where `alkahest service install --user` puts the unit: `$XDG_CONFIG_HOME` or
/// `$HOME/.config`, then `systemd/user/alkahest.service`. Linux only.
pub fn user_unit_file(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let base = env("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(base.join("systemd/user").join(USER_UNIT))
}

/// Puts the child in its own process group (Unix) or detaches it from the console
/// (Windows), so the terminal's Ctrl-C and the app's exit do not take it down.
fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`.
        const FLAGS: u32 = 0x0000_0008 | 0x0000_0200;
        cmd.creation_flags(FLAGS);
    }
}

/// Whether `url` points at this machine, so starting a local server can help.
pub fn is_local_url(url: &str) -> bool {
    let rest = url
        .trim()
        .strip_prefix("http://")
        .or_else(|| url.trim().strip_prefix("https://"))
        .unwrap_or(url.trim());
    let authority = rest.split('/').next().unwrap_or_default();
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or_default()
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    matches!(host, "localhost" | "::1") || host.starts_with("127.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_urls_are_local() {
        assert!(is_local_url("http://127.0.0.1:11435"));
        assert!(is_local_url("http://localhost:11435/"));
        assert!(is_local_url("http://[::1]:11435"));
        assert!(!is_local_url("http://gpu-box:11435"));
        assert!(!is_local_url("https://10.0.0.5:443"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn user_unit_follows_xdg_config_home() {
        let env = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some(OsString::from("/x/cfg")),
            "HOME" => Some(OsString::from("/home/u")),
            _ => None,
        };
        assert_eq!(
            user_unit_file(&env),
            Some(PathBuf::from("/x/cfg/systemd/user/alkahest.service"))
        );
        let env = |k: &str| (k == "HOME").then(|| OsString::from("/home/u"));
        assert_eq!(
            user_unit_file(&env),
            Some(PathBuf::from(
                "/home/u/.config/systemd/user/alkahest.service"
            ))
        );
    }

    #[test]
    #[cfg(unix)]
    fn spawns_a_detached_serve_child() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("args");
        let script = dir.path().join("fake-alkahest");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"$@\" > '{}'\n", out.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let launcher =
            LocalLauncher::with_unit(script, vec!["--config".into(), "/tmp/c.toml".into()], None);
        assert!(launcher.service_unit().is_none());
        let method = launcher.launch().unwrap();
        assert!(matches!(method, LaunchMethod::Process { .. }));
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&out)
                && !text.is_empty()
            {
                assert_eq!(text.trim(), "--config /tmp/c.toml serve");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the child did not run");
    }

    #[test]
    #[cfg(unix)]
    fn stop_signals_the_server_process() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let launcher =
            LocalLauncher::with_unit(PathBuf::from("/nonexistent/alkahest"), Vec::new(), None);
        assert!(matches!(launcher.stop(None), Err(LaunchError::Stop(_))));
        assert_eq!(
            launcher.stop(Some(child.id())).unwrap(),
            StopMethod::Signalled
        );
        let status = child.wait().unwrap();
        assert!(!status.success(), "terminated by the signal");
        assert!(launcher.stop(Some(std::process::id())).is_err());
    }

    /// What `alkahest serve` prints when it refuses a network bind without keys (ALK-SP-0006).
    const REFUSAL: &str = "error: refusing to serve 0.0.0.0:11435: no active API keys exist, so \
                           remote clients could never authenticate; create one with `alkahest \
                           keys create <name>`, set ALKAHEST_API_KEY (or ALKAHEST_API_KEY_FILE), \
                           or pass --allow-unauthenticated to start anyway";

    #[test]
    fn the_summary_keeps_the_last_error_line() {
        let stderr = format!(
            "\u{1b}[2m2026-10-19T10:00:00Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m loading config\n\
             \u{1b}[33m WARN\u{1b}[0m something minor\n\n{REFUSAL}\n"
        );
        let s = startup_error_summary(&stderr).unwrap();
        assert!(s.starts_with("refusing to serve 0.0.0.0:11435"), "{s}");
        assert!(s.contains("alkahest keys create"));
        assert_eq!(
            startup_error_summary("INFO a\nWARN the port is taken\n").as_deref(),
            Some("WARN the port is taken"),
            "no error line: the last line"
        );
        assert_eq!(startup_error_summary(" \n\n"), None);
        let long = format!("error: {}", "x".repeat(2000));
        assert!(startup_error_summary(&long).unwrap().chars().count() <= SUMMARY_MAX_CHARS + 1);
        assert!(unit_failure_message("").contains("journalctl --user -u alkahest.service"));
        assert!(unit_failure_message(REFUSAL).contains("refusing to serve"));
    }

    #[test]
    #[cfg(unix)]
    fn an_early_exit_reports_the_servers_error() {
        // ALK-US-0131: the detached server refuses to start; its stderr reaches the user.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-alkahest");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho '{REFUSAL}' >&2\nexit 1\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let log = dir.path().join("logs/desktop-serve.stderr.log");
        let launcher =
            LocalLauncher::with_unit(script, Vec::new(), None).with_stderr_log(Some(log.clone()));
        let method = launcher.launch().unwrap();
        for _ in 0..500 {
            if let Some(why) = launcher.startup_failure(&method) {
                assert!(why.starts_with("the server exited while starting: refusing to serve"));
                assert!(why.contains("--allow-unauthenticated"));
                assert!(log.is_file());
                // Another pid (an older server) is not this launch.
                assert_eq!(
                    launcher.startup_failure(&LaunchMethod::Process { pid: 1 }),
                    None
                );
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the early exit was not reported");
    }

    #[test]
    #[cfg(unix)]
    fn a_running_server_is_not_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-alkahest");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let launcher = LocalLauncher::with_unit(script, Vec::new(), None)
            .with_stderr_log(Some(dir.path().join("e.log")));
        let method = launcher.launch().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(launcher.startup_failure(&method), None);
        let LaunchMethod::Process { pid } = method else {
            panic!("{method:?}");
        };
        launcher.stop(Some(pid)).unwrap();
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let launcher =
            LocalLauncher::with_unit(PathBuf::from("/nonexistent/alkahest"), Vec::new(), None);
        assert!(matches!(launcher.launch(), Err(LaunchError::Spawn(_))));
    }
}
