---
id: ALK-US-0057
type: story
title: Speech-to-text with whisper.cpp
status: done
priority: medium
parent: ALK-EP-0011
milestone: ALK-M-0004
labels: [engine, audio]
estimate: 5
created: 2026-10-16T22:42:15Z
updated: 2026-10-17T04:21:36Z
started: 2026-10-17T03:43:56Z
closed: 2026-10-17T04:21:36Z
links:
  - { kind: implements, target: ALK-SP-0005.R4 }
---

## Description

As a user, I want audio transcription via `/v1/audio/transcriptions`.

## Acceptance Criteria

- whisper.cpp recipe and engine adapter
- Formats: json, text, srt, vtt

## Notes

Implements [[ALK-SP-0005.R4]]. Plan: [[plan/alkahest-v1-plan]]
