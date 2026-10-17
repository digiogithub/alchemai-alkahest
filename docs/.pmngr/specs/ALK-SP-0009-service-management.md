---
id: ALK-SP-0009
type: spec
title: Service management
status: backlog
labels: [service]
created: 2026-10-16T22:37:39Z
updated: 2026-10-17T00:54:11Z
requirements:
  R1:
    status: backlog
    trace:
      code: [crates/alkahest/src/service_cmd.rs#run]
      tests:
        - crates/alkahest/tests/service.rs#system_install_into_a_root_writes_the_unit_and_runs_nothing
        - crates/alkahest/tests/service.rs#status_json_reports_the_listen_address
  R2:
    status: backlog
    trace:
      code:
        - crates/alkahest/src/service_cmd.rs#plan_install
        - packaging/systemd/alkahest.service
      tests:
        - crates/alkahest/src/service_cmd.rs#system_unit_has_restart_always_and_groups
  R3:
    status: backlog
  R4:
    status: backlog
    trace:
      code:
        - crates/alkahest-api/src/server.rs#serve_with
        - crates/alkahest-api/src/shutdown.rs#ShutdownHooks
        - crates/alkahest-api/src/shutdown.rs#stop_signal
        - crates/alkahest/src/serve_cmd.rs#serve
      tests:
        - crates/alkahest-api/tests/shutdown.rs#no_orphans
        - crates/alkahest-api/tests/shutdown.rs#in_flight_requests_finish_and_new_ones_are_refused
        - crates/alkahest-api/tests/shutdown.rs#drain_timeout_bounds_the_wait_and_hooks_still_run
        - crates/alkahest/tests/serve.rs#sigterm_stops_the_server_with_exit_code_0
---

## Purpose

Run Alkahest as a background service that starts automatically.

## Scope

Covers install, uninstall, start, stop and status on Linux, macOS and Windows, and graceful shutdown.

Plan: [[plan/alkahest-v1-plan]]

## Requirements

### ALK-SP-0009.R1 — Service commands

`alkahest service install|uninstall|start|stop|status` SHALL manage a systemd unit (system or `--user`), a launchd agent or daemon, or a Windows service.

#### Scenario: status
- **WHEN** the service is running
- **THEN** `alkahest service status` reports running and the listen address

### ALK-SP-0009.R2 — Linux system service

The system install SHALL create a dedicated `alkahest` user in the `render` and `video` groups, a unit with `Restart=always`, and a models directory owned by that user.

#### Scenario: reboot
- **WHEN** the host reboots
- **THEN** the service is running and serves requests

### ALK-SP-0009.R3 — Configuration and logs

The service SHALL read the system configuration file and log to journald, the macOS unified log or the Windows event log plus a rotating file.

#### Scenario: logs
- **WHEN** the service starts
- **THEN** `journalctl -u alkahest` shows the startup line

### ALK-SP-0009.R4 — Graceful shutdown

On stop the server SHALL stop accepting requests, drain in-flight requests up to a timeout and terminate all engine processes.

#### Scenario: no orphans
- **WHEN** the service stops while a model is loaded
- **THEN** no engine process remains
