---
id: ALK-US-0135
type: story
title: Long model names stay inside their box in the desktop app
status: in_progress
priority: medium
parent: ALK-EP-0013
milestone: ALK-M-0005
author: mcp
labels: [desktop, bug]
estimate: 2
created: 2026-10-22T18:05:32Z
updated: 2026-10-22T18:05:32Z
started: 2026-10-22T18:05:32Z
---

## Description

Names such as `hf.co/limcheekin/CodeRankEmbed-GGUF:Q4_K_M` have no spaces and ran out of their column: over the PROCESSOR column on Running, character-per-line titles in the Discover detail, past the card edge in Live requests.

## Acceptance Criteria

- A model name wraps onto at most two lines and ends in an ellipsis beyond that (one line with ellipsis in breadcrumbs and table cells); it never overflows its column.
- Hovering a shortened name shows the whole name.
- Applies to Library rows, Running cards and Live requests, the Model settings header and breadcrumb, the Hugging Face and Ollama detail headers.

## Notes

Plan: [[plan/alkahest-v1-plan]]
