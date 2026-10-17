---
id: ALK-SP-0006
type: spec
title: Access control
status: backlog
labels: [security]
created: 2026-10-16T22:37:36Z
updated: 2026-10-17T00:39:01Z
requirements:
  R1:
    status: backlog
    trace:
      code:
        - crates/alkahest-core/src/config.rs#ServerConfig
        - crates/alkahest-core/src/config.rs#AuthConfig
        - crates/alkahest-api/src/auth.rs#AuthPolicy
        - crates/alkahest-api/src/auth.rs#authenticate
        - crates/alkahest/src/serve_cmd.rs#serve
      tests:
        - crates/alkahest-api/tests/server.rs#default_bind
        - crates/alkahest-api/tests/auth.rs#loopback_needs_no_key
        - crates/alkahest-api/tests/auth.rs#require_on_loopback_demands_keys_locally
        - crates/alkahest-api/src/auth.rs#ipv4_and_ipv6_loopback_and_mapped_addresses_are_local
        - crates/alkahest/tests/serve.rs#serve_binds_the_host_flag_and_answers_version
  R2:
    status: backlog
    trace:
      code:
        - crates/alkahest-api/src/auth.rs#authenticate
        - crates/alkahest-api/src/auth.rs#presented_key
        - crates/alkahest-api/src/auth.rs#check_exposure
        - crates/alkahest-api/src/dialect.rs#DefaultErrorRenderer
      tests:
        - crates/alkahest-api/tests/auth.rs#missing_key
        - crates/alkahest-api/tests/auth.rs#remote_with_valid_key_via_bearer_or_x_api_key
        - crates/alkahest-api/tests/auth.rs#wrong_and_unknown_keys_get_the_same_answer
        - crates/alkahest-api/tests/auth.rs#spoofed_forwarded_for_does_not_grant_loopback_trust
        - crates/alkahest-api/src/auth.rs#startup_refusal
        - crates/alkahest/tests/serve.rs#startup_refusal_without_keys_on_a_network_bind
  R3:
    status: backlog
    trace:
      code:
        - crates/alkahest-api/src/keys/token.rs#generate_key
        - crates/alkahest-api/src/keys/token.rs#hash_key
        - crates/alkahest-api/src/keys/store.rs#KeyStore
        - crates/alkahest-api/src/keys/verify.rs#FileKeyVerifier
        - crates/alkahest/src/keys_cmd.rs#run
      tests:
        - crates/alkahest-api/src/keys/verify.rs#revocation
        - crates/alkahest-api/src/keys/store.rs#create_stores_only_a_salted_hash
        - crates/alkahest-api/src/keys/store.rs#store_file_is_owner_only
        - crates/alkahest-api/tests/auth.rs#revoked_key_is_refused_on_the_next_request
        - crates/alkahest/tests/keys.rs#create_shows_the_key_once_and_stores_only_a_hash
  R4:
    status: backlog
    trace:
      code:
        - crates/alkahest-api/src/dialect.rs#requires_admin
        - crates/alkahest-api/src/keys/verify.rs#Principal
        - crates/alkahest-api/src/auth.rs#authenticate
      tests:
        - crates/alkahest-api/tests/auth.rs#inference_key
        - crates/alkahest-api/tests/auth.rs#opted_out_remote_access_is_anonymous_but_never_admin
        - crates/alkahest-api/src/dialect.rs#admin_routes
  R5:
    status: backlog
    trace:
      code:
        - crates/alkahest-api/src/audit.rs#UsageTracker
        - crates/alkahest-api/src/audit.rs#AuditLog
        - crates/alkahest-api/src/auth.rs#authenticate
        - crates/alkahest/src/keys_cmd.rs#ls
      tests:
        - crates/alkahest-api/tests/audit.rs#last_used
        - crates/alkahest-api/tests/audit.rs#refusals_are_audited_without_secrets_and_local_traffic_is_not
        - crates/alkahest/tests/serve.rs#last_used_shows_in_keys_ls_and_the_audit_log
  R6:
    status: backlog
  R7:
    status: backlog
    trace:
      code:
        - crates/alkahest-core/src/config.rs#EndpointsConfig
        - crates/alkahest-api/src/dialect.rs#Family
        - crates/alkahest-api/src/router.rs#cors_layer
      tests:
        - crates/alkahest-api/tests/auth.rs#disabled_family
        - crates/alkahest-api/tests/auth.rs#cors_is_off_by_default_and_follows_configured_origins
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
