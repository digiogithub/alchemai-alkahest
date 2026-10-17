---
id: ALK-US-0120
type: story
title: "Model catalog: entries, search and pull from git catalogs"
status: backlog
priority: high
parent: ALK-EP-0015
milestone: ALK-M-0006
author: mcp
labels: [catalog, models, store]
estimate: 8
created: 2026-10-17T18:30:04Z
updated: 2026-10-17T18:42:00Z
links:
  - { kind: implements, target: ALK-SP-0011.R7 }
  - { kind: implements, target: ALK-SP-0011.R12 }
  - { kind: implements, target: ALK-SP-0011.R13 }
  - { kind: implements, target: ALK-SP-0011.R14 }
---

## Description

Models that live outside Hugging Face and the Ollama registry are described in `models/*.toml` of a
git catalog and installed by Alkahest like any other model.

Entry (draft, final in the format spec):

```toml
name = "acme/assistant"            # catalog name, referenced as catalog:acme/assistant:<tag>
description = "..."
license = "apache-2.0"
tags = ["chat", "tools"]

[[variants]]
tag = "q4_k_m"                     # default tag = first variant
engine = "llama-cpp"               # optional; ALK-SP-0003.R2 selection when absent
template = "chatml"                # or an inline Go/Jinja template
params = { num_ctx = 8192, temperature = 0.7 }
[[variants.files]]
kind = "model"                     # model | projector | adapter | vae | ...
url = "https://example.org/assistant-q4_k_m.gguf"
sha256 = "..."
size = 4370000000
```

Behaviour:

- `alkahest pull catalog:<name>[:<tag>]` downloads every file with `alkahest-fetch` (resumable,
  parallel, digest-checked), writes blobs and a manifest into the Alkahest store (never the Ollama
  store) with engine, template and params as config layers.
- `alkahest search --source catalog` (and `all`) lists catalog models with size, quant and licence.
- Mirrors: a file may list several `urls`; the downloader falls back in order.
- Authenticated URLs: optional `auth = "<host>"` resolved from the keyring.

## Acceptance Criteria

- Pull of a fixture catalog model over a local HTTP server produces a model `alkahest ls`, `show`
  and `run` handle; a wrong digest fails and leaves no partial model.
- `catalog:` references parse in the store reference grammar; `ls` shows the origin.
- Search ranks catalog entries with the existing HF/Ollama results; `--source catalog` filters.
- Admin API exposes catalog search so the desktop Discover screen can use it later.
- Tests named after the spec scenarios; network tests behind `ALKAHEST_TEST_NET=1`.
