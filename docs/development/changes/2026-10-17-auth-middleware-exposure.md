# Authentication middleware and exposure policy

Story [[ALK-US-0042]], requirements [[ALK-SP-0006.R1]], [[ALK-SP-0006.R2]],
[[ALK-SP-0006.R4]], [[ALK-SP-0006.R7]]. Builds on [[2026-10-17-api-key-store]].
Plan: [[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest-api/src/dialect.rs`: `Family` (`Base`, `Ollama` `/api/*`, `OpenAi`
  `/v1/*`, `Anthropic` `/v1/messages`, `Decisions` `/v1/systemone|decisions`, `Admin`
  `/alkahest/*`, `Other`) with `of_path` and `enabled(&EndpointsConfig)`;
  `requires_admin(method, path)` (admin API, `/api/pull|push|create|copy|delete`,
  non-GET `/api/blobs/*`, `DELETE /v1/models/*`); `ErrorKind` (401/403/404);
  `ErrorRenderer` trait, the hook for ALK-US-0038; `DefaultErrorRenderer` already emits
  the Ollama, OpenAI and Anthropic error shapes and `WWW-Authenticate: Bearer` on 401.
- `crates/alkahest-api/src/auth.rs`:
  - `AuthPolicy::from_config` and `resolve_client(peer, headers)`: client = socket peer
    (canonicalised, so `::ffff:127.0.0.1` is loopback); `X-Forwarded-For` only from
    `auth.trusted_proxies`, walked right to left; a loopback peer sending
    `X-Forwarded-For`/`Forwarded` without being trusted loses loopback trust (local
    reverse proxy case).
  - `authenticate` middleware: disabled family -> 404 in dialect (R7); `/` and `/healthz`
    open; loopback needs no key unless `auth.require_on_loopback` (R1); remote needs a
    valid key in `Authorization: Bearer` or `x-api-key` unless
    `auth.require_key_for_remote = false` (R2); an Alkahest-shaped key is always verified,
    foreign placeholders (`Bearer ollama`) from callers that need no key are ignored;
    admin routes need `admin` scope for non-loopback callers, and on loopback once keys are
    required there (R4). Rejections log client, family, status, reason and the public key
    id, never the secret; clients get one generic message so there is no oracle.
  - `RequestContext { client, local, family, caller: Caller::{Local, Anonymous, Key} }`
    inserted into request extensions for handlers (model allowlist, usage accounting).
  - `check_exposure(bind, loopback_bind, policy, verifier, allow_unauthenticated)` ->
    `Exposure::{LoopbackOnly, KeyProtected, Unprotected}` or `ExposureError`: a
    non-loopback bind is refused when remote keys are disabled or no active key exists,
    unless `alkahest serve --allow-unauthenticated`.
- `AppState`: `key_verifier(Arc<dyn KeyVerifier>)`, `error_renderer(Arc<dyn
  ErrorRenderer>)`; no verifier = every key refused (fail closed).
- `ApiRouter`: auth layer inside tracing and CORS; CORS only when
  `server.cors_origins` is non-empty (`*` allowed), outside auth so preflights work.
- `alkahest-core` config: `auth.require_on_loopback` (false), `auth.trusted_proxies`
  (empty), `server.cors_origins` (empty), `[server.endpoints]` `ollama|openai|anthropic|
  decisions` (all true).
- `alkahest serve`: builds a `FileKeyVerifier` on the key store, runs the exposure check
  before binding, prints the exposure mode; new flag `--allow-unauthenticated`.

## Security checklist

| Threat | Mitigation | Proof (`crates/alkahest-api/tests/auth.rs` unless noted) |
|---|---|---|
| LAN client without key | 401 in dialect | `missing_key` (scenario R2) |
| Spoofed `X-Forwarded-For: 127.0.0.1` | header ignored unless peer is a trusted proxy | `spoofed_forwarded_for_does_not_grant_loopback_trust`, `auth.rs#spoofed_forwarding_headers_are_ignored_or_downgrade` |
| Local reverse proxy turns LAN traffic into loopback | forwarding headers from untrusted loopback peer -> remote | same tests; `trusted_proxy_forwards_the_real_client` |
| IPv6 / IPv4-mapped loopback misdetected | `to_canonical().is_loopback()` | `loopback_needs_no_key`, `ipv4_and_ipv6_loopback_and_mapped_addresses_are_local` |
| Revoked key keeps working | verifier reloads on file change | `revoked_key_is_refused_on_the_next_request` |
| Oracle on wrong vs unknown key | identical 401 body; constant-time compare + dummy hash | `wrong_and_unknown_keys_get_the_same_answer` |
| Inference key performs admin actions | admin scope check | `inference_key` (scenario R4), `opted_out_remote_access_is_anonymous_but_never_admin` |
| Missing verifier silently admits | fail closed | `no_verifier_fails_closed` |
| Accidental unauthenticated network exposure | startup refusal unless `--allow-unauthenticated` | `auth.rs#startup_refusal`; `crates/alkahest/tests/serve.rs#startup_refusal_*` |
| Cross-site browser calls | CORS off by default, allowlist only | `cors_is_off_by_default_and_follows_configured_origins` |
| Disabled family still reachable | 404 before auth | `disabled_family` (scenario R7) |

Open points for product owner review: `/api/version` is authenticated for remote callers
(it belongs to the Ollama family); `/` and `/healthz` stay open. TLS (R6) is not part of
this story.

## Verification

`scripts/ci-local.sh`: fmt, clippy, nextest (386 passed, 1 skipped), deny, check-deps,
headless build, typos: all passed. `cargo machete` clean.
