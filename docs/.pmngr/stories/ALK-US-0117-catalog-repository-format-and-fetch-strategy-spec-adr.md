---
id: ALK-US-0117
type: story
title: Catalog repository format and fetch strategy (spec + ADR)
status: in_progress
priority: high
parent: ALK-EP-0015
milestone: ALK-M-0006
author: mcp
labels: [catalog, spec, adr]
estimate: 3
created: 2026-10-17T18:30:04Z
updated: 2026-10-17T18:42:05Z
started: 2026-10-17T18:34:46Z
links:
  - { kind: relates_to, target: ALK-SP-0011 }
---

## Description

Define the git catalog layout and how Alkahest fetches it, before any code depends on it.

Proposed layout:

```
catalog.toml          schema_version, name, description, min_alkahest
engines/<id>.toml     recipe format of recipes/*.toml (plus the extensions of the dependency / native stories)
models/<name>.toml    model entries (format defined in the model catalog story)
```

Fetch strategy to decide in an ADR: git CLI (`git fetch` of a shallow clone), a pure-Rust client
(`gix`, MIT/Apache), or an HTTPS archive of a ref (no git needed on the host). Criteria: works on
Linux, macOS and Windows without extra tools, proxy and TLS handling consistent with
`alkahest-fetch`, binary size, licence (no GPL), private repositories (token from the keyring).

## Acceptance Criteria

- New spec in gintrack with requirements for layout, `schema_version` handling (newer major
  refused with a clear message, unknown fields ignored), layer order and offline behaviour.
- ADR under `docs/development/decisions/` choosing the fetch strategy, with the alternatives measured.
- An example catalog under `docs/research/` or a fixture directory that the later stories test against.

## Notes

Blocks the other stories of [[ALK-EP-0015]].
