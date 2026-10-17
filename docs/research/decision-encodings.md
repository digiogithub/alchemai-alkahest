# Decision model encodings (System One / Jev contract)

Status: research for [[ALK-US-0061]] (task ALK-T-0166) and [[ALK-US-0062]], 2026-10-17.
Spec: [[ALK-SP-0007]]. Plan: [[plan/alkahest-v1-plan]].

## Sources

- Ollama `decision/` package at v0.40.6-rc0 (MIT), `/www/MCP/Alchemai/Alkahest-refs/ollama`,
  plus the v0.35.0 tag (the installed Ollama is 0.35.0).
- LocalAI `core/schema/systemone.go`, `core/schema/decisions.go`, `core/systemone/` (MIT).
- llama.cpp server README at tag b11669 (`tools/server/README.md`, section
  "POST /v1/systemone: TypeSafe-compatible System One API").
- The `tev1:0.8b` GGUF in the local Ollama store (read only) and the current `tev1:0.8b`
  GGUF header on registry.ollama.ai.

## Contract

`POST /v1/systemone` takes `{model, state, images?, questions, keep_alive?}` and returns
`{model, answers, usage}`; Ollama adds `runner`, `videos` and
`prompt_eval_cached_count`. `questions` maps ids to `{type, instructions, criteria}`:

| type | criteria | answer |
|---|---|---|
| `noul` (yes/no) | optional `{"true": desc, "false": desc}` | `{"type":"noul","noul":P(true)}` |
| `choice` | `{option: desc or null}` | `{"type":"choice","choice","probabilities","confidence"}` |
| `score` | array of 2 to 10 level descriptions, lowest first | `{"type":"score","score","legend","probabilities","confidence"}` |

Answers keep question order. `score` is the expected level index; `confidence` is
`1 - H(p)/ln(n)` (Kev uses its own measure, rounded to four decimals). 1 to 64 questions.
Ollama requires `type`; the spec makes `noul` the default, so a missing `type` is `noul`.

## Encodings

All encodings render JSON the way Python `json.dumps(ensure_ascii=False)` does (`", "` and
`": "` separators, floats always with a fraction or exponent, `1e-05`, `1e+16`), keeping
object key order. Only clef sorts keys (its reference decodes into maps) and uses compact
separators.

| Encoding | Model input | Candidates |
|---|---|---|
| generic (Ollama default, renderer empty) | per question: `{"context", "schema": [all fields]}` plus `\n\nRequested field: "<id>"`; `<` and `>` escaped as `<` / `>` so state text cannot close the chat turn; choices `{code, value, description?}` | letters `A`.. (2 to 26) |
| tev1 (Together Tev1) | per question: `{"state": <raw state>, "question", "options": [{label, key, description}]}`; noul described `No`/`Yes`, bare choices by key | letters, at most 24 |
| kev | shared state as indented `key: value` / `- item` lines (`True`, `False`, empty null), per question type, instructions and option texts | model-native head, 1 to 255 options |
| clef | one joint prompt in segments (system, STATE, SCHEMA FIELDS, per field ID/TYPE/INSTRUCTION and OPTION lines with compact sorted `{"description","option_id"}`), choice options sorted by key and mapped back for the answer | model-native head, 2 to 26 |
| strands | per question `<state>` prefix (JSON indented 2) and `<question type>` block with numbered options; byte spans of each option line | pointer head, 2 to 255 (score 10) |

The letter encodings are wrapped in the model's chat template with thinking disabled, after
the Ollama `system` layer when the model has one (Tev1 falls back to its published system
instruction).

## How models select an encoding (R4)

1. GGUF `decision.type`, or a `tokenizer.chat_template.systemone` template: llama-server
   b11669 implements `/v1/systemone` natively (it answers 501 "This model is not a decision
   model" otherwise). Alkahest validates and forwards. Old clef GGUFs on `qwen35` are
   refused, as Ollama does.
2. Ollama config `renderer` in {tev1, kev, clef, strands}.
3. Ollama `decision` capability or the `decision-model` GGUF tag: tev1 when the basename is
   Tev1, generic otherwise. The local `tev1:0.8b` (pulled 2026-09-30) is this case: no
   `decision.type`, no `systemone` template, capability `decision`. The current registry
   build of the same tag carries a `systemone` template (case 1).
4. Anything else: 400, not a decision model.

kev, clef and strands need model-native heads that llama-server only exposes through its own
`/v1/systemone`, so on the prompt path they compile (golden-tested) but are answered with
501 by the llama-server scorer.

## Scoring through llama-server (R3)

Ollama 0.35 scores letter candidates with plain llama-server endpoints, which b11669 still
has: `/apply-template` (with `chat_template_kwargs.enable_thinking=false`), `/tokenize`
(`parse_special`), and `/completion` with `n_predict: 1`, a logit bias of +100 on every
candidate (+1000 retry when another token still outranks one), samplers `top_k` (k =
candidates) then `temperature 1`, `n_probs` = candidates and `post_sampling_probs: true`.
A shared bias leaves the softmax over the candidates unchanged, so the returned
probabilities are exactly the candidates' renormalised distribution. Each candidate must
append exactly one ordinary token to the prompt.

Measured with the CPU llama-server b11669 and the local tev1:0.8b:

- Ollama 0.35's generic prompt reproduces Ollama 0.35's own answers (input tokens 728 in
  both; probabilities within 0.004, the llama.cpp builds differ).
- Sequential requests are bit-for-bit repeatable, with and without `cache_prompt`.
  Concurrent requests on several slots are not (batch composition changes the float
  results) and were slower on CPU. The scorer therefore defaults to one row at a time and
  `cache_prompt: false`; concurrency is an option for throughput where repeatability does
  not matter.
