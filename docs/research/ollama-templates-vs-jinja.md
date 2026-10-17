# Ollama Go templates vs GGUF Jinja: inventory and findings

Spike [[ALK-US-0015]]; decision in [[development/decisions/0001-chat-template-strategy]].
Date 2026-10-17. Tooling: `alkahest ls --json` / `show --template` from this repository,
a read-only scan of `/usr/share/ollama/.ollama/models` (manifests, config blobs, GGUF
header key `tokenizer.chat_template`), Ollama reference tree `Alkahest-refs/ollama`,
`llama-server` build b11669 (`0.7.0-dev`, ubuntu-x64 CPU asset, sha256 verified).
Nothing under the Ollama store was written.

## How Ollama renders (reference tree)

- `server/prompt.go: renderPrompt`: if the model config has `renderer`, call a Go renderer
  from `model/renderers/` (families: qwen3-coder, qwen3-vl, qwen3.5/tev1, qwen3.8, cogito,
  deepseek3.1, olmo3, nemotron, gemma4, glm-4.7, lfm2, cohere, ...), else execute the Go
  `TEMPLATE` (`template/template.go`, `Execute`, which also collates system messages,
  converts tools to template shapes and handles `.Suffix`/`.Response`).
- `server/routes.go: usesOllamaRenderedChat`: renderer, parser, Harmony or Go template
  models render in Go and call llama-server `/completion`; others use
  `/v1/chat/completions` with llama-server Jinja (`llm/llama_server.go`, with `--no-jinja
  --chat-template chatml` passed only as a startup placeholder in the Go-rendered case).
- `server/images.go: shouldPreferChatTemplate`: a GGUF template wins over a Go template only
  if it has more capabilities (tools/thinking) or tool round-trip support the Go one lacks;
  `OLLAMA_GO_TEMPLATE` overrides.
- Embedded named Go templates in `template/*.gotmpl` + `index.json` (37 index entries, 19
  distinct names) are used to infer a template for imported models.

## llama-server b11669 facts (from `--help`, `/props`, `/apply-template`)

| Item | Value |
|---|---|
| `--jinja` / `--no-jinja` | Jinja engine for chat, default enabled |
| `--chat-template <name or jinja>` | override; built-in names: chatml, llama2, llama3, gemma, phi3, phi4, mistral-v1/v3/v7, zephyr, vicuna, command-r, granite, deepseek*, gpt-oss, ... |
| `--chat-template-file <f>` | override from file |
| `--chat-template-kwargs '{...}'` | extra template variables (`enable_thinking`) |
| `GET /props` | `chat_template`, `chat_template_caps` (tools, system role, parallel tool calls, string/typed content, reasoning flags) |
| `POST /apply-template` | renders messages to a prompt without generating (usable as a test oracle) |
| Library lookup | binary `RUNPATH=$ORIGIN`; no `LD_LIBRARY_PATH` needed on Linux |

## Inventory of the local store

22 manifests: 15 local, 7 cloud stubs (`layers: null`, `remote_host` set, nothing to render).
"Go TPL" = a `template` layer exists. "GGUF Jinja" = `tokenizer.chat_template` in the GGUF
header (bytes). No local config has `renderer` or `parser`.

| Model | Kind | Go TPL | GGUF Jinja | Notes |
|---|---|---|---|---|
| agent-m1-4b:latest | vision chat (qwen35) | yes, 931 B | 8913 | same Go template as nova4b |
| agentm1:latest | chat (qwen35moe) | yes, 931 B | 7756 | same Go template |
| nova4b:latest | chat (qwen35) | yes, 931 B | 8913 | Go template differs from Jinja, see below |
| qwen2.5-coder:0.5b | chat/FIM (qwen2) | yes, 1615 B | 2507 | plain chat identical; Go adds FIM |
| hf.co/hungrynovalabs/nova-pup-4b-GGUF:Q8_0 | chat (qwen35) | no | 8913 | HF pull, Jinja only |
| hf.co/KikoCis/Agents-A1-IQ2_M-GGUF:IQ2_M | chat (qwen35moe) | no | 7756 | HF pull |
| hf.co/InternScience/Agents-A1-4B-Q4_K_M-GGUF:Q4_K_M | vision chat | no | 8913 | HF pull, projector layer |
| tev1:0.8b | decision (qwen35) | no | 7650 | capability `decision`, no config renderer |
| qwen3-embedding:0.6b | embedding | no | 4116 | template irrelevant |
| nomic-embed-text:latest, :v1.5 | embedding | no | none | no template needed |
| hf.co/brandtcormorant/CodeRankEmbed-Q4_K_M / Q8_0, hf.co/limcheekin/CodeRankEmbed-GGUF, hf.co/ggml-org/jina-embeddings-v2-base-code | embedding | no | none | no template needed |
| deepseek-v4-flash, glm-5.1, kimi-k2.5, minimax-m2.7, nemotron-3-super, qwen3-coder-next, qwen3.5 (all `:cloud`) | remote | none | none | not served locally |

Totals over the 8 local chat/vision/decision models: 4 have both, 4 have only Jinja,
0 have only a Go template, 0 have neither.

## Rendered comparison (same 4-message conversation)

Conversation: system "You are terse.", user "Hi", assistant "Hello!", user "Bye".
Go side: Ollama's own `template` package executed from a scratch Go program against the
reference tree. Jinja side: `llama-server` `/apply-template` with the model's GGUF.

- `qwen2.5-coder:0.5b`: identical, `<|im_start|>system\nYou are terse.<|im_end|>\n<|im_start|>user\nHi<|im_end|>\n<|im_start|>assistant\nHello!<|im_end|>\n<|im_start|>user\nBye<|im_end|>\n<|im_start|>assistant\n`.
- `nova4b:latest`:
  - Go: a leading blank line, then the system block **twice** (once from `.System`, once from
    ranging `.Messages`), no thinking prefix, and a blank line before the final
    `<|im_start|>assistant`.
  - Jinja: system block once, no stray newlines, ends with `<|im_start|>assistant\n<think>\n`.
  - Conclusion: the Go template of this model is not a faithful rendition of the model's
    Jinja; executing it is worse than not executing it. `/props` reports `supports_tools`,
    `supports_system_role`, `supports_parallel_tool_calls` all true for the Jinja.

Not done: the Go-subset prototype (ALK-T-0050) as code. The comparison above plus the
reference code show the cost and the quirks (message collation, tool shapes), and the
sampled data gives no model that would need it, so the ADR defers it.

## Risks and gaps

- The local sample is small (8 chat-capable models) and biased toward recent Qwen-family
  GGUFs. Older Ollama library models are known to ship without `tokenizer.chat_template`
  (Ollama itself logs a warning for that case); that population is where the built-in-name
  fallback and the per-model override matter. Not measured here.
- Families that Ollama renders with Go code (`renderer`) are not covered by any template
  path; we rely on their GGUF Jinja.
- Config blobs of cloud manifests fail to parse in the store reader ("invalid type: null,
  expected a sequence"), logged as warnings by `alkahest ls`. Already tracked as ALK-US-0082.
