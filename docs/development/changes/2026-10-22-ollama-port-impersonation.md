# Ollama port impersonation and configurable Alkahest port

Story [[ALK-US-0133]], requirement [[ALK-SP-0005]] R7. Plan: [[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest-core/src/config.rs`: `[server] ollama_compat` (default `false`) and
  `ollama_port` (default `DEFAULT_OLLAMA_PORT` = 11434); `ServerConfig::bind_port` and
  `ServerConfig::ollama_bind` (none when disabled or equal to `server.port`);
  `ALKAHEST_OLLAMA_COMPAT` environment override.
- `crates/alkahest-api/src/server.rs`: `Server::also_listen` adds listeners served exactly like
  the main one (same router, TLS, drain and abort); the accept loop races them with
  `futures::future::select_all` (`accept` is cancel safe). `bind_ollama_port` binds the Ollama
  port when enabled and turns a bind failure into `OllamaPortStatus::Skipped` instead of an
  error.
- `crates/alkahest-api/src/admin/{context,settings}.rs`: `AdminContext::set_ollama_port`;
  network settings carry `ollama_compat` / `ollama_port` (GET, PUT, file write) and the
  document reports `ollama_port_status` (`disabled`, `listening`, `skipped`).
- `crates/alkahest/src/serve_cmd.rs`: binds the Ollama port after the main listener, prints
  `also listening on …` or `Ollama port … is in use, not taken over (…)`, never fails on it.
- CLI (`crates/alkahest/src/{cli,main,config_set_cmd}.rs`): global `--port`;
  `serve --ollama-compat | --no-ollama-compat | --ollama-port`; new
  `alkahest config set KEY=VALUE…` / `config unset KEY…` that edit the `--config` file or the
  user file with `toml_edit` (comments kept, result validated before an atomic replace).
- Client and desktop: `alkahest_client::admin::OllamaPortStatus`, new patch fields; screen 14
  (API keys & network) gets an "Also answer on Ollama's port (11434)" switch, saved at once
  like the exposure switch, with a status line (`ollama_port_line`). The port field already
  existed there.
- Docs: `guide/serving.md` (changing the port, answering on Ollama's port); `reference/cli.md`
  and `reference/configuration.md` regenerated with `cargo xtask docs-cli`.

## Why

Users moving from Ollama want tools that hard-code `localhost:11434` to reach Alkahest without
reconfiguring them, while a still-running Ollama must not stop Alkahest from starting. Off by
default: grabbing 11434 unasked would break an Ollama started later.

## Verification

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo nextest run --workspace --locked`: 1440 passed, 1 skipped. New tests:
  `crates/alkahest/tests/serve.rs#free_ollama_port_is_served_too`,
  `#busy_ollama_port_is_skipped_without_error`, `#port_saved_with_config_set_is_bound_next_start`;
  `crates/alkahest/src/config_set_cmd.rs#tests`;
  `crates/alkahest-core/src/config.rs#ollama_bind_only_when_enabled_and_on_another_port`;
  `crates/alkahest-api/src/admin/settings.rs#ollama_compat_is_patched_and_written`;
  `crates/alkahest-desktop/src/keys.rs#ollama_compat_toggle_saves_and_explains_the_port`.
- `cargo xtask check-deps`, headless build, `cargo xtask tokens --check --contrast`,
  `alkahest desktop --smoke-test` (xvfb): OK.
- `cargo deny check`: bans, licences, sources OK; advisories fail on RUSTSEC-2026-0382/0383
  (certificate roots), which fail identically on `main` without this change.
