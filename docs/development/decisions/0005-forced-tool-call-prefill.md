# ADR 0005: prefill the tool-call marker for forced tool choices on Hermes-style templates

Status: accepted 2026-10-17. Story: [[ALK-US-0096]]. Extends
[[development/decisions/0001-chat-template-strategy]]. Plan: [[plan/alkahest-v1-plan]].

## Context

With `tool_choice: "required"` (OpenAI), `{"type": "any"}` / `{"type": "tool"}` (Anthropic) or
a named function, `qwen2.5-coder:0.5b` served by llama-server b11669 answered with the call as
plain JSON text until `max_tokens`, and no `tool_calls` were parsed. Investigation (scratch
runs of llama-server alone, `-v`):

- The GGUF Jinja template is byte-identical to `Qwen-Qwen2.5-7B-Instruct.jinja`, which
  llama.cpp's own tests cover; the differential autoparser detects `tool_mode: JSON_NATIVE`
  with `per_call_start: '<tool_call>'`; `--jinja` is passed. The template is not the problem.
- llama.cpp's JSON-native tool parser (`common/chat-auto-parser-generator.cpp`,
  `build_tool_parser_tag_json`) accepts `optional(content(until("<tool_call>")))` before the
  calls even when the call is required. The grammar therefore lets the model write anything
  that does not contain the marker; a small model writes the JSON bare, cannot end (EOS is not
  allowed before a call) and stops at the token limit.
- Prefilling the assistant turn with `<tool_call>\n` (a final assistant message, which
  llama-server continues and feeds to the grammar as accepted tokens) makes the same request
  return a parsed `tool_calls` entry and `finish_reason: tool_calls`, streamed or not.

## Decision

For forced tool choices only, when the template the engine uses (an inline Jinja override, else
the GGUF `tokenizer.chat_template`) is Hermes-style JSON (`<tool_call>` present, no
`<function=` / `<parameter=` XML form), the API layer (`alkahest_api::tool_prefill`) appends
an assistant message `<tool_call>\n` before proxying, on the OpenAI chat, Responses and
Anthropic Messages routes. A named choice narrows `tools` to that tool and becomes
`required`; `parallel_tool_calls` defaults to `false` unless the client set it (otherwise a
forced call repeats until the limit on small models). A conversation that already ends with an
assistant turn is the client's own prefill and is left alone. `auto`, other template
families and the Ollama API (which has no `tool_choice`) are unchanged.

No per-model override is needed for this model. Models whose template has no tool format at
all (for example `smollm2:135m`) still cannot call tools; the recommendation for those stays a
per-model `template` override with a tool-capable Jinja template (`alkahest config model`).

## Consequences

- The SDK conformance scripts pass with `ALKAHEST_CONFORMANCE_TOOLS=1` against
  `qwen2.5-coder:0.5b`.
- When llama.cpp stops allowing content before a required call, the prefill stays harmless
  (it is what the grammar would force anyway) and can be removed.
