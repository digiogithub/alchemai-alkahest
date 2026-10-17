# Introduction

Alkahest is a local and on-premise AI model server and manager. One compact binary gives you:

- an **Ollama-style command line**: `pull`, `ls`, `ps`, `rm`, `show`, `cp`, `run`, `stop`, `serve`;
- **model downloads** from the Ollama registry and from Hugging Face, resumable and verified;
- **read-only reuse of an existing Ollama store**, so models you already have are not downloaded twice;
- **on-demand loading**: a model starts when the first request arrives and is unloaded after a
  keep-alive period; several models can share the machine, and the scheduler checks memory first;
- **several engines** run as separate processes: llama.cpp, the PrismML fork for Bonsai ternary
  models, vLLM, whisper.cpp and stable-diffusion.cpp;
- **one API** in several dialects: Ollama native, OpenAI (chat, completions, responses,
  embeddings, audio, images), Anthropic Messages, a decision API and an admin API;
- a **desktop app** (`alkahest desktop`) to browse, download, configure and monitor models.

Everything runs on your machine. Nothing is sent anywhere except the downloads you ask for.

## How the pieces fit

```text
 clients (SDKs, curl, IDEs, the desktop app)
          |  HTTP: Ollama / OpenAI / Anthropic / decisions / admin
          v
 alkahest serve  --  scheduler (load, keep-alive, admission, eviction, queues)
          |
          +--> llama-server (llama.cpp)   one process per loaded model
          +--> prism llama-server | vllm | whisper-server | sd-server
          |
 model store (Alkahest) + Ollama store (read-only)
```

`alkahest serve` is the only long-running process. The CLI commands (`ls`, `ps`, `run`, ...)
talk to it over HTTP, and so does the desktop app, which means they work against a remote server
too (`--host`).

## Where to go next

- New here: [Install](install.md), then the [Quick start](quick-start.md).
- Pointing an application at Alkahest: [Serving and security](guide/serving.md) and the
  [API reference](reference/api.md).
- Something is not working: [Troubleshooting](troubleshooting.md).

The plan, specifications and development records live in the `docs/` tree of the repository;
this book is the user-facing part.
