---
id: ALK-US-0034
type: story
title: Ollama native API
status: in_progress
priority: critical
parent: ALK-EP-0007
milestone: ALK-M-0003
labels: [api, ollama]
estimate: 5
created: 2026-10-16T22:40:37Z
updated: 2026-10-17T01:11:43Z
started: 2026-10-17T01:11:43Z
links:
  - { kind: implements, target: ALK-SP-0005.R1 }
---

## Description

As a user of Ollama clients, I want them to work against Alkahest by changing the port.

## Acceptance Criteria

- Endpoints listed in the spec
- NDJSON streaming
- Tested with the official ollama Python client

## Notes

Implements [[ALK-SP-0005.R1]]. Plan: [[plan/alkahest-v1-plan]]
