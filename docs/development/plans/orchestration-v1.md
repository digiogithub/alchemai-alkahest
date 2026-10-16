# Plan: orchestrated implementation of the ALK backlog

Status: active, started 2026-10-17. Builds on [[plan/alkahest-v1-plan]] and the
rules in `AGENTS.md`.

## How work runs

- One orchestrator session (Claude Code) picks stories whose dependencies are done,
  milestone by milestone (M0 to M5), critical first.
- Each story is implemented by one internal Claude Code subagent, in its own git
  worktree, on its own branch. Sonnet is the default model; Opus is used for stories
  that need deeper reasoning (scheduler, engine supervisor, API translation layers,
  decision scoring, auth, store invariants, GPUI architecture).
- At most two worktrees exist at any time (product owner, 2026-10-17: Rust `target/`
  directories use a lot of disk). Each worktree, with its `target/`, is removed as soon
  as its branch is merged.
- The orchestrator owns gintrack status transitions on stories (`in_progress`,
  `in_review`, `done`). Subagents may move their own tasks, add comments, and record
  `trace.code` / `trace.tests` on the requirements their story implements.
- The orchestrator reviews each branch, runs the full verification on `main` after the
  merge, re-indexes the code, and closes the story.

## Subagent contract

1. Start with `kb_search_documents` on the story topic; read `AGENTS.md`,
   the story (`get_item` with body and children) and `spec_context`.
2. Touch only the crates the story names. Shared types in `alkahest-core` change only
   when the story is about them; otherwise add a local type and raise an inbox item.
3. New third-party dependencies go into `[workspace.dependencies]` with an exact pin
   (`=x.y.z`) and are referenced with `dep.workspace = true`. Check licences (no GPL).
4. Spec scenarios become tests named after them. Network/GPU tests are opt-in
   (`ALKAHEST_TEST_NET=1`, `ALKAHEST_TEST_GPU=1`).
5. Run the full verification block from `AGENTS.md` section 5 before reporting.
6. Write the change record `docs/development/changes/<date>-<slug>.md` with wiki links
   to the story and spec requirements.
7. Commit on the worktree branch, Conventional Commits with the story id. Never push.
8. Report: branch name, commit id, files touched, verification output summary,
   open issues.

## Waves (initial)

| Wave | Stories | Notes |
|---|---|---|
| 0 | ALK-US-0001 | workspace scaffold; everything else depends on it |
| 1 | ALK-US-0002, 0003, 0004, 0005, 0006 | CI, config, CLI tree, GGUF, sysinfo |
| 2 | ALK-US-0007, 0008, 0011, 0016, 0018 | classifier, estimator, refs, downloader, HF client |
| later | by milestone and dependency | recomputed after each wave |
