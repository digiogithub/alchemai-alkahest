# Change: setup scripts for Linux, macOS and Windows (ALK-US-0052)

Implements [[ALK-US-0052]] (tasks ALK-T-0149, ALK-T-0150, ALK-T-0151). Plan: [[plan/alkahest-v1-plan]].

## What

- `scripts/setup/install.sh`: detects OS/arch/GPU, installs the binary from a release archive
  (SHA-256 checked against `SHA256SUMS`, `https` and TLS 1.2 only) or `--from-local`, into
  `~/.local/bin` / `/usr/local/bin` / `--prefix` (asks before `sudo`), then runs
  `alkahest engines install llama-cpp [--variant]` and optionally `service install --user`.
  Flags: `--dry-run`, `--variant`, `--prefix`, `--no-engine`, `--service`, `--version`,
  `--base-url`, `--from-local`, `-y`. Idempotent (identical binary skipped).
- `scripts/setup/install.ps1`: Windows equivalent with the same switches in PowerShell style
  (`-DryRun`, `-Variant`, ...), user PATH update, `Get-FileHash` checksum verification.
- `scripts/setup/test-install.sh`: harness running `install.sh` in a temporary HOME with a fake
  binary (dry-run, install, idempotency, prefix, engine and service calls, bad input).
- `docs/install.md`: per-platform documentation.

## Deviations

- The story text said `scripts/setup.sh`; the task brief placed the scripts under
  `scripts/setup/install.*`. The scripts do not install compilers, cmake or CUDA: prebuilt
  engines need none, and the engine manager already reports missing source-build tools.
- Release URLs (`github.com/digio-es/alkahest/releases/...`, archive names
  `alkahest-<ver>-<target>.tar.gz|zip`, `SHA256SUMS`) are placeholders until [[ALK-US-0074]].

## Verification

`scripts/setup/test-install.sh` passes. `shellcheck` and `PSScriptAnalyzer`/`pwsh` are not
installed on the dev host, so `install.ps1` was reviewed but never executed; `bash -n` passes.
