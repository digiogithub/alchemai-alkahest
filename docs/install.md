# Installing Alkahest

Setup scripts live in `scripts/setup/`. Both scripts detect the platform and GPU, install the
`alkahest` binary, put it on PATH, install the recommended llama.cpp engine build
(`alkahest engines install llama-cpp`, which picks the best accelerator variant for the machine
unless you name one) and can install the per-user service.

Release archives are named `alkahest-<version>-<target>.tar.gz` (`.zip` on Windows) with a
`SHA256SUMS` file next to them. **The release URLs are placeholders until the release pipeline
([[ALK-US-0074]]) publishes archives**; until then use a local build (`--from-local` /
`-FromLocal`) or `--base-url`. The scripts never run anything they downloaded before its
SHA-256 matches `SHA256SUMS`, and they are meant to be downloaded, read and then run; do not
pipe them into a shell from an unverified source.

## Linux and macOS

```sh
cargo build --release -p alkahest
scripts/setup/install.sh --from-local target/release/alkahest   # local build
scripts/setup/install.sh --dry-run                               # show the plan
```

| Option | Effect |
|---|---|
| `--dry-run` | print every step; change and download nothing |
| `--variant NAME` | engine build (`cpu`, `cuda12.8`, `vulkan`, `rocm`, `metal`, ...) |
| `--prefix DIR` | install into `DIR/bin`; default `~/.local`, or `/usr/local` as root. A prefix you cannot write to asks before using `sudo` (`-y` skips the question) |
| `--no-engine` | skip the engine install |
| `--service` | run `alkahest service install --user` (systemd user unit on Linux) |
| `--version`, `--base-url`, `--from-local` | where the binary comes from |

Re-running the script is safe: an identical binary is left alone and `engines install` is
idempotent. Prerequisites: `curl`, `tar` and `sha256sum` or `shasum` (only when downloading).
Prebuilt engines need no compiler; building an engine from source
(`alkahest engines install llama-cpp --from-source`) needs git, cmake and ninja. On Linux the
CUDA variants need the CUDA 12/13 runtime libraries on the host (see
[[2026-10-17-mainline-llama-cpp-recipes]]), Vulkan builds need a Vulkan driver, and ROCm builds
need ROCm.

Test the script with `scripts/setup/test-install.sh` (throwaway HOME, fake binary, no network).

## Windows

```powershell
cargo build --release -p alkahest
pwsh scripts\setup\install.ps1 -FromLocal target\release\alkahest.exe
pwsh scripts\setup\install.ps1 -DryRun
```

