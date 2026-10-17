# Change: `alkahest run` interactive chat (ALK-US-0039)

Implements [[ALK-US-0039]] (tasks ALK-T-0119, ALK-T-0120). Builds on
[[2026-10-17-ps-stop-cli]] (typed client) and [[2026-10-17-ollama-native-api]].
Plan: [[plan/alkahest-v1-plan]].

## What

Binary `alkahest`, `run_cmd.rs` (`Command::Run(RunArgs)` in `cli.rs`):

- `alkahest run <model> [prompt...]` with `--format json|<schema>`, `--think[=WHEN]`
  (`true`/`false`/`high`/`medium`/`low`), `--hidethinking`, `--keepalive DURATION`,
  `--no-start`, and `--verbose` meaning timing statistics for this command (Ollama's
  meaning; logging is not raised for `run`, so debug lines never interleave with the answer).
- One-shot when a prompt is given or stdin is piped (piped text first, then the words,
  joined with a space, as Ollama does): `/api/generate` streamed to stdout.
- Interactive otherwise: rustyline editor with history in `<data_dir>/history`, the model
  preloaded (`Client::load`), the conversation kept and sent to `/api/chat`. Commands
  `/bye` (`/exit`), `/clear`, `/set parameter <name> <value>` (numbers and booleans typed,
  `stop` accumulates), `/set system`, `/set format json` / `noformat`, `/set think
  [level]` / `nothink`, `/set verbose` / `quiet`, `/show
  info|parameters|system|template|license|modelfile`, `/load <model>`, `/?`. `"""` opens
  and closes a multi-line message. Ctrl+C stops a generation (the partial answer is kept)
  or clears the line; Ctrl+D leaves.
- Thinking streams to stderr between `Thinking...` and `...done thinking.`, so stdout
  carries only the answer; `--verbose` prints Ollama's statistics block to stderr.
- Server not running: the command starts nothing by itself. It prints where it looked and
  how to start the server; on a terminal (and without `--no-start`) it offers to start
  `alkahest serve` in the background and waits for it. Unknown model: "pull it first with
  `alkahest pull <model>`"; embedding-only models are refused with an explanation.

Crate `alkahest-client`: `Client::load(model, keep_alive)` (empty `/api/generate`, the
Ollama load convention). Workspace dependency `rustyline = "=18.0.1"` (MIT, default
features off, `with-file-history`).

## Verification

- Unit tests in `crates/alkahest/src/run_cmd.rs`: multi-line blocks, value parsing,
  thinking vs answer streams, and a scripted REPL session against a mock server (options,
  `stop` list, system message, thinking, history across turns, `"""` block, unknown command,
  `/clear`, `/bye` stops reading, statistics).
- `crates/alkahest/tests/run.rs`: one-shot with every flag, piped stdin (with and without
  words), missing and embedding models, stopped server (guidance, exit 1, nothing started).
- `crates/alkahest-client/tests/client.rs#load_sends_an_empty_generate_with_keep_alive`.
- Real engine (llama.cpp b11669 CPU, `qwen2.5-coder:0.5b` from the read-only Ollama
  store): one-shot with `--verbose --keepalive 1m`, and an interactive session driven
  through a pseudo-terminal (history remembered across turns, `/set parameter`, `"""`
  block, `/show info`, `/bye`).
- `scripts/ci-local.sh` (fmt, clippy, nextest 695 passed, deny, check-deps, headless
  build, typos) and `cargo machete`.
