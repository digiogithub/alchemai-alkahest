---
id: ALK-SP-0005
type: spec
title: API compatibility surface
status: backlog
labels: [api]
created: 2026-10-16T22:37:35Z
updated: 2026-10-16T22:37:35Z
requirements:
  R1:
    status: backlog
  R2:
    status: backlog
  R3:
    status: backlog
  R4:
    status: backlog
  R5:
    status: backlog
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
