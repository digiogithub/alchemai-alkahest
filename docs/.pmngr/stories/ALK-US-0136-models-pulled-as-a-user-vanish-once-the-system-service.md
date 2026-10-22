---
id: ALK-US-0136
type: story
title: Models pulled as a user vanish once the system service takes the port
status: in_review
priority: high
parent: ALK-EP-0009
milestone: ALK-M-0003
author: mcp
labels: [service, store, cli, bug]
estimate: 3
created: 2026-10-22T21:06:27Z
updated: 2026-10-22T21:12:08Z
started: 2026-10-22T21:06:27Z
---

## Description

Reported on 2026-10-22: Bonsai 2 models pulled with Alkahest "disappeared" after installing the latest .deb.

Nothing was deleted. The models are still in the user store (`~/.local/share/alkahest/models`, 6.1 GB: Ternary-Bonsai-2-9B PQ2_0, Ternary-Bonsai-2-4B PTQ1_0, MiniCPM5-2B, with their per-model settings under `model-config/`). The system service installed with `sudo alkahest service install` runs as the `alkahest` user with `ALKAHEST_HOME=/var/lib/alkahest`, so it has its own, empty store.

When the service was installed (2026-10-18), the user's own `alkahest serve` still held 127.0.0.1:11435. The service crash-looped (`cannot listen on 127.0.0.1:11435: Address already in use`, restart every 2 s) without anybody noticing. After the next reboot the service won the port, and from then on the desktop and CLI talked to a server whose store does not hold the user's models.

Two gaps:

1. There is no way to move models from one Alkahest store to another. `cp` only copies from the Ollama store, and `import` takes GGUF files or model folders.
2. `service install` says nothing when the installing user already has models in their own store, or when the port is already taken.

## Acceptance Criteria

- `alkahest import --store <DIR>` copies every model of another Alkahest store into this one. `<DIR>` is the data folder or its `models/` folder.
  - Blobs are verified against their digests.
  - A model already present with the same manifest is skipped.
  - The per-model settings (`model-config/*.toml`, `*.recommended.json`) come along when the destination has none.
  - The source is never modified.
- Run as root into a store owned by another user (the service store), the new files and directories get the store owner's uid and gid, so the service can read and later delete them.
- `--json` prints a summary. Human output prints one line per model (`imported`, `already present`, `failed: …`).
- System `service install` (Linux):
  - Warns when the port is already in use (a user server is still running), and names the command to stop it.
  - When the invoking user (`SUDO_USER`) has models in their own store, prints the exact `import --store` command that copies them into the service store.
- Tests cover the import, the skip, the settings, the source left untouched, and the install hint.

## Notes

Engines built or installed as the user (`~/.local/share/alkahest/engines`, for example prism-llama-cpp) are not copied. The service installs its own engines on first use or with `alkahest engines install`.
