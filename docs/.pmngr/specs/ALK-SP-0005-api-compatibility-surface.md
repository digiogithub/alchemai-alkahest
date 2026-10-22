---
id: ALK-SP-0005
type: spec
title: API compatibility surface
status: backlog
labels: [api]
created: 2026-10-16T22:37:35Z
updated: 2026-10-22T17:56:28Z
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
        - crates/alkahest-api/src/generation.rs#ollama_logprob
        - crates/alkahest-api/src/ollama/create.rs#create
        - crates/alkahest-api/src/ollama/create.rs#post_blob
        - crates/alkahest-store/src/create.rs#AlkahestStore::create_model
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
        - crates/alkahest-api/tests/ollama.rs#chat_logprobs_are_translated
        - crates/alkahest-api/tests/ollama.rs#raw_generate_logprobs_use_n_probs
        - crates/alkahest-api/tests/ollama.rs#blobs_upload_and_create_from_a_gguf
        - crates/alkahest-api/tests/ollama.rs#create_from_an_ollama_model_overrides_layers
        - crates/alkahest-api/tests/ollama.rs#blob_uploads_bypass_the_body_limit
        - crates/alkahest/tests/serve_tools.rs#forced_tool_calls_parse_on_a_small_hermes_model
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
        - crates/alkahest-api/src/tool_prefill.rs#apply
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
        - crates/alkahest-api/tests/openai.rs#forced_tool_choice_prefills_hermes_marker
        - crates/alkahest/tests/serve_tools.rs#forced_tool_calls_parse_on_a_small_hermes_model
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
        - crates/alkahest-api/src/audio/transcribe.rs#routes
        - crates/alkahest-api/src/audio/transcribe.rs#shape_verbose
        - crates/alkahest-api/src/audio/speech.rs#routes
        - crates/alkahest-api/src/imagegen/generations.rs#routes
        - crates/alkahest-api/src/imagegen/generations.rs#parse_request
        - crates/alkahest-engine/src/whisper_cpp.rs#WhisperCppEngine
        - crates/alkahest-engine/src/sd_cpp.rs#StableDiffusionCppEngine
        - crates/alkahest-hf/src/ggml.rs#pull_ggml
        - crates/alkahest-store/src/manifest.rs#DiffusionComponent
        - crates/alkahest-engine/src/capability.rs#kind_capabilities
      tests:
        - crates/alkahest-api/tests/rerank.rs#rerank_returns_jina_shape_sorted
        - crates/alkahest-api/tests/rerank.rs#rerank_options_and_errors
        - crates/alkahest-api/tests/rerank.rs#ollama_embed_truncates_long_inputs_unless_told_not_to
        - crates/alkahest-engine/tests/llama_cpp.rs#embedding_and_rerank_kinds
        - crates/alkahest-gguf/tests/classify.rs#pooling_less_rerankers_are_rerank
        - crates/alkahest-api/tests/audio.rs#transcription_returns_openai_json
        - crates/alkahest-api/tests/audio.rs#speech_is_a_clear_501_until_an_engine_exists
        - crates/alkahest/tests/serve_audio.rs#real_whisper_transcribes_the_jfk_sample
        - crates/alkahest-api/tests/imagegen.rs#generation_returns_openai_shape
        - crates/alkahest/tests/serve_images.rs#real_sd_turbo_generates_a_picture
        - crates/alkahest-engine/tests/whisper_cpp.rs#command_line_with_and_without_ffmpeg
        - crates/alkahest-engine/tests/sd_cpp.rs#command_line_in_order_with_extra_args_last
        - crates/alkahest-engine/tests/sd_cpp.rs#multi_file_models_pass_their_components
        - crates/alkahest-api/tests/ollama.rs#create_a_multi_file_diffusion_model
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
    trace:
      code:
        - crates/alkahest-api/src/admin/mod.rs#ROUTES
        - crates/alkahest-api/src/admin/mod.rs#openapi_document
        - crates/alkahest-api/src/admin/context.rs#AdminContext
        - crates/alkahest-api/src/admin/jobs.rs#JobManager
        - crates/alkahest-api/src/admin/models.rs
        - crates/alkahest-api/src/admin/pulls.rs
        - crates/alkahest-api/src/admin/discover.rs
        - crates/alkahest-api/src/admin/engines.rs#RecipeEngineManager
        - crates/alkahest-api/src/admin/keys.rs
        - crates/alkahest-api/src/admin/settings.rs
        - crates/alkahest-api/src/admin/system.rs
        - crates/alkahest-client/src/admin.rs
        - crates/alkahest-api/src/admin/discover.rs#ollama_tags
        - crates/alkahest-api/src/admin/discover.rs#hf_estimate
        - crates/alkahest-api/src/admin/engines.rs#put_override
        - crates/alkahest-api/src/admin/engines.rs#reload
        - crates/alkahest-api/src/admin/pulls.rs#job_log
        - crates/alkahest-api/src/admin/runtime.rs#requests
        - crates/alkahest-api/src/admin/runtime.rs#kill
        - crates/alkahest-api/src/feed.rs#RequestFeed
        - crates/alkahest-client/src/library_tags.rs
        - crates/alkahest-client/src/engine_admin.rs
        - crates/alkahest-client/src/monitor.rs
        - crates/alkahest-client/src/hf_estimate.rs
      tests:
        - crates/alkahest-api/tests/admin_api.rs#openapi
        - crates/alkahest-api/tests/admin_api.rs#admin_routes_need_the_admin_scope_remotely_and_nothing_on_loopback
        - crates/alkahest-api/tests/admin_api.rs#catalog_lists_kind_source_engine_context_paths_and_runtime
        - crates/alkahest-api/tests/admin_api.rs#pull_job_reports_progress_completes_and_streams_job_events
        - crates/alkahest-api/tests/admin_api.rs#engines_list_install_job_and_remove
        - crates/alkahest-api/tests/admin_api.rs#create_key_shows_the_secret_once_and_listing_never_does
        - crates/alkahest-api/tests/admin_api.rs#network_settings_are_saved_for_the_next_start
        - crates/alkahest-api/src/admin/mod.rs#openapi_describes_every_route
        - crates/alkahest-api/tests/admin_api.rs#engine_removal_is_refused_while_a_loaded_model_uses_it
        - crates/alkahest-api/tests/admin_api.rs#engine_admin_log_reload_and_override
        - crates/alkahest-api/tests/admin_api.rs#hf_estimate_reads_the_remote_header
        - crates/alkahest-api/tests/admin.rs#request_feed_records_inference_calls_without_content
        - crates/alkahest-api/tests/admin.rs#utilization_and_forced_kill
        - crates/alkahest-client/tests/client.rs#monitor_feed_utilization_and_kill
  R7:
    status: backlog
    trace:
      code:
        - crates/alkahest-core/src/config.rs#ServerConfig::ollama_bind
        - crates/alkahest-api/src/server.rs#bind_ollama_port
        - crates/alkahest-api/src/server.rs#Server::also_listen
        - crates/alkahest/src/serve_cmd.rs#serve
        - crates/alkahest/src/config_set_cmd.rs#set
        - crates/alkahest-desktop/src/keys.rs#ollama_port_line
      tests:
        - crates/alkahest/tests/serve.rs#free_ollama_port_is_served_too
        - crates/alkahest/tests/serve.rs#busy_ollama_port_is_skipped_without_error
        - crates/alkahest/tests/serve.rs#port_saved_with_config_set_is_bound_next_start
        - crates/alkahest-core/src/config.rs#ollama_bind_only_when_enabled_and_on_another_port
        - crates/alkahest-api/src/admin/settings.rs#ollama_compat_is_patched_and_written
        - crates/alkahest-desktop/src/keys.rs#ollama_compat_toggle_saves_and_explains_the_port
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

### ALK-SP-0005.R7 — Ollama port impersonation

WHERE `server.ollama_compat` is enabled, the system SHALL additionally serve the same API on the same host at `server.ollama_port` (default 11434), so Ollama clients work without reconfiguration; WHEN that port cannot be bound (Ollama or another program holds it), the system SHALL start normally on its own port only, log that the Ollama port was skipped, and report it in the network settings. The Alkahest port (`server.port`, default 11435) SHALL be changeable from the CLI and the desktop app.

#### Scenario: free Ollama port is served too
- GIVEN `ollama_compat = true` and port 11434 free
- WHEN the server starts
- THEN it answers on both the Alkahest port and 11434

#### Scenario: busy Ollama port is skipped without error
- GIVEN `ollama_compat = true` and port 11434 held by another process
- WHEN the server starts
- THEN it starts successfully on the Alkahest port only and reports the Ollama port as not acquired

#### Scenario: port changed from the CLI
- WHEN the user runs `alkahest config set server.port=11500`
- THEN the user configuration file holds the port and the next `alkahest serve` binds it
