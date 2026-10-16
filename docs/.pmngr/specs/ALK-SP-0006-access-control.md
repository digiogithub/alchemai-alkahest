---
id: ALK-SP-0006
type: spec
title: Access control
status: backlog
labels: [security]
created: 2026-10-16T22:37:36Z
updated: 2026-10-16T22:37:36Z
requirements:
  R1:
    status: backlog
  R2:
    status: backlog
  R3:
    status: backlog
  R4:
    status: backlog
  R5:
    status: backlog
  R6:
    status: backlog
  R7:
    status: backlog
---

## Purpose

Serve local clients without friction and require API keys for any access from outside the machine.

## Scope

Covers bind address policy, API keys, scopes, audit and TLS.

Plan: [[plan/alkahest-v1-plan]]

## Requirements

### ALK-SP-0006.R1 — Loopback by default

The server SHALL bind `127.0.0.1:11435` by default and SHALL serve loopback requests without a key unless `auth.require_on_loopback` is set.

#### Scenario: default bind
- **WHEN** the server starts with default configuration
- **THEN** it does not listen on non-loopback interfaces

### ALK-SP-0006.R2 — API key required for external access

WHEN a request comes from a non-loopback address, the server SHALL require a valid key in `Authorization: Bearer` or `x-api-key` and SHALL answer 401 in the endpoint dialect otherwise.

#### Scenario: missing key
- **WHEN** a LAN client calls `/v1/models` without a key
- **THEN** 401

### ALK-SP-0006.R3 — Key storage

Keys SHALL be generated with a CSPRNG, shown once, stored only as salted hashes with name, creation time, scopes (`inference`, `admin`), optional model allowlist and optional expiry, and be revocable.

#### Scenario: revocation
- **WHEN** a key is revoked
- **THEN** the next request with it gets 401

### ALK-SP-0006.R4 — Admin scope

Admin API routes and model pull/delete SHALL require the `admin` scope for non-loopback callers.

#### Scenario: inference key
- **WHEN** an inference-only key calls `/api/pull` from the LAN
- **THEN** 403

### ALK-SP-0006.R5 — Audit

The server SHALL record per key the last use time, request count and token counts, without logging the key or request bodies.

#### Scenario: last used
- **WHEN** a key is used
- **THEN** `alkahest keys ls` shows the new last-used time

### ALK-SP-0006.R6 — Optional TLS

The server SHALL serve HTTPS with a configured certificate and key (rustls) when TLS is enabled.

#### Scenario: tls
- **WHEN** TLS is configured
- **THEN** clients connect over HTTPS

### ALK-SP-0006.R7 — Endpoint families and CORS

The server SHALL let the administrator enable or disable each endpoint family (Ollama native `/api/*`, OpenAI `/v1/*`, Anthropic `/v1/messages`, decisions `/v1/systemone` and `/v1/decisions`) and configure allowed CORS origins, answering 404 for a disabled family.

#### Scenario: disabled family
- **WHEN** the Anthropic family is disabled and a client posts to `/v1/messages`
- **THEN** the server answers 404
