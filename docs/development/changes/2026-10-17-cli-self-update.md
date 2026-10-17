# 2026-10-17 — `alkahest update`: CLI self-update from GitHub releases

Story: [[ALK-US-0112]] (epic [[ALK-EP-0014]]). Plan: [[plan/alkahest-v1-plan]]. No spec
requirement covers self-update yet; the behaviour is defined by the story's acceptance criteria.
It ports the behaviour of `gintrack update` (`/www/git-in-track/internal/selfupdate`) and of
Pando's stable-release filter; no code was copied.

## What

`alkahest update [VERSION] [--check] [--prerelease] [--force] [--yes]` (plus the global
`--json`) replaces the running binary with a release of `digiogithub/alchemai-alkahest`:

1. Detects the install channel and refuses package-manager installs with the right upgrade
   command unless `--force`: deb/rpm (`/usr/...` outside `/usr/local`), Homebrew (Cellar,
   `/opt/homebrew`, Linuxbrew), `Alkahest.app`, the NSIS install (`uninstall.exe` next to the
   exe), AppImage (`APPIMAGE`, `/tmp/.mount_*`). A cargo build output (`target/{debug,release}`)
   only accepts an explicit version.
2. Lists releases (up to 10 pages) and picks the newest one with a headless archive for this
   platform, skipping drafts always and pre-releases (GitHub flag or semver suffix) unless
   `--prerelease`. An explicit version uses `/releases/tags/v<X>` (then `<X>`) and bypasses the
   filter, so it can downgrade or reinstall.
3. Same version: "up to date". Running version newer than the latest stable: "up to date".
   `--check` reports and exits 10 when another release would be installed.
4. Confirms on a terminal (`Update` / `Reinstall` / `DOWNGRADE`); off a terminal it refuses
   without `--yes`.
5. Stages under `<exe dir>/.alkahest-update-*`, fetches `SHA256SUMS`, downloads the archive with
   `alkahest_fetch::Downloader` (expected size and SHA-256 enforced), unpacks it with
   `alkahest_recipes::extract` (path traversal and link checks), then swaps the binary by
   rename-aside and refreshes `engines/`, `licenses/`, `LICENSE`, `NOTICE`,
   `THIRD_PARTY_LICENSES` only where they already exist next to the binary.

Exit codes: 0 done or up to date, 1 failure, 3 refused or cancelled, 4 verification failed,
10 `--check` found another release.

## Files and symbols

- `crates/alkahest/src/update/mod.rs` — `run`, `Updater::{execute, find_release, confirm,
  install}`, `archive_root`, `EXIT_REFUSED`, `EXIT_VERIFY`, `EXIT_AVAILABLE`, `COMPANION_DIRS`.
- `crates/alkahest/src/update/release.rs` — `Release`, `Asset`, `Source` (`ALKAHEST_UPDATE_API_URL`,
  `ALKAHEST_UPDATE_REPO`), `ReleaseClient::{list, by_version, fetch_checksums, check_asset_url}`,
  `select_latest`, `lookup_checksum`, `host_target`, `archive_name`.
- `crates/alkahest/src/update/channel.rs` — `Channel`, `ChannelEnv`, `detect`.
- `crates/alkahest/src/update/apply.rs` — `replace_file` (rename-aside with rollback, keeps mode,
  keeps owner when run as root), `replace_dir`, `cleanup_old`, `ApplyError`.
- `crates/alkahest/src/cli.rs` — `UpdateArgs`, `Command::Update`; `src/main.rs` dispatch and the
  Windows start-up removal of `.alkahest.exe.old`.
- `crates/alkahest/tests/update.rs` — fake GitHub with wiremock; the built binary is hard-linked
  into `CARGO_TARGET_TMPDIR` and run from there.
- `crates/alkahest/Cargo.toml` (semver, serde, tempfile; dev: flate2, hex, sha2, tar, zip — all
  already pinned), `THIRD_PARTY_LICENSES` (semver now in the headless binary).
- `docs/book/src/install.md` "Update" section; `docs/book/src/reference/cli.md` regenerated.

## Why

Product owner request: the same self-update `gintrack update` and `pando update` have. Logic
stays in the bin crate (a module, not a new crate) because only the CLI replaces its own
executable; the downloader and the safe unpacker are reused from `alkahest-fetch` and
`alkahest-recipes`.

## Verification

- Unit tests: `update::release::tests::stable_selection_skips_draft_prerelease_flag_and_rc_tags`,
  `selection_needs_an_asset_for_the_platform`, `checksum_lookup_reads_sha256sum_format`,
  `update::channel::tests::channel_detection_*`, `update::apply::tests::atomic_replace_*`,
  `a_failed_copy_leaves_the_target_untouched`, `directories_are_swapped_*`, `cleanup_removes_leftovers`.
- Integration (`tests/update.rs`, no network): stable selection skips draft / prerelease flag /
  `-rc` tag; update replaces binary, engines and licence, no side files left; checksum mismatch
  and missing checksum entry exit 4 with the install untouched; up to date; explicit downgrade;
  unknown version; no terminal without `--yes` exits 3; Homebrew path refused, `--force` works.
- Opt-in `real_github_answers_a_check` (`ALKAHEST_TEST_NET=1`) passed against api.github.com
  (answer today: "no published stable release ... has an archive for x86_64-unknown-linux-gnu",
  since v0.1.0 is still a draft, which anonymous requests do not see).
- `scripts/ci-local.sh`: fmt, clippy `-D warnings`, nextest 1169 passed, deny, check-deps,
  notice, tokens, headless build, docs-cli, mdbook, typos — all passed. `cargo machete` clean.

## Security notes for product owner review

- Verification is mandatory: size cap (2 GiB) and exact size, SHA-256 from `SHA256SUMS`. The
  checksum file comes from the same release as the archive, so this protects integrity, not
  authenticity: a compromised GitHub account or release could ship a matching pair. Signed
  checksums (minisign/cosign) would close that gap.
- `GITHUB_TOKEN` is sent only to `https://api.github.com`, never to an overridden API root or to
  asset downloads. With the default API root, non-HTTPS asset URLs are refused; with
  `ALKAHEST_UPDATE_API_URL` set, plain HTTP is accepted (tests, mirrors).
- No privilege escalation: an unwritable install directory fails before download with a hint.
- The new binary is not executed before it replaces the old one (no `--version` smoke check).
