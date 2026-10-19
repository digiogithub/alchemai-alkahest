---
id: ALK-SP-0003
type: spec
title: Engines and recipes
status: backlog
labels: [engine, recipes]
created: 2026-10-16T22:37:34Z
updated: 2026-10-19T15:33:31Z
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
        - crates/alkahest-engine/src/registry.rs#EngineRegistry::resolve_engine
        - crates/alkahest-engine/src/error.rs#EngineError
        - crates/alkahest-engine/src/llama_cpp.rs#LlamaCppEngine
        - crates/alkahest-engine/src/vllm.rs#VllmEngine
        - crates/alkahest-recipes/src/build.rs#build_venv
        - crates/alkahest-recipes/src/recipe.rs#VenvBuild
        - crates/alkahest-sched/src/safetensors.rs#estimate_vllm
        - recipes/vllm.toml
      tests:
        - crates/alkahest-engine/tests/selection.rs#bonsai_routing
        - crates/alkahest-engine/tests/selection.rs#rules_by_format_kind_and_quant
        - crates/alkahest-engine/tests/selection.rs#per_model_override_wins
        - crates/alkahest-engine/tests/selection.rs#vllm_only_where_supported
        - crates/alkahest-engine/tests/selection.rs#not_installed_names_the_install_command
        - crates/alkahest-engine/tests/selection.rs#agrees_with_the_catalog_default_engine
        - crates/alkahest-engine/tests/llama_cpp.rs#real_llama_server_serves_and_stops
        - crates/alkahest-engine/tests/vllm.rs#full_command_line_in_order
        - crates/alkahest-engine/tests/vllm.rs#platform_rules_name_what_is_missing
        - crates/alkahest-engine/tests/vllm.rs#real_vllm_serves_a_snapshot
        - crates/alkahest-recipes/tests/venv.rs#venv_install_with_uv_creates_the_environment_in_place
        - crates/alkahest-recipes/tests/venv.rs#venv_install_falls_back_to_python_and_pip_without_uv
        - crates/alkahest-recipes/tests/venv.rs#builtin_vllm_recipe_is_pinned_and_follows_the_hardware
        - crates/alkahest-engine/tests/forks.rs#automatic_selection_never_picks_a_fork
        - crates/alkahest-engine/tests/forks.rs#an_engine_setting_naming_the_fork_runs_it
        - crates/alkahest-engine/tests/forks.rs#a_fork_that_is_not_installed_names_its_install_command
        - crates/alkahest-engine/tests/forks.rs#a_fork_is_never_taken_from_path
        - crates/alkahest-engine/tests/forks.rs#an_unknown_engine_is_refused_with_the_known_ones
        - crates/alkahest-engine/tests/forks.rs#an_installed_fork_runs_without_its_recipe
  R3:
    status: backlog
    trace:
      code:
        - crates/alkahest-recipes/src/recipe.rs#parse_recipes
        - crates/alkahest-recipes/src/catalog.rs#Catalog
      tests:
        - crates/alkahest-recipes/tests/recipes.rs#recipe_validation_rejects_a_recipe_without_a_pinned_version
        - crates/alkahest-recipes/tests/recipes.rs#recipe_validation_names_the_broken_rule
        - crates/alkahest-recipes/tests/recipes.rs#discovery_layers_directories_over_the_builtin_recipes
  R4:
    status: backlog
    trace:
      code:
        - crates/alkahest-recipes/src/recipe.rs#select_variant
        - crates/alkahest-recipes/src/build.rs#build_from_source
        - crates/alkahest/src/engines_cmd.rs#install_cmd
        - crates/alkahest-desktop/src/engines.rs#cards
        - crates/alkahest-desktop/src/views/engines.rs#EnginesView
      tests:
        - crates/alkahest-recipes/tests/recipes.rs#cuda_pick_chooses_the_variant_the_driver_supports
        - crates/alkahest-recipes/tests/recipes.rs#variant_pick_follows_the_hardware_preference_order
        - crates/alkahest-recipes/tests/recipes.rs#source_build_runs_the_steps_logs_and_keeps_only_the_products
        - crates/alkahest-desktop/src/engines.rs#cuda_pick
  R5:
    status: backlog
    trace:
      code:
        - crates/alkahest-engine/src/locator.rs#FsLocator
        - crates/alkahest-engine/src/locator.rs#split_install_dir
        - crates/alkahest-engine/src/launch.rs#LaunchSpec::prepend_library_dirs
        - crates/alkahest-engine/src/select.rs#parse_engine_setting
        - crates/alkahest-recipes/src/install.rs#install
        - crates/alkahest-recipes/src/install.rs#remove_installed
      tests:
        - crates/alkahest-engine/tests/registry.rs#two_versions_default_newest_and_pin
        - crates/alkahest-engine/tests/registry.rs#variant_follows_accelerator
        - crates/alkahest-engine/tests/supervisor.rs#library_path_reaches_the_child
        - crates/alkahest-engine/tests/registry.rs#bundled_roots_are_searched_after_the_engines_dir
        - crates/alkahest-recipes/tests/recipes.rs#two_versions_coexist_and_the_pin_or_newest_is_used
        - crates/alkahest-recipes/tests/recipes.rs#remove_deletes_only_marked_installs_of_the_selected_version
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

The system SHALL select the engine from the model format and quantization: PQ2_0 and PTQ1_0 GGUF to the PrismML llama.cpp engine, other GGUF to mainline llama.cpp, safetensors to vLLM on Linux with NVIDIA and on macOS with Apple Silicon (M1 to M4, aarch64) where vLLM supports it; a per-model `engine` setting SHALL override the rule. Fork engines (recipes with `base`, ADR 0007) SHALL never be selected automatically; the per-model `engine` setting MAY name a fork engine (`<id>[@<version>]`), which then runs through its family's adapter.

#### Scenario: bonsai routing
- **WHEN** a Ternary-Bonsai-2 PQ2_0 model is requested
- **THEN** the PrismML engine is launched, not mainline llama.cpp

#### Scenario: fork engines are opt-in
- **GIVEN** an installed fork engine `llama-cpp-turboquant` with `base = "llama-cpp"`
- **WHEN** a GGUF model without an `engine` setting is requested
- **THEN** mainline llama.cpp is launched, and only a model with `engine=llama-cpp-turboquant` runs on the fork

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
