# Desktop server connection and lifecycle (ALK-US-0065)

Date: 2026-10-17. Story [[ALK-US-0065]], requirement [[ALK-SP-0008.R2]], plan
[[plan/alkahest-v1-plan]]. Tasks ALK-T-0178, ALK-T-0179. Builds on [[ALK-US-0064]].

## What changed

- `alkahest-client` (additive): `Client::runtime_status` (`GET /alkahest/v1/ps`) and
  `Client::events` (`GET /alkahest/v1/events`, server-sent events) with the lenient types
  `SchedulerStatus`, `RuntimeModel`, `ServerEvent` (`status()` decodes a `status` event) and
  `EventStream`; an SSE line parser (`SseParser`, comments and CRLF handled).
- `alkahest-desktop/src/connection.rs` (GPUI-free):
  - `Machine`: pure state machine over `ConnectionState` (Connecting, Connected(ServerInfo),
    Starting { method, deadline }, NotRunning { error }, Unauthorized, Error). Reconnect
    `Backoff` 0.5 s doubling to 10 s; readiness polling every 0.5 s for 30 s after a start;
    health probe every 10 s while connected.
  - `ServerApi` trait (`probe`, `status_events`), implemented for `alkahest_client::Client`
    (version + tags + ps; events mapped to `StatusUpdate`); `ProbeError` from `ClientError`
    (401/403 -> Unauthorized, refused -> NotRunning).
  - `ConnectionManager::new(api, launcher, url) -> (manager, ConnectionHandle)`; `run()` on the
    tokio runtime; `Command::{StartServer, Retry}` in, `Snapshot` out through `watch`.
    `Snapshot::service_card`, `card_action` (`CardAction::{StartServer, Retry}`), `counters`.
- `alkahest-desktop/src/launcher.rs`: `ServerLauncher` trait and `LocalLauncher`: systemd user
  unit (`systemctl --user start alkahest.service`) when its file exists, else a detached
  `alkahest [--config f] [--host h] serve` (own process group / DETACHED_PROCESS, null stdio,
  reaper thread). `is_local_url`: Start is only offered for loopback servers.
- `alkahest-desktop/src/settings.rs`: `DesktopSettings` (`desktop.toml`: `server_url`,
  `theme`; unknown fields refused so a key cannot land in the file), `ApiKeyStore` /
  `KeyringKeyStore` (service `alkahest`, user `desktop-api-key`), `resolve_api_key`
  (`ALKAHEST_API_KEY` first, broken keyring = no key).
- GUI: `app.rs` loads settings, builds the client (remote URL override, key), spawns the
  manager on the tokio bridge; `Shell::follow` re-renders from snapshots; the service card
  shows the state and a secondary `Start server` / `Retry` button; rail counters show the
  library size and the live loaded count. The smoke test skips the keyring.
- `alkahest` bin (desktop arm only): passes `ALKAHEST_API_KEY`, `<config dir>/desktop.toml`,
  `current_exe()` and the global `--config` / `--host` to `LaunchOptions`.

## Why

ALK-SP-0008.R2: the app connects to the configured server and offers to start one when none
answers. The machine is separated from the driver and the driver from the client so the
scenario is a fast unit test with a fake server and paused tokio time.

## Verification

- Scenario test `connection::tests::no_server` (fake server + fake launcher, paused clock):
  NotRunning offers Start -> Starting -> Connected, live event updates the loaded counter.
  Plus machine, backoff, card mapping, launcher (real detached spawn of a script), settings
  and client SSE tests (`events_stream_decodes_status_snapshots`,
  `runtime_status_and_unauthorized_events`, `sse_lines_assemble_events`).
- `cargo nextest run --workspace --locked`: 763 passed; GUI tests (`--features gui-test`):
  27 passed; clippy (default and desktop features), fmt, deny, check-deps,
  `tokens --check --contrast`, machete, typos: OK; headless build links no GPUI/X11/Wayland/
  Vulkan.
- End to end under Xvfb against an empty port: card "Service not running" with
  `Start server`; a click (xdotool) started `alkahest serve`, the card turned to
  "Service running", `alkahest 0.1.0`, Library counter filled; the started server was then
  stopped. Smoke test exit 0.

## Follow-ups

- No in-app settings form yet (Settings is not designed): inbox [[ALK-US-0095]].
- Stopping a server from the UI is not offered (only start); the Running screen
  ([[ALK-US-0070]]) or the tray ([[ALK-US-0073]]) is the natural home.
