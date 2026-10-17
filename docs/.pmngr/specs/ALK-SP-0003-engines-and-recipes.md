---
id: ALK-SP-0003
type: spec
title: Engines and recipes
status: backlog
labels: [engine, recipes]
created: 2026-10-16T22:37:34Z
updated: 2026-10-17T00:54:11Z
requirements:
  R1:
    status: backlog
    trace:
      code:
        - crates/alkahest-engine/src/engine.rs#Engine
        - crates/alkahest-engine/src/capability.rs#Capabilities
        - crates/alkahest-engine/src/capability.rs#kind_capabilities
        - crates/alkahest-engine/src/launch.rs#LaunchSpec
        - crates/alkahest-engine/src/builtin.rs#LlamaCppEngine
        - crates/alkahest-engine/src/registry.rs#EngineRegistry
        - crates/alkahest-engine/src/supervisor/mod.rs#EngineHandle
      tests:
        - crates/alkahest-engine/tests/registry.rs#capabilities_drive_routing
        - crates/alkahest-engine/tests/registry.rs#registry_holds_every_builtin_engine
        - crates/alkahest-engine/tests/registry.rs#llama_server_spec_from_request
        - crates/alkahest-engine/tests/supervisor.rs#engine_becomes_ready_and_stops_cleanly
  R2:
    status: backlog
    trace:
      code:
        - crates/alkahest-engine/src/select.rs#select_engine
        - crates/alkahest-engine/src/select.rs#Platform
        - crates/alkahest-engine/src/registry.rs#EngineRegistry::plan
        - crates/alkahest-engine/src/error.rs#EngineError
        - crates/alkahest-engine/src/llama_cpp.rs#LlamaCppEngine
      tests:
        - crates/alkahest-engine/tests/selection.rs#bonsai_routing
        - crates/alkahest-engine/tests/selection.rs#rules_by_format_kind_and_quant
        - crates/alkahest-engine/tests/selection.rs#per_model_override_wins
        - crates/alkahest-engine/tests/selection.rs#vllm_only_where_supported
        - crates/alkahest-engine/tests/selection.rs#not_installed_names_the_install_command
        - crates/alkahest-engine/tests/selection.rs#agrees_with_the_catalog_default_engine
        - crates/alkahest-engine/tests/llama_cpp.rs#real_llama_server_serves_and_stops
  R3:
    status: backlog
  R4:
    status: backlog
  R5:
    status: backlog
    trace:
      code:
        - crates/alkahest-engine/src/locator.rs#FsLocator
        - crates/alkahest-engine/src/locator.rs#split_install_dir
        - crates/alkahest-engine/src/launch.rs#LaunchSpec::prepend_library_dirs
        - crates/alkahest-engine/src/select.rs#parse_engine_setting
      tests:
        - crates/alkahest-engine/tests/registry.rs#two_versions_default_newest_and_pin
        - crates/alkahest-engine/tests/registry.rs#variant_follows_accelerator
        - crates/alkahest-engine/tests/supervisor.rs#library_path_reaches_the_child
        - crates/alkahest-engine/tests/registry.rs#bundled_roots_are_searched_after_the_engines_dir
  R6:
    status: backlog
    trace:
      code:
        - crates/alkahest-recipes/src/install.rs#install
        - crates/alkahest-recipes/src/install.rs#locate
        - crates/alkahest-recipes/src/recipe.rs#Recipe
        - recipes/llama-cpp-cpu.toml
        - crates/alkahest/src/engines_cmd.rs#run
      tests:
        - crates/alkahest-recipes/tests/install.rs#install_unpacks_strips_the_wrapper_and_keeps_the_executable_bit
        - crates/alkahest-recipes/tests/install.rs#real_cpu_llama_server_runs
        - crates/alkahest-recipes/tests/install.rs#locate_prefers_the_newest_tag_and_the_data_dir_on_a_tie
        - crates/alkahest/tests/engines.rs#ls_shows_an_installed_engine_once_with_its_path
---

## Purpose

Run each model on the engine that supports it, and install engine builds for the accelerators of the host from declarative recipes.

## Scope

Covers the engine abstraction, selection rules and the recipe format and manager. Scheduling is [[ALK-SP-0004]].

Plan: [[plan/alkahest-v1-plan]]

## Requirements

### ALK-SP-0003.R1 — Engine abstraction

Each engine SHALL declare its capabilities (chat, completion, embeddings, rerank, vision, speech-to-text, text-to-speech, image, decision), build a launch command from a model and its configuration, expose a health check and stop cleanly.

#### Scenario: capabilities drive routing
- **WHEN** an embeddings request targets a chat-only engine
- **THEN** the API answers 400 explaining the model does not support embeddings

### ALK-SP-0003.R2 — Engine selection

The system SHALL select the engine from the model format and quantization: PQ2_0 and PTQ1_0 GGUF to the PrismML llama.cpp engine, other GGUF to mainline llama.cpp, safetensors to vLLM on Linux with NVIDIA and on macOS with Apple Silicon (M1 to M4, aarch64) where vLLM supports it; a per-model `engine` setting SHALL override the rule.

#### Scenario: bonsai routing
- **WHEN** a Ternary-Bonsai-2 PQ2_0 model is requested
- **THEN** the PrismML engine is launched, not mainline llama.cpp

### ALK-SP-0003.R3 — Recipe format

A recipe SHALL be a TOML file naming the engine, a pinned version and variants per accelerator (cpu, cuda12, cuda13, vulkan, rocm, metal), each with either a prebuilt artifact URL template plus sha256 or build-from-source steps (repository, ref, cmake flags).

#### Scenario: recipe validation
- **WHEN** a recipe without a pinned version is loaded
- **THEN** it is rejected with a precise error

### ALK-SP-0003.R4 — Hardware-aware install

`alkahest engine install <recipe>` SHALL pick the best variant for the detected hardware (CUDA > Vulkan > CPU on NVIDIA; ROCm > Vulkan > CPU on AMD; Metal on Apple), fall back to CPU, and accept `--variant` and `--build` overrides.

#### Scenario: cuda pick
- **WHEN** an NVIDIA GPU with driver for CUDA 12.8 is present
- **THEN** the cuda12 variant is installed

### ALK-SP-0003.R5 — Versioned side-by-side installs

Engines SHALL install into `engines/<name>/<version>-<variant>/`, several versions SHALL coexist, and the child process SHALL get the library search path (`LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH` or `PATH`) of its install directory.

#### Scenario: two versions
- **WHEN** two llama.cpp versions are installed
- **THEN** each model uses the version its configuration pins, default the newest

### ALK-SP-0003.R6 — CPU fallback in the base package

The base package SHALL ship a CPU-only llama.cpp build so a fresh install can serve GGUF models without any recipe.

#### Scenario: fresh install
- **WHEN** Alkahest is installed on a host without GPU
- **THEN** a GGUF chat model answers without running `engine install`
