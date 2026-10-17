---
id: ALK-SP-0008
type: spec
title: Desktop application
status: backlog
labels: [desktop]
created: 2026-10-16T22:37:38Z
updated: 2026-10-17T03:27:50Z
requirements:
  R1:
    status: backlog
    trace:
      code:
        - crates/alkahest/src/main.rs#desktop
        - crates/alkahest-desktop/src/app.rs#run
        - crates/alkahest-desktop/src/views/shell.rs#Shell
        - crates/alkahest-desktop/src/ui/mod.rs#open_main_window
      tests:
        - crates/alkahest/tests/cli.rs#headless_build
        - crates/alkahest-desktop/src/ui/mod.rs#only_ui_names_gpui_kit
        - crates/alkahest-desktop/src/nav.rs#rail_lists_the_five_designed_screens_in_order
        - xtask/src/tokens.rs#alembic_tokens_pass_the_contrast_rules
  R2:
    status: backlog
    trace:
      code:
        - crates/alkahest-desktop/src/connection.rs#Machine
        - crates/alkahest-desktop/src/connection.rs#ConnectionManager
        - crates/alkahest-desktop/src/launcher.rs#LocalLauncher
        - crates/alkahest-desktop/src/settings.rs#DesktopSettings
        - crates/alkahest-client/src/lib.rs#Client::events
        - crates/alkahest-desktop/src/views/shell.rs#Shell::apply
      tests:
        - crates/alkahest-desktop/src/connection.rs#no_server
        - crates/alkahest-desktop/src/connection.rs#starting_polls_until_healthy_or_deadline
        - crates/alkahest-desktop/src/connection.rs#a_failed_launch_is_reported_and_retry_probes_again
        - crates/alkahest-desktop/src/launcher.rs#spawns_a_detached_serve_child
        - crates/alkahest-client/tests/client.rs#events_stream_decodes_status_snapshots
  R3:
    status: backlog
    trace:
      code:
        - crates/alkahest-desktop/src/library.rs#LibraryState
        - crates/alkahest-desktop/src/views/library.rs#LibraryView
        - crates/alkahest-desktop/src/ops.rs#ModelOps
        - crates/alkahest-desktop/src/present.rs#Pill
        - crates/alkahest-client/src/library.rs#Client::library
      tests:
        - crates/alkahest-desktop/src/library.rs#unload
        - crates/alkahest-desktop/src/library.rs#deleting_an_ollama_model_is_refused_with_a_hint
        - crates/alkahest-desktop/src/views/library.rs#rows_render_and_delete_asks_first
        - crates/alkahest-client/tests/library.rs#library_composes_tags_models_and_show
  R4:
    status: backlog
  R5:
    status: backlog
  R6:
    status: backlog
    trace:
      code:
        - crates/alkahest-desktop/src/running.rs#cards
        - crates/alkahest-desktop/src/running.rs#tiles
        - crates/alkahest-desktop/src/views/running.rs#RunningView
        - crates/alkahest-desktop/src/connection.rs#RUNTIME_POLL
        - crates/alkahest-client/src/lib.rs#RuntimeModel
      tests:
        - crates/alkahest-desktop/src/running.rs#activity
        - crates/alkahest-desktop/src/connection.rs#runtime_is_polled_every_two_seconds_while_connected
        - crates/alkahest-desktop/src/views/running.rs#live_snapshot_renders_cards_and_stop_all_asks_first
        - crates/alkahest-client/tests/library.rs#runtime_status_reads_the_full_snapshot
  R7:
    status: backlog
---

## Purpose

Manage models, engines, keys and monitor runtime from a native desktop app started with `alkahest desktop`.

## Scope

Covers the GPUI app on Linux, macOS and Windows. It talks to the server over the admin API ([[ALK-SP-0005]]).

Plan: [[plan/alkahest-v1-plan]]

## Requirements

### ALK-SP-0008.R1 — Desktop entry point

`alkahest desktop` SHALL open the GPUI window on Linux, macOS and Windows; a binary built without the `desktop` feature SHALL print how to get the desktop build and exit non-zero.

#### Scenario: headless build
- **WHEN** a server-only build runs `alkahest desktop`
- **THEN** it prints a clear message and exits 2

### ALK-SP-0008.R2 — Server connection

The app SHALL connect to the configured server (local service by default) and offer to start a server when none answers.

#### Scenario: no server
- **WHEN** no server is running
- **THEN** the app offers to start one and connects after it is healthy

### ALK-SP-0008.R3 — Model catalog view

The app SHALL list local models from all sources with kind badges (chat, embeddings, vision, audio, image, video, decision, rerank), format (GGUF, vLLM-ready), size and state, and allow delete, load and unload.

#### Scenario: unload
- **WHEN** the user clicks unload on a loaded model
- **THEN** the model process stops and the row updates

### ALK-SP-0008.R4 — Model browsers

The app SHALL search Hugging Face and the Ollama library, show model cards, let the user pick a quantization or tag and download to the store with progress, pause and resume.

#### Scenario: download
- **WHEN** the user downloads a HF GGUF
- **THEN** progress is shown and the model appears in the catalog

### ALK-SP-0008.R5 — Model settings editor

The app SHALL edit per-model settings: context window, KV cache key and value types, flash attention, GPU layers, parallel slots, keep-alive, chat template, tools enablement and sampling defaults, prefilled from the HF model card and GGUF metadata, with a live memory estimate.

#### Scenario: estimate
- **WHEN** the user doubles the context window
- **THEN** the memory estimate updates

### ALK-SP-0008.R6 — Runtime monitor

The app SHALL show loaded models with CPU/GPU placement, RAM and VRAM use, in-flight and queued requests, refreshed at least every 2 seconds.

#### Scenario: activity
- **WHEN** a client streams a completion
- **THEN** the in-flight count is 1 during the stream

### ALK-SP-0008.R7 — Keys and network settings

The app SHALL create, list and revoke API keys and toggle external exposure (bind address).

#### Scenario: create key
- **WHEN** the user creates a key
- **THEN** the secret is shown once with a copy button
