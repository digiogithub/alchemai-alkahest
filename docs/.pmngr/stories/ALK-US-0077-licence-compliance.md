---
id: ALK-US-0077
type: story
title: Licence compliance
status: backlog
priority: critical
parent: ALK-EP-0014
milestone: ALK-M-0006
labels: [release, legal]
estimate: 2
created: 2026-10-16T22:43:33Z
updated: 2026-10-16T22:43:33Z
---

## Description

As the product owner, I want no GPL in shipped binaries and correct notices for llama.cpp, vLLM and other engines.

## Acceptance Criteria

- cargo deny clean
- NOTICE and third-party licences bundled
- Engine licences listed per recipe
