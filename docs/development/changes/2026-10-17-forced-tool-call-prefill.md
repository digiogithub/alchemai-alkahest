# Change: forced tool calls parse on small Hermes-style models (ALK-US-0096)

Fixes [[ALK-US-0096]] ([[ALK-SP-0005]].R2, R3). Decision and investigation:
[[development/decisions/0005-forced-tool-call-prefill]]. Builds on
[[2026-10-17-openai-api]], [[2026-10-17-anthropic-messages-api]],
[[2026-10-17-openai-responses-api]]. Plan: [[plan/alkahest-v1-plan]].

## What

- `crates/alkahest-api/src/tool_prefill.rs` (new): `is_hermes_json_template`,
  `forced_choice` (`Forced::{No, Any, Named}` from an OpenAI body), `apply_with_template`
  (assistant prefill `<tool_call>\n`, named choice narrowed to that tool + `required`,
  `parallel_tool_calls: false` unless set) and `apply(state, model, lease, body)`, which
  takes the template from an inline Jinja `template` setting or the catalog's GGUF
  `tokenizer.chat_template`.
- Called on `/v1/chat/completions` (`openai.rs`), `/v1/messages` (`anthropic/mod.rs`,
  generation only, not `count_tokens`) and `/v1/responses` (`responses/mod.rs`).
- `crates/alkahest/tests/serve_tools.rs` (new, opt-in `ALKAHEST_TEST_ENGINE=1`): OpenAI
  `required`, streamed named choice, Anthropic `any` against the real model, plus Ollama
  `logprobs` of ALK-US-0087.
- `docs/testing.md`: `ALKAHEST_CONFORMANCE_TOOLS=1` works with `qwen2.5-coder:0.5b`.

## Why

llama.cpp b11669's grammar for required JSON-native tool calls allows free content before the
`<tool_call>` marker; a 0.5B model writes the JSON bare and runs to `max_tokens` (details in
the ADR). The model's template and `--jinja` were correct.

## Verification

- Unit: `tool_prefill::tests::{required_choice_prefills_the_marker,
  named_choice_narrows_the_tools, other_cases_are_left_alone}`; harness:
  `tests/openai.rs#forced_tool_choice_prefills_hermes_marker`.
- Real, scratch `ALKAHEST_HOME`, llama.cpp b11669 CPU, `qwen2.5-coder:0.5b` from the
  read-only Ollama store: `cargo test -p alkahest --test serve_tools` passed (tool call
  `get_weather({"city": "Paris"})`, `finish_reason: tool_calls`, Anthropic `tool_use` with
  `stop_reason: tool_use`); `scripts/conformance.sh --url ... --chat qwen2.5-coder:0.5b` with
  `ALKAHEST_CONFORMANCE_TOOLS=1`: ollama, openai (incl. Responses function_call) and
  anthropic (tools stream, tool_result follow-up) all checks passed.
