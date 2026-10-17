# Install

Pick one of: a release package, the setup script, or a build from source. All of them give you
the same `alkahest` binary. Check it with `alkahest --version`.

## Linux packages

Tagged releases publish, next to a `SHA256SUMS` file:

| File | Content |
|---|---|
| `alkahest-<version>-<target>.tar.gz` | Headless binary (no GUI libraries) with a bundled CPU llama.cpp, systemd unit templates and licences. Targets `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`. |
| `alkahest_<version>_amd64.deb` | The desktop build (menu entry, icons), CLI and server in `/usr/bin/alkahest`. |
| `alkahest_<version>_x86_64.AppImage` | The same desktop build, self-contained. |

The packages never run maintainer scripts and create no user or service. To run the server as a
system service afterwards, ask for it explicitly: `sudo alkahest service install` (see
[Running as a service](guide/service.md)).

The bundled CPU engine is found automatically. GPU builds are installed with
`alkahest engines install llama-cpp` (see [Engines and recipes](guide/engines.md)); they take
precedence over the bundled copy.

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
