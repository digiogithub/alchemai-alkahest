//! Install channel detection: which installs may replace their own binary.
//!
//! A binary a package manager owns (deb/rpm, Homebrew, the macOS app bundle, the Windows NSIS
//! install, an AppImage) must be upgraded through that channel, or the manager's records and
//! the files on disk disagree. A development build under `target/` is refused too, so
//! `cargo run -- update` does not overwrite the build output by accident.

use std::ffi::OsString;
use std::path::{Component, Path};

/// How this `alkahest` was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// A release archive or a setup script: may self-update.
    Release,
    /// A deb or rpm package under `/usr`.
    SystemPackage,
    /// Homebrew (Cellar, `/opt/homebrew`, Linuxbrew).
    Homebrew,
    /// `Alkahest.app` from the dmg.
    AppBundle,
    /// The Windows NSIS installer.
    WindowsInstaller,
    /// An AppImage.
    AppImage,
    /// A cargo build output (`target/debug`, `target/release`).
    Source,
}

impl Channel {
    /// Stable machine name (the `channel` field of `--json`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::SystemPackage => "system-package",
            Self::Homebrew => "homebrew",
            Self::AppBundle => "app-bundle",
            Self::WindowsInstaller => "windows-installer",
            Self::AppImage => "appimage",
            Self::Source => "source",
        }
    }

    /// What to do instead of a self-update, `None` for [`Channel::Release`].
    pub fn hint(self) -> Option<&'static str> {
        Some(match self {
            Self::Release => return None,
            Self::SystemPackage => {
                "alkahest was installed from a deb or rpm package; upgrade it with your package \
                 manager (for example `sudo apt install ./alkahest_<version>_amd64.deb` or \
                 `sudo dnf upgrade ./alkahest-<version>.rpm` with the files from the release page)"
            }
            Self::Homebrew => "alkahest was installed with Homebrew; run `brew upgrade alkahest`",
            Self::AppBundle => {
                "alkahest runs from Alkahest.app; download the new .dmg from the release page and \
                 replace the app"
            }
            Self::WindowsInstaller => {
                "alkahest was installed with the Windows installer; run the new \
                 alkahest_<version>_x64-setup.exe from the release page"
            }
            Self::AppImage => {
                "alkahest runs from an AppImage; download the new .AppImage from the release page"
            }
            Self::Source => {
                "this is a development build; rebuild it from source, or pass an explicit \
                 version (`alkahest update vX.Y.Z`) to replace it with a release"
            }
        })
    }
}

/// What detection reads besides the executable path, injectable for tests.
#[derive(Debug, Clone, Default)]
pub struct ChannelEnv {
    /// The `APPIMAGE` variable the AppImage runtime sets.
    pub appimage: Option<OsString>,
    /// Whether the NSIS uninstaller sits next to the executable (Windows).
    pub has_nsis_uninstaller: bool,
}

impl ChannelEnv {
    /// Reads the real environment for `exe`.
    pub fn for_exe(exe: &Path) -> Self {
        let has_nsis_uninstaller = cfg!(windows)
            && exe
                .parent()
                .is_some_and(|d| d.join("uninstall.exe").is_file());
        Self {
            appimage: std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()),
            has_nsis_uninstaller,
        }
    }
}

/// Classifies the install at `exe` (symlinks already resolved).
pub fn detect(exe: &Path, env: &ChannelEnv) -> Channel {
    let p = exe.to_string_lossy().replace('\\', "/");
    let lower = p.to_ascii_lowercase();
    if env.appimage.is_some() || p.starts_with("/tmp/.mount_") {
        return Channel::AppImage;
    }
    if [
        "/cellar/",
        "/caskroom/",
        "/opt/homebrew/",
        "/home/linuxbrew/",
    ]
    .iter()
    .any(|m| lower.contains(m))
    {
        return Channel::Homebrew;
    }
    if crate::in_app_bundle(exe) {
        return Channel::AppBundle;
    }
    if env.has_nsis_uninstaller {
        return Channel::WindowsInstaller;
    }
    // deb and rpm put the binary in /usr/bin and the engines in /usr/lib/alkahest; the setup
    // script installs into /usr/local or ~/.local, which stay self-updatable.
    if p.starts_with("/usr/") && !p.starts_with("/usr/local/") {
        return Channel::SystemPackage;
    }
    if is_cargo_output(exe) {
        return Channel::Source;
    }
    Channel::Release
}

/// True for `.../target/{debug,release}/alkahest` and `.../target/<triple>/{debug,release}/...`.
fn is_cargo_output(exe: &Path) -> bool {
    let parts: Vec<_> = exe
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    parts.windows(2).enumerate().any(|(i, w)| {
        w[0] == "target"
            && (matches!(w[1].as_str(), "debug" | "release")
                || parts
                    .get(i + 2)
                    .is_some_and(|p| matches!(p.as_str(), "debug" | "release")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(p: &str) -> Channel {
        detect(Path::new(p), &ChannelEnv::default())
    }

    #[test]
    fn channel_detection_recognises_package_managers() {
        assert_eq!(d("/usr/bin/alkahest"), Channel::SystemPackage);
        assert_eq!(d("/usr/local/bin/alkahest"), Channel::Release);
        assert_eq!(d("/home/u/.local/bin/alkahest"), Channel::Release);
        assert_eq!(
            d("/usr/local/Cellar/alkahest/0.1.0/bin/alkahest"),
            Channel::Homebrew
        );
        assert_eq!(d("/opt/homebrew/bin/alkahest"), Channel::Homebrew);
        assert_eq!(
            d("/Applications/Alkahest.app/Contents/MacOS/alkahest"),
            Channel::AppBundle
        );
        assert_eq!(d("/tmp/.mount_alkaXYZ/usr/bin/alkahest"), Channel::AppImage);
        assert_eq!(
            d("/home/u/src/alkahest/target/debug/alkahest"),
            Channel::Source
        );
        assert_eq!(
            d("/home/u/src/alkahest/target/x86_64-unknown-linux-gnu/release/alkahest"),
            Channel::Source
        );
    }

    #[test]
    fn channel_detection_reads_the_environment() {
        let env = ChannelEnv {
            appimage: Some("/home/u/Alkahest.AppImage".into()),
            ..ChannelEnv::default()
        };
        assert_eq!(
            detect(Path::new("/home/u/bin/alkahest"), &env),
            Channel::AppImage
        );
        let env = ChannelEnv {
            has_nsis_uninstaller: true,
            ..ChannelEnv::default()
        };
        assert_eq!(
            detect(Path::new("C:\\Program Files\\Alkahest\\alkahest.exe"), &env),
            Channel::WindowsInstaller
        );
        assert!(Channel::Release.hint().is_none());
        assert!(Channel::Homebrew.hint().is_some());
    }
}
