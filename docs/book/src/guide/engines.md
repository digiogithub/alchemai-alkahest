# Engines and recipes

Alkahest does not run inference itself. It starts an **engine** as a separate process per loaded
model, speaks HTTP to it, translates the API dialects and supervises it (health checks, restart
rules, clean shutdown, no orphans). Engines are installed from **recipes**.

| Engine id | Runs | Notes |
|---|---|---|
| `llama-cpp` | any GGUF model: chat, completion, embeddings, rerank, vision | mainline llama.cpp; the default engine |
| `prism-llama-cpp` | Bonsai ternary models (`PQ2_0`, `PTQ1_0` quantisations) | the PrismML fork of llama.cpp |
| `vllm` | safetensors snapshots | Linux with NVIDIA, experimental on Apple Silicon; a Python virtual environment |
| `whisper-cpp` | speech-to-text (`/v1/audio/transcriptions`) | upstream publishes no binaries, so it builds from source |
| `stable-diffusion-cpp` | image generation (`/v1/images/generations`) | prebuilt for most platforms |

Next to these, recipes can define [fork engines](#fork-engines): performance forks of llama.cpp
(or of another engine) that install side by side with mainline and run only for the models
that ask for them.

```sh
alkahest engines ls                  # installed and installable engines
alkahest engines info llama-cpp      # versions, variants, requirements, what is installed
alkahest engines install llama-cpp   # the best build for this machine
alkahest engines rm llama-cpp --version b11669
```

## Which engine runs a model

Alkahest chooses; you only override. In order:

1. the model's own `engine` setting (`alkahest config model <m> set engine=prism-llama-cpp`,
   optionally pinned: `engine=llama-cpp@b11669`);
2. image and video kinds run on stable-diffusion.cpp, speech-to-text on whisper.cpp;
3. safetensors snapshots run on vLLM;
4. `PQ2_0` and `PTQ1_0` quantisations run on the PrismML fork;
5. every other GGUF runs on mainline llama.cpp.

