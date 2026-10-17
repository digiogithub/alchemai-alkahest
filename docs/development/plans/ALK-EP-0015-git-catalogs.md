# Plan: git-backed catalogs for engines and models (ALK-EP-0015)

Status: implemented 2026-10-17, all stories in review. Milestone [[ALK-M-0006]] (v1.0). Orchestration rules as in
[[development/plans/orchestration-v1]]: internal subagents, at most two worktrees, orchestrator
owns story status, merges and full verification.

## Decisions taken with the product owner (2026-10-17)

- The catalog is read from git so every installation sees the current catalog without a release.
- The official catalog is the top-level `catalog/` folder of this public repository
  (`github.com/digiogithub/alchemai-alkahest`, ref `main`). Catalog sources are therefore
  `url + ref + subdir`; extra (private) catalogs can be added by users.
- The same mechanism covers models outside Hugging Face and the Ollama registry.
- Built-in `recipes/*.toml` stay as the offline fallback.

## Waves

| Wave | Stories | Model | Worktree | Depends on |
|---|---|---|---|---|
| 1 | [[ALK-US-0117]] format spec + fetch ADR + fixture | Opus | main tree (docs, spec) | - |
| 2 | [[ALK-US-0118]] sync + catalog CLI (security) | Opus | yes | 0117 |
| 2 | [[ALK-US-0119]] recipe dependencies + native builds | Sonnet | yes | 0117 |
| 3 | [[ALK-US-0120]] model catalog pull/search | Opus | yes | 0117, 0118 |
| 3 | [[ALK-US-0121]] `catalog/` seed + catalog-check CI | Sonnet | yes | 0117, 0119 |
| 4 | [[ALK-US-0122]] desktop Discover + settings | Opus | yes | 0118, 0120; canvas design first |
| 3b | [[ALK-US-0124]] admin API for catalogs (added after the design review) | Opus | yes | 0118, 0120 |

Each phase is testable: the spec scenarios of 0117 become the tests of 0118 to 0121.

## Outcome (2026-10-17)

All seven stories are merged into `main` and in review: 0117 (spec ALK-SP-0011, ADR 0006
accepted), 0119, 0121, 0118 and 0124 (each after an independent security review whose findings
were fixed before merge), 0120, 0122 (design approved by the product owner, then GPUI).
Verification on `main` after the last merge: fmt, clippy, 1359 tests, check-deps, tokens,
catalog-check, headless build, desktop smoke test. Pending outside the epic: inbox
[[ALK-US-0123]] (`cargo deny` advisories for zbus/zvariant).
