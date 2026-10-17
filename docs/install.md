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
