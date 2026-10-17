---
id: ALK-US-0077
type: story
title: Licence compliance
status: done
priority: critical
parent: ALK-EP-0014
milestone: ALK-M-0006
labels: [release, legal]
estimate: 2
created: 2026-10-16T22:43:33Z
updated: 2026-10-17T05:18:32Z
started: 2026-10-17T04:50:51Z
closed: 2026-10-17T05:18:32Z
---

## Description

As the product owner, I want no GPL in shipped binaries and correct notices for llama.cpp, vLLM and other engines.

## Acceptance Criteria

- cargo deny clean
- NOTICE and third-party licences bundled
- Engine licences listed per recipe

## Notes

Alkahest is MIT. Bundled fonts (Schibsted Grotesk, IBM Plex Mono) are OFL-1.1: ship their licence texts. Engines keep their own licences (llama.cpp MIT, vLLM Apache-2.0).
