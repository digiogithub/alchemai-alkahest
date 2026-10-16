---
id: ALK-EP-0012
type: epic
title: Decision models API
status: backlog
priority: high
milestone: ALK-M-0004
labels: [decision, api]
created: 2026-10-16T22:37:57Z
updated: 2026-10-16T22:52:00Z
---

## Description

Decision model ('System One' / Jev-style) API: `/v1/systemone` and `/v1/decisions` with logit scoring, compatible with Ollama >= 0.35 and LocalAI.

Plan: [[plan/alkahest-v1-plan]]

## Notes

Jev was the first commercial decision-model API. Ollama and LocalAI cloned it as `/v1/systemone`; Alkahest implements the same contract so clients move between Jev, Ollama, LocalAI and Alkahest without changes (product owner, 2026-10-17). Parity tests should cover Jev-documented request/response examples where available.
