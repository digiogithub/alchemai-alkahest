# Binary size and startup budget (ALK-US-0078)

Implements [[ALK-US-0078]] (task ALK-T-0206). Builds on [[2026-10-17-binary-size-report]].
See [[plan/alkahest-v1-plan]].

## Numbers (Linux x86-64, headless, `--no-default-features`)

| Profile | Size |
|---|---|
| before: thin LTO, cgu 1, `strip = "debuginfo"` | 27 014 752 B (25.76 MiB) |
| thin LTO, `strip = "symbols"` | 22 340 672 B (21.3 MiB) |
| **fat LTO, cgu 1, `strip = "symbols"` (adopted)** | **21 021 344 B (20.05 MiB)** |
| fat LTO, `opt-level = "s"` (rejected) | 14 485 664 B (13.8 MiB) |

Startup on the same machine with the adopted binary (`scripts/binary-size.sh`, empty
`ALKAHEST_HOME`): `alkahest --version` median of 21 runs 1.6 ms; `alkahest serve` from spawn to
the first successful `/healthz` 40 ms (includes a 5 ms poll step). Both far inside the story's
50 ms CLI criterion.

## Decisions

- `panic = "abort"` is **not** safe: `alkahest-api` mounts tower-http `CatchPanicLayer` so a
  handler panic becomes a JSON 500 (ALK-US-0038 dialect errors); with abort one bad request
  would kill the server and its engine subprocesses. Documented next to the profile in `Cargo.toml`.
- `opt-level = "s"` saves a further 6 MiB but costs throughput in SHA-256 verification of
  multi-GB downloads, argon2 and streaming JSON; rejected. No heavy dependency was found worth
  feature-gating: the graph is already rustls-only, `rustyline` has default features off, and
  the GPUI stack is behind the `desktop` feature.
- Budgets: size 30 MiB (was 60), CLI 50 ms, serve 500 ms; all overridable by env var, enforced by
  `scripts/binary-size.sh`, which the CI `size` job and `scripts/ci-local.sh --size` run.

## Verification

`scripts/binary-size.sh` prints the three lines above and exits 0. Release build time with fat
LTO is about 2 minutes on 12 threads.
