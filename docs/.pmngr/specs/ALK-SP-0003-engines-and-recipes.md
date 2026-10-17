---
id: ALK-SP-0003
type: spec
title: Engines and recipes
status: backlog
labels: [engine, recipes]
created: 2026-10-16T22:37:34Z
updated: 2026-10-17T00:09:41Z
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
