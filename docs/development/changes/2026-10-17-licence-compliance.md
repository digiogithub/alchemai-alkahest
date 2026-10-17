# Licence compliance: `cargo xtask notice`, engine licences (ALK-US-0077)

Implements [[ALK-US-0077]] (tasks ALK-T-0204, ALK-T-0205). Decision: [[0004-licence-policy-and-notices]].
See [[plan/alkahest-v1-plan]].

## What

- `xtask/src/notice.rs` (`cargo xtask notice [--check]`): `cargo_metadata` graphs for the headless
  (`--no-default-features`) and desktop (`alkahest/desktop`) builds, normal dependencies only;
  902 third-party crates. Writes `THIRD_PARTY_LICENSES` (crate index with H/D flavour, licence
  texts de-duplicated with a "used by" list, OFL font texts, engine texts) and the generated
  tail of `NOTICE` (Apache-2.0 dependency NOTICE files; none exist today). `--check` fails when
  either file differs; it also rejects non-permissive crates, recipes without MIT/Apache-2.0 or
  without `packaging/licenses/<id>.txt`. Symbols: `run`, `collect_crates`, `check_crates`,
  `fallback_text`, `render_licenses`, `render_notice`, `check_engines`.
- `packaging/licenses/`: verbatim texts of the five engines (fetched from the pinned upstream
  tags) and `spdx/` standard texts used as fallback for crates that ship no licence file.
- `alkahest-recipes`: `engine_license_text`, `ENGINE_LICENSE_FILE`; every install path writes
  `ALKAHEST-ENGINE-LICENSE.txt` into the engine directory.
- `NOTICE` hand-written part updated; CI `lint` job and `scripts/ci-local.sh` run
  `cargo xtask notice --check`; `typos.toml` excludes the generated and verbatim files.

## Verification

`cargo xtask notice` then `--check` (902 crates, 5 engines), `cargo test -p xtask` (24 tests incl.
6 for notice), `cargo test -p alkahest-recipes --test recipes` (new
`builtin_recipes_declare_licence_and_ship_text`), `cargo deny check licenses` ok.
