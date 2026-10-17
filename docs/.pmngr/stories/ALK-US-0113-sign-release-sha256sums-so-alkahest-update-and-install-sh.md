---
id: ALK-US-0113
type: story
title: Sign release SHA256SUMS so `alkahest update` and install.sh verify authenticity, not only integrity
status: triage
priority: medium
author: mcp
labels: [security, release]
created: 2026-10-17T10:27:35Z
updated: 2026-10-17T10:27:35Z
inbox:
  status: pending
  source: mcp
  received: 2026-10-17T10:27:35Z
---

Found while implementing ALK-US-0112. `alkahest update`, `install.sh` and `install.ps1` verify the archive against `SHA256SUMS` from the same GitHub release. That catches corrupt or truncated downloads, but anyone who can publish a release (a compromised maintainer account or CI token) can publish a matching archive and checksum file. Proposal: sign `SHA256SUMS` in `.github/workflows/release.yml` (minisign or cosign keyless), embed the public key in the binary and the scripts, and verify the signature before trusting the checksums. Needs a product owner decision on key management.