Parameters mirror the shell options in PowerShell style: `-DryRun`, `-Variant`, `-Prefix`
(default `%LOCALAPPDATA%\Programs\Alkahest`, binary in `bin\`), `-NoEngine`, `-Service`,
`-Version`, `-BaseUrl`, `-FromLocal`. The user PATH is updated once; open a new terminal
afterwards. No administrator rights are needed. CUDA variants on Windows need the CUDA runtime
DLLs; see [[ALK-US-0086]] (`engines install --with-runtime`).

Related: [[plan/alkahest-v1-plan]].

## Release packages

Tagged releases (`.github/workflows/release.yml`, built with `cargo xtask bundle`) publish, next
to `SHA256SUMS`:

| File | Content |
|---|---|
| `alkahest-<version>-<target>.tar.gz` | headless binary (no GUI libraries), bundled CPU llama.cpp under `engines/`, systemd unit templates under `systemd/` (Linux), `LICENSE`, `NOTICE`, `THIRD_PARTY_LICENSES`, `licenses/`. What `install.sh` downloads. Targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin`. |
| `alkahest-<version>-x86_64-pc-windows-msvc.zip` | the same for Windows (`alkahest.exe`); what `install.ps1` downloads. |
| `alkahest_<version>_amd64.deb` | desktop build (`alkahest desktop`, menu entry, icons), CLI and server in `/usr/bin/alkahest`, engine and licences in `/usr/lib/alkahest/`. |
| `alkahest-<version>-<release>.x86_64.rpm` | the same layout as the `.deb` for Fedora, RHEL and openSUSE (`Requires`: `vulkan-loader`, `libxkbcommon-x11`, `fontconfig`, `libwayland-client`, `alsa-lib`). |
| `alkahest-headless-<version>-<release>.<arch>.rpm` | CLI and server only, no GUI libraries; conflicts with `alkahest`. `x86_64` and `aarch64`. |
| `alkahest_<version>_x86_64.AppImage` | the same desktop build, self-contained. |
| `Alkahest_<version>_aarch64.dmg`, `Alkahest_<version>_x64.dmg` | macOS app (`Alkahest.app`, Apple Silicon and Intel, macOS 12+). |
| `alkahest_<version>_x64-setup.exe` | Windows installer (NSIS). |
| `alkahest.rb` | Homebrew formula for the CLI, to be copied into the tap. |

The Linux packages never run maintainer scripts and create no user or service. To run the server
as a system service after installing the `.deb`, the `.rpm` or the archive, ask for it
explicitly: `sudo alkahest service install` (system user `alkahest`, ALK-SP-0009.R2) or
`alkahest service install --user`. The bundled engine in `/usr/lib/alkahest/engines` (or
`engines/` next to the binary) is found automatically; `alkahest engines install` still adds GPU
builds into the data directory, which win over the bundled copy.

### macOS

Open the `.dmg` and drag `Alkahest.app` to Applications. Opening the app starts the desktop
(Dock icon plus a menu bar icon); its bundled llama.cpp (Metal on Apple Silicon) lives in
`Alkahest.app/Contents/Resources/engines`. The command line is inside the bundle:
`/Applications/Alkahest.app/Contents/MacOS/alkahest`, or link it with
`ln -s /Applications/Alkahest.app/Contents/MacOS/alkahest /usr/local/bin/alkahest`. Releases are
signed with a Developer ID and notarised when the maintainers' Apple secrets are configured; an
unsigned build (a fork, a release candidate) needs right-click, Open the first time.

For the CLI alone, `brew install digiogithub/tap/alkahest` (the formula `alkahest.rb` attached to
the release) installs the headless archive; `brew services start alkahest` runs the server at
login.

### Windows

Run `alkahest_<version>_x64-setup.exe` (administrator: it installs for all users into
`C:\Program Files\Alkahest`). The components page offers:

| Component | Default | What it does |
|---|---|---|
| Alkahest | required | `alkahest.exe` (CLI, server and desktop), bundled CPU llama.cpp, licences, Start menu shortcut that runs `alkahest.exe desktop` |
| Add to PATH | on | adds the install folder to the machine `PATH` (open a new terminal) |
| Windows service | off | runs `alkahest service install`: service `alkahest`, started at boot |
| CUDA 12 engine | off | downloads llama.cpp for CUDA 12 with its runtime DLLs (about 650 MB) into `engines\` |

Silent install: `alkahest_<version>_x64-setup.exe /S [/SERVICE] [/CUDA] [/NOPATH] [/D=C:\dir]`.
The uninstaller removes the service, the PATH entry and the files; models and settings in the data
directories are kept. Upgrading stops the service, replaces the files and starts it again.

## HTTPS

The server speaks plain HTTP on loopback by default. To expose it on a network with TLS
(ALK-SP-0006.R6):

```sh
alkahest certs generate --name myhost.lan   # CA + server certificate in <data_dir>/tls/
```

```toml
[server]
host = "0.0.0.0"

[server.tls]
enabled = true
# cert = "/etc/alkahest/tls/cert.pem"    # defaults: <data_dir>/tls/cert.pem and key.pem
# key = "/etc/alkahest/tls/key.pem"
# reload_interval_secs = 60             # 0 = reload on SIGHUP only
```

A renewed certificate (for example from an ACME client) is picked up on `SIGHUP` or when the
files change; open connections keep the old one. The CLI switches to `https://` by itself and
trusts the generated `ca.pem` when it shares the data directory; elsewhere set
`ALKAHEST_CA_CERT=/path/to/ca.pem`. The key file is written with mode 0600.

## Running as a service

`alkahest service install|uninstall|start|stop|status` (ALK-SP-0009.R1) targets the platform's
service manager; `--dry-run` prints the files and commands without changing anything, and
`--manager systemd|launchd|windows` with `--dry-run` or `--root <dir>` shows what another
platform would get.

| Platform | System service | `--user` |
|---|---|---|
| Linux | systemd unit, user `alkahest`, data `/var/lib/alkahest` | `~/.config/systemd/user/alkahest.service` |
| macOS | LaunchDaemon `/Library/LaunchDaemons/es.digio.alkahest.plist`, hidden user `_alkahest`, data `/Library/Application Support/Alkahest`, output `/Library/Logs/Alkahest/` | LaunchAgent `~/Library/LaunchAgents/es.digio.alkahest.plist`, output `~/Library/Logs/Alkahest/` |
| Windows | service `alkahest` (`sc.exe`, delayed auto start, `NT AUTHORITY\LocalService`, restart on failure), data `%ProgramData%\Alkahest`, events in the Application log (source `Alkahest`) | not available (services are machine-wide) |

On macOS `service stop` unloads the job (`launchctl bootout`), so `KeepAlive` does not restart it;
it loads again at the next boot or login, or with `service start`.

Engines never outlive a killed server: Linux uses the parent-death signal, Windows a job object
per engine (closing it kills the engine's process tree), macOS a small watchdog per engine that
stops the engine within about a second of the server's death. Each start also sweeps engines a
previous server left behind.
