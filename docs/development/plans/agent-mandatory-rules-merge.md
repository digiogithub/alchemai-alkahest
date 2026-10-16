---
created_at: 2026-10-16T22:55:58.35728473Z
updated_at: 2026-10-16T22:55:58.35728473Z
tags:
    - plan
    - agent-instructions
---
# Plan: Merge mandatory AI-agent rules into Alkahest AGENTS.md

## Goal
Evaluate the canonical mandatory operating checklist against the repository instructions, integrate missing or weak rules into the existing AGENTS.md, and preserve all Alkahest-specific guidance.

## Phases
1. **Context and clause audit:** Search the KB; read root AGENTS.md and CLAUDE.md; compare existing instructions with all seven canonical topics. Verified by document review.
2. **Merge:** Edit only AGENTS.md, retaining existing project guidance while marking the applicable operating clauses explicitly mandatory and adapting research/browser tools to Alkahest's tool suite.
3. **Verify and record:** Reread the entire final file, confirm all checklist topics and retained project content, run `git diff --check`, and write a KB change record. No code tests apply to this documentation-only change.

Builds on [[plan/alkahest-v1-plan]].