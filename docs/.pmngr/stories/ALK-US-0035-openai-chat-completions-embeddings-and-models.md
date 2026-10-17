---
id: ALK-US-0035
type: story
title: OpenAI chat, completions, embeddings and models
status: done
priority: critical
parent: ALK-EP-0007
milestone: ALK-M-0003
labels: [api, openai]
estimate: 5
created: 2026-10-16T22:40:42Z
updated: 2026-10-17T01:47:16Z
started: 2026-10-17T01:11:43Z
closed: 2026-10-17T01:47:16Z
links:
  - { kind: implements, target: ALK-SP-0005.R2 }
---

## Description

As a user of OpenAI SDKs and tools, I want Alkahest to be a drop-in base URL.

## Acceptance Criteria

- SSE streaming, tools, response_format, image_url
- Usage token counts
- Tested with the official openai SDK

## Notes

Implements [[ALK-SP-0005.R2]]. Plan: [[plan/alkahest-v1-plan]]
