# ADR 0004: licence policy and how notices are produced

Status: accepted 2026-10-17. Story: [[ALK-US-0077]]. Plan: [[plan/alkahest-v1-plan]].

## Decision

1. **No GPL-family code in anything Alkahest ships or downloads for the user.** Rust
   dependencies are gated three times: `cargo deny check licenses` (allow list in `deny.toml`),
   `cargo xtask notice`, which refuses any crate whose SPDX expression has no permissive
   alternative (a `GPL-2.0 OR MIT` crate passes by choosing MIT; `LGPL AND MIT` fails), and the
   recipe check below. The same rule already excluded Piper and `espeak-ng` in [[0003-tts-engine]].
2. **Engines are separate processes, not linked.** Their licences (llama.cpp, PrismML fork,
   whisper.cpp, stable-diffusion.cpp: MIT; vLLM: Apache-2.0) are declared in `recipes/*.toml`
   (`license`), their verbatim texts live in `packaging/licenses/<recipe id>.txt`, are embedded
   in the binary and written as `ALKAHEST-ENGINE-LICENSE.txt` next to every installed engine
   (archive, source build and venv alike); packages also copy them under `engines/`.
   `cargo xtask notice` fails when a recipe declares anything but MIT or Apache-2.0 or lacks its text.
3. **`THIRD_PARTY_LICENSES` and the generated tail of `NOTICE` are committed and generated** by
   `cargo xtask notice` from the resolved normal-dependency graphs of the headless and the
   desktop binary (all target platforms), with licence texts read from the crate sources. CI
   runs `cargo xtask notice --check`, so a dependency bump without regeneration fails.
4. A crate that publishes no licence file gets the standard SPDX text (Apache-2.0, MPL-2.0,
   BSL-1.0, CC0-1.0 from `packaging/licenses/spdx/`); MIT is rebuilt with the manifest
   `authors` as the copyright line and marked as such. BSD-family crates without a file fail the
   check (their text needs a real copyright line); none exists today.
5. Embedded OFL fonts are data files, not crates: their texts are appended to
   `THIRD_PARTY_LICENSES` (section 2).
6. MPL-2.0 (file-level copyleft: `option-ext`, `dwrote`) is allowed, unmodified, as in `deny.toml`.

## Consequences

Release packages must contain `LICENSE`, `NOTICE`, `THIRD_PARTY_LICENSES` and the engine texts
([[ALK-US-0074]]). Adding an engine recipe means adding its licence text and re-running the tool.
