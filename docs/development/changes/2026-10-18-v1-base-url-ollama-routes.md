# Ollama routes reachable from a `/v1` base URL

Date: 2026-10-18. Related: [[2026-10-18-ollama-path-normalisation]], [[ALK-SP-0007]].

## What

`normalize_path` (`crates/alkahest-api/src/router.rs`) now drops a leading `/v1` when the next
segment is `api` or `v1`: `/v1/api/version` reaches `/api/version`, `/v1/api/tags` reaches
`/api/tags` and `/v1/v1/systemone` reaches `/v1/systemone`.

## Why

Pando's decision (Jev) provider of kind `ollama` probes `base + "/api/version"`, requires a
version of at least 0.35, then reads `/api/tags` capabilities and posts `/v1/systemone`. With
the OpenAI-compatible base URL (`http://127.0.0.1:11435/v1`) pasted in, every probe hit a 404.
`/api/version` already answers the Alkahest version (`1.0.1`), which passes the 0.35 check.

## Verification

- New test `a_v1_base_url_reaches_the_ollama_routes` in `crates/alkahest-api/tests/ollama.rs`.
- `cargo nextest run -p alkahest-api --locked`: 314 passed. Clippy and fmt clean.
