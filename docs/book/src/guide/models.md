# Models

## Names

A model reference is one of:

| Form | Example | Where it comes from |
|---|---|---|
| Ollama name | `llama3.2:3b`, `library/qwen3:8b`, `all-minilm` | the Ollama registry (default tag `latest`) |
| Hugging Face | `hf.co/ggml-org/tiny-llamas:Q4_0` | a Hugging Face repository; the part after `:` is a quantisation or a file name |
| Hugging Face (other spellings) | `huggingface.co/org/repo`, `hf://org/repo/file.gguf` | same as above |
| Catalog | `alkahest/qwen/qwen2.5-0.5b-instruct:q4_k_m` | a model entry of a git catalog (pseudo-host `alkahest`); without a tag, the entry's first variant. See [Writing a catalog](catalogs.md) |
| Scoped | `ollama:llama3.2`, `alkahest:llama3.2` | force the Ollama store or Alkahest's own store when a name exists in both |
| Digest | the first characters of the `ID` column of `alkahest ls` | any installed model |

Hugging Face models are stored under the same name Ollama would give them
(`hf.co/org/repo:Q4_K_M`), so both tools see the same names.

## Downloading

```sh
alkahest pull llama3.2:3b
alkahest pull hf.co/Qwen/Qwen3-8B-GGUF:Q4_K_M
alkahest pull hf.co/Qwen/Qwen3-8B-GGUF             # no quantisation: Q4_K_M if present, else the smallest
alkahest pull hf.co/org/repo --safetensors         # a safetensors snapshot, for vLLM
alkahest search qwen3 coder                        # Ollama library and Hugging Face
alkahest search --source hf --gguf "tiny llama"
```

Downloads are resumable (run the same command again after an interruption), parallel and
verified against the SHA-256 the registry or the Hub publishes. Use `ALKAHEST_PULL_MAX_BPS`
or `pull.max_bytes_per_sec` to cap the bandwidth.

Gated or private Hugging Face repositories need a token: `alkahest hf login` stores it in the OS
keyring (it is read from standard input and never printed), or set `HF_TOKEN`. The token is sent
to the Hub only, never to the download CDN. `alkahest hf whoami` shows the account.

A Hugging Face repository with a vision projector (`mmproj`) downloads it too, and the model is
listed as a vision model.

### Quantisations

The quantisation (`Q4_K_M`, `Q8_0`, `IQ2_M`, ...) trades size and speed against quality.
`Q4_K_M` is the usual balance; `Q8_0` is near lossless; the `IQ` and `Q2`/`Q3` families fit
bigger models in less memory at a quality cost. The `PQ2_0` and `PTQ1_0` quantisations of the
Bonsai ternary models run on the PrismML fork of llama.cpp, which Alkahest selects for you
(see [Engines and recipes](engines.md)). `alkahest ls` shows the quantisation of each model and
`alkahest show <model>` the details.

## Listing and inspecting

```sh
alkahest ls                  # NAME, ID, SIZE, MODIFIED, SOURCE, KIND, FORMAT, QUANT
alkahest ls --json
alkahest show llama3.2:3b    # architecture, parameters, context, quantisation, template, licence
alkahest show llama3.2:3b --modelfile
```

`KIND` is detected from the weights: `chat`, `completion` (no chat template), `embeddings`,
`rerank`, `vision`, `decision`, speech-to-text, image generation. The kind decides which API
accepts the model and which engine runs it. A completion-only model answers `/v1/completions`
and `/api/generate` but refuses chat with `does not support chat`.

