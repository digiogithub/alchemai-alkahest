---
id: ALK-US-0093
type: story
title: "Decision models now refuse chat: opt-in tests using tev1:0.8b as chat model need another model"
status: in_progress
priority: low
parent: ALK-EP-0007
author: mcp
labels: [test, decision]
created: 2026-10-17T02:26:49Z
updated: 2026-10-17T02:30:36Z
started: 2026-10-17T02:30:36Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T02:26:49Z
---

After ALK-US-0089, `tev1:0.8b` (Ollama capability `decision`) is `ModelKind::Decision`, whose engine capabilities have no `chat` (like Ollama, which lists only `decision`). The ALK-US-0034 change record ran `crates/alkahest/tests/serve_inference.rs` with `ALKAHEST_TEST_CHAT_MODEL=tev1:0.8b`; that run would now get a 400 on `/api/chat`. Use `qwen2.5-coder:0.5b` (local Ollama store, verified with the conformance scripts) for the chat part.
