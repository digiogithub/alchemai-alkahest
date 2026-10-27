---
id: ALK-US-0138
type: story
title: "cargo deny advisories fails: RUSTSEC-2026-0382/0383/0385 and yanked clipboard-win"
status: in_review
priority: high
parent: ALK-EP-0014
author: mcp
labels: [security, ci]
created: 2026-10-27T14:48:57Z
updated: 2026-10-27T15:58:06Z
started: 2026-10-27T15:58:06Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-27T14:48:57Z
---

Found 2026-10-27 while verifying ALK-US-0137; fails identically on main before that change (Cargo.lock untouched).

`cargo deny check advisories`:
- RUSTSEC-2026-0382 certificate verification may accept distrusted intermediates (rustls-webpki; fixed in 0.103.16)
- RUSTSEC-2026-0383 bundled roots trust distrusted emSign intermediates (webpki-roots; 1.0.10+ / 0.26.11 wrapper)
- RUSTSEC-2026-0385 non-constant-time mask generation and u128 comparisons
- yanked crate `clipboard-win` (`cargo update -p clipboard-win`)

Needs a dependency bump and `cargo xtask notice` regeneration; security-tagged, product owner review.