Rules 2 to 5 only ever pick the built-in engines above. A [fork engine](#fork-engines) runs a
model only when that model's `engine` setting names it.

If the engine a model needs is missing, the request fails with the exact command that installs
it (`alkahest engines install prism-llama-cpp`). Set `engines.auto_install = true` to let the
server install it on first use instead; this is off by default because it downloads executables.

## Accelerators and variants

A recipe has one **variant** per accelerator. Without `--variant`, Alkahest detects the hardware
(`alkahest sysinfo` shows what it sees) and installs the best match.

| Variant | Hardware | Needs on the host |
|---|---|---|
| `cpu` | everything | nothing; SIMD backends are chosen at run time |
| `cuda12.8`, `cuda13.x` | NVIDIA, Linux | a driver for that CUDA version and the CUDA runtime libraries (below) |
| `cuda12.4` | NVIDIA, Windows | a driver for CUDA 12.4+, the CUDA DLLs |
| `vulkan` | any GPU with a Vulkan 1.x driver | the Vulkan loader and driver |
| `rocm` | AMD, Linux and Windows | ROCm / HIP |
| `metal` | Apple Silicon | nothing (built into the macOS archive) |
| `sycl` | Intel oneAPI | installed on request only, never picked automatically |
| `cpu-native` | this machine's CPU | built from source on request (`--native`); git, cmake, ninja |
| `cuda-native` | this machine's NVIDIA GPUs | built from source on request (`--native` on a CUDA host); also the CUDA toolkit (`nvcc`) |

Several versions and variants can be installed side by side under
`<data dir>/engines/<id>/<version>-<variant>/`. Engines in the data directory win over the
bundled copy shipped in packages.

### CUDA runtime libraries

Upstream CUDA builds do not include `libcudart` and `libcublas`. Either install the CUDA runtime
system-wide (the toolkit or your distribution's runtime packages), or let Alkahest unpack the
runtime next to the engine:

```sh
alkahest engines install llama-cpp --variant cuda12.8 --with-runtime   # +150 to 600 MB
```

### Building from source

```sh
alkahest engines install llama-cpp --from-source    # needs git, cmake and ninja
```

Every variant carries the source build steps for its accelerator. A recipe without a prebuilt
archive for your platform (whisper.cpp today) builds from source automatically; the CUDA build
needs `nvcc`, the Vulkan build `glslc`.

**Native builds.** The portable builds run on any CPU and any GPU of their backend. A native
build is compiled for this machine only, which is usually a little faster and never portable:

```sh
alkahest engines install llama-cpp --native                       # cuda-native on a CUDA host, else cpu-native
alkahest engines install llama-cpp --variant cuda-native --from-source
alkahest engines install llama-cpp --variant cuda-native --cuda-arch 89      # an RTX 40xx
alkahest engines install llama-cpp --variant cuda-native --cuda-arch "86;89" # several GPUs
alkahest engines install llama-cpp --native --dry-run             # print the recipe and cmake flags only
```

`cuda-native` uses the flags of the other llama.cpp source builds plus `-DGGML_CUDA=ON
-DCMAKE_CUDA_ARCHITECTURES=native`, so nvcc compiles for the GPUs it finds. `--cuda-arch`
replaces `native` with an explicit list (useful when the build machine has no GPU, or to cover
several cards); it works with any CUDA source build and is refused for other variants. Native
variants are never picked automatically; once installed, a native build of a backend is
preferred over the portable build of the same backend.

## Bonsai and the PrismML fork

Bonsai models use ternary quantisations that mainline llama.cpp does not read. Pull such a model
and Alkahest selects `prism-llama-cpp`, a pinned build of the PrismML fork
(`prism-b10777-e3de715`):

```sh
alkahest engines install prism-llama-cpp
alkahest pull hf.co/<org>/<bonsai-repository>:PQ2_0
alkahest run <that model> "Hello"
```

A newer fork build is used when the tested one is not installed; pin it strictly with
`engine=prism-llama-cpp@<version>`.

## Pins and upgrades

Recipes pin one upstream release with the SHA-256 of every archive; nothing is installed unless
the digest matches. A weekly workflow reports newer upstream releases. Installing a newer
version leaves older ones in place, so models can keep the version their settings pin.

## Catalogs: recipes without a new release

Besides the recipes compiled into the binary, Alkahest reads engine recipes and model entries
from **catalogs**: folders of TOML in git repositories. Out of the box there is one, `official`,
the `catalog/` folder of `github.com/digiogithub/alchemai-alkahest` at `main`.

```sh
alkahest catalog ls                       # catalogs, commit, age, entry counts, problems
alkahest catalog sync [<name>]            # fetch now
alkahest catalog add acme https://git.example.org/acme/catalog --ref stable [--subdir .] [--pin <commit>]
alkahest catalog rm acme [--delete-token]  # removes the source and its snapshot
alkahest catalog disable acme             # keeps it configured, contributes nothing
alkahest catalog enable acme
alkahest catalog edit acme --ref v2 [--subdir .] [--pin <commit> | --unpin] [--refresh-interval 6h]
alkahest catalog token set acme           # reads the token from a hidden prompt or stdin
alkahest catalog token clear acme
```

`official` cannot be removed: `catalog rm official` disables it. The desktop app and remote
tools do the same through the admin API (`/alkahest/v1/catalogs`, see the API reference).

- **Layers.** Lowest first: built-in recipes, then the catalogs in configured order (a catalog
  added later overrides earlier ones), then `recipes/` next to the binary, then
  `<data dir>/recipes/`. The same engine, version and variant in a higher layer wins.
  `engines ls` (column `origin`) and `engines info` (`from ...`) show where each entry comes from,
  for example `catalog official@c1c1877bc8fb`.
- **Snapshots.** Each catalog is stored under `<data dir>/catalogs/<name>/` with the commit it
  came from. A new snapshot replaces the old one only when every file validates; otherwise the
  last good snapshot stays in use and the error appears once on the next command and in
  `catalog ls`. Unknown keys from a newer catalog format are ignored with a warning.
- **Refresh.** `serve` and `engines` refresh a catalog older than its `refresh_interval`
  (default `24h`) in the background and never wait for the network. A catalog that was never
  fetched is fetched once in the foreground (at most 10 s); offline, the built-in recipes are
  used and one notice says so. After a failed sync the next attempt comes 15 minutes later
  (or at the refresh interval, if shorter). `ALKAHEST_CATALOG_AUTO_SYNC=0` turns automatic
  refresh off; `pin = "<commit>"` freezes a catalog.
- **Limits.** `[catalog_limits]` bounds a sync: `max_snapshot_bytes` (16 MiB),
  `max_file_bytes` (1 MiB) and `max_archive_bytes` (256 MiB, the commit archive of the
  fallback transport; its decompressed stream is bounded to four times that).
- **Transport.** No `git` is needed: the ref is resolved over git smart HTTP, then only the
  changed files of that commit are downloaded (HTTPS, system proxy settings honoured). GitHub,
  GitLab and Gitea/Forgejo work as is; other hosts need `raw_url` and `archive_url` templates
  in the `[[catalogs]]` entry. A `file://` URL reads a plain directory (air-gapped installs).
- **Private catalogs.** Store the token with `alkahest catalog token set <name>` (it goes to
  the OS keyring, service `alkahest`, user `catalog:<name>`; never pass it as an argument) or
  set `ALKAHEST_CATALOG_TOKEN_<NAME>` (name upper-cased, `-` and `.` as
  `_`). It is looked up only after the host refuses anonymous access (401/403), sent only to
  the repository's own host, and never written to configuration, snapshots or logs.
- **Safety.** A sync only downloads and parses TOML. It never runs anything and never
  downloads an engine or a model; that happens on `engines install` or `pull`, verified against
  the sha256 the catalog pins.

## Fork engines

A fork engine is a build of an engine from another repository (a llama.cpp fork with a
TurboQuant KV cache, multi-token prediction, new kernels) that runs **next to** the mainline
engine instead of replacing it. A fork:

- is defined by a recipe whose `id` is new and whose `base` names the built-in engine family it
  behaves like (`llama-cpp` for anything that is a `llama-server`): same command line, same
  HTTP API, same Alkahest adapter;
- installs under its own id, `<data dir>/engines/<id>/<version>-<variant>/`, and shows up in
  `engines ls`, `engines info`, `engines install`, `engines rm`, the admin API and the desktop
  Engines view;
- has its own `engines.overrides.<id>` (it does not inherit the family's);
- is never found on `PATH` (its `llama-server` would be indistinguishable from mainline's);
- is **never chosen automatically**. A model runs on it only when its `engine` setting names it.

A recipe skeleton, for `<data dir>/recipes/llama-cpp-myfork.toml` or a catalog's
`engines/llama-cpp-myfork.toml` (a fork id may not reuse a built-in id or alias):

```toml
id = "llama-cpp-myfork"
base = "llama-cpp"                 # the family whose adapter runs it
version = "f1a2b3c"                # pin a tag or commit
description = "llama.cpp fork with <feature>"
license = "MIT"
homepage = "https://github.com/<owner>/<fork>"
executable = "llama-server"

[variants."cpu"]
accelerator = "cpu"
[variants."cpu".source]
git = "https://github.com/<owner>/<fork>.git"
rev = "f1a2b3c"
cmake_flags = ["-DBUILD_SHARED_LIBS=ON", "-DLLAMA_CURL=OFF", "-DCMAKE_BUILD_WITH_INSTALL_RPATH=ON", "-DCMAKE_INSTALL_RPATH=$ORIGIN"]
targets = ["llama-server"]

[variants."cuda-native"]
accelerator = "cuda"
native = true
[variants."cuda-native".source]
git = "https://github.com/<owner>/<fork>.git"
rev = "f1a2b3c"
cmake_flags = ["-DBUILD_SHARED_LIBS=ON", "-DLLAMA_CURL=OFF", "-DCMAKE_BUILD_WITH_INSTALL_RPATH=ON", "-DCMAKE_INSTALL_RPATH=$ORIGIN", "-DGGML_CUDA=ON", "-DCMAKE_CUDA_ARCHITECTURES=native"]
targets = ["llama-server"]
```

Prebuilt archives (`[[variants.<v>.assets]]` with `sha256`) work exactly as for the built-in
recipes. A recipe whose `id` is unknown and has no `base`, or whose `base` is not a built-in
engine, is rejected with the reason (`engines ls` prints it as a warning).

Install it and opt a model in:

```sh
alkahest engines install llama-cpp-myfork --native            # or --variant cpu --from-source
alkahest config model qwen3:8b set engine=llama-cpp-myfork    # or engine=llama-cpp-myfork@f1a2b3c
alkahest config model qwen3:8b set "extra_args=--cache-type-k turbo3 --cache-type-v turbo3"
alkahest show qwen3:8b                                        # engine and engine args
```

Fork-only flags (new KV cache types, speculative decoding switches) go in the model's
`extra_args` or in `engines.overrides.<fork id>.extra_args`. A model that names a fork which is
not installed fails with `alkahest engines install <fork id>`; with
`engines.auto_install = true` the server installs it on first use (a source-only fork then
compiles, which takes minutes). A fork that is installed keeps working when its recipe is
removed: the install records its family.

### Benchmarking a fork against mainline

Compare like with like: the same GGUF file, the same context length (`num_ctx`), the same
`gpu_layers`, flash attention setting, batch size and number of slots, `temperature=0` and a
fixed `seed`, and nothing else using the GPU. Measure:

| Metric | How |
|---|---|
| prompt processing (t/s) | `llama-bench -p 512` or `timings.prompt_per_second` of a `/completion` response |
| decode (t/s) | `llama-bench -n 128` or `timings.predicted_per_second` |
| time to first token | streaming request, time until the first chunk; same prompt, warm model |
| peak VRAM | `nvidia-smi --query-gpu=memory.used --format=csv -lms 200` during the run, or `alkahest ps` |

Prebuilt llama.cpp archives include `llama-bench` next to `llama-server`; a source build only
compiles the recipe's `targets`, so add `"llama-bench"` there (or build it from the same
checkout). Run it from each install directory (`<data dir>/engines/<id>/<version>-<variant>/`)
with identical arguments, for example
`./llama-bench -m model.gguf -p 512 -n 128 -ngl 999 -fa 1 -r 5`. To measure through Alkahest,
create a copy of the model (`alkahest cp qwen3:8b qwen3-fork`), set `engine=` on the copy only,
and send the same request several times to each name, discarding the first (load) run. Report
the median of at least five runs and the build versions (`alkahest engines ls`).

## Overriding an engine binary

Point a model family at your own build in the configuration file:

```toml
[engines.overrides.llama-cpp]
path = "/opt/llama.cpp/build/bin/llama-server"
extra_args = ["--no-mmap"]
```

`extra_args` are appended to the command line whenever that engine starts. A model's own
`extra_args` (`alkahest config model <m> set "extra_args=..."`) come after them, so for flags
whose last value wins the model's setting overrides the engine-wide one. The flags Alkahest
manages (`--host`, `--port`, `-m`, `--model`, `--api-key`, `--api-key-file`) cannot be set per
model.

## Engine logs

Each engine process writes its own log file under the log directory. `GET
/alkahest/v1/logs/engines` and the desktop app's Running view show them.
