# Change: memory estimate of a Hub quant before download, more KV cache types (ALK-US-0104)

Implements [[ALK-US-0104]] ([[ALK-SP-0004]].R3 estimate, [[ALK-SP-0005]].R6) for the
Discover screen's fit hints ([[ALK-US-0067]]). Builds on [[2026-10-17-memory-estimator]],
[[2026-10-17-estimator-hybrid-encoder-gpu]] and [[2026-10-17-hf-hub-client]]. Plan:
[[plan/alkahest-v1-plan]].

## What

- `alkahest-hf/src/remote_gguf.rs` (new): `HfClient::file_range(repo, revision, file,
  start, len)` (HTTP `Range`, `Accept-Encoding: identity`, token only to the Hub host as
  before; a server that ignores `Range` is read only up to the needed prefix),
  `HfClient::gguf_header` (first 2 MiB, growing fourfold while the GGUF parser hits the end
  of the bytes, up to 128 MiB) and `HfClient::gguf_header_split` (every shard's tensor table
  merged into the first shard's header). New `HfError::InvalidGguf`. `alkahest-gguf` moved
  from dev- to normal dependency of `alkahest-hf`.
- `alkahest-api`: `GET /alkahest/v1/hf/estimate?repo=&file=|quant=&revision=&context_length=
  &num_parallel=&kv_cache_type_k=&kv_cache_type_v=&flash_attention=&gpu_layers=` resolves
  the variant (file name, quant, or the default preference), reads the header remotely,
  counts the repository's projector as a pull would, and answers the local estimate's shape
  plus `files` and `download_bytes`. The estimate body of
  `GET /alkahest/v1/models/{name}/estimate` became `admin::models::estimate_document`, shared
  by both routes.
- `alkahest-sched`: `KvCacheType` covers every value llama-server's `--cache-type-k/v`
  accepts: `f32` (32 bits), `f16`, `bf16` (16), `q8_0` (8.5), `q5_1` (6), `q5_0` (5.5),
  `q4_1` (5), `q4_0`, `iq4_nl` (4.5); `KvCacheType::ALL`, `as_str`. The settings validation
  uses `parse` alone now, so every accepted type is also estimable.
- `alkahest-client` (new file `hf_estimate.rs`): `HfQuant`, `Client::hf_estimate`.

## Verification

- `crates/alkahest-hf/tests/remote_gguf.rs`: growing ranges against a range-honouring mock
  (two requests for a header larger than the first range), a server ignoring `Range`, split
  shards merged, invalid and missing files; `live_hub_header` (`ALKAHEST_TEST_NET=1`,
  `ggml-org/tiny-llamas/stories260K.gguf`): passed.
- `crates/alkahest-api/tests/admin_api.rs#hf_estimate_reads_the_remote_header` (quant and
  file selection, q5_1 / iq4_nl caches, 404s, bad repo and cache type 400).
- `crates/alkahest-sched/tests/memory.rs#every_llama_server_cache_type_is_estimable`.
- `crates/alkahest-client/tests/client.rs#hf_estimate_sends_the_quant_and_overrides`.
- fmt, clippy `-D warnings`, nextest for sched, hf, api, client: passed.
