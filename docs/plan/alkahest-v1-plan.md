# Alkahest v1 — analysis and project plan

Status: draft, 2026-10-17. Source: `initial_requirements.md` (product owner, Spanish),
analysis of the reference repositories listed below. Backlog lives in gintrack, project `ALK`.

## 1. Goal

Alkahest is an easy-to-use platform to deploy and use LLMs and other AI models on-premise
and locally. One compact Rust binary provides:

- an Ollama-style CLI (`pull`, `ls`, `ps`, `rm`, `show`, `cp`, `run`, `stop`, `serve`);
- model download from the Ollama registry and Hugging Face (GGUF and safetensors for vLLM),
  with optional HF token;
- read access to models already pulled by a local Ollama install;
- a service that starts on boot, loads models on demand and unloads them after an idle
  keep-alive;
- several engines per model kind: llama.cpp (mainline), PrismML llama.cpp fork (Bonsai
  ternary PQ2_0/PTQ1_0), vLLM, embeddings, vision, audio, image generation, decision models;
- one unified API: Ollama native `/api/*`, OpenAI `/v1/*`, Anthropic `/v1/messages`,
  decision API `/v1/systemone` + `/v1/decisions`;
- `alkahest desktop`: a GPUI desktop app for model management and monitoring.

Out of scope for v1: web chat, web UI (LocalAI-style), fine-tuning, distributed/P2P mode.

## 2. Findings from the reference repositories

### Ollama (`/www/MCP/Alchemai/Alkahest-refs/ollama`, HEAD d9b86018)

- Store: `$OLLAMA_MODELS` (default `~/.ollama/models`; service install uses
  `/usr/share/ollama/.ollama/models`). Blobs `blobs/sha256-<hex>`. Manifests in
  `manifests-v2/<host>/<ns>/<model>/<tag>` first, then legacy `manifests/...`; default host
  `registry.ollama.ai` (also `ollama.com`), namespace `library`, tag `latest`.
- Manifest = Docker distribution v2 JSON; layers `application/vnd.ollama.image.{model,
  projector,adapter,template,system,params,messages,license,draft,embed}`. Config blob
  `ConfigV2` carries family, capabilities, context_length, renderer, parser.
- Pull: `GET /v2/<ns>/<model>/manifests/<tag>`, blobs via CDN redirect, up to 16 parallel
  Range parts (100 MB–1 GB), `-partial` files with per-part state for resume, sha256 check.
- Scheduler: on-demand load, `OLLAMA_KEEP_ALIVE` default 5m (negative = forever, 0 = now),
  per-request `keep_alive`, `OLLAMA_MAX_LOADED_MODELS` (0 = 3 x GPUs), `OLLAMA_NUM_PARALLEL`,
  queue `OLLAMA_MAX_QUEUE` 512, VRAM prediction vs 80% of free memory, eviction of idle
  runner first. `ps` columns NAME, ID, SIZE, PROCESSOR (CPU/GPU %), CONTEXT, RUNNER, UNTIL.
- Engine: all GGUF models run on an upstream `llama-server` subprocess (pinned b11669) with
  `--port <rand> --host 127.0.0.1 --no-webui -c -np --cache-type-k/v --flash-attn`, health
  polling, `LD_LIBRARY_PATH` set for the child. A compat layer patches old Ollama GGUFs.
- Templates: GGUF jinja via llama-server `--jinja`, or Go-side renderer/Go TEMPLATE layer.
- API: native `/api/*`, OpenAI `/v1/chat/completions|completions|embeddings|models|responses`,
  Anthropic `/v1/messages`, decision API `POST /v1/systemone` (question types
  `noul`/`choice`/`score`, logit scoring, no text generation; encodings tev1, kev, clef,
  strands; llama-server launched with `-np 16 --kv-unified --cache-ram 0`).
- Service: systemd unit, `ollama` user in `render`/`video` groups, `Restart=always`.

### LocalAI (`/www/MCP/Alchemai/Alkahest-refs/LocalAI`)

- Backends are separate gRPC processes (`backend/backend.proto`): LoadModel, Predict(Stream),
  Embedding, Rerank, GenerateImage, AudioTranscription, TTS, Status (memory), ModelMetadata.
- Backend gallery with meta backends mapping hardware capability to concrete builds
  (cpu, cuda12, cuda13, rocm, sycl, vulkan, metal, l4t) — the model for our "recipes".
  Includes a `bonsai` backend built from PrismML-Eng/llama.cpp.
- Per-model YAML config (backend, context_size, gpu_layers, cache_type_k/v, flash_attention,
  mmproj, template, known_usecases); GGUF auto-defaults (context from header, jinja template).
- HF download via `hf://`, `HF_TOKEN`, `HF_ENDPOINT`; importer picks GGUF quant (default
  q4_k_m) and mmproj.
- Watchdog: idle and busy timeouts, LRU max active backends, memory reclaimer at 80%.
- Usecase flags (chat, embeddings, rerank, image, transcript, tts, vision, video...) for
  model type classification.
- Decisions API: `/v1/systemone`, `/v1/systemone/permute|separate`, `/v1/decisions`; this is
  what the product owner calls the "Jev" API (to confirm, see open questions).

### Bonsai setup (`/www/Pips/llm`)

- llama-swap proxy + PrismML fork `llama-server`, release `prism-b10777-e3de715`, prebuilt
  assets `llama-<tag>-bin-linux-cuda-{12.4,12.8,13.3}-x64.tar.gz`, also vulkan, rocm, cpu,
  macOS. Shared-library layout, needs `LD_LIBRARY_PATH` or `$ORIGIN` RPATH.
