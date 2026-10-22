# Moving models from a user's store to the system service

Story [[ALK-US-0136]]. Epic: service installation and autostart. Plan: [[plan/alkahest-v1-plan]].

## What happened

After the product owner installed the 1.2.0 .deb, their Bonsai 2 models looked deleted. They
were not. They are still in the user store (`~/.local/share/alkahest`, 6.1 GB).

- On 2026-10-18 `sudo alkahest service install` started the system service, which runs as user
  `alkahest` with `ALKAHEST_HOME=/var/lib/alkahest` and its own empty store.
- The user's own `alkahest serve` still held 127.0.0.1:11435. The service crash-looped
  (`Address already in use`, `Restart=always` every 2 s) and nobody noticed.
- After the next reboot the service won the port. From then on the desktop and the CLI talked to
  a store without the user's models.

## What changed

- `crates/alkahest-store/src/transfer.rs` (new):
  - `AlkahestStore::open_existing` opens a store without creating or cleaning anything.
  - `AlkahestStore::transfer_model` copies one model with the same manifest bytes, so the digest
    and the provenance stay as they are. Missing blobs are streamed through a verified writer;
    blobs the destination already has are shared. It returns `TransferOutcome`:
    `Imported`, `AlreadyPresent`, or `NameTaken` (another model with this name is never
    overwritten).
  - `StoreLocation::find` accepts a data folder or a models folder.
  - `transfer_overlays` copies `model-config/<name>.toml` and `.recommended.json` when the
    destination has none.
  - `local.rs` gained `AlkahestStore::at`.
- `crates/alkahest/src/store_import_cmd.rs` (new), plus `cli.rs` and `main.rs`:
  `alkahest import --store <DIR>`.
  - Human output: one line per model. `--json` prints one summary.
  - The exit code is a failure if any model failed.
  - Run as root into a data folder owned by another account, every entry not already owned by
    that account under `models/` and `model-config/` is `lchown`ed to it.
- `crates/alkahest/src/service_cmd.rs`: `Host` gained `invoking_user_store` (`SUDO_USER`'s
  `~/.local/share/alkahest` when it holds manifests) and `port_in_use` (11435 not bindable on
  loopback). The system `plan_install` turns both into notes; the models note carries the exact
  `sudo env ALKAHEST_HOME=/var/lib/alkahest <bin> import --store <dir>` command. Both are read
  only for a real system install (not `--user`, not `--root`).
- Docs:
  - `guide/service.md`: new section "Models pulled before the system service", plus a note to
    restart the service after a package upgrade.
  - `reference/cli.md` regenerated.

## Why

The service store being separate is by design (ALK-SP-0009.R2: a dedicated account and state
directory). What was missing was a way to move the models, and a warning at install time.
Engines (`~/.local/share/alkahest/engines`) are not copied. The service installs its own, built
for its account.

## Verification

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`:
  clean.
- `cargo nextest run --workspace --locked`: 1446 passed, 1 skipped. New tests:
  - `crates/alkahest-store/src/transfer.rs#tests`: digest kept, source tree byte-identical,
    shared blobs, idempotent, name taken, locations, overlays.
  - `crates/alkahest/tests/import.rs#import_every_model_of_another_store`: CLI, `--json`,
    settings visible through `config model show`, models-folder form, second run reports
    "already present", error on a folder that is not a store.
  - `crates/alkahest/src/service_cmd.rs#system_install_points_at_the_users_models_and_a_busy_port`,
    `#home_comes_from_passwd`.
- `cargo xtask check-deps`, headless build: OK.
- `cargo deny check bans licenses sources`: OK. Advisories are still RUSTSEC-2026-0382/0383; see
  [[ALK-US-0134]].
