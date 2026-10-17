---
id: ALK-US-0058
type: story
title: Text-to-speech
status: in_progress
priority: low
parent: ALK-EP-0011
milestone: ALK-M-0004
labels: [engine, audio]
estimate: 3
created: 2026-10-16T22:42:19Z
updated: 2026-10-17T03:43:56Z
started: 2026-10-17T03:43:56Z
links:
  - { kind: implements, target: ALK-SP-0005.R4 }
---

## Description

As a user, I want `/v1/audio/speech` with a local TTS engine.

## Acceptance Criteria

- Spike chooses engine (kokoro, piper, others)
- Endpoint implemented for the chosen engine

## Notes

Implements [[ALK-SP-0005.R4]]. Plan: [[plan/alkahest-v1-plan]]