- Formats: Q2_0 (mainline), PQ2_0 and PTQ1_0 (fork only). Bonsai 2 also needs Hadamard
  transforms: old runtimes load but output garbage, so engine selection must be by format.
- HF repos `prism-ml/Ternary-Bonsai-2-{2B,4B,9B,27B}-gguf`, files `*-PQ2_0.gguf`,
  `*mmproj-Q8_0.gguf`.
- Reference flags: `-ngl 999 -fa on -c 65536 --jinja`, optional `--cache-type-k/v q4_0`,
  `--chat-template-kwargs '{"enable_thinking":false}'`, ttl 1800 s.

### GPUI apps (Bitacora, Pando Assistant)

- Cargo workspace, edition 2024, toolchain 1.98.1, `resolver = "3"`, exact pins in
  `[workspace.dependencies]`, clippy denies unwrap/expect/dbg, `deny.toml` bans GPL.
- GPUI via crates.io `gpui-kit = "=0.7.1"` behind a `src/ui/` facade; design tokens
  generated by `cargo xtask tokens`; tokio bridge runtime as a GPUI Global.
- Packaging with cargo-packager (deb, AppImage, dmg, NSIS/MSI), Velopack self-update,
  tray (ksni / tray-icon), autostart, keyring for secrets.
- Pando Assistant `crates/models`: manifest-pinned SHA-256 resumable downloader to reuse.

## 3. Architecture

One binary `alkahest`; the GPUI part is behind a cargo feature `desktop` so headless and
server builds do not link Vulkan/X11. The desktop app talks to the local server over its
HTTP API (plus an admin API), never in-process, so it can manage a system service.

```
crates/
  alkahest            bin: clap CLI, `serve`, `desktop` (feature)
  alkahest-core       shared types, config (TOML + env), paths, errors
  alkahest-gguf       GGUF header/metadata reader, quant/format detection
  alkahest-sysinfo    CPU/RAM, NVIDIA (NVML), AMD (ROCm SMI), Vulkan, Metal detection
  alkahest-store      own model store + read-only Ollama store adapter, model catalog
  alkahest-fetch      resumable parallel downloader (Range, sha256, partial state)
  alkahest-ollama     Ollama registry client (manifests, blobs, library search)
  alkahest-hf         Hugging Face Hub client (search, files, model card, token)
  alkahest-engine     Engine trait, process supervisor, engine registry
  alkahest-recipes    engine recipes: download prebuilt / build from source per accelerator
  alkahest-sched      scheduler: on-demand load, keep-alive, LRU, VRAM fit, queues
  alkahest-api        axum server: native, OpenAI, Anthropic, decisions, admin API, auth
  alkahest-decision   decision request compiler + logit scoring (tev1/kev/clef/strands)
  alkahest-desktop    GPUI views (gpui-kit facade), talks to alkahest-api client
  alkahest-client     typed Rust client for the API (used by CLI and desktop)
xtask/                check-deps, tokens, bundle, release
recipes/              TOML recipe files (llama.cpp mainline, prism fork, vllm, whisper, sd)
packaging/            systemd, launchd, Windows service, installers
```

Engines run as subprocesses speaking their own HTTP (llama-server, vLLM OpenAI server,
whisper-server, sd-server). Alkahest proxies and translates; it does not embed inference.

## 4. Milestones

| Id | Name | Due | Exit criteria |
|---|---|---|---|
| M0 | Foundations | 2026-10-30 | workspace, CI, config, GGUF reader, hardware detection |
| M1 | Model management CLI | 2026-11-20 | pull from Ollama and HF, ls/rm/show/cp, Ollama store compat |
| M2 | Serve MVP (CPU) | 2026-12-18 | `serve`, llama-server engine, scheduler, OpenAI + native API, API keys, Linux service |
| M3 | Multi-engine | 2027-01-29 | recipes (CUDA/Vulkan/ROCm/prism), vLLM, embeddings/rerank, vision, audio, image, decisions, Anthropic API |
| M4 | Desktop | 2027-03-05 | `alkahest desktop` with search, download, config, monitoring, keys |
| M5 | v1.0 release | 2027-03-26 | installers for Linux/macOS/Windows, docs, hardening, perf |

## 5. Product owner decisions (2026-10-17)

1. Jev was the first commercial decision-model API; Ollama and LocalAI cloned it as
   `/v1/systemone` so clients move between services transparently. Alkahest implements the
   same contract plus the LocalAI `/v1/decisions` alias (ALK-SP-0007).
2. Milestone dates in section 4 are accepted. Implementation runs with subagents.
3. Default bind `127.0.0.1:11435` (Ollama keeps 11434).
4. vLLM on Linux + NVIDIA, and on macOS Apple Silicon (M1–M4, aarch64) where vLLM supports
   it (spike ALK-T-0212).
5. Licence: MIT.
6. Desktop design hand-off lives in `design/` (Alembic design system, brand, screens 10–14)
   with the master canvas at https://claude.ai/artifact/Fv6snHtv63gBKhtrzvZbKv. Engines,
   Settings and empty/error states are not designed yet (ALK-US-0081).

## 6. Open questions

1. ~~Ollama models stored with Go TEMPLATE layers~~ Settled by ALK-US-0015, see
   [[development/decisions/0001-chat-template-strategy]]: Jinja through llama-server is the
   only v1 template path (per-model override, then GGUF `tokenizer.chat_template`, then a
   built-in llama.cpp template name mapped from the Go template). A Go-template renderer
   in Rust is deferred until a real model needs it.
