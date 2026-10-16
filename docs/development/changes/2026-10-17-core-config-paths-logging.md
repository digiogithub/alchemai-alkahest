# Change: configuration, paths and logging in alkahest-core

Story [[ALK-US-0003]], requirement [[ALK-SP-0010]] R4 (global configuration is the last link of
the request > model config > recommended defaults > global chain). Plan: [[plan/alkahest-v1-plan]].

## What

`alkahest-core` now provides:

- `config`: `Config` (sections `server`, `store`, `scheduler`, `engines`, `auth`, `hf`,
  `logging`), layered loading (defaults, system file, user file, explicit file, `ALKAHEST_*`
  env), `Loaded` with the contributing `Source`s, `Config::resolve_paths`.
- `paths`: `AlkahestPaths` (directories crate, `ALKAHEST_HOME` relocation),
  `detect_ollama_store` (honours `OLLAMA_MODELS`, read-only use).
- `duration`: `KeepAlive` with Ollama semantics (`5m`, `1h30m`, bare seconds, negative = forever,
  `0` = unload now).
- `logging`: `init`/`build` with stderr plus rotating file appender, `LogOptions` for
  `-v` and `--log-level`.
- `error`: `Error`/`Result`.

New pinned dependency: `tracing-appender =0.2.5` (MIT).

## Decisions

- Unknown config keys are rejected (typo detection); each file is validated on its own so errors
  name the file.
- `hf.token` in a file is refused with `Error::SecretInConfig`; the token comes from `HF_TOKEN`
  (keyring lookup is added by the Hugging Face client story).
- Env access is injectable (`EnvFn`) so tests never touch the process environment.

## Verification

fmt, clippy `-D warnings`, nextest (36 tests), `cargo deny check`, `cargo xtask check-deps`,
headless build: all pass. Tests in `crates/alkahest-core/src/{config,duration,paths,logging}.rs`.
