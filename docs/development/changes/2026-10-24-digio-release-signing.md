# Release signing on the Digio scheme, fast dev profiles

Stories: [[ALK-US-0075]] (macOS packages), [[ALK-US-0076]] (Windows installer). Plan:
[[plan/alkahest-v1-plan]]. Supersedes the signing parts of
[[development/changes/2026-10-17-macos-packages]] and
[[development/changes/2026-10-17-windows-installer]].

## What changed

`.github/workflows/release.yml` now signs the way digiogithub/pando and digiogithub/bitacora do,
and signing is mandatory: there is no unsigned fallback for any run of the workflow.

- **macOS** (`macos` job, matrix aarch64 / x86_64, `timeout-minutes: 120`):
  1. guard step fails unless the secret `MACOS_SIGNING_BUNDLE` is set;
  2. `digiogithub/ci-actions/macos-signing-keychain@v1` (`env-file: kvagerc`,
     `keychain-name: alkahest-build`, `notary-profile: alkahest-notary`);
  3. `cargo xtask bundle --flavour headless` builds the headless binary and stages the bundled
     llama.cpp in `target/bundle/`; the desktop binary is built with `--features desktop`; the
     unsigned tar.gz is dropped;
  4. every Mach-O file under `target/bundle/alkahest-headless` and `target/bundle/engines` is
     signed in place with `macos-codesign@v1` and `packaging/macos/entitlements.plist`;
  5. `cargo xtask bundle --no-build` with `APPLE_SIGNING_IDENTITY="$SIGNING_IDENTITY"` and every
     `APPLE_ID` / API key variable unset: tar.gz from the signed files, `.app` and `.dmg` signed
     by cargo-packager (hdiutil retry kept);
  6. smoke test, `codesign --verify` (app deep/strict, hardened runtime flag, dmg, every Mach-O
     of the staged headless tree);
  7. `macos-notarize@v1`: the dmg (stapled) and a `ditto` zip of the headless tree
     (submit-only); `xcrun stapler validate` on the dmg and `spctl --assess` on the app;
  8. `macos-keychain-cleanup@v1` with `if: always()`; upload `dist-macos-<target>`.
- **Windows** (`windows` job): `environment: release`, `permissions: id-token: write`.
  `cargo xtask bundle --flavour headless` + desktop build, `azure/login@v3` with
  `vars.AZURE_CLIENT_ID` / `AZURE_TENANT_ID` / `AZURE_SUBSCRIPTION_ID`,
  `azure/trusted-signing-action@v2.0.0` on both `alkahest.exe` builds and the bundled engine's
  `.exe`/`.dll` files, then `cargo xtask bundle --no-build` (zip + NSIS from the signed files),
  then the installer is signed. `Get-AuthenticodeSignature` checks the loose binaries, the
  installer, the engine files and the contents of the extracted zip, failing unless `Valid`.
  The silent install / uninstall smoke test is unchanged and now runs the signed installer.
- Linux, the tag check, artefact names, SHA256SUMS, the Homebrew formula and the draft release
  are unchanged. Only `dist-*` artefacts reach the release job and they are uploaded after the
  signature checks.
- The old secrets (`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`,
  `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`, `APPLE_API_KEY`, `APPLE_API_ISSUER`,
  `APPLE_API_KEY_P8`, `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD`) are no longer read.
  The xtask knobs (`APPLE_SIGNING_IDENTITY`, `ALKAHEST_WINDOWS_SIGN_COMMAND`,
  `ALKAHEST_WINDOWS_CERT_THUMBPRINT`) stay for local signing.
- Docs: `docs/install.md` and `docs/book/src/install.md` no longer describe releases as
  conditionally signed; comments in `crates/alkahest/Cargo.toml` and `xtask/src/bundle.rs` match.
- `Cargo.toml`: `[profile.dev]` `debug = "line-tables-only"`, `split-debuginfo = "unpacked"`;
  `[profile.dev.package."*"]` `debug = false`, `opt-level = 1`. `[profile.release]` unchanged,
  `.cargo/config.toml` untouched.

## Known limits

- The NSIS uninstaller (`uninstall.exe`, generated inside the installer) is not signed: the
  Azure action cannot run inside cargo-packager. Bitácora ships the same way.
- The `.app` inside the dmg is not stapled itself; the stapled dmg ticket and Gatekeeper's online
  check cover it (same as Bitácora).
- The bundled llama.cpp binaries are re-signed with Digio's identity (Developer ID / Authenticode)
  because Gatekeeper and Smart App Control reject unsigned code shipped inside a signed product.

## Verification

- `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"`: parses; jobs
  `meta, linux, macos, windows, release`.
- `actionlint` v1.7.12 on `release.yml`: no findings.
- `cargo check --workspace --locked`: passes with the new dev profiles; `cargo fmt --all --check`
  clean.
- Not run: the workflow itself (needs the `release` environment, the six `AZURE_*` variables and
  `MACOS_SIGNING_BUNDLE`). First end-to-end check is a pre-release tag such as `v1.3.1-rc.1`.
