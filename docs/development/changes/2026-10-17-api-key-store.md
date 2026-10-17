# API key store and `alkahest keys`

Story [[ALK-US-0041]], requirement [[ALK-SP-0006.R3]]. Builds on
[[2026-10-17-api-server-skeleton]]. Plan: [[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest-api/src/keys/`:
  - `token.rs`: key format `alk_<id>_<secret>`: 8-char public id + 52-char secret,
    lower-case base32 from `rand::rngs::SysRng` (OS CSPRNG, unbiased 5-bit mapping),
    260 bits of secret entropy (compile-time asserted >= 256). `ApiKeySecret` (redacted
    `Debug`, `expose()` for the one-time display), `parse_key` (exact shape check before
    any hashing), `hash_key` = hex `SHA-256(salt16 || key)`, `constant_time_eq`.
  - `store.rs`: `KeyStore` over `ResolvedPaths::key_store` (default
    `<data_dir>/keys.db`, JSON `{"version":1,"keys":[...]}`). `KeyRecord` (id, name,
    salt, hash, created, scopes, optional model allowlist, optional expiry, revoked time;
    no secret). `create(NewKey, now)`, `revoke(id | alk_id | name, now)`, `load()`.
    Writes: `<store>.lock` writer lock (`create_new`, 30 s stale break), temp file with
    mode 0600, `fsync`, rename. Warns if the store is group/world readable.
  - `verify.rs`: `KeyVerifier` trait (`verify`, `active_keys`) and `FileKeyVerifier`,
    which re-reads the store whenever its mtime, length or inode changes (one `stat` per
    request), so a revocation applies to the very next request. `Principal`
    (`has_scope`, admin implies inference; `allows_model`). `KeyRejection` reasons for
    logs; fail closed on an unreadable store.
  - `mod.rs`: `Scope` (`inference`, `admin`), `KeyError`.
- `crates/alkahest/src/keys_cmd.rs`: `keys create <name> [--scope ..] [--model ..]
  [--expires 12h|30d|8w|RFC3339]` prints the key once on stdout (warning on stderr),
  `--json` adds it as `key`; `keys ls` (id, name, scopes, models, created, expires,
  status; never salt/hash/secret); `keys revoke <id|name>`. `Keys` no longer reports
  "not implemented".

## Decision: salted SHA-256, no verification cache

The orchestrator brief suggested Argon2 plus a verification cache. The secret is 260
random bits, so offline guessing is infeasible regardless of hash speed; a slow hash
would only add latency and make the auth path a CPU amplifier for anyone sending made-up
keys with a known id. SHA-256 over ~80 bytes costs microseconds, so no cache (and no
cache-invalidation-on-revoke problem) is needed. This matches the sibling Radar project
(`rdr_` keys, SHA-256 at rest, constant-time compare). The spec asks for "salted hashes",
which this satisfies.

## Security checklist

| Threat | Mitigation | Proof |
|---|---|---|
| Weak or predictable keys | OS CSPRNG, 260-bit secret, unbiased alphabet mapping | `token.rs` tests `generated_keys_have_the_documented_shape`, `keys_are_unique` |
| Secret disclosure from disk | only salted SHA-256 stored; file 0600 | `store.rs#create_stores_only_a_salted_hash`, `store_file_is_owner_only`; `tests/keys.rs#create_shows_the_key_once_and_stores_only_a_hash` |
| Secret in logs/listings | `ApiKeySecret` redacted `Debug`; `ls` and JSON never show salt/hash/secret | `debug_never_prints_the_secret`, `tests/keys.rs#revoke_by_name_then_listing_shows_revoked` |
| Timing oracle on compare | constant-time compare; unknown ids spend the same hashing work | `constant_time_eq_matches_slice_eq`, `verify.rs` code review |
| Revoked key keeps working | file identity checked each request | `verify.rs#revocation` (scenario) |
| Torn writes / concurrent CLI writers | lock file, temp + fsync + rename | `stale_lock_is_broken`, `store_file_is_owner_only` (no leftovers) |
| Corrupt store accepted | fail closed: every key refused | `corrupt_store_fails_closed` |

Known limits (product owner review): secrets are not zeroized in memory (no `zeroize`
pin); on Windows the store relies on the per-user data directory ACL.

## Verification

`scripts/ci-local.sh`: fmt, clippy, nextest (360 passed, 1 skipped), deny, check-deps,
headless build, typos: all passed. `cargo machete` clean for the touched crates.
