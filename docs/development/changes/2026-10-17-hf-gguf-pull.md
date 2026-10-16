# Pull GGUF models from Hugging Face

Story [[ALK-US-0019]], requirement [[ALK-SP-0002]] R2. Builds on [[ALK-US-0018]] (Hub client),
[[ALK-US-0016]] (downloader) and the pull plumbing of [[ALK-US-0017]]. Plan: [[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest-hf/src/pull.rs`:
  - `select_files(repo, files, selector)`: pure selection. No selector: `pick_variant` default
    preference (`Q4_K_M` first, then the smallest); `:QUANT`: exact quant; file selector: the
    variant containing that file, so naming any shard takes all shards. A `mmproj` projector is
    added when the repository has one. Errors list the available quants (`HfPullError::NotFound`)
    or say the repository has no GGUF (`NoGguf`).
  - `pull_hf(transfer, client, hf_ref)`: `model_info` pins the commit sha, every file is fetched
    from `resolve/<sha>/<file>` with the token sent to the Hub host only (`bearer_for`), verified
    against its LFS sha256 and adopted by `alkahest_ollama::Transfer`. Files without an LFS sha256
    are refused (`NoDigest`). The shared store lock is held from the first blob to the manifest.
  - Manifest under `hf.co/<org>/<repo>:<quant>` (an unqualified pull is named after the quant that
    was picked): config blob (`model_format`, `file_type`, family and context from Hub GGUF
    metadata, `vision` capability when a projector exists), one `image.model` layer per shard,
    `image.projector`, an `application/vnd.alkahest.source.v1+json` layer (`SourceKind::HuggingFace`,
    reference, commit sha, file list, retrieval time) and, when the repository documents sampling
    advice, an `application/vnd.alkahest.recommended.v1+json` layer holding `RecommendedParams`.
    No template layer: the engine uses the Jinja template inside the GGUF.
  - `RECOMMENDED_MEDIA_TYPE` is defined locally with a `TODO(ALK-T-0073)`: the store exposes no
    recommended-parameters API yet.
- `crates/alkahest-hf/Cargo.toml`: now depends on `alkahest-store` and `alkahest-ollama` (for the
  shared `Transfer`, `BlobSpec`, `PullEvent`).
- `crates/alkahest/src/pull_cmd.rs`: the `hf.co/...` and `hf://...` references route to `pull_hf`
  with the same bars and `--json` events as registry pulls.

## Verification

- `crates/alkahest-hf/tests/pull.rs` (mock Hub on one server, mock CDN on another):
  `quant_selection_downloads_only_that_file_and_the_projector` (scenario "quant selection"),
  `default_pull_picks_q4_k_m_and_names_the_model_after_it`,
  `token_is_sent_to_the_hub_but_not_to_the_cdn_host`,
  `unknown_quant_is_an_error_listing_the_available_ones`,
  `gated_repository_without_token_explains_how_to_authenticate`,
  `recommended_params_are_stored_as_an_extra_layer`; unit tests for shards and fallbacks in
  `pull.rs`; opt-in `real_hub_pull_of_a_tiny_gguf` (`ALKAHEST_TEST_NET=1`).
- Manual: `alkahest --json pull hf.co/ggml-org/tiny-llamas` (19 MB) against huggingface.co
  installed `hf.co/ggml-org/tiny-llamas:Q4_0`.
- fmt, clippy `-D warnings`, nextest, `cargo deny check`, `check-deps`, headless build: green.

## Open

The ALK-T-0065 end-to-end with `prism-ml/Ternary-Bonsai-2-2B-gguf` PQ2_0 needs the real
multi-GB repository; selection of `PQ2_0` is covered by the quant table tests of
[[ALK-US-0018]], the network path by the tiny-GGUF test above.
