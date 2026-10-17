# API reference

One server, several dialects. Base URL `http://127.0.0.1:11435`. Which key a remote caller needs
is described in [Serving and security](../guide/serving.md); on loopback none is needed by
default. Send keys as `Authorization: Bearer <key>` or `x-api-key: <key>`.

The `model` field of every request is a [model reference](../guide/models.md). A model is loaded
on demand; a request for a model that does not exist is `404` with `model 'x' not found, try
pulling it first`, in the error shape of the dialect. A model that cannot do what is asked
(chat with a completion-only model, an image to a text model, tools to a model without tool
support) is a `400` that says why.

Errors always use the format of the family of the route: `{"error": "..."}` for `/api/*`,
`{"error": {"message", "type", "param", "code"}}` for `/v1/*` and Anthropic's `{"type":
"error", ...}` for `/v1/messages`, including errors produced by the HTTP layer itself (404,
405, 413, 408, 500). The official Ollama, OpenAI and Anthropic SDKs are tested against the
server (`scripts/conformance.sh`).

## Ollama native API

| Route | Notes |
|---|---|
| `POST /api/generate` | streaming (NDJSON) or `stream: false`; `system`, `template`, `raw`, `options`, `keep_alive`, `format` (`json` or a schema), `images`, `suffix` (fill-in-the-middle through the engine's infill), `logprobs` / `top_logprobs` |
| `POST /api/chat` | messages, `tools`, `think`, `format`, `options`, `keep_alive`, `logprobs` / `top_logprobs`; the model's own system prompt is prepended unless the conversation starts with one |
| `POST /api/embed`, `POST /api/embeddings` | batch (`input` array) and legacy single prompt |
| `GET /api/tags` | local models |
| `POST /api/show` | details, template, parameters, licence, capabilities |
| `GET /api/ps` | loaded models with `expires_at` and `size_vram` |
| `POST /api/pull` | streaming progress |
| `POST /api/copy`, `DELETE /api/delete` | models of the Ollama store are read-only (403) |
| `HEAD`, `POST /api/blobs/:digest` | upload a GGUF into the Alkahest store, verified against the digest (no body size limit) |
| `POST /api/create` | from a model (`from`) or one uploaded GGUF (`files`), with `template`, `system`, `parameters`, `license`, or a Modelfile with `FROM`, `PARAMETER`, `TEMPLATE`, `SYSTEM`, `LICENSE`; `ADAPTER`, `MESSAGE`, quantisation and safetensors imports answer 400 |
| `GET /api/version`, `GET /` | version; `/` answers "Alkahest is running" |

`pull`, `copy`, `create`, blob uploads and `delete` need the admin scope for remote callers.

## OpenAI-compatible API

| Route | Notes |
|---|---|
| `POST /v1/chat/completions` | streaming with `data: [DONE]`; `tools`, `tool_choice`, `response_format` (JSON mode and JSON schema), image parts for vision models, `seed`, `stop`, `logprobs`; `n` other than 1 is a 400; usage is included when the client asks with `stream_options` |
| `POST /v1/completions` | text completion |
| `POST /v1/responses`, `GET`/`DELETE /v1/responses/{id}` | Responses API: `instructions`, string or item `input`, function tools, `previous_response_id`, `text.format`, `reasoning.effort`, streaming events with sequence numbers. Hosted tools, `background` and `truncation: auto` are not supported |
| `POST /v1/embeddings` | `encoding_format` `float` or `base64`, `dimensions` |
| `GET /v1/models`, `GET /v1/models/{id}` | models the caller's key may use; ids may contain slashes |
| `POST /v1/audio/transcriptions`, `POST /v1/audio/translations` | speech to text on whisper.cpp; `response_format` `json`, `text`, `verbose_json`, `srt`, `vtt` |
| `POST /v1/audio/speech` | answers 501 `not_implemented`: no licence-clean text-to-speech engine is shipped yet |
| `POST /v1/images/generations` | stable-diffusion.cpp; `prompt`, `size`, `n`, `response_format: b64_json` (URL responses and edits are not supported) |
| `POST /v1/rerank` | Jina/Cohere shape: `query`, `documents` (strings or `{text}`), `top_n`, `return_documents`; needs a rerank model |

Vision: images in chat requests as data URLs or `http(s)` URLs. URLs that resolve to private
networks are refused (server-side request forgery protection; see
`ALKAHEST_IMAGE_FETCH_ALLOW_PRIVATE`), as are `file://` URLs and oversized or non-image content.

