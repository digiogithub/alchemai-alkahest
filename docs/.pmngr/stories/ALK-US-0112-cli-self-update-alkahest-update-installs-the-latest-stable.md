---
id: ALK-US-0112
type: story
title: "CLI self-update: `alkahest update` installs the latest stable GitHub release"
status: in_progress
priority: high
parent: ALK-EP-0014
milestone: ALK-M-0006
author: mcp
labels: [security, cli, release]
estimate: 5
created: 2026-10-17T10:14:39Z
updated: 2026-10-17T10:14:39Z
started: 2026-10-17T10:14:39Z
---

## Description

Product owner request (2026-10-17): add `alkahest update`, the same feature `gintrack update` and `pando update` have. It downloads the newest GitHub release of `digiogithub/alchemai-alkahest` for the running platform and replaces the installed binary. Drafts and releases GitHub flags as pre-release are ignored, and so are tags with a semver pre-release suffix, unless `--prerelease` is passed. Reference implementations: `/www/git-in-track/internal/selfupdate` + `cmd/gintrack/update.go` (primary) and `/www/MCP/Pando/pando/cmd/update.go` + `internal/updatecheck/release.go`.

## Acceptance Criteria

- `alkahest update` finds the newest stable release that has an asset for this platform: the headless `alkahest-<version>-<target>.tar.gz`, or `.zip` on Windows. If it is newer than the running version it downloads the asset, checks it against the release `SHA256SUMS`, and atomically replaces the binary. If it is not newer, it reports "up to date".
- `alkahest update --check` reports what would change without changing anything. `alkahest update vX.Y.Z` installs that exact release, which can be a downgrade or a reinstall.
- `--prerelease` includes pre-releases. `--yes` skips the confirmation. `--force` overrides the package-manager refusal. `--json` gives machine output.
- Installs owned by a package manager (deb/rpm under /usr, Homebrew, a .dmg app bundle, the NSIS install, an AppImage) are detected. The command refuses with a hint to use that channel, unless `--force` is passed.
- The bundled engines next to the binary (`engines/`) are updated from the archive together with the binary.
- Windows: the running exe is replaced with the rename-aside technique.
- Tests run without network against a fake release server: stable selection skips draft, prerelease flag and `-rc` tags; checksum mismatch aborts; atomic replace; channel detection. A real GitHub run is opt-in behind `ALKAHEST_TEST_NET=1`.

## Notes

Security-tagged: downloads and replaces executables. Needs product owner review.
