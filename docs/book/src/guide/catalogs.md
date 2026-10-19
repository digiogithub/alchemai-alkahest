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

The `id` is either a built-in engine (`llama-cpp`, `prism-llama-cpp`, `vllm`, `whisper-cpp`,
`stable-diffusion-cpp`), in which case the recipe adds versions or variants to it, or a new
**fork engine**, which must declare the built-in family whose command line and API it speaks:

```toml
id = "llama-cpp-turboquant"
base = "llama-cpp"
```

A recipe with an unknown `id` and no `base`, a `base` that is not a built-in id, or a fork that
reuses a built-in id or alias (`llama.cpp`, `prism`, `sd`, ...) is rejected by the catalog
sync and by `cargo xtask catalog-check`. See [Fork engines](engines.md#fork-engines) for what a
fork is and how models opt in.

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

The check is strict: unknown keys, plain `http` URLs (other than to a loopback mirror such as
`http://127.0.0.1:8080/...`), missing digests, mismatching file names and a stale index all
fail. Run `catalog-check` in your own CI. It applies the same model-entry rules as a sync, so a
catalog that passes the check is accepted by every Alkahest.

In the Alkahest repository, `catalog/engines/` is the source of the built-in `recipes/`
directory. After editing an engine run `cargo xtask catalog-index --sync-recipes`, which also
copies the recipes; CI fails when the two differ.

## Finding and pulling catalog models

Catalog models are named with the pseudo-host `alkahest`: `alkahest/<namespace>/<model>[:<tag>]`,
for example `alkahest/qwen/qwen2.5-0.5b-instruct:q4_k_m`. The name works like a
`hf.co/<org>/<repo>` name: the first of the three segments is the host. `alkahest` is never
contacted as a registry; the entry is looked up in the catalog snapshots.

```sh
alkahest search --source catalog qwen          # catalog entries only, one row per tag
alkahest search qwen                           # catalogs first, then Hugging Face and Ollama
alkahest search --catalog acme assistant       # entries of one catalog

alkahest pull alkahest/qwen/qwen2.5-0.5b-instruct            # default tag (first variant)
alkahest pull alkahest/qwen/qwen2.5-0.5b-instruct:q8_0
alkahest pull alkahest/acme/assistant --catalog official     # take the entry from one catalog
```

- **Tag.** Without a tag (or with `latest`, unless the entry has a variant named `latest`) the
  first variant of the entry is pulled, and the model is stored under that concrete tag:
  `alkahest/qwen/qwen2.5-0.5b-instruct:q4_k_m`. Afterwards the name without a tag still finds
  the model as long as only one tag of it is installed.
- **Which catalog.** When several catalogs define the same entry and tag, the catalog listed
  last in `[[catalogs]]` wins; `--catalog <name>` picks one explicitly.
- **Downloads.** Every file of the variant is downloaded with the resumable, parallel
  downloader and verified against its `sha256` and `size`. With `urls = [...]` the mirrors are
  tried in order. A file whose digest does not match fails the pull: no model is registered and
  nothing of it stays in the store. An interrupted download resumes on the next `pull`.
- **Private files.** A file with `auth = "<host>"` sends a token only to URLs on that host. The
  token comes from `ALKAHEST_HOST_TOKEN_<HOST>` (host upper-cased, other characters as `_`) or
  the OS keyring (service `alkahest`, user `host:<host>`); it is never written anywhere else.
- **Engine, template, parameters.** `engine`, `template` (or a Jinja `template_text`) and the
  parameters Alkahest understands (`num_ctx`, `temperature`, `top_p`, `top_k`, `min_p`,
  `repeat_penalty`, `presence_penalty`, `num_predict`, `stop`) become the model's recommended
  defaults, so `alkahest run` and the server use them; your own
  `alkahest config model <name> set ...` still wins. The manifest also records them, with the
  catalog name and commit, in an `application/vnd.alkahest.config.v1+json` layer, and the
  parameters as an Ollama `params` layer that `alkahest show` prints. A Go `template_text` is
  stored as the Ollama `TEMPLATE` layer.
- **Listing.** `alkahest ls` shows the model under its `alkahest/...` name, so its origin is
  visible at a glance. `alkahest rm` and `cp` work as for any other model.

Variants with `format = "safetensors"` are listed by `search` but cannot be pulled yet.
