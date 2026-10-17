---
id: ALK-SP-0005
type: spec
title: API compatibility surface
status: backlog
labels: [api]
created: 2026-10-16T22:37:35Z
updated: 2026-10-17T03:13:14Z
requirements:
  R1:
    status: backlog
    trace:
      code:
        - crates/alkahest-api/src/ollama/mod.rs#routes
        - crates/alkahest-api/src/ollama/generate.rs#generate
        - crates/alkahest-api/src/ollama/generate.rs#chat
        - crates/alkahest-api/src/ollama/embed.rs#embed
        - crates/alkahest-api/src/ollama/models.rs#ps
        - crates/alkahest-api/src/ollama/models.rs#pull
        - crates/alkahest-api/src/generation.rs#chat_pieces
        - crates/alkahest-api/src/generation.rs#completion_pieces
        - crates/alkahest-api/src/sampling.rs#ollama_options
        - crates/alkahest-api/src/pull.rs#PullService
        - crates/alkahest/src/serve_cmd.rs#serve
      tests:
        - crates/alkahest-api/tests/ollama.rs#ollama_client_chat_streams_ndjson
        - crates/alkahest-api/tests/ollama.rs#ollama_client_list_show_and_ps
        - crates/alkahest-api/tests/ollama.rs#ollama_client_pull_streams_progress
        - crates/alkahest-api/tests/ollama.rs#options_map_to_load_settings_and_sampling
        - crates/alkahest-api/tests/ollama.rs#tools_pass_through_and_tool_calls_come_back
        - crates/alkahest-api/tests/ollama.rs#images_become_image_url_parts
        - crates/alkahest-api/tests/ollama.rs#format_becomes_response_format
        - crates/alkahest-api/tests/ollama.rs#embed_and_legacy_embeddings
        - crates/alkahest-api/tests/ollama.rs#empty_prompt_loads_and_keep_alive_zero_unloads
        - crates/alkahest/tests/serve_inference.rs#real_engine_answers_generate_chat_completions_and_embeddings
  R2:
    status: backlog
    trace:
      code:
        - crates/alkahest-api/src/openai.rs#routes
        - crates/alkahest-api/src/openai.rs#proxy
        - crates/alkahest-api/src/openai.rs#sse_response
        - crates/alkahest-api/src/openai.rs#embeddings
        - crates/alkahest-api/src/openai.rs#list_models
        - crates/alkahest-api/src/responses/mod.rs#routes
        - crates/alkahest-api/src/responses/request.rs#translate
        - crates/alkahest-api/src/responses/stream.rs#Translator
        - crates/alkahest-api/src/responses/store.rs#ResponseStore
        - crates/alkahest-api/src/images.rs#normalize_messages
        - crates/alkahest-api/src/images.rs#fetch_as_data_url
      tests:
        - crates/alkahest-api/tests/openai.rs#openai_sdk_streams_chunks_and_tool_calls
        - crates/alkahest-api/tests/openai.rs#include_usage_adds_the_usage_chunk
        - crates/alkahest-api/tests/openai.rs#non_streamed_chat_records_usage
        - crates/alkahest-api/tests/openai.rs#response_format_images_and_n
        - crates/alkahest-api/tests/openai.rs#completions_stream_and_not
        - crates/alkahest-api/tests/openai.rs#embeddings_float_and_base64
        - crates/alkahest-api/tests/openai.rs#models_list_and_get
        - crates/alkahest-api/tests/responses.rs#non_streaming_response
        - crates/alkahest-api/tests/responses.rs#streaming_events
        - crates/alkahest-api/tests/responses.rs#function_calls_and_previous_response
        - crates/alkahest-api/tests/conformance.rs#openai_python_conformance
        - crates/alkahest-api/tests/vision.rs#remote_images_are_fetched_into_data_urls_in_every_dialect
        - crates/alkahest-api/tests/vision.rs#private_addresses_are_refused_by_default
        - crates/alkahest-api/tests/vision.rs#models_without_vision_refuse_images_before_fetching
  R3:
    status: backlog
    trace:
      code:
        - crates/alkahest-api/src/anthropic/mod.rs#routes
        - crates/alkahest-api/src/anthropic/request.rs#translate
        - crates/alkahest-api/src/anthropic/stream.rs#Translator
        - crates/alkahest-api/src/anthropic/stream.rs#aggregate
      tests:
        - crates/alkahest-api/tests/anthropic.rs#anthropic_sdk_receives_a_message
        - crates/alkahest-api/tests/anthropic.rs#streaming_event_sequence
        - crates/alkahest-api/tests/anthropic.rs#tools_round_trip
        - crates/alkahest-api/tests/anthropic.rs#images_and_sampling_reach_the_engine
        - crates/alkahest-api/tests/conformance.rs#anthropic_python_conformance
  R4:
    status: backlog
    trace:
      code:
        - crates/alkahest-api/src/rerank.rs#routes
        - crates/alkahest-api/src/rerank.rs#shape_results
        - crates/alkahest-api/src/pooled.rs#fit_inputs
        - crates/alkahest-engine/src/llama_cpp.rs#EMBED_BATCH_DEFAULT
      tests:
        - crates/alkahest-api/tests/rerank.rs#rerank_returns_jina_shape_sorted
        - crates/alkahest-api/tests/rerank.rs#rerank_options_and_errors
        - crates/alkahest-api/tests/rerank.rs#ollama_embed_truncates_long_inputs_unless_told_not_to
        - crates/alkahest-engine/tests/llama_cpp.rs#embedding_and_rerank_kinds
        - crates/alkahest-gguf/tests/classify.rs#pooling_less_rerankers_are_rerank
  R5:
    status: backlog
    trace:
      code:
        - crates/alkahest-api/src/dialect.rs#DefaultErrorRenderer
        - crates/alkahest-api/src/dialect.rs#ErrorKind
        - crates/alkahest-api/src/dialect.rs#dialect_errors
        - crates/alkahest-api/src/service.rs#respond
        - crates/alkahest-api/src/service.rs#HttpError
      tests:
        - crates/alkahest-api/tests/openai.rs#unknown_model
        - crates/alkahest-api/tests/ollama.rs#unknown_model_is_404_in_ollama_shape
        - crates/alkahest-api/tests/errors.rs#unknown_model
        - crates/alkahest-api/tests/errors.rs#unknown_routes_and_methods_answer_in_dialect
        - crates/alkahest-api/tests/errors.rs#oversized_bodies_answer_413_in_dialect
        - crates/alkahest-api/tests/errors.rs#malformed_requests_are_400_in_dialect
        - crates/alkahest-api/tests/errors.rs#every_kind_renders_in_every_family_shape
        - crates/alkahest-api/tests/conformance.rs#ollama_python_conformance
        - crates/alkahest-api/tests/conformance.rs#openai_python_conformance
  R6:
    status: backlog
