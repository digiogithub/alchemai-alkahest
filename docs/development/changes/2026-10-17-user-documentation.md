# Change: user documentation (ALK-US-0080)

Implements [[ALK-US-0080]] (tasks ALK-T-0209 CLI and configuration reference, ALK-T-0210 API
compatibility and engine recipes guides, ALK-T-0211 desktop user guide). Builds on
[[2026-10-17-cli-command-tree]], [[2026-10-17-engine-selection]],
[[2026-10-17-auth-middleware-exposure]] and [[2026-10-17-desktop-library-view]]. Plan:
[[plan/alkahest-v1-plan]]. Related: [[install]], [[testing]].

## What

- mdBook at `docs/book/` (`book.toml`, sources in `docs/book/src/`; the build directory is
  `target/book`, outside `docs/`, because everything under `docs/` is indexed by the knowledge
  base). Pages: introduction, install (Linux packages, setup scripts, from source), quick start,
  models (Ollama and Hugging Face references, quantisations, Ollama store reuse, safetensors and
  vLLM, per-model settings), engines and recipes (selection rules, accelerators, CUDA runtime,
  source builds, Bonsai and the PrismML fork, overrides), serving and security (exposure rules,
  keys, audit, reverse proxy, TLS, limits, keep-alive, monitoring), configuration guide, desktop
  guide with screenshots (copied to `docs/book/src/images/`), running as a service,
  troubleshooting, FAQ, licences, API reference (Ollama, OpenAI incl. Responses, Anthropic,
  decisions, rerank, audio, images, admin) and the two generated references.
- `cargo xtask docs-cli [--check]` (`xtask/src/docs.rs`): generates
  `reference/cli.md` by running the binary's `--help` over the whole command tree (global options
  printed once) and `reference/configuration.md` from the doc comments of the structs in
  `alkahest-core` `config.rs` / `pull.rs`, defaults from `alkahest config show --json`, and the
  environment variables from `apply_env`. `--check` fails when a page is stale. Unit tests in
  `xtask/src/docs_tests.rs`.
- CI: job `docs` in `.github/workflows/ci.yml` (`docs-cli --check`, `mdbook build`);
  `scripts/ci-local.sh` runs both. README gains a short Documentation section.

## Decisions

- The CLI reference is produced from the built binary, not from a library, because the clap
  tree lives in the binary crate; it avoids a heavy dependency in xtask and cannot drift from
  what users see.
- TLS is documented as "terminate in a reverse proxy; native TLS is `ALK-SP-0006.R6`" because
  that requirement is being implemented concurrently; the generated configuration page picks the
  keys up automatically once they exist.

## Verification

`cargo xtask docs-cli --check` passes, `mdbook build docs/book` passes, a script checked that
every relative link in the book resolves, `cargo test -p xtask`, clippy `-D warnings`, `typos`.
Commands and flags quoted in the pages were run against the built binary (the quick start flow is
the `cpu` leg of [[2026-10-17-e2e-test-suite]]).
