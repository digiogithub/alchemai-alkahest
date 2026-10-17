//! Starting a local server from the desktop app (ALK-SP-0008.R2, "offer to start a server").
//!
//! [`LocalLauncher`] prefers the systemd user unit when `alkahest service install --user`
//! installed one (so the server keeps running under the service manager), and otherwise
//! spawns `alkahest serve` as a detached child: its own process group, no inherited stdio,
//! reaped by a background thread. Closing the app leaves that server running, as a service
//! would; it writes its own log files.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
}

/// Starts a server. Implemented by [`LocalLauncher`]; tests use a fake.
pub trait ServerLauncher: Send + Sync + 'static {
    /// Starts a server and returns how. Must return quickly: readiness is polled by the
    /// connection manager.
    fn launch(&self) -> Result<LaunchMethod, LaunchError>;

    /// Name of the installed service unit, if any (shown in the service card).
    fn service_unit(&self) -> Option<String>;
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
}

impl LocalLauncher {
    /// A launcher running `program [args] serve`, using the systemd user unit when its file
    /// exists.
    pub fn new(program: PathBuf, args: Vec<OsString>) -> Self {
        Self {
            program,
            args,
            user_unit: user_unit_file(&|k| std::env::var_os(k)).filter(|p| p.is_file()),
        }
    }

    fn spawn_detached(&self) -> Result<LaunchMethod, LaunchError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| LaunchError::Spawn(e.to_string()))?;
        let pid = child.id();
        // Reap the child when it exits so it does not linger as a zombie while the app runs.
        std::thread::Builder::new()
            .name("alkahest-serve-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            })
            .map_err(|e| LaunchError::Spawn(e.to_string()))?;
        Ok(LaunchMethod::Process { pid })
    }
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
        let launcher = LocalLauncher {
            program: script,
            args: vec!["--config".into(), "/tmp/c.toml".into()],
            user_unit: None,
        };
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
    fn a_missing_program_is_a_spawn_error() {
        let launcher = LocalLauncher {
            program: PathBuf::from("/nonexistent/alkahest"),
            args: Vec::new(),
            user_unit: None,
        };
        assert!(matches!(launcher.launch(), Err(LaunchError::Spawn(_))));
    }
}
