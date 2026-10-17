# Optional TLS (HTTPS listener, certificate reload, `alkahest certs generate`)

Story [[ALK-US-0044]] (task ALK-T-0130), requirement [[ALK-SP-0006.R6]]. Builds on
[[2026-10-17-api-server-skeleton]], [[2026-10-17-auth-middleware-exposure]],
[[2026-10-17-graceful-shutdown]] and [[2026-10-17-admin-api]]. Plan: [[plan/alkahest-v1-plan]].

## What changed

- `alkahest-core::config`: `[server.tls]` = `TlsConfig { enabled (false), cert, key,
  reload_interval_secs (60) }` with `cert_path`/`key_path`/`ca_path` (defaults
  `<data_dir>/tls/{cert,key,ca}.pem`); `ServerConfig::scheme()`; `client_url()` returns
  `https://` when TLS is on.
- `alkahest-api::tls` (new):
  - `TlsAcceptor::from_files(cert, key)`: rustls 0.23 with the aws-lc-rs provider (the one
    reqwest already links, no second crypto library), TLS 1.2/1.3, ALPN `h2` + `http/1.1`, no
    client auth. The certificate comes from a `ResolvesServerCert` slot (`RwLock<Arc<
    CertifiedKey>>`), so `reload()` swaps it for new handshakes without touching the listener or
    open connections. `CertifiedKey::from_der` checks that key and leaf match. Key bytes are read
    into `Zeroizing` buffers; a group/world-readable key is warned about.
  - `reload_if_changed()` (mtime + size of both files), `with_reload_interval`, `reloader()`:
    SIGHUP handler (installed eagerly) plus the periodic check; a failed reload keeps serving the
    previous certificate and logs the error once per change.
  - `generate_self_signed(names, days)`: throwaway ECDSA P-256 CA (`pathlen 0`) plus a leaf
    (`serverAuth`, SANs = DNS names and IPs) signed by it. The CA key is dropped right after
    signing and never written. `MAX_VALID_DAYS = 825` (Apple limit). `write_generated`: key file
    created 0600 (never widened), certificates 0644, each via temp file + fsync + rename,
    refuses to overwrite without `overwrite`. `default_names()`: `localhost`, `127.0.0.1`, `::1`,
    host name.
- `alkahest-api::server`: own accept loop (`Server` builder: `new`, `tls`, `options`, `hooks`,
  `force_stop`, `run`) on hyper-util's auto HTTP/1 + HTTP/2 builder, replacing `axum::serve`.
  Each connection is a `JoinSet` task watched by hyper-util's `GracefulShutdown`; TLS handshakes
  are bounded (10 s); `ConnectInfo<SocketAddr>` is inserted per request as before;
  `TCP_NODELAY` set. After the drain timeout the remaining connection tasks are aborted inside the
  library (was: abandoned until process exit). `ShutdownReport` gains `forced` and `aborted`.
  `serve`/`serve_with` keep their signatures.
- Admin network settings: `NetworkSettings.tls`, `PUT {"tls": true}` writes
  `[server.tls] enabled`; the plain-HTTP warning only appears without TLS; a warning names the
  missing certificate/key files. Client: `NetworkSettings.tls`, `NetworkPatch.tls`.
