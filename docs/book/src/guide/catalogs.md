# Writing a catalog

A catalog is a folder of plain TOML files in a git repository. Alkahest's own catalog lives in
the `catalog/` folder of the Alkahest repository; you can publish one of your own, public or
private, and add it as a source.

## Layout

```text
catalog/
  catalog.toml                  manifest: schema_version, name, description
  index.toml                    generated list of files with sha256 and size
  engines/<id>.toml             one engine recipe per file
  models/<namespace>/<model>.toml   one model entry per file
```

Names use lower-case letters, digits, `-`, `_` and `.`. The file name must equal the `id` of the
recipe, and the path must equal the `name` of the model. Other files (README, scripts) are
ignored and never fetched.

## Adding an engine

Write `engines/<id>.toml` in the recipe format described in [Engines and recipes](engines.md).
In a catalog every download needs a `sha256` and an `https` URL; `tofu = true` is not accepted.

## Adding a model

```toml
name = "acme/assistant"
description = "Small assistant model."
license = "apache-2.0"
tags = ["chat"]

[[variants]]            # the first variant is the default tag
tag = "q4_k_m"
format = "gguf"
quant = "Q4_K_M"
template = "chatml"
params = { num_ctx = 8192 }
[[variants.files]]
kind = "model"          # exactly one model file per variant
url = "https://example.org/assistant-q4_k_m.gguf"   # or urls = [mirror, mirror]
sha256 = "..."          # mandatory
size = 123456789        # mandatory, bytes
```

For Hugging Face files, `https://huggingface.co/api/models/<repo>/tree/main` lists the
`lfs.oid` (the sha256) and `size` of each file.

## Validate

Regenerate the index after every change, then check:

```sh
cargo xtask catalog-index catalog   # rewrites index.toml
cargo xtask catalog-check catalog   # strict validation, offline
cargo xtask catalog-check catalog --net   # also checks every URL answers with the declared size
```

The check is strict: unknown keys, plain `http` URLs, missing digests, mismatching file names
and a stale index all fail. Run `catalog-check` in your own CI.

In the Alkahest repository, `catalog/engines/` is the source of the built-in `recipes/`
directory. After editing an engine run `cargo xtask catalog-index --sync-recipes`, which also
copies the recipes; CI fails when the two differ.
