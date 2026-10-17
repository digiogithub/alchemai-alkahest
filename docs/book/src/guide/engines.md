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

## Overriding an engine binary

Point a model family at your own build in the configuration file:

```toml
[engines.overrides.llama-cpp]
path = "/opt/llama.cpp/build/bin/llama-server"
extra_args = ["--no-mmap"]
```

`extra_args` are appended to the command line whenever that engine starts.

## Engine logs

Each engine process writes its own log file under the log directory. `GET
/alkahest/v1/logs/engines` and the desktop app's Running view show them.