---

## Purpose

Let existing Ollama, OpenAI and Anthropic clients use Alkahest by changing only the base URL.

## Scope

Covers the HTTP endpoints, their request and response shapes, streaming and errors, and the admin API. Decision endpoints are [[ALK-SP-0007]], authentication [[ALK-SP-0006]].

Plan: [[plan/alkahest-v1-plan]]

## Requirements

### ALK-SP-0005.R1 — Ollama native API

The server SHALL implement `/api/version`, `/api/tags`, `/api/show`, `/api/pull`, `/api/delete`, `/api/copy`, `/api/ps`, `/api/generate`, `/api/chat`, `/api/embed` and `/api/embeddings` with Ollama request and response shapes, including NDJSON streaming, `options`, `keep_alive`, `format`, `tools` and `images`.

#### Scenario: ollama client
- **WHEN** the official `ollama` Python client points at Alkahest
- **THEN** `chat`, `list` and `pull` work unchanged

### ALK-SP-0005.R2 — OpenAI API

The server SHALL implement `/v1/chat/completions` (streaming, tools, `response_format`, image inputs), `/v1/completions`, `/v1/embeddings`, `/v1/models`, `/v1/models/{id}` and `/v1/responses`.

#### Scenario: openai sdk
- **WHEN** the official `openai` SDK streams a chat completion with tools
- **THEN** chunks and tool calls parse without errors

### ALK-SP-0005.R3 — Anthropic Messages API

The server SHALL implement `POST /v1/messages` with streaming events, system prompt, tools and image content blocks.

#### Scenario: anthropic sdk
- **WHEN** the official `anthropic` SDK sends a message
- **THEN** it receives a valid `message` response

### ALK-SP-0005.R4 — Modality endpoints

The server SHALL implement `/v1/audio/transcriptions`, `/v1/audio/speech`, `/v1/images/generations` and `/v1/rerank` (Jina shape) for models whose engine supports them.

#### Scenario: transcription
- **WHEN** a WAV file is posted to a whisper model
- **THEN** the text is returned in OpenAI shape

### ALK-SP-0005.R5 — Dialect-correct errors

Errors SHALL use the shape of the dialect of the endpoint (Ollama `{error}`, OpenAI `{error:{message,type,code}}`, Anthropic `{type:error,error:{type,message}}`) with matching HTTP status codes.

#### Scenario: unknown model
- **WHEN** an OpenAI request names an unknown model
- **THEN** 404 with an OpenAI error object

### ALK-SP-0005.R6 — Admin API

The server SHALL expose `/alkahest/v1/*` for catalog, downloads, model configuration, engines, runtime metrics, API keys and settings, documented with OpenAPI, used by the desktop app and the CLI.

#### Scenario: openapi
- **WHEN** the server starts
- **THEN** `/alkahest/v1/openapi.json` describes every admin route
