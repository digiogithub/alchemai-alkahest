# 2026-10-17 — Cross-platform test fixes (CI on Ubuntu, macOS, Windows)

Part of [[ALK-US-0109]] (cross-platform verification); plan: [[plan/alkahest-v1-plan]].

GitHub CI on `main` failed in `cargo nextest run --profile ci` on all three runners. Every
failure was triaged from its panic message; the result is three product fixes and test fixes
for host assumptions. One assertion is gated by platform, and nothing is skipped.

## Product fixes

| Area | Problem | Fix |
|---|---|---|
| Checkout (`.gitattributes`, new) | Windows runners check text out with CRLF (`core.autocrlf=true`). `include_str!` then embedded CRLF systemd units, and `xtask` compared the generated tokens file byte for byte | `* text=auto eol=lf`, binary assets marked, and the one upstream CRLF licence text (`OFL-IBMPlexMono.txt`) kept as `-text` |
| Store lookups on APFS/NTFS (`alkahest-store`: `fsutil::exists_with_exact_case`, `AlkahestStore::read_manifest`, `OllamaStore::get`) | On case-insensitive filesystems the exact-path shortcut also opened `QWEN3/LATEST` when only `qwen3/latest` exists, so the entry carried the requested spelling instead of the stored one | The shortcut is taken only when every path component below the manifests root matches exactly (a directory read, never a write). Anything else falls through to the existing case-insensitive scan, which returns the stored name |
| Concurrent commits on Windows (`fsutil::persist`, `BlobWriter::commit`, `fsutil::write_atomic`) | `MoveFileEx` replace fails with "access denied" while another writer replaces the same blob or manifest, so parallel pulls failed | Renames refused with `PermissionDenied` are retried on Windows (8 tries, 10 to 160 ms backoff). A blob commit whose target already exists after a failed rename succeeds, because blobs are content-addressed and the other copy has the same bytes |
| Service plans rendered off-target (`service_cmd::posix_join`, `under`, `unit_path`, `service_launchd::plist_path`, `plan_install`) | A launchd or systemd `--dry-run` on Windows printed `/Library/LaunchDaemons\es.digio.alkahest.plist` | Target paths are joined with `/` on every host. On Unix this is still `Path::join` |

## Test fixes

- `alkahest-engine` `llama_cpp::cpu_only_build_never_offloads` and
  `registry::llama_server_spec_from_request` passed only on hosts with a CUDA 12 toolkit
  (the dev box). The CUDA build now gets empty stand-in runtime libraries next to it.
- `alkahest-engine` `supervisor::crash`: on Windows a refused loopback connect is retried
  for about 2 s, so readiness can take more than the 500 ms crash delay. The delay is now 5 s.
- `alkahest-recipes` `companions::*`, `install::*` and `tofu`: the fixture archives now ship
  `srv.exe` / `llama-server.exe` on Windows, which is what the installer looks for.
  `companions_are_skipped_unless_requested` no longer indexes into the empty CUDA library
  list on macOS.
- `alkahest-store` `catalog::detail_reads_text_layers_and_modelfile` expects
  `FROM <absolute blob path>` and no longer assumes a leading `/`.
- `alkahest-api` `server_settings` patch test and `admin_api::engine_admin_log_reload_and_override`
  use absolute paths under the temp dir (`/data/models` is not absolute on Windows).
- `alkahest` `engines::ls_shows_an_installed_engine_once_with_its_path` compares path
  components.
- `alkahest-hf` `safetensors_flag_downloads_the_snapshot_and_builds_the_directory`: snapshots
  use symlinks on Unix and hard links on Windows (by design, `snapshot::link_file`). The
  "resolves into `blobs/`" check is `#[cfg(unix)]`. On Windows the test checks that the blob
  holds the same bytes.

Fixed by `.gitattributes` with no test change: `service_cmd::tests::missing_groups_*` and
`xtask tokens::tests::generated_file_is_up_to_date`. Fixed by `posix_join`: the three
`service_launchd::tests::*` and `service::launchd_dry_run_*`.

## Dependencies

Separate commit: `tar` =0.4.46 (PAX header desync) and `rpassword` =7.5.0 (Dependabot);
`THIRD_PARTY_LICENSES` regenerated.

## Verification

- `scripts/ci-local.sh` passes on Linux: fmt, clippy `-D warnings`, nextest (1169 passed),
  deny, check-deps, notice, tokens, headless build, docs-cli, typos.
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnullvm --exclude alkahest-desktop`
  (C code built with `zig cc`) compiles the Windows code paths.
- macOS and Windows behaviour is verified only by the next CI run.
