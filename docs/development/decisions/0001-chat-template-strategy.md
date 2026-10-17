# ADR 0001: Chat template strategy for Ollama Go TEMPLATE layers and GGUF Jinja

Status: accepted (2026-10-17). Spike: [[ALK-US-0015]] (tasks ALK-T-0049, ALK-T-0050,
ALK-T-0051). Evidence: [[research/ollama-templates-vs-jinja]].
Related: [[plan/alkahest-v1-plan]] open question 1, [[ALK-SP-0010]] R2, [[ALK-SP-0003]] R2,
[[ALK-US-0025]].

## Context

Models in an Ollama store may carry a Go `TEMPLATE` layer
(`application/vnd.ollama.image.template`); GGUF files may carry a Jinja
`tokenizer.chat_template`. Alkahest runs `llama-server` as a subprocess and Rust cannot run Go
templates directly, so we must decide who renders the prompt.

Findings (details and tables in the research note):

- Ollama itself, in the reference tree, now drives `llama-server` and has *two* chat paths
  (`llm/llama_server.go`, `server/routes.go: usesOllamaRenderedChat`): models with a
  `renderer`/`parser` config field (Go code, per family), Harmony models, or a Go `TEMPLATE`
  render the prompt in Go and call `/completion`; every other GGUF chat model goes through
  `/v1/chat/completions` with `llama-server`'s Jinja. When both a Go template and a GGUF
  template exist, Ollama prefers the Go template unless the GGUF template has more
  capabilities (`shouldPreferChatTemplate`).
- Local store (15 local models, 7 cloud stubs): every chat model that has a Go `TEMPLATE`
  (4 of 4) also has a GGUF `tokenizer.chat_template`. No local model is Go-template-only.
  No local model uses a `renderer` or `parser` field.
- On the 4 models with both, the two disagree. For `nova4b` (and the identical `agentm1`,
  `agent-m1-4b` templates) the Go template, executed with Ollama's own `template` package on
  a 4-message conversation, emits the system block twice and no `<think>\n` generation
  prefix; `llama-server` `/apply-template` (build b11669) with the GGUF Jinja emits the system
  block once and ends with `<|im_start|>assistant\n<think>\n`. For `qwen2.5-coder:0.5b`
  both renderers produce byte-identical output for plain chat; the Go template additionally
  carries FIM (`.Suffix`) logic that Jinja does not.
- `llama-server` b11669 enables Jinja by default (`--jinja, --no-jinja ... default: enabled`),
  reads the template from GGUF metadata, accepts `--chat-template-file <f>` /
  `--chat-template <s>` overrides and `--chat-template-kwargs '{json}'`, applies it for
  `/v1/chat/completions`, exposes `POST /apply-template` and reports `chat_template_caps`
  (tools, system role, parallel tool calls) on `GET /props`. Tool-call and reasoning parsing
  for the response is derived from the same template, which a Go-rendered prompt does not get.

## Options

1. **Jinja via `llama-server --jinja`, always.** Zero template code in Alkahest; correct tool
   calling and thinking; identical to what upstream GGUF authors test. Fails for a GGUF
   without `tokenizer.chat_template`.
2. **Go-template subset renderer in Rust** (e.g. `gtmpl`-like). Faithful to Ollama output for
   models whose Go template is the source of truth. Costs a Rust implementation of Go
   `text/template` semantics plus Ollama's pre-processing (message collation, `.Tools` JSON
   shapes, `.Function`, `.Think`, `slice`, `len`, `.Response`) and a separate tool-call
   parser per family; reproduces Ollama's quirks (the duplicated system block above). Cannot
   cover `renderer` families, which are Go code, not templates.
3. **Translate Go template to Jinja** and pass `--chat-template-file`. Mechanical for the
   constructs in simple templates (`if`, `range`, `.Role`, `.Content`, `eq`), but Go
   whitespace trimming and `.Tools` serialisation do not map one to one, so each translation
   needs per-template verification; a generic translator is a large, fragile project.
4. **Per-model override** (user supplies a Jinja file/string). Cheap, an escape hatch,
   not a strategy by itself.

## Decision

Jinja rendered by `llama-server` is the only template path of v1, with a strict precedence,
resolved by the engine adapter when it builds the argument list:

