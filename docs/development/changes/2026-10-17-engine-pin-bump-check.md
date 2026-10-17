# Engine pin bump check (ALK-US-0053)

Implements [[ALK-US-0053]] (task ALK-T-0152). Recipes: [[2026-10-17-recipe-format-engine-manager]].
See [[plan/alkahest-v1-plan]].

## What

- `xtask/src/engines.rs`, `cargo xtask engines-check [--fail-on-update]`: reads the pins from
  `recipes/*.toml` (`parse_pin`, `read_pins`), asks GitHub releases (llama.cpp, PrismML fork,
  whisper.cpp, stable-diffusion.cpp) or PyPI (vLLM, from the venv package) through `curl`
  (`Http` trait, `Curl`), and prints a Markdown report, also appended to `$GITHUB_STEP_SUMMARY`.
  For a newer GitHub release it lists the replacement of every prebuilt asset of the pin
  (matched on the part after `-bin-`) with the `digest` GitHub publishes, or "no counterpart".
- Version comparison is `tag_key` (integers of the tag, hashes and words ignored) inside the same
  `tag_family` (`b`, `v`, `prism-b`, `master-`), so unrelated tag series and prereleases and drafts
  are never offered; PyPI pre-releases and fully yanked versions are skipped.
- `.github/workflows/engines-check.yml`: weekly (Monday 06:17 UTC) and manual; reports only,
  opens nothing.

## Verification

Eight unit tests with JSON fixtures (tag keys, newer release with digests, up to date,
prerelease/other family, missing asset, PyPI, fixture HTTP, real recipes parse). A live run on
2026-10-17 reported llama.cpp, the Prism fork, whisper.cpp and stable-diffusion.cpp up to date
and vLLM 0.11.0 behind 0.31.0 (32 releases).
