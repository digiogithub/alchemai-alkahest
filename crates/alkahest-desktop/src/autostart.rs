//! Start the desktop app at login (ALK-US-0073), per platform:
//!
//! - Linux: an XDG autostart entry, `$XDG_CONFIG_HOME/autostart/es.digio.alkahest.desktop`
//!   (else `~/.config/autostart/...`).
//! - macOS: a LaunchAgent, `~/Library/LaunchAgents/es.digio.alkahest.desktop.plist`
//!   (`RunAtLoad`).
//! - Windows: the `Alkahest` value of `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`,
//!   written with `reg.exe` (no registry crate needed for one value).
//!
//! The entry starts `alkahest desktop`; it does not install or start the server (that is the
//! service manager's job: `alkahest service install --user`).

use std::path::{Path, PathBuf};

/// File name of the Linux entry (matches the app id of the window and the packages).
pub const LINUX_ENTRY: &str = "es.digio.alkahest.desktop";
/// Label of the macOS LaunchAgent.
pub const MAC_LABEL: &str = "es.digio.alkahest.desktop";
/// Value name under the Windows `Run` key.
pub const WINDOWS_VALUE: &str = "Alkahest";
const WINDOWS_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Autostart failures.
#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    /// The entry file could not be written or removed.
    #[error("{path}: {source}")]
    Io {
        /// File.
        path: String,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// No home or config directory to put the entry in.
    #[error("cannot find the user's configuration directory")]
    NoHome,
    /// `reg.exe` failed (Windows).
    #[error("reg.exe: {0}")]
    Registry(String),
}

/// The entry file for this platform under `env` (Linux and macOS; `None` on Windows, which
/// uses the registry).
pub fn entry_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        let base = env("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| env("HOME").map(|h| Path::new(&h).join(".config")))?;
        Some(base.join("autostart").join(LINUX_ENTRY))
    } else if cfg!(target_os = "macos") {
        let home = env("HOME")?;
        Some(
            Path::new(&home)
                .join("Library/LaunchAgents")
                .join(format!("{MAC_LABEL}.plist")),
        )
    } else {
        None
    }
}

/// The XDG desktop entry that starts `exe desktop`.
pub fn linux_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Alkahest\nComment=Local AI model server \
         and manager\nExec=\"{}\" desktop\nIcon=alkahest\nTerminal=false\nCategories=Development;\
         \nX-GNOME-Autostart-enabled=true\n",
        exe.display()
    )
}

/// The LaunchAgent property list that starts `exe desktop` at login.
pub fn mac_plist(exe: &Path) -> String {
    let exe = exe
        .display()
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST \
         1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n\
         <dict>\n  <key>Label</key>\n  <string>{MAC_LABEL}</string>\n  \
         <key>ProgramArguments</key>\n  <array>\n    <string>{exe}</string>\n    \
         <string>desktop</string>\n  </array>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n\
         </plist>\n"
    )
}

/// The process environment lookup.
pub fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Whether the app starts at login.
pub fn is_enabled(env: &dyn Fn(&str) -> Option<String>) -> bool {
    if cfg!(windows) {
        return std::process::Command::new("reg")
            .args(["query", WINDOWS_RUN_KEY, "/v", WINDOWS_VALUE])
            .output()
            .is_ok_and(|o| o.status.success());
    }
    entry_path(env).is_some_and(|p| p.is_file())
}

/// Turns starting at login on (writing an entry for `exe`) or off.
pub fn set_enabled(
    on: bool,
    exe: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(), AutostartError> {
    if cfg!(windows) {
        let status = if on {
            let value = format!("\"{}\" desktop", exe.display());
            std::process::Command::new("reg")
                .args([
                    "add",
                    WINDOWS_RUN_KEY,
                    "/v",
                    WINDOWS_VALUE,
                    "/t",
                    "REG_SZ",
                    "/d",
                    &value,
                    "/f",
                ])
                .status()
        } else {
            std::process::Command::new("reg")
                .args(["delete", WINDOWS_RUN_KEY, "/v", WINDOWS_VALUE, "/f"])
                .status()
        };
        return match status {
            Ok(s) if s.success() || !on => Ok(()),
            Ok(s) => Err(AutostartError::Registry(s.to_string())),
            Err(e) => Err(AutostartError::Registry(e.to_string())),
        };
    }
    let path = entry_path(env).ok_or(AutostartError::NoHome)?;
    let io = |source| AutostartError::Io {
        path: path.display().to_string(),
        source,
    };
    if on {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let text = if cfg!(target_os = "macos") {
            mac_plist(exe)
        } else {
            linux_entry(exe)
        };
        std::fs::write(&path, text).map_err(io)
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn entry_is_written_and_removed_under_xdg_config_home() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_string_lossy().to_string();
        let env = move |k: &str| (k == "XDG_CONFIG_HOME").then(|| base.clone());
        assert!(!is_enabled(&env));
        set_enabled(true, Path::new("/opt/alkahest/bin/alkahest"), &env).unwrap();
        assert!(is_enabled(&env));
        let text = std::fs::read_to_string(dir.path().join("autostart").join(LINUX_ENTRY)).unwrap();
        assert!(
            text.contains("Exec=\"/opt/alkahest/bin/alkahest\" desktop"),
            "{text}"
        );
        set_enabled(false, Path::new("/x"), &env).unwrap();
        assert!(!is_enabled(&env));
        set_enabled(false, Path::new("/x"), &env).unwrap();
    }

    #[test]
    fn plist_starts_the_desktop_at_load() {
        let p = mac_plist(Path::new(
            "/Applications/Alkahest.app/Contents/MacOS/alkahest",
        ));
        assert!(p.contains("<string>desktop</string>") && p.contains("<key>RunAtLoad</key>"));
        assert!(p.contains(MAC_LABEL));
    }
}