- `alkahest-client`: `Client::for_server(host, port, https)`, `with_ca_cert_pem`,
  `with_ca_cert_file` (merged with system roots), `CA_CERT_ENV = "ALKAHEST_CA_CERT"`,
  `ClientError::Tls` (certificate/handshake failures are no longer reported as "server not
  running") and `ClientError::CaCert`.
- Binary: `alkahest certs generate [--name N]... [--days 397] [--force]` (`certs_cmd.rs`);
  `alkahest serve` loads the certificate before anything else when TLS is on (start refused with
  a hint to `certs generate` when missing), prints `https://`, warns on stderr about plain HTTP on
  a network bind; CLI clients (`ps`, `stop`, `run`) use `https` and trust `ALKAHEST_CA_CERT`, else
  the generated `ca.pem` next to the configured certificate.
- Workspace pins: `hyper-util =0.1.21`, `rustls =0.23.46`, `tokio-rustls =0.26.6` (all already in
  the lock), `rcgen =0.14.10` (MIT/Apache-2.0, `aws_lc_rs` + `pem`, no ring), `zeroize =1.9.1`.
  `THIRD_PARTY_LICENSES` regenerated.
- `docs/install.md`: HTTPS section.

## Decisions

- Own accept loop instead of `axum-server`: one dependency fewer, and the same loop gives the
  connection abort ALK-US-0084 asked for.
- A CA + leaf rather than one self-signed leaf: macOS and Windows platform verifiers accept a
  CA as an extra root but not a self-signed end-entity certificate.
- SIGHUP is handled only when TLS is on, so a foreground `alkahest serve` without TLS keeps the
  default "terminal hangup stops it" behaviour and the systemd unit needs no change.
- No HTTP-to-HTTPS redirect: one listener, one protocol.

## Security checklist

| Threat | Mitigation | Proof |
|---|---|---|
| Keys and prompts sniffed on the LAN | HTTPS with rustls when `[server.tls] enabled` | `crates/alkahest-api/tests/tls.rs#tls` (scenario R6), `crates/alkahest/tests/tls.rs#generated_certificate_serves_https_to_the_cli` |
| Private key readable by other users | written 0600 at creation, temp + rename; warning when an existing key is group/world readable | `tls.rs#generated_pair_loads_and_key_is_owner_only`, CLI test checks mode |
| Generated CA abused to sign other certificates | CA key never written, dropped after signing, `pathlen 0` | code review (`tls.rs#generate_self_signed`) |
| Mismatched or broken certificate after renewal takes the server down | reload validates first, keeps the old certificate on failure | `reload_follows_file_changes_and_keeps_the_old_certificate_on_errors` |
| Expired certificate needs a restart | reload on SIGHUP and on file change | `renewed_certificate_is_served_after_a_file_change`, `sighup_reloads_the_certificate` |
| Slowloris on the handshake holds tasks | 10 s handshake timeout; tasks aborted after the drain | code review (`server.rs#connection`) |
| Client silently trusts anything | CA merged with system roots, never "accept invalid certs"; untrusted server reported as a TLS error | `tls#tls` (stranger CA refused), CLI test (`TLS` error) |
| Server starts without the certificate and is unreachable or falls back to HTTP | start refused before binding | `crates/alkahest/tests/tls.rs#serve_refuses_to_start_without_the_certificate` |
| Plain HTTP on a network bind goes unnoticed | stderr warning at start; admin settings warning | `admin_api.rs#tls_is_toggled_in_the_network_settings`, `network_settings_are_saved_for_the_next_start` |

Open points for product owner review: the desktop app connects with `Client::new(url)` and has no
CA option yet (inbox item); the TLS key is not encrypted at rest (same model as other servers:
file permissions).

## Verification

- `scripts/ci-local.sh` (fmt, clippy `-D warnings`, nextest 1005 passed / 1 skipped, deny,
  check-deps, notice, headless build, typos): passed. `cargo machete` clean for the touched
  crates.
- New tests: `alkahest-api` `tls::tests` (4), `tests/tls.rs` (3), `tests/admin_api.rs#
  tls_is_toggled_in_the_network_settings`; `alkahest-client` `ca_certificates_must_be_pem`,
  `bind_addresses_map_to_reachable_urls`; `alkahest-core` `tls_section_parses_and_defaults_to_the_data_dir`;
  `alkahest` `tests/tls.rs` (2). Existing shutdown tests pass unchanged on the new accept loop.
- `cargo check --target x86_64-pc-windows-gnullvm -p alkahest-api -p alkahest` passes (zig as
  the C compiler for aws-lc-sys).
