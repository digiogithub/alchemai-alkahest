# 2026-10-17 — Cargo workspace, toolchain and lint configuration

Story [[ALK-US-0001]] (tasks ALK-T-0001, ALK-T-0002, ALK-T-0003, ALK-T-0004,
ALK-T-0214). Plan: [[plan/alkahest-v1-plan]], orchestration
[[development/plans/orchestration-v1]].

## What

- `Cargo.toml`: workspace (`resolver = "3"`, edition 2024, `rust-version = "1.98"`),
  `[workspace.package]` (version 0.1.0, `license = "MIT"`, authors Digio, placeholder
  repository URL), `[workspace.lints]` (rust: `unsafe_code = "deny"`, `missing_docs` and
  `missing_debug_implementations` warn; clippy: `unwrap_used`, `expect_used`, `dbg_macro`
  deny, `todo` warn), release profile, and `[workspace.dependencies]` with every internal
  crate by path plus exact `=x.y.z` pins of the third-party crates later stories need.
- Crates (all `[lints] workspace = true`, each lib with a crate-level doc comment stating
  its responsibility): `alkahest` (bin; features `default = []`,
  `desktop = ["dep:alkahest-desktop"]`; `alkahest --version` via clap), `alkahest-core`,
  `alkahest-gguf`, `alkahest-sysinfo`, `alkahest-store`, `alkahest-fetch`,
  `alkahest-ollama`, `alkahest-hf`, `alkahest-engine`, `alkahest-recipes`,
  `alkahest-sched`, `alkahest-api`, `alkahest-decision`, `alkahest-client`,
  `alkahest-desktop` (headless skeleton; gpui-kit arrives with ALK-US-0064), `xtask`.
- `xtask`: `check-deps` implemented (`xtask/src/deps.rs#check_graph`,
  `#scan_sources`): fails when a workspace crate other than `alkahest-desktop` declares or
  transitively reaches `gpui`/`gpui-kit` (paths through `alkahest-desktop` are allowed), or
  when a `.rs` file outside `crates/alkahest-desktop/src/ui/` names the gpui-kit facade.
  `tokens`, `icons`, `notice`, `bundle` print "not implemented yet" and exit 0.
- Config: `rust-toolchain.toml` (1.98.1, rustfmt, clippy), `rustfmt.toml`, `clippy.toml`
  (unwrap/expect/dbg allowed in tests), `deny.toml` (permissive licences only, no
  GPL/LGPL/AGPL; MPL-2.0 allowed for `option-ext` via `directories`; `openssl-sys` and
  `native-tls` banned), `typos.toml`, `.cargo/config.toml` (`cargo xtask` alias),
  `.config/nextest.toml` (`ci` profile, JUnit at `target/nextest/ci/junit.xml`).
- `LICENSE` (MIT, 2026 Digio), `NOTICE` skeleton, `README.md`, `.gitignore`
  (`/.claude/worktrees/` added), `Cargo.lock` committed.

## Pinned third-party versions

tokio 1.53.3, tokio-util 0.7.20, futures 0.3.34, async-stream 0.3.6, bytes 1.12.1,
axum 0.8.9, tower 0.5.3, tower-http 0.7.1, hyper 1.12.0, reqwest 0.13.5 (rustls, no
default features), url 2.5.8, utoipa 6.0.0, serde 1.0.229, serde_json 1.0.154,
toml 1.1.8, directories 6.0.0, thiserror 2.0.21, anyhow 1.0.104, clap 4.6.7,
tracing 0.1.44, tracing-subscriber 0.3.23, indicatif 0.18.6, sha2 0.11.1, hex 0.4.3,
uuid 1.28.0, time 0.3.55, regex 1.13.1, semver 1.0.28, bytesize 2.7.0, keyring 4.2.0,
argon2 0.6.0, rand 0.10.3, sysinfo 0.39.6, libloading 0.9.0, memmap2 0.9.11,
byteorder 1.5.0, walkdir 2.5.0, parking_lot 0.12.5, dashmap 6.1.0 (7.x is still a release
candidate), cargo_metadata 0.23.1; dev: tempfile 3.27.0, insta 1.49.0, wiremock 0.6.5,
assert_cmd 2.2.2, predicates 3.1.4, proptest 1.12.0.

All pins, with the features declared in the workspace, were compiled together
(`cargo check --all-targets` on toolchain 1.98.1) in a scratch crate and passed
`cargo deny check` there, so later stories can opt in without licence surprises.

## Why

Every later story depends on the workspace; pinning shared dependencies up front avoids
merge conflicts between parallel story branches (orchestration plan, wave 0). The repo
stays plain git as AGENTS.md section 8 states (ALK-T-0004 mentioned jj; not initialised).

## Verification

```
cargo fmt --all --check                                   OK
cargo clippy --workspace --all-targets --locked -- -D warnings   OK
cargo nextest run --workspace --locked --profile ci       8 passed (JUnit written)
cargo deny check                                          advisories ok, bans ok, licenses ok, sources ok
cargo xtask check-deps                                    OK (16 workspace crates)
cargo build -p alkahest --no-default-features --locked    OK; `alkahest --version` -> alkahest 0.1.0
cargo build -p alkahest --features desktop --locked       OK
typos                                                     OK
```
