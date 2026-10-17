# 2026-10-17 — API key from the environment, headless Docker image, `cargo desktop`

Items: [[ALK-US-0114]] (env key, security-tagged), [[ALK-US-0115]] (Docker image, alias).
Specs: [[ALK-SP-0006]] R2, R3, R4. Plan: [[plan/alkahest-v1-plan]].

## Why

Local `cargo build` produces the headless binary by design (`desktop` is off by default so the
workspace never compiles GPUI). The release pipeline already ships both flavours (desktop
`.deb`/`.AppImage`/`.rpm`/`.dmg`/NSIS and the headless tar.gz/zip), but there was no container
image, and a container cannot run `alkahest keys create` before its server starts: its key has to
come from the environment.

## What changed

- `crates/alkahest-api/src/keys/env.rs` (new): `EnvKey` reads `ALKAHEST_API_KEY` or
  `ALKAHEST_API_KEY_FILE` (Docker/Kubernetes secrets; one trailing newline dropped) and
  `ALKAHEST_API_KEY_SCOPES` (default `inference`). Any value of at least 32 characters without
  whitespace; only its SHA-256 digest is kept; comparison is constant time over digests; `Debug`
  never prints it. Both variables set, a short value, an unreadable file or a bad scope list stop
  the start. `EnvKeyVerifier` checks it before the store (`FileKeyVerifier`), reports principal
  key id `env`, and counts it as one active key for `check_exposure` while still surfacing store
  errors.
- `crates/alkahest/src/serve_cmd.rs#serve`: wraps the store verifier when the env key is set.
- `crates/alkahest-api/src/auth.rs#ExposureError::NoKeys`: message names `ALKAHEST_API_KEY`.
- `crates/alkahest-engine/src/supervisor/mod.rs#spawn` and
  `crates/alkahest-recipes/src/build.rs`: engine processes and recipe build steps start without
  `ALKAHEST_API_KEY`/`ALKAHEST_API_KEY_FILE`.
- `Dockerfile`, `.dockerignore` (new): multi-stage, `rust:1.98.1-bookworm` builds
  `-p alkahest --no-default-features --release`, installs the CPU llama.cpp through
  `alkahest engines install llama-cpp --variant cpu` (recipe SHA-256 checked) into
  `/usr/local/lib/alkahest/engines` (a `bundled_roots` location); runtime `debian:bookworm-slim`
  plus `ca-certificates` and `libgomp1`, user `alkahest` (uid 10001),
  `ALKAHEST_HOME=/var/lib/alkahest` volume, `ALKAHEST_HOST=0.0.0.0:11435`, JSON logs,
  health check `alkahest ps`.
- `.cargo/config.toml`: aliases `cargo desktop` (build with `--features desktop`) and
  `cargo desktop-run` (runs `alkahest desktop`).
- Docs: `docs/book/src/guide/docker.md` (new, in `SUMMARY.md`), "A key from the environment" in
  `guide/serving.md`, alias in `README.md`.

## Verification

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  `cargo deny check`, `cargo xtask check-deps`, `cargo xtask docs-cli --check`,
  `mdbook build docs/book`: clean.
- `cargo nextest run -p alkahest-api -p alkahest-engine -p alkahest-recipes -p alkahest`:
  558 passed, including `keys::env::tests::*` (8) and
  `tests/auth.rs#env_key_authenticates_remote_clients`.
- `cargo desktop`: binary links libxcb/libxkbcommon/libwayland/libvulkan;
  `xvfb-run alkahest desktop --smoke-test`: first frame in 749 ms.
- `docker build -t alkahest:dev .` (x86_64): 225 MB image. In the container: no key → start
  refused; `ALKAHEST_API_KEY=changeme` → refused (too short); with a 64-hex key: no key 401,
  wrong key 401, env key 200, inference key on `/alkahest/v1/settings` 403; `alkahest ps` via
  `docker exec` works without key; health `healthy`; `engines ls` shows bundled
  `llama-cpp b11669 cpu installed`; binary links no GUI libraries; runs as uid 10001.
  With the host Ollama store mounted read-only, `/v1/embeddings` on `nomic-embed-text` returned
  vectors; the `llama-server` process environment has `ALKAHEST_HOME` but no
  `ALKAHEST_API_KEY`.

## Not verified / follow-ups

- aarch64 image not built (no arm64 builder here); the Dockerfile is arch-neutral and the recipe
  has the `ubuntu-arm64` asset.
- No CI or release job builds or publishes the image yet (GHCR push is outward-facing; needs the
  product owner).
- `alkahest pull qwen3:0.6b` failed both in the container and on the host with rustls
  `CaUsedAsEndEntity` on the Cloudflare R2 blob host. Root cause: the ISP blocks the R2 IPs
  (172.64.66.1, 172.64.190.1) under the LaLiga court order and serves a self-signed
  `CN=core1.netops.test` certificate; plain HTTP shows the block notice. Not an Alkahest bug;
  rejecting the certificate is correct. Better diagnostics tracked in [[ALK-US-0116]].
- Security review by the product owner pending for [[ALK-US-0114]].
