# Change: Anthropic Messages API (ALK-US-0037)

Implements [[ALK-US-0037]] / [[ALK-SP-0005]].R3 (tasks ALK-T-0114, ALK-T-0115,
ALK-T-0116). Builds on [[2026-10-17-openai-api]], [[2026-10-17-dialect-errors-conformance]]
and [[development/decisions/0001-chat-template-strategy]]. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-api`, module `anthropic` (`anthropic::routes()`, merged by
`inference_routes()`):

- `request::translate(&body, count_only) -> Translated`: Anthropic request onto
  llama-server's OpenAI chat body. `system` string or text blocks -> leading system message
  (the model's Ollama `SYSTEM` layer is used when the request has none); user `text` /
  `image` (base64 -> data URL, `url` source passed on) / `tool_result` (-> `tool`
  messages, `is_error` prefixed `Error:`) / plain-text `document` blocks; assistant `text`,
  `thinking` (-> `reasoning_content`), `tool_use` (-> `tool_calls`, arguments as a JSON
  string), `redacted_thinking` dropped; `tools` -> function tools (`input_schema` ->
  `parameters`; server tools such as `web_search_*` -> 400); `tool_choice` `auto` / `any` /
  `tool` / `none` -> `auto` / `required` / named function / `none`;
  `disable_parallel_tool_use` -> `parallel_tool_calls: false`; `thinking` `enabled` /
  `adaptive` / `disabled` -> `chat_template_kwargs.enable_thinking` (budget not enforced);
  `max_tokens` required (positive integer); `temperature`, `top_p`, `top_k` pass through;
  text-only content collapses to a string for template compatibility.
- `stream::Translator`: engine `chat.completion.chunk`s -> Anthropic events
  (`message_start` with `usage.input_tokens`, `ping`, `content_block_start` /
  `content_block_delta` with `text_delta`, `thinking_delta`, `input_json_delta` /
  `content_block_stop`, `message_delta` with `stop_reason` (`end_turn`, `max_tokens`,
  `tool_use`, `stop_sequence`), `stop_sequence` and cumulative usage, `message_stop`).
  `message_start` waits for the first chunk carrying prompt timings (the engine is asked for
  `timings_per_token`) so the input token count is real. Client `stop_sequences` are matched
  on answer text in the translator, holding back possible prefixes, because llama-server does
  not report which stop word matched; a match drops the engine connection (cancelling the
  generation). `stream::aggregate` folds the events into the non-streaming `message`, so both
  shapes come from one code path.
- `POST /v1/messages`: always streams from the engine; SSE (`event:` + `data:`) when
  `stream: true`, one `message` object otherwise. Engine loss mid-stream -> `event: error`
  with the Anthropic error object. Usage to the key counters and scheduler metrics.
- `POST /v1/messages/count_tokens`: the translated messages rendered by the engine's
  `/apply-template` (with tools) and counted by `/tokenize` -> `{"input_tokens": n}`.
- Credentials: `x-api-key` (auth layer, already supported); `anthropic-version` /
  `anthropic-beta` accepted and ignored. Errors use `{"type": "error", "error": {...}}`
  through the existing renderer (`Family::Anthropic`).

Conformance: `tests/conformance/anthropic_sdk.py` (official `anthropic` SDK: create,
stream `text_stream` + `get_final_message`, tools streamed with `tool_choice any`,
tool_result follow-up, `count_tokens`, `NotFoundError`, `BadRequestError`), driven by
`tests/conformance.rs#anthropic_python_conformance`; `scripts/conformance.sh` installs
`anthropic` too and runs the script in `--url` mode. `ALKAHEST_CONFORMANCE_TOOLS=0` skips
the tool-call assertions for tiny real models without a usable tool template.

## Deviations

- Thinking blocks carry an empty `signature` and no `signature_delta` (local models have no
  signed reasoning); `budget_tokens` is accepted and not enforced.
- `image` blocks with a `url` source are passed to the engine as is; server-side fetching
  is ALK-US-0056.
- Real run: `qwen2.5-coder:0.5b` from the Ollama store answers tool requests in plain text
  (also on `/v1/chat/completions`): inbox item ALK-US-0096.

## Verification

- `crates/alkahest-api/tests/anthropic.rs` (8 tests): scenario `anthropic sdk` as
  `anthropic_sdk_receives_a_message`, `streaming_event_sequence` (exact event order with
  thinking), `tools_round_trip`, `images_and_sampling_reach_the_engine`,
  `stop_sequences_and_max_tokens`, `errors_use_the_anthropic_shape`,
  `engine_loss_mid_stream_is_an_error_event`, `count_tokens_uses_the_template`; unit tests in
  `anthropic::request` and `anthropic::stream`.
- `scripts/conformance.sh`: ollama, openai and anthropic SDKs, all checks passed (fake
  engine).
- Real engine: `anthropic_sdk.py` against `alkahest serve` (llama.cpp b11669 CPU,
  `qwen2.5-coder:0.5b`, `ALKAHEST_CONFORMANCE_TOOLS=0`): all checks passed; raw SSE with
  `stop_sequences: ["4"]` stopped at "1 2 3 " with `stop_reason: stop_sequence`.
- `scripts/ci-local.sh`: passed (750 tests).
