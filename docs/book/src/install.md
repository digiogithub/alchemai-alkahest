# Install

Pick one of: a release package, the setup script, or a build from source. All of them give you
the same `alkahest` binary. Check it with `alkahest --version`.

## Release packages

Tagged releases publish, next to a `SHA256SUMS` file:

| File | Content |
|---|---|
| `alkahest-<version>-<target>.tar.gz` | Headless binary (no GUI libraries), bundled CPU llama.cpp, systemd unit templates (Linux) and licences. Targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin`. |
| `alkahest-<version>-x86_64-pc-windows-msvc.zip` | The same for Windows (`alkahest.exe`). |
| `alkahest_<version>_amd64.deb` | Desktop build (menu entry, icons); CLI and server in `/usr/bin/alkahest`. |
| `alkahest-<version>-<release>.x86_64.rpm` | The same layout for Fedora, RHEL and openSUSE. |
| `alkahest-headless-<version>-<release>.<arch>.rpm` | CLI and server only, no GUI libraries (`x86_64`, `aarch64`). **Conflicts with `alkahest`**: install one of the two. |
| `alkahest_<version>_x86_64.AppImage` | The desktop build, self-contained. |
| `Alkahest_<version>_aarch64.dmg`, `Alkahest_<version>_x64.dmg` | macOS app (Apple Silicon and Intel, macOS 12+). |
| `alkahest_<version>_x64-setup.exe` | Windows installer (NSIS). |
| `alkahest.rb` | Homebrew formula for the CLI, rendered at release time. |

The Linux packages never run maintainer scripts and create no user or service. To run the
server as a system service afterwards, ask for it explicitly: `sudo alkahest service install`
(see [Running as a service](guide/service.md)). The bundled CPU engine is found automatically;
GPU builds installed with `alkahest engines install llama-cpp` take precedence over it.

### macOS

Open the `.dmg` and drag `Alkahest.app` to Applications. Opening the app starts the desktop (Dock
icon plus a menu bar icon); its bundled llama.cpp (Metal on Apple Silicon) lives inside the app.
The command line is inside the bundle:
`/Applications/Alkahest.app/Contents/MacOS/alkahest`, or link it:

```sh
ln -s /Applications/Alkahest.app/Contents/MacOS/alkahest /usr/local/bin/alkahest
```

Releases are signed and notarised when the maintainers' Apple credentials are configured; an
unsigned build (a fork, a release candidate) needs right-click, Open the first time.

For the CLI alone, use Homebrew with the formula attached to the release:
`brew install digiogithub/tap/alkahest`, then `brew services start alkahest` to run the server at
login.

### Windows

Run `alkahest_<version>_x64-setup.exe` as administrator (it installs for all users into
`C:\Program Files\Alkahest`). The components page offers:

| Component | Default | What it does |
|---|---|---|
| Alkahest | required | `alkahest.exe`, bundled CPU llama.cpp, licences, Start menu shortcut |
| Add to PATH | on | adds the install folder to the machine `PATH` (open a new terminal) |
| Windows service | off | runs `alkahest service install` (service `alkahest`, started at boot) |
| CUDA 12 engine | off | downloads llama.cpp for CUDA 12 with its runtime DLLs (about 650 MB) |

Silent install:

```text
alkahest_<version>_x64-setup.exe /S [/SERVICE] [/CUDA] [/NOPATH] [/D=C:\dir]
```

`/S` is silent, `/SERVICE` installs the service, `/CUDA` the CUDA engine, `/NOPATH` skips the PATH
entry and `/D=` sets the folder (it must be the last argument). The uninstaller removes the
service, the PATH entry and the files; models and settings are kept. Upgrading stops the service,
replaces the files and starts it again.

## Setup scripts

`scripts/setup/install.sh` (Linux, macOS) and `scripts/setup/install.ps1` (Windows) detect the
platform and GPU, install the binary, put it on `PATH`, install the recommended llama.cpp build
and can install the per-user service.

```sh
scripts/setup/install.sh --dry-run          # show the plan, change nothing
scripts/setup/install.sh --service          # install, then run `alkahest service install --user`
scripts/setup/install.sh --variant vulkan   # force an engine build
```

| Option | Effect |
|---|---|
| `--dry-run` | print every step; change and download nothing |
| `--variant NAME` | engine build (`cpu`, `cuda12.8`, `vulkan`, `rocm`, `metal`, ...) |
| `--prefix DIR` | install into `DIR/bin`; default `~/.local`, or `/usr/local` as root |
| `--no-engine` | skip the engine install |
| `--service` | install the per-user service |
| `--version`, `--base-url`, `--from-local` | where the binary comes from |

The scripts verify the SHA-256 of everything they download against `SHA256SUMS` before using it.
Download them, read them, then run them; do not pipe them into a shell from an untrusted source.
On Windows the parameters have the same names in PowerShell style (`-DryRun`, `-Variant`,
`-Prefix`, `-NoEngine`, `-Service`, `-FromLocal`); no administrator rights are needed.

## From source

You need the Rust toolchain pinned in `rust-toolchain.toml` (rustup installs it for you).

```sh
cargo build --release -p alkahest --no-default-features   # headless: CLI and server
cargo build --release -p alkahest --features desktop      # with the desktop app
```

The headless build links no GPUI, X11, Wayland or Vulkan libraries. The desktop build needs the
usual X11 or Wayland and Vulkan development packages on Linux.

Then install an engine (the headless build does not bundle one):

```sh
target/release/alkahest engines install llama-cpp
```

## Uninstall

Remove the binary, then the data directory if you want to delete downloaded models and engines
(`alkahest config path` and `alkahest config show` print where everything lives). An Ollama
store is never touched.