`SOURCE` is `alkahest` (Alkahest's store), `ollama` (an Ollama store, read-only) or
`ollama-cloud` (Ollama cloud placeholders, not runnable locally).

## Importing local files

Models you already have on disk (a GGUF downloaded by hand, a llama.cpp conversion, a
Hugging Face safetensors checkout) are registered with `import` or `create`:

```sh
alkahest import ./Qwen3-4B-Q4_K_M.gguf                 # name: Qwen3-4B-Q4_K_M
alkahest import ./big-model/ --name big                # a folder (see below)
alkahest create mine --from ./model.gguf --link        # no copy: the file must stay put
alkahest create mine -f ./Modelfile                    # like `ollama create -f`
alkahest run ./model.gguf "Hello"                      # imports with --link, then runs
```

What a path may be:

- **A `.gguf` file**: one model. Format, family, quantisation and context length come from the
  GGUF header; the name defaults to the file stem. A shard of a split model
  (`x-00001-of-00003.gguf`) brings all its shards.
- **A folder** with one GGUF model: every shard of a split model (all of them must be there),
  plus an `mmproj*.gguf` projector, which becomes the vision layer. A folder that holds
  several different GGUF models is refused; import one file instead.
- **A Hugging Face safetensors folder** (`config.json` and `*.safetensors`, with the tokenizer
  files): a safetensors model for the vLLM engine, the same layout `pull --safetensors`
  produces.
- A **`Modelfile`** inside the folder is honoured: `TEMPLATE`, `PARAMETER`, `SYSTEM`,
  `LICENSE`, and a `FROM ./file.gguf` that picks a file of the folder. With `create -f`,
  `FROM ./file.gguf` and `FROM ./folder` are relative to the Modelfile, and `FROM <model>`
  builds on an existing model. `ADAPTER` is refused: no engine applies LoRA adapters yet.

The files are **copied** into Alkahest's store by default, hashed on the way (the blob digests
are the sha256 of your files), with a progress bar per file. With `--link` nothing is copied:
the store gets a hard link to each file when it is on the same filesystem, otherwise a
symbolic link to it. A linked model depends on the files staying as they are: before every
load Alkahest checks each linked file's size and modification time and refuses to load the
model, naming the file, when one changed or disappeared (import it again). With a hard link,
deleting the original is harmless, since the store keeps the data. Either way **the source
files are never modified, moved or deleted**, and nothing is written in an Ollama store
(files inside it are only ever linked symbolically).

`alkahest run ./model.gguf` (or a folder) imports in link mode under the default name and
runs that model; the next run of the same unchanged file reuses the import. `alkahest rm`
removes the model (and its links), never your file.

The server offers the same import to programs on the machine:
`POST /alkahest/v1/models/import` (see the [API reference](../reference/api.md)).

## Copying and deleting

```sh
alkahest cp llama3.2:3b my-assistant    # a new name for the same weights; nothing is copied on disk
alkahest rm my-assistant
alkahest gc --dry-run                   # blobs no model references, abandoned temporary files
alkahest gc
```

`rm` on a model that lives in the Ollama store is refused: that store is read-only.

## Reusing an Ollama store

If Ollama is installed, Alkahest finds its store in this order: `store.ollama.path` or
`ALKAHEST_OLLAMA_MODELS`, `OLLAMA_MODELS`, `~/.ollama/models`, then the Linux service location
(`/usr/share/ollama/.ollama/models`). Its models appear in `alkahest ls` and can be served
without any download. The guarantee is strict: **Alkahest never creates, modifies or deletes a
file there**, and a test compares the store before and after a run. Pulling a model that the
Ollama store already has downloads nothing; use `pull --force` (alias `--copy`) to download it
into Alkahest's own store anyway. Turn the feature off with `store.ollama.enabled = false`.

The service user of a system install needs read access to a system Ollama store; the unit adds
the `ollama` group when it exists.

## Safetensors models and vLLM

A Hugging Face repository without GGUF files is pulled as a safetensors snapshot (or force it
with `--safetensors`) and stored as `hf.co/org/repo:safetensors`. Snapshots run on **vLLM**, which
is available on Linux with an NVIDIA GPU (and experimentally on Apple Silicon):

```sh
alkahest engines install vllm      # a Python virtual environment with a pinned vLLM
alkahest pull hf.co/org/repo --safetensors
```

On other platforms a snapshot cannot be loaded and the error says why.

## Per-model settings

Every model can carry its own settings (context length, GPU layers, KV cache types, flash
attention, sampling defaults, keep-alive, engine, extra engine arguments, ...):

```sh
alkahest config model llama3.2:3b show
alkahest config model llama3.2:3b set num_ctx=8192 kv_cache_type_k=q8_0 keep_alive=30m
alkahest config model llama3.2:3b unset num_ctx
```

Precedence, highest first: the request, the model's own settings, the recommended defaults
stored when the model was pulled (from the model card), the global configuration. `show` prints
the layer each value came from. Model settings survive `rm` followed by `pull`. The settings
file of a model is plain TOML; `config model <model> path` prints where it is.

Pin an engine for one model with the `engine` setting: `engine=llama-cpp` or a version pin
such as `engine=llama-cpp@b11669`.