Embedding and rerank models truncate inputs to the model's context instead of failing.

Python:

```python
from openai import OpenAI
c = OpenAI(base_url="http://127.0.0.1:11435/v1", api_key="alk_...")
r = c.embeddings.create(model="all-minilm", input=["hello", "world"])
```

## Anthropic Messages API

| Route | Notes |
|---|---|
| `POST /v1/messages` | `system` (string or blocks), text, image, document and tool blocks, `tools`/`tool_choice`, `thinking`, `max_tokens` (required), streaming events (`message_start` ... `message_stop`) |
| `POST /v1/messages/count_tokens` | token count of a request |

Server tools (such as web search) are refused with 400. Point the Anthropic SDK at it with
`base_url="http://127.0.0.1:11435"`.

```python
import anthropic
c = anthropic.Anthropic(base_url="http://127.0.0.1:11435", api_key="alk_...")
m = c.messages.create(model="llama3.2:3b", max_tokens=200,
                      messages=[{"role": "user", "content": "Hello"}])
```

## Decision API

For classification and routing with a decision model (such as `tev1`): the model answers
structured questions by scoring candidate tokens (logits), so answers are deterministic and come
with probabilities.

| Route | Shape |
|---|---|
| `POST /v1/systemone` | Ollama 0.35 / llama-server body: `model`, `state`, `questions` as a map of `choice`, `noul` (yes/no/unknown) and `score` questions |
| `POST /v1/decisions` | OpenAI Decisions shape: `input` (text or messages with `input_text` and `input_image`), ordered `questions` of type `predicate`, `choice` (with `choices[{value, description}]`) and `score`; answers with `probability` and `usage` |

```sh
curl http://127.0.0.1:11435/v1/decisions -d '{
  "model": "tev1:0.8b",
  "input": "I was charged twice for my order.",
  "questions": [
    {"type": "choice", "name": "route", "instructions": "Which team handles this?",
     "choices": [{"value": "billing"}, {"value": "shipping"}, {"value": "technical"}]},
    {"type": "predicate", "instructions": "Is the customer angry?"}
  ]}'
```

## Health and metrics

| Route | Notes |
|---|---|
| `GET /healthz` | liveness, never needs a key |
| `GET /metrics` | Prometheus text format |

## Admin API

Under `/alkahest/v1/`, described by an OpenAPI 3.1 document served at
`/alkahest/v1/openapi.json` (the authoritative, always current list). Every route needs the
`admin` scope for remote callers. It is what the desktop app uses.

| Area | Routes |
|---|---|
| System | `GET info`, `GET ps` (with an estimated `load_progress` while a model loads), `GET events` (server-sent events: scheduler and job updates), `POST unload` |
| Models | `GET models`, `GET/DELETE models/{name}`, `POST copy`, `GET/PUT/DELETE models/{name}/settings`, `GET models/{name}/recommended`, `GET models/{name}/estimate` (memory estimate) |
| Downloads | `POST/GET pulls`, `DELETE pulls/{id}`, `GET jobs`, `GET jobs/{id}`, `GET jobs/{id}/log` (output tail, source build log), `POST jobs/{id}/cancel`, `DELETE jobs/{id}` |
| Discovery | `GET hf/search`, `GET hf/model`, `GET hf/files`, `GET/PUT/DELETE hf/token`, `GET ollama/search`, `GET ollama/tags` (tags of a library model) |
| Engines | `GET engines`, `POST engines/install`, `DELETE engines/{id}` (409 while a loaded model runs on the build), `POST engines/reload`, `GET/PUT engines/{id}/override` (executable path and extra arguments, applied on restart); listings carry `bundled` and `download_bytes` |
| Keys | `GET keys`, `POST keys` (the secret is returned once), `DELETE keys/{id}` |
| Settings | `GET/PUT settings/network`, `GET/PUT settings/server` |
| Logs | `GET logs/engines`, `GET logs/engines/{name}`, `GET logs/server` |

## Compatibility notes

- Model names containing `/` and `:` work in all dialects (`hf.co/org/repo:Q4_K_M`).
- `keep_alive` is accepted by the Ollama and decision routes; OpenAI and Anthropic clients use
  the server default or the model's setting.
- Request fields the engine understands but the dialect does not define pass through to the
  engine, so new sampler options usually work without a server update.
- Tokens used are counted per API key (`alkahest keys ls`) and in `/metrics`.
