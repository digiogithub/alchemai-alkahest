---
id: ALK-SP-0010
type: spec
title: Model classification and optimization defaults
status: backlog
labels: [gguf, config]
created: 2026-10-16T22:37:40Z
updated: 2026-10-16T23:12:39Z
requirements:
  R1:
    status: backlog
    trace:
      code: [crates/alkahest-gguf/src/classify.rs#classify]
      tests: [crates/alkahest-gguf/tests/classify.rs#known_models_table]
  R2:
    status: backlog
    trace:
      code:
        - crates/alkahest-gguf/src/accessors.rs#Gguf
        - crates/alkahest-gguf/src/reader.rs#Gguf
      tests:
        - crates/alkahest-gguf/tests/gguf_reader.rs#template_gives_tools_and_thinking
        - crates/alkahest-gguf/tests/gguf_reader.rs#reads_v3_header_metadata_and_tensors
  R3:
    status: backlog
  R4:
    status: backlog
    trace:
      code: [crates/alkahest-core/src/config.rs#Config]
      tests: [crates/alkahest-core/src/config.rs#env_vars_override_file_values]
  R5:
    status: backlog
---

## Purpose

Know what each model is and run it with sensible, recommended settings by default.

## Scope

Covers kind classification, defaults from metadata and model cards, the per-model configuration overlay and memory estimation.

Plan: [[plan/alkahest-v1-plan]]

## Requirements

### ALK-SP-0010.R1 — Kind classification

The system SHALL classify each model as chat, completion, embeddings, rerank, vision, speech-to-text, text-to-speech, image, video or decision, and its format as GGUF or vLLM-ready safetensors, from GGUF metadata, Ollama config and HF `pipeline_tag` and library.

#### Scenario: embedding
- **WHEN** nomic-embed-text is classified
- **THEN** kind is embeddings

### ALK-SP-0010.R2 — Defaults from GGUF metadata

The system SHALL derive trained context length, chat template, tool-call support and thinking support from GGUF metadata.

#### Scenario: template
- **WHEN** a GGUF with `tokenizer.chat_template` is loaded
- **THEN** the engine is started with jinja templating

### ALK-SP-0010.R3 — Recommended parameters from model cards

WHEN a model comes from Hugging Face, the system SHALL read `generation_config.json` and recommended settings from the model card and store them as the model's recommended defaults.

#### Scenario: sampling defaults
- **WHEN** a Qwen3 GGUF is pulled
- **THEN** temperature, top_p, top_k and min_p defaults match the card

### ALK-SP-0010.R4 — Configuration overlay and precedence

Each model SHALL have an editable configuration file, and effective settings SHALL resolve request > model configuration > recommended defaults > global configuration.

#### Scenario: precedence
- **WHEN** a request sets `num_ctx` and the model config sets another value
- **THEN** the request value is used

### ALK-SP-0010.R5 — Memory estimation

The system SHALL estimate RAM and VRAM for a model from weights size, layers offloaded, context length, KV cache types and parallel slots, within 15% of measured usage on the reference models.

#### Scenario: kv q8
- **WHEN** KV cache switches from f16 to q8_0
- **THEN** the KV part of the estimate halves
