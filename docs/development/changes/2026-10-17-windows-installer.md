# Windows installer: NSIS, PATH, service, shortcuts (ALK-US-0076)

Implements [[ALK-US-0076]] (task ALK-T-0202). Builds on [[2026-10-17-windows-service-and-engine-ties]]
and [[2026-10-17-desktop-tray-autostart]]; shares `xtask bundle` with [[2026-10-17-macos-packages]].
Plan: [[plan/alkahest-v1-plan]].

## What

- NSIS (via cargo-packager) rather than MSI: one tool chain, runs on Linux for verification, and
  the components page the story needs. `packaging/windows/installer.nsi` is a modified copy of
  cargo-packager 0.11.8's template (attribution in the header and in `NOTICE`). Changes:
  components page (Alkahest required, **Add to PATH** on, **Windows service** off, **CUDA 12
  engine** off), shortcuts and "run after install" start `alkahest.exe desktop` with the shipped
  `alkahest.ico`, service stopped before files are replaced and restarted after, uninstaller runs
  `alkahest.exe service uninstall` before it kills or deletes anything and removes the PATH entry
  and `engines\`, silent switches `/SERVICE`, `/CUDA`, `/NOPATH`.
- `packaging/windows/path.ps1` (shipped as `tools\path.ps1`): registry based, idempotent PATH edit
  that keeps `REG_EXPAND_SZ` and broadcasts `WM_SETTINGCHANGE`.
- The CUDA component sets `ALKAHEST_ENGINES_DIR=$INSTDIR\engines` and runs
  `alkahest engines install llama-cpp --variant cuda12.4 --with-runtime`, so the engine and its
  runtime DLLs land beside the bundled CPU one, visible to all users and to the service.
- Install mode `perMachine` (Program Files, HKLM). Product name `Alkahest`. Installer output
  `alkahest_<version>_x64-setup.exe`; the headless archive is `alkahest-<version>-<triple>.zip`
  (what `install.ps1` downloads), written in-process by `archive::write_zip`.
- Signing, opt-in: `ALKAHEST_WINDOWS_SIGN_COMMAND` (command with `%1`) or
  `ALKAHEST_WINDOWS_CERT_THUMBPRINT` (+ `ALKAHEST_WINDOWS_TIMESTAMP_URL`); the workflow imports a
  `.pfx` from secrets into the store and checks `Get-AuthenticodeSignature`.
- `alkahest desktop` on Windows detaches its own console (`detach_own_console`: `FreeConsole` when
  it is the only process on it), so the shortcut does not leave a terminal window open. The binary
  stays a console program because the CLI and server are the same exe.
- Release job `windows` (windows-latest): bundle, signature check, then a silent install into
  `C:\AlkahestSmoke` with `/SERVICE`, checks (version, engine, icon, PATH, service running), silent
  uninstall (`_?=` so it is synchronous) and checks nothing is left.

## Verification (Linux host, zig shim)

- `cargo zigbuild --target x86_64-pc-windows-gnullvm -p alkahest --no-default-features` produced
  `alkahest.exe`; `detach_own_console` compiled for that target too (temporarily without its
  `desktop` cfg, GPUI cannot be cross-built).
- `cargo xtask bundle --no-build --formats nsis --target x86_64-pc-windows-gnullvm` with makensis
  3.09 (Ubuntu package, `NSISDIR`) compiled the real template into `alkahest_0.1.0_x64-setup.exe`
  and wrote the `.zip`. Only warning: `File /a` is ignored on non-Windows hosts.
- `cargo test -p xtask` (`windows_config_*`, `windows_archive_*`, `a_cross_build_*`).

## Not verified (no Windows machine)

Running the installer and uninstaller, `path.ps1` (no PowerShell here), the service stop/wait loop,
`alkahest service install` from the installer, the CUDA download, the silent-install smoke test in
CI, signing, the desktop (GPUI) build on `windows-latest`, console detaching, Windows on ARM (not
packaged), the exe icon (the shortcuts use the shipped `.ico`; embedding it in the exe needs a
resource compiler step, left open). MSI/WiX is not provided.

## Update 2026-10-24

Release signing moved to the Digio scheme (Developer ID from `MACOS_SIGNING_BUNDLE`, Windows
Authenticode with Azure Artifact Signing over OIDC) and is mandatory; the `APPLE_*` /
`WINDOWS_CERTIFICATE*` secrets above are no longer read. See
[[development/changes/2026-10-24-digio-release-signing]].
