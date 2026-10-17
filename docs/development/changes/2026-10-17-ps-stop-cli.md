# Change: `alkahest ps`, `alkahest stop` and the typed API client (ALK-US-0031)

Implements [[ALK-US-0031]] / [[ALK-SP-0004]].R5 (CLI side) and R7 (`alkahest ps`, `/api/ps`).
Builds on [[2026-10-17-ollama-native-api]] and [[2026-10-17-scheduler-runtime-metrics]].
Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-client` (reqwest, used by the CLI now and by the desktop app and `run`
later):

- `Client::new(base_url)`, `Client::for_bind(host, port)` (wildcard binds reached through
  loopback), `with_api_key(Option<String>)` (Bearer), `base_url()`.
- `version() -> Version`, `tags() -> Tags`, `show(model) -> ShowResponse`,
  `ps() -> Running { models: Vec<RunningModel> }`, `stop(model)` (Ollama convention:
  `/api/generate` without prompt and `keep_alive: 0`, works with inference keys),
  `generate(&GenerateRequest) -> JsonStream<GenerateChunk>`,
  `chat(&ChatRequest) -> JsonStream<ChatChunk>` (NDJSON decoded incrementally; an
  `{"error"}` line ends the stream with `ClientError::Api`).
- `ClientError::{NotRunning { url }, Api { status, message }, Http, Decode, InvalidUrl}`;
  a refused connection is `NotRunning` ("start it with `alkahest serve`"). Error bodies of
  every dialect are read.
- Lenient response types (`#[serde(default)]`), `Message`, `Stats`, constants
  `DEFAULT_PORT`, `API_KEY_ENV` (`ALKAHEST_API_KEY`).

Binary, `ps_cmd.rs`:

- `alkahest ps`: NAME, ID, SIZE, PROCESSOR, CONTEXT, ENGINE, QUEUE (`active/queued`),
  UNTIL (`4 minutes from now`, `Forever`, `Stopping...`, `Loading...`); `--json` prints the
  `/api/ps` rows. Server address from configuration / `ALKAHEST_HOST` / `--host`, API key
  from `ALKAHEST_API_KEY`.
- `alkahest stop <model>`: checks the model is loaded (`llama3.2` matches
  `llama3.2:latest`), else exits 1 with "model `x` is not running"; then unloads it.

## Verification

`crates/alkahest-client/tests/client.rs` (not running, ps/version/stop with a key, API
errors, chat NDJSON stream) and unit tests; `crates/alkahest/tests/ps.rs` (no server ->
clear message, empty server -> header with every column, `--json` -> `[]`, stop of an idle
model refused); unit tests of rows and UNTIL wording in `ps_cmd.rs`. Scenarios `ps` and
`stop` with a real engine in `crates/alkahest/tests/serve_inference.rs` (opt-in): the
loaded model is listed with its processor split and deadline, `stop` unloads it and `ps`
no longer lists it: passed. `scripts/ci-local.sh` passed.
