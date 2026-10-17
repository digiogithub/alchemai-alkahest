# Plan: git-backed catalogs for engines and models (ALK-EP-0015)

Status: active, started 2026-10-17. Milestone [[ALK-M-0006]] (v1.0). Orchestration rules as in
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

Each phase is testable: the spec scenarios of 0117 become the tests of 0118 to 0121.
