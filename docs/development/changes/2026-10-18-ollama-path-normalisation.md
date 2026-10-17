# Serve doubled and trailing slashes like Ollama

Date: 2026-10-18. Story [[ALK-US-0125]]. Epic [[ALK-EP-0007]].

## What happened

Pando's connection test against Alkahest worked, but its Ollama model listing answered 404.
Pando has two Ollama listing paths: `models.fetchOllamaModels` uses `url.JoinPath` (clean URLs),
while `ollamasetup.ListModels` concatenates `baseURL + "/api/tags"`. With a base URL ending in
`/` (`http://127.0.0.1:11435/`) the second one requests `//api/tags`. Measured on 2026-10-18
against a local Ollama and Alkahest:

| Path | Ollama | Alkahest before |
|---|---|---|
| `//api/tags`, `//api/version`, `//v1/models` | 307 to the clean path | 404 |
| `/api/tags/`, `/v1/models/` | 301 to the clean path | 404 |
| `/v1/models`, `/api/tags`, `/api/ps`, `/api/version`, `POST /api/show` | 200 | 200 |
| `/models`, `/v1/api/tags`, `/api/models` | 404 | 404 |

## Change

`crates/alkahest-api/src/router.rs`: `normalize_path` collapses repeated slashes and drops a
trailing slash (query kept). `ApiRouter::into_router` wraps the finished router in
`Router::new().fallback_service(router.map_request(normalize_path))`, because a `Router::layer`
runs after the route match and could not change which route matches. Alkahest serves the clean
route directly instead of redirecting, which also covers clients that do not follow redirects.

## Verification

Test `crates/alkahest-api/tests/ollama.rs#doubled_and_trailing_slashes_reach_the_clean_route`.
`cargo fmt --check`, `clippy -D warnings`, `cargo nextest run --workspace` (1360 passed),
`cargo deny check`, `cargo xtask check-deps`, headless build: all OK.
