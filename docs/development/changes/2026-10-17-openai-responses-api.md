# Change: OpenAI Responses API (ALK-US-0036)

Implements [[ALK-US-0036]] / the `/v1/responses` part of [[ALK-SP-0005]].R2 (tasks
ALK-T-0112, ALK-T-0113). Builds on [[2026-10-17-openai-api]] and
[[2026-10-17-anthropic-messages-api]] (same translate-and-stream design). Plan:
[[plan/alkahest-v1-plan]].

## What

Crate `alkahest-api`, module `responses` (`responses::routes()`, merged by
`inference_routes()`):

- `request::translate(&body, history) -> Translated`: `instructions` -> system message
  (not carried to follow-ups); `input` string or items: messages (`user` / `system` /
  `developer` -> system / `assistant`, string or `input_text` / `input_image` /
  `output_text` parts), `function_call` (consecutive calls merged into one assistant
  `tool_calls` turn), `function_call_output` (-> `tool` message), `reasoning` dropped;
  `tools` of type `function` (hosted tools -> 400), `tool_choice`, `parallel_tool_calls`,
  `max_output_tokens` -> `max_tokens`, `temperature`, `top_p`, `text.format`
  (`json_schema` / `json_object` -> `response_format`), `reasoning.effort` ->
  `chat_template_kwargs` (`none` disables thinking).
- `stream::Translator`: engine chunks -> `response.created`, `response.in_progress`,
  per item `response.output_item.added` / `.done`; `message` items with
  `content_part.added` / `output_text.delta` / `output_text.done` / `content_part.done`;
  `reasoning` items with `reasoning_text.delta` / `.done`; `function_call` items with
  `function_call_arguments.delta` / `.done`; final `response.completed` or
  `response.incomplete` (`incomplete_details.reason = max_output_tokens`) with usage
  (cached tokens from llama-server timings). Every event has a `sequence_number`; the
  response object echoes the request fields with OpenAI's defaults. Mid-stream failures are
  an `error` event.
- `store::ResponseStore`: in-memory, one hour TTL, 1000 responses, oldest evicted;
  overridable as an `Arc<ResponseStore>` state service. `previous_response_id` continues
  the stored conversation (unknown or expired -> 400 `Previous response with id '...' not
  found.`), `store: false` skips storing. `GET /v1/responses/{id}` and
  `DELETE /v1/responses/{id}`.
- `service::random_id(prefix)` (shared with the Anthropic message ids).

Conformance: `tests/conformance/openai_sdk.py` gained `responses.create`,
`responses.stream` (deltas and `get_final_response`), `previous_response_id`,
`responses.retrieve`, function calling with a `function_call_output` follow-up and the
unknown-previous-id `BadRequestError`.

## Deviations

- Stored responses are visible to any caller that knows the id (random, 18 characters); no
  per-key ownership yet. Responses are lost on restart.
- `file_id` image inputs, hosted tools (web/file search, code interpreter, MCP), `include`,
  `background` and `truncation: auto` are not supported.

## Verification

- `crates/alkahest-api/tests/responses.rs` (6 tests): non-streaming response + retrieve,
  streaming event order and sequence numbers with reasoning, function calls and
  `previous_response_id` continuation (tool message reaches the engine), `store: false` and
  delete, `max_output_tokens` -> incomplete, errors and image inputs; unit tests in
  `responses::request`, `responses::stream`, `responses::store`.
- `scripts/conformance.sh`: all three SDKs passed (fake engine).
- Real engine: `openai_sdk.py` against `alkahest serve` (llama.cpp b11669 CPU,
  `qwen2.5-coder:0.5b`, `ALKAHEST_CONFORMANCE_TOOLS=0`, see ALK-US-0096): all checks passed.
- `scripts/ci-local.sh`: passed (762 tests).
