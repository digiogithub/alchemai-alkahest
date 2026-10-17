# Auth and shutdown hardening follow-ups

Story [[ALK-US-0084]]; requirements [[ALK-SP-0006.R3]], [[ALK-SP-0006.R4]], [[ALK-SP-0009.R4]].
Closes the open points of [[2026-10-17-api-key-store]], [[2026-10-17-auth-middleware-exposure]]
and [[2026-10-17-graceful-shutdown]]; builds on [[2026-10-17-optional-tls]] (the accept loop).
Plan: [[plan/alkahest-v1-plan]].

## What changed

1. **Secrets wiped from memory** (`zeroize =1.9.1`, pinned with ALK-US-0044):
   - `alkahest-api::keys::token`: `ApiKeySecret(Zeroizing<String>)`; the random bytes and the
     secret part are generated into `Zeroizing` buffers and the key is assembled in place (no
     intermediate `format!` copy); presented keys parsed by the verifier are wrapped the same way.
   - `alkahest-hf::Token(Zeroizing<String>)` (HF token from env, keyring or admin API).
   - `alkahest-client::Client`: the API key is a `Zeroizing<String>` and `Debug` no longer prints
     it (it did: `#[derive(Debug)]` included `api_key`).
   - TLS private key bytes were already read into `Zeroizing` buffers (ALK-US-0044).
2. **Owner-only files on Windows** (`alkahest-api::private_file::create_new`): Unix keeps mode
   0600; Windows opens the new file with no sharing and `FILE_GENERIC_WRITE | WRITE_DAC`, then
   sets a protected DACL with a single full-control entry for the token user
   (`OpenProcessToken`/`GetTokenInformation(TokenUser)`/`SetEntriesInAclW`/`SetSecurityInfo`)
   before writing anything; on failure the file is removed. Used by the key store and usage file
   (`write_private_atomic`), a newly created audit log, and the generated TLS key. This matters
   for the Windows service, whose data directory is under `C:\ProgramData`, where children
   inherit read access for all local users.
3. **Second stop signal forces the stop** (`alkahest serve`): `alkahest-api::StopSignals`
   (repeatable SIGINT/SIGTERM, Windows console events; `stop_signal()` now built on it). First
   signal: graceful drain, stderr hint "send it again to stop now". Second: `Server::force_stop`
   fires: open connections are aborted at once and hooks run with `FORCED_HOOK_TIMEOUT` (3 s);
   engines are still torn down. Third: immediate `exit(130)`.
4. **Connections aborted inside the library after the drain timeout**: landed with the accept
   loop of ALK-US-0044 (`JoinSet::abort_all`, `ShutdownReport.aborted`); now covered by tests.
5. **Admin route classification confirmed**: every entry of `admin::ROUTES` (the final
   ALK-US-0040 table, 31 routes incl. `GET /metrics`) and the Ollama/OpenAI management routes
   need the admin scope; inference and listing routes do not. `HEAD /api/blobs/*` stays admin
   (it is the existence check of the `ollama create` upload workflow). No code change was needed.

## Security checklist

| Threat | Mitigation | Proof |
|---|---|---|
| Key secrets or HF token left in freed memory (core dumps, swap) | `Zeroizing` buffers for generation, parsing, client and HF token | code review (`token.rs`, `alkahest-hf/src/token.rs`, client) |
| API key printed in logs via `Debug` of the client | manual `Debug` with `<redacted>` | `alkahest-client` `tests::debug_never_prints_the_api_key` |
| Key store / usage / audit log readable by other Windows users | protected owner-only DACL set on a no-share handle before writing | Windows clippy; code review (`private_file.rs`); Unix: `private_file::tests::created_files_are_owner_only_and_never_reused` |
| Stuck client keeps the server from stopping | drain timeout aborts connections; second signal aborts at once | `tests/shutdown.rs#drain_timeout_bounds_the_wait_and_hooks_still_run` (aborted = 1, client sees the close), `#forced_stop_aborts_connections_at_once`, `crates/alkahest/tests/serve.rs#second_stop_signal_forces_the_stop` |
| Forced stop leaves engines behind | hooks still run (3 s budget each); engines also die with the server (job object, watchdog, parent-death signal) | `forced_stop_aborts_connections_at_once` (hook completed) |
| Admin route reachable with an inference key | middleware classification checked against the full route table | `tests/auth.rs#every_admin_route_requires_the_admin_scope` |

Known limits: the presented key also lives in the HTTP request buffers (hyper `HeaderValue`),
which cannot be wiped; `sha2`'s internal state is not zeroized. The Windows ACL code is compiled
and linted for `x86_64-pc-windows-gnullvm` but not run on Windows.

## Verification

`scripts/ci-local.sh` passed (nextest 1030 passed, 1 skipped; clippy, deny, check-deps, notice,
headless build, typos). `cargo machete` clean for `alkahest-api`, `alkahest-client`,
`alkahest-hf`. `cargo clippy --target x86_64-pc-windows-gnullvm -p alkahest -p alkahest-api -p
alkahest-engine -p alkahest-client -p alkahest-hf -- -D warnings` passes.
