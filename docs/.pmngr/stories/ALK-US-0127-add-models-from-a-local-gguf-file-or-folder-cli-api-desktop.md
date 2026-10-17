---
id: ALK-US-0127
type: story
title: Add models from a local GGUF file or folder (CLI, API, desktop)
status: in_progress
priority: high
parent: ALK-EP-0004
milestone: ALK-M-0006
author: mcp
labels: [models, store, cli, desktop, api]
estimate: 8
created: 2026-10-17T22:55:11Z
updated: 2026-10-17T23:09:53Z
started: 2026-10-17T23:04:24Z
links:
  - { kind: implements, target: ALK-SP-0002.R7 }
  - { kind: implements, target: ALK-SP-0002.R8 }
  - { kind: implements, target: ALK-SP-0002.R9 }
  - { kind: implements, target: ALK-SP-0002.R10 }
---

## Description

Product owner request (2026-10-18): besides pulling by URL / registry / Hugging Face / catalog,
Alkahest must register models from a local file or folder.

- `alkahest create <name> --from <path>` (alias `alkahest import <path> [--name <name>]`):
  - `<path>` a `.gguf` file: one model; template, context length, family and quant read from the
    GGUF metadata (alkahest-gguf); name defaults to the file stem.
  - `<path>` a folder: split GGUF shards (`*-00001-of-0000N.gguf`) become one model; a projector
    (`mmproj*.gguf`) is attached as the vision layer; a `Modelfile` in the folder is honoured
    (FROM, TEMPLATE, PARAMETER, SYSTEM, ADAPTER); a Hugging Face safetensors folder
    (`config.json` + `*.safetensors`) becomes a safetensors model for the vLLM engine.
  - `-f Modelfile` as Ollama's `ollama create -f`, with `FROM ./file.gguf` or `FROM ./folder`.
  - `--link` (default copy): register without copying when the file stays where it is
    (hardlink on the same filesystem, otherwise a referenced external blob recorded with its
    digest and re-verified on load); never moves or modifies the source.
- Admin API: `POST /alkahest/v1/models/import {path, name, link}` as a job with progress
  (hashing, copying), allowed only for keyless loopback callers (local paths); remote callers
  keep using `POST /api/blobs` + `POST /api/create`.
- Desktop Library: "Add from file…" / "Add from folder…" (native file dialog), progress, result;
  designed in the canvas first.
- `alkahest run ./model.gguf` runs a file directly by importing it (or offers to).

## Acceptance Criteria

- Importing a GGUF file and a split/multimodal folder produces models that `ls`, `show`, `run`
  and `serve` handle, with metadata from the GGUF header; digests match the source files.
- Copy and `--link` modes; a linked file that changed or disappeared is reported clearly on load.
- The source files and the Ollama store are never modified (tests).
- CLI reference, user guide and API docs updated; spec ALK-SP-0002 (acquisition) gains the
  requirements and scenarios, which become the tests.

## Notes

Reuses `POST /api/create` / blob logic from [[ALK-US-0087]] where possible.
