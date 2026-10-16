---
id: ALK-US-0025
type: story
title: llama-server engine adapter
status: backlog
priority: critical
parent: ALK-EP-0005
milestone: ALK-M-0003
labels: [engine, llamacpp]
estimate: 5
created: 2026-10-16T22:39:55Z
updated: 2026-10-16T22:40:01Z
links:
  - { kind: implements, target: ALK-SP-0003.R2 }
  - { kind: implements, target: ALK-SP-0010.R2 }
---

## Description

As a user, I want GGUF models (including Ollama blobs) served by llama-server with my model settings.

## Acceptance Criteria

- Argument builder: -m, -c, -np, -ngl, --cache-type-k/v, -fa, --mmproj, --jinja, --chat-template-kwargs, --no-webui
- Serves Ollama blobs in place
- Request translation to llama-server /v1 and /completion

## Notes

Implements [[ALK-SP-0003.R2]], [[ALK-SP-0010.R2]]. Plan: [[plan/alkahest-v1-plan]]
