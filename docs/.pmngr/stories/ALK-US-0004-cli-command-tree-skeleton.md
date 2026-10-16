---
id: ALK-US-0004
type: story
title: CLI command tree skeleton
status: in_progress
priority: high
parent: ALK-EP-0001
milestone: ALK-M-0001
labels: [cli]
estimate: 2
created: 2026-10-16T22:38:14Z
updated: 2026-10-16T23:07:00Z
started: 2026-10-16T23:07:00Z
---

## Description

As a user, I want an Ollama-like command set (`serve`, `pull`, `ls`, `ps`, `rm`, `show`, `cp`, `run`, `stop`, `search`, `engine`, `keys`, `service`, `desktop`, `sysinfo`, `config`), so that the tool feels familiar.

## Acceptance Criteria

- All commands appear in `--help`
- Every listing command supports `--json`
- Shell completions generated
