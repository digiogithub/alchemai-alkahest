# Recommended parameters from Hugging Face model cards

Implements [[ALK-US-0022]] / [[ALK-SP-0010]].R3 (T-0071, T-0072). Plan: [[plan/alkahest-v1-plan]].

## What

- `alkahest-hf::recommended`: `RecommendedParams` (serde) with `SamplingParams` (temperature,
  top_p, top_k, min_p, repeat/presence penalty), separate `thinking` / `non_thinking` sets,
  `context_length`, `max_tokens`, `kv_cache_type_k/v`, `chat_template_kwargs`, `sources`.
  Constructors: `from_generation_config(&Value)`, `from_card(&ModelCard)`,
  `with_trained_context(Option<u64>)`, `over(&weaker)` and `combine(gen_cfg, card)`.
  Precedence: card general advice > `generation_config.json` > card thinking set > trained
  context. Card extraction is line based: prose `Key=value` outside code fences, llama.cpp flags
  anywhere (`--temp`, `--cache-type-k`, `--chat-template-kwargs`), range-checked values.
- `HfClient::generation_config(repo, rev)` (None on 404) and
  `HfClient::recommended_params(repo, rev)`: own files, trained context from `gguf.context_length`,
  and one hop to `base_model` when the repo itself yields nothing (GGUF conversions).
- Fixtures: Qwen3 "Best Practices" card excerpt, Llama 3.1 generation config. The Bonsai
  (prism-ml) card shape is unknown, so no fixture for it.

## Not done

ALK-T-0073 (persist as the recommended-defaults layer) needs `alkahest-store`; filed in the
inbox. The struct is plain serde JSON, ready to be stored.

## Verification

Unit tests in `recommended.rs`, wiremock tests in `crates/alkahest-hf/tests/recommended.rs`
(scenario "sampling defaults"); clippy, fmt, nextest green.
