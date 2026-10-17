---
id: ALK-US-0055
type: story
title: Embeddings and rerank
status: in_progress
priority: high
parent: ALK-EP-0011
milestone: ALK-M-0004
labels: [engine, embeddings]
estimate: 3
created: 2026-10-16T22:42:08Z
updated: 2026-10-17T02:30:48Z
started: 2026-10-17T02:30:48Z
links:
  - { kind: implements, target: ALK-SP-0005.R4 }
---

## Description

As a RAG user, I want embedding and rerank models served efficiently.

## Acceptance Criteria

- llama-server --embedding / --reranking modes
- /v1/embeddings, /api/embed, /v1/rerank

## Notes

Implements [[ALK-SP-0005.R4]]. Plan: [[plan/alkahest-v1-plan]]