1. **Per-model override** (model configuration key `chat_template_file`, a Jinja file, or
   `chat_template`, an inline string): passed as `--chat-template-file` / `--chat-template`.
   Overrides everything below.
2. **GGUF `tokenizer.chat_template`** (the normal case): no template flag; `llama-server` reads
   it. Always pass `--jinja` explicitly (it is the default in b11669 but not in older builds,
   and being explicit makes the argument list self-describing). Requests go to
   `/v1/chat/completions`; `chat_template_kwargs` from the model configuration go through
   `--chat-template-kwargs`. Capability flags (tools, thinking) come from the GGUF reader
   (ALK-SP-0010.R2) and are cross-checked with `/props` `chat_template_caps`.
3. **No GGUF template, Go `TEMPLATE` layer present**: do not execute the Go template. Match
   the Go template (whitespace-normalised) against the named templates of Ollama's
   `template/*.gotmpl` set and, on a hit, start `llama-server` with the equivalent built-in
   `--chat-template <name>` (b11669 list, checked with `--help`). Name mapping, Ollama to
   llama.cpp: `chatml`->`chatml`, `llama3-instruct`->`llama3`, `llama2-chat`->`llama2`,
   `gemma-instruct`/`gemma3-instruct`->`gemma`, `phi-3`->`phi3`, `zephyr`->`zephyr`,
   `openchat`->`openchat`, `vicuna`->`vicuna`, `command-r`->`command-r`,
   `granite-instruct`->`granite`, `mistral-instruct`->`mistral-v1`/`mistral-v3` (by
   `[INST]` spacing); templates with no counterpart (`alpaca`, `chatqa`, `magicoder`,
   `alfred`, `falcon-instruct`, `solar-instruct`, `starcoder2-instruct`,
   `codellama-70b-instruct`) count as a miss. On a miss, log a warning, mark the model `template: missing`
   in `show`, and serve `/v1/chat/completions` only when the user sets an override (1);
   `/api/generate` with raw prompts keeps working.
4. **A Go-template subset renderer is NOT built now.** It is deferred behind a trigger: an
   inbox item is filed when a real user model falls into case 3's miss branch. The renderer,
   if ever needed, lives in `alkahest-engine` behind the same precedence list and is
   revisited in a new ADR.

Ollama `renderer`/`parser` config fields are ignored: the GGUF Jinja is used. Where the
family is known to depend on a Go renderer for correct tool calls, `show` reports the divergence.
A Go `TEMPLATE` layer is never preferred over a GGUF template (this reverses Ollama's own
default); the two disagree in the sampled models and Jinja is the variant `llama-server` can
also parse tool calls and reasoning from.

### Engine flags

| Case | Flags |
|---|---|
| GGUF template | `--jinja` (+ `--chat-template-kwargs '{"enable_thinking":true}'` when configured) |
| Override file | `--jinja --chat-template-file <path>` |
| Override string | `--jinja --chat-template '<jinja>'` |
| Go template matches a built-in | `--jinja --chat-template <name>` |
| Embedding / decision / raw | none of the above; `--no-jinja` not needed |

## Consequences

- No Go template code in v1; the engine adapter ([[ALK-US-0025]]) only builds flags and
  uses `/v1/chat/completions`. The Go `TEMPLATE` layer is read, shown and preserved by `cp` but
  not executed. `alkahest show --template` still prints it.
- Prompts for the Ollama models in the sampled store differ from what Ollama renders for
  the same model (for the better, in the sample). Users comparing byte-for-byte output with
  Ollama will see differences, documented here.
- Legacy GGUFs without `tokenizer.chat_template` and without a recognised Go template need
  the per-model override; the local sample has none, so the size of this population is
  unknown. The inbox trigger in decision 4 measures it.
- The engine adapter needs an `/apply-template` self-check test fixture and a `show` field
  for the template source (`override`, `gguf`, `builtin:<name>`, `missing`).
- Spec impact (specs are not edited by this ADR; the orchestrator decides):
  [[ALK-SP-0010]] R2 scenario "template" is consistent, and a scenario for the
  override and for the built-in fallback should be added. The plan open question 1
  ("Go-template subset renderer otherwise") is narrowed: the subset renderer becomes a
  deferred option, not a v1 deliverable.
