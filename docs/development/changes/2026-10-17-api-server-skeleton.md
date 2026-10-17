# API server skeleton and `alkahest serve`

Story [[ALK-US-0033]], requirement [[ALK-SP-0006.R1]] (default bind). Plan:
[[plan/alkahest-v1-plan]].

## What changed

- New crate content `crates/alkahest-api`:
  - `state.rs`: `AppState` (cheap `Arc` clone) with `ServerInfo` and a typed service slot
    map (`AppStateBuilder::service::<T>()`, `AppState::service::<T>()`). Later stories
    register `Arc<dyn Scheduler>`, `Arc<dyn Catalog>`, metrics and so on under their own
    trait types without changing this crate.
  - `router.rs`: `ApiRouter::new(state, settings).merge(family_routes).into_router()`.
    Base routes `GET|HEAD /` (`Alkahest is running`, plain text like Ollama's probe),
    `GET /api/version` (`{"version": ...}`, Ollama shape), `GET /healthz`
    (`{"status":"ok"}`), JSON 404 fallback. Middleware, outermost first:
    sensitive-header marking (`Authorization`, `Proxy-Authorization`, `Cookie`,
    `x-api-key`), `x-request-id` generation (UUID, client value kept), tracing span
    (method, path, request id; never the query string), request id propagation, panic
    catching, optional whole-request timeout, body limit. `ApiSettings::from_config`.
  - `server.rs`: `bind(addr)` and `serve(listener, app, shutdown)`; serve attaches
    `ConnectInfo<SocketAddr>` so the auth layer can see the real peer.
  - `error.rs`: `ApiError` (`Bind`, `Serve`).
- `alkahest-core::config::ServerConfig`: `max_body_bytes` (default 64 MiB, validated
  non-zero) and `request_timeout_secs` (default 0 = off, because streamed generations run
  for minutes).
- `crates/alkahest/src/serve_cmd.rs`: `alkahest serve` binds `server.host:port` (default
  `127.0.0.1:11435`, `--host` and `ALKAHEST_HOST` honoured by the existing loader), logs to
  the rotating log file (existing `logging::init` with `file = true` for `serve`), prints
  the listening URL and serves until ctrl-c. `Serve` no longer reports "not implemented".
- Workspace: `tower-http` features `request-id`, `sensitive-headers`, `catch-panic`.

## Why

The serve MVP needs one composition point where each API family adds routes and inherits
the same cross-cutting behaviour. Extension slots on `AppState` let the scheduler and
catalog stories plug in without rewriting the server.

## Security checklist

| Threat | Mitigation | Proof |
|---|---|---|
| Accidental LAN exposure | default bind `127.0.0.1:11435` | `tests/server.rs#default_bind` |
| Credentials in logs | `Authorization`, `x-api-key`, `Cookie` marked sensitive; span records path only | code review (`router.rs#ApiRouter::into_router`, `make_span`) |
| Memory exhaustion by huge bodies | 64 MiB body limit, configurable | `tests/server.rs#oversized_bodies_are_refused` |
| Handler panic kills connection task silently | `CatchPanicLayer` returns 500 | code review |

## Verification

`scripts/ci-local.sh`: fmt, clippy `-D warnings`, nextest (335 passed, 1 skipped), cargo
deny, `xtask check-deps`, headless build, typos: all passed. New tests:
`crates/alkahest-api/tests/server.rs` (base routes, request ids, body limit, merged
routes, bind error, settings), `crates/alkahest/tests/serve.rs` (binary binds `--host`,
answers `/api/version`, writes a log file).
