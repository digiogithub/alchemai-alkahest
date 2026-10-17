---
id: ALK-US-0111
type: story
title: Drop the <tool_call> prefill once llama.cpp forbids content before required tool calls
status: backlog
priority: low
parent: ALK-EP-0007
author: mcp
labels: [engine, api]
created: 2026-10-17T07:50:37Z
updated: 2026-10-17T07:53:56Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T07:50:37Z
---

ADR 0005 (ALK-US-0096) prefills the assistant turn with `<tool_call>\n` for forced tool choices on Hermes-style JSON templates, because llama.cpp b11669's JSON-native tool parser (`common/chat-auto-parser-generator.cpp`, `build_tool_parser_tag_json`) accepts `optional(content(until("<tool_call>")))` before the calls even with `tool_choice: required`. When an engine pin bump (ALK engine pin check) brings a llama.cpp that no longer allows that prefix, re-run `crates/alkahest/tests/serve_tools.rs` without the prefill and remove `alkahest_api::tool_prefill` if it passes; consider reporting the behaviour upstream.
