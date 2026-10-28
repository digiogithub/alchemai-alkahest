# Desktop: saved server connections, and no rogue local server next to a system service

Date: 2026-10-28. Story [[ALK-US-0144]]. Requirement [[ALK-SP-0008]] (R2, start a local server).
Builds on [[2026-10-17-desktop-server-connection]], [[2026-10-17-desktop-keys-engines-settings]]
(ALK-US-0095 settings form) and [[2026-10-19-desktop-network-bind-needs-key]] (ALK-US-0131).
Plan: [[plan/alkahest-v1-plan]].

## Problem (reported by the product owner)

The desktop app was pointed at a remote server through an SSH tunnel
(`https://127.0.0.1:21435`, its own CA and API key). Afterwards it could not be brought back to
the local system service (`/etc/systemd/system/alkahest.service`, `127.0.0.1:11435`):

1. `is_local_url` treated the loopback tunnel as local, so with the tunnel down the app started
   `alkahest serve` itself — with the *user* configuration (`~/.config/alkahest/config.toml`,
   `host = 0.0.0.0`, no keys), which refused to start:
   `refusing to serve 0.0.0.0:11435: no active API keys exist` (`desktop-serve.stderr.log`).
2. With a system unit installed, Start / restart still spawned a second server; only the
   start-at-login path checked `external_service`.
3. One `server_url` and one keyring key served every server: the remote key went to the
   local server and switching back meant retyping URL, CA and key. The unauthorized banner hid
   "Use local server" for loopback URLs, so a tunnel left no way back.

Also seen: the system service was still running 1.2.0 after the 1.4.0 package was installed
(`service alkahest start` on a running unit is a no-op); `systemctl restart alkahest` fixes that.

## What changed

- `crates/alkahest-desktop/src/settings.rs`: `DesktopSettings` now holds
  `connections: Vec<SavedConnection>` (name, url, ca_cert) and `active`; `LOCAL` is the
  built-in connection to the configured server (not stored, not deletable).
  `active_connection`, `select`, `upsert`, `remove`, `find`, `validate_name`. A legacy
  `server_url` / `ca_cert` is read once and migrated to the active connection `remote`
  (`migrated()`); the file is then saved in the new form. `KeyringKeyStore` is now one entry
  per connection (`desktop-api-key:<name>`); `KeyringKeyStore::legacy()` and
  `migrate_legacy_key` move the old single key to the migrated connection (or to `local` when
  none was ever saved).
- `crates/alkahest-desktop/src/app.rs`: `connect` returns `Connected { name, url, handle,
  client }`; builds the client from the active connection with its own key and CA; passes a
  launcher **only for the local connection** and never when a system service runs the server
  (`ConnectionManager::with_external_service`). `migrate_settings` runs outside the smoke test.
- `crates/alkahest-desktop/src/launcher.rs`: `LocalLauncher::launch` refuses to spawn when
  `external_service` is set ("… runs the server; start that service instead").
- `crates/alkahest-desktop/src/connection.rs`: `Snapshot.external_service`; the service card
  names the service while connected and says to start it when it does not answer.
- `crates/alkahest-desktop/src/settings_screen.rs`: `ConnectionDraft` gains `name`,
  `connection()` (validated `SavedConnection`) and `clear()`.
- `crates/alkahest-desktop/src/views/settings.rs`: "Server connections" card: list (Local plus
  saved, active marked), Use, Edit, Delete (second click confirms; removes the key too), "Add
  connection…" form (name, http/https, URL, CA, key), "Save & connect". The key row acts on the
  active connection.
- `crates/alkahest-desktop/src/views/shell.rs`: tracks the active connection; the unauthorized
  banner stores the key for it and offers "Use local server" whenever it is not local.

## Verification

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  `cargo clippy -p alkahest-desktop --features gui-test --all-targets -- -D warnings`: clean.
- `cargo nextest run --workspace --locked`: 1508 passed, 1 skipped.
- `cargo nextest run -p alkahest-desktop --features gui-test`: 172 passed, including new
  `settings::tests::{a_loopback_tunnel_is_still_a_remote_connection,
  a_legacy_server_url_becomes_the_active_remote_connection,
  the_legacy_key_moves_to_the_migrated_connection_once, connections_switch_replace_and_delete,
  connection_names_are_validated}`, `launcher::tests::never_spawns_next_to_an_external_service`,
  `views::settings::tests::connections_switch_edit_delete_and_add`.
- `cargo deny check`, `cargo xtask check-deps`, headless build, desktop build: OK.
- `alkahest desktop --smoke-test` on Wayland with a copy of the reported legacy `desktop.toml`:
  log `connecting connection=remote server=https://127.0.0.1:21435`, first frame in 440 ms.
- The real client against the system service on `http://localhost:11435` (with and without a
  wrong key) reaches `Connected` (manual probe before the change; the failure was the launcher
  and the shared settings, not the HTTP path).
