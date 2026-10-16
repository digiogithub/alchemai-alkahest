# Change: model reference parsing (ALK-US-0011)

Implements [[ALK-US-0011]] / [[ALK-SP-0001]].R3. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-store`, modules `reference` and `digest`:

- `ModelName`: fully qualified `host/namespace/model:tag` with Ollama defaults
  (`registry.ollama.ai`, `library`, `latest`), Ollama's part grammar and length limits, host
  lowercased and `ollama.com` folded into `registry.ollama.ai`. `display_short()` (as
  `ollama ls`), `eq_ignore_case()`, `relative_path()` / `from_relative_parts()` (manifest
  tree key, host port encoded `%3A` like Ollama's `manifests-v2`), `to_hf()`.
- `HfRef` + `HfSelector::{Quant, File}`: `hf.co/<org>/<repo>[:<quant|file>]`,
  `huggingface.co/...`, `https://huggingface.co/...`, `hf://org/repo[/path/file.gguf]`.
  A selector ending in `.gguf` is a file, otherwise a quant; `latest` means none.
  `to_model_name()` gives the storage key `hf.co/<org>/<repo>:<quant|file basename|latest>`,
  identical to what Ollama writes for `ollama pull hf.co/...`.
- `ModelRef::{Name{name, digest}, HuggingFace, Digest}` and `ScopedRef { origin, reference }`
  for the `ollama:` / `alkahest:` prefixes; `Origin::{Alkahest, Ollama}`.
- `Digest`: SHA-256, parses `sha256:<hex>` and `sha256-<hex>`, formats either.

## Decisions

- Normalisation happens at parse time, so `Display` round-trips (`parse(display(r)) == r`)
  for every value; property tests (`tests/reference_props.rs`) enforce it, plus "parser never
  panics" and "every stored name is a safe relative path" (no `..`, no separators).
- Scope prefixes always win (a model literally named `ollama` is written `library/ollama:tag`).
- Under `hf.co` the namespace may contain `.`, repositories up to 96 chars and tags up to 128
  (file names); elsewhere Ollama's limits apply.
- Bare hex digest prefixes (`ollama show 1a2b3c...`) are not parsed here: they are
  indistinguishable from model names and need store lookup; the catalog (ALK-US-0012) can
  resolve them.

## Verification

`cargo nextest run -p alkahest-store`: 12 unit tests (incl. `short_name_resolves`, the spec
scenario) and 6 property tests. The never-panic property found a UTF-8 slicing bug in the
scope-prefix check, fixed before commit. fmt, clippy `-D warnings`, deny, check-deps and the
headless build pass.
