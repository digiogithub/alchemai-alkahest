# CI back to green: zbus advisories and cross-platform dependency paths

Date: 2026-10-18. Commits `67fa9a8`, `0e5d045`. Links: [[ALK-US-0123]], [[ALK-US-0119]],
[[ALK-SP-0011]], [[development/plans/ALK-EP-0015-git-catalogs]].

## What

- `deny.toml`: reviewed ignores for RUSTSEC-2026-0370/0371/0372 (`zvariant`, `zbus_names`,
  `zvariant_derive` unmaintained because they were folded into zbus 6). They come in through
  keyring's Linux Secret Service store (keyring 4 -> zbus-secret-service-keyring-store ->
  secret-service -> zbus 5), not gpui. Product owner chose ignoring over waiting; re-evaluate when
  keyring moves to zbus 6. The failing `lint` job had been skipping every other CI job.
- `crates/alkahest-recipes/src/recipe.rs` `check_relative`: a dependency `dest` (and the other
  recipe-relative paths) is refused when it starts with `/` or `\`, contains `:`, or has a `..`
  segment under either separator, on every host. `Path::is_absolute` alone let `/etc` through on
  Windows. Test `dest_escaping_the_install_directory_refused` gains `C:/Windows`, `\x`,
  `a\..\..\b`.
- `crates/alkahest/tests/{catalog,catalog_models}.rs`: `file://` URLs written into TOML use `/`
  (Windows backslashes were invalid TOML escapes).

## Verification

CI run 39074203635 on `0e5d045`: lint (incl. `cargo deny`), machete, desktop, binary size, SDK
conformance, mdBook, tests on ubuntu, macOS and Windows all green. Local: 1359 tests passed.
