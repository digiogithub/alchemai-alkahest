# Key usage audit

Story [[ALK-US-0043]], requirement [[ALK-SP-0006.R5]]. Builds on
[[2026-10-17-auth-middleware-exposure]]. Plan: [[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest-api/src/audit.rs`:
  - `KeyUsage` (last_used, requests, prompt_tokens, completion_tokens);
    `UsageRecorder` trait (`record_request`, `record_tokens`); `UsageTracker` keeps the
    counters in memory and persists them with `flush()` (only when dirty; atomic,
    owner-only write) or `run_flusher(interval, stop)` (periodic plus a final flush).
    File: `usage_path(key_store)` = `keys.usage.json` next to the key store;
    `load_usage(path)`.
  - Why a separate file: the server is its only writer and never rewrites the key store,
    so a `keys revoke` racing with a usage update can never be lost.
  - `AuditEntry` (time, request id, key id, client, method, path without query, status,
    outcome); `AuditSink` trait; `AuditLog::start(path, max_bytes, keep)` writes JSON
    lines on a dedicated thread through a bounded queue (drops and counts when full,
    never blocks a request), rotates `audit.jsonl` -> `.1` .. `.keep`, files 0600.
- Middleware (`auth.rs#authenticate`): after a key-authenticated request, records usage
  and an `allowed` audit line; every refusal is audited with its reason (`missing`,
  `revoked`, `mismatch`, `unknown`, `malformed`, `expired`, `scope`) and the public key id
  when one was presented. Keyless loopback traffic is neither counted nor audited.
- `AppState`: `usage_recorder(..)`/`usage()` (handlers report token counts here once the
  inference stories land), `audit_sink(..)`/`audit()`.
- `alkahest-core`: `auth.audit_log` (default true).
- `alkahest serve`: `UsageTracker` on the key store, flushed every second and on exit;
  `AuditLog` at `<log_dir>/audit.jsonl` (10 MiB x 5) when `auth.audit_log`.
- `alkahest keys ls`: new columns `last_used`, `requests`, `tokens` (prompt/completion).

## Security checklist

| Threat | Mitigation | Proof |
|---|---|---|
| Key or body in audit records | entry type has no field for them; path without query | `tests/audit.rs#refusals_are_audited_without_secrets_and_local_traffic_is_not`, `last_used`; `crates/alkahest/tests/serve.rs#last_used_shows_in_keys_ls_and_the_audit_log` |
| Usage update clobbers revocation | separate server-owned usage file | design; `revoked_key_is_refused_on_the_next_request` still passes |
| Disk I/O stalls requests / log flooding DoS | background writer, bounded queue, size rotation | `audit.rs#background_writer_appends`, `audit_lines_rotate_by_size` |
| Audit/usage files readable by others | 0600 on Unix | `usage_file_is_owner_only` |
| Brute force goes unnoticed | refusals audited with reason and client | `refusals_are_audited_...` |

## Verification

`scripts/ci-local.sh`: fmt, clippy, nextest (395 passed, 1 skipped), deny, check-deps,
headless build, typos: all passed. `cargo machete` clean. Scenario test
`last_used` (library) and `last_used_shows_in_keys_ls_and_the_audit_log` (binary).
