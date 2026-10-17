# Troubleshooting

Start with the three commands that show most problems:

```sh
alkahest sysinfo          # CPU, memory, GPUs and the accelerators Alkahest detected
alkahest engines ls       # which engines are installed
alkahest config show      # the effective configuration and where each part came from
```

For more detail run the failing command with `-v` (debug) or `-vv` (trace), or set
`ALKAHEST_LOG=debug`. The server writes a rotating log file in the log directory
(`alkahest config show` prints the path; `GET /alkahest/v1/logs/server` returns its tail), and
each engine process has its own log (`GET /alkahest/v1/logs/engines`).

## The command cannot connect

`error: ... connection refused`: the CLI client commands (`ls`, `ps`, `run`, `stop`, ...) talk
to a running server. Start it with `alkahest serve` or `alkahest service start`. If it runs on
another address or port, pass `--host` or set `ALKAHEST_HOST`. Check that the port is free: the
default is 11435.

## 401 Unauthorized

The caller is not on loopback (or `auth.require_on_loopback` is on) and sent no valid key. Create
one with `alkahest keys create <name>` and send it as `Authorization: Bearer <key>` or
`x-api-key`. Reverse proxy on the same machine? Its requests look like loopback unless you list
it in `auth.trusted_proxies`; see [Serving and security](guide/serving.md). The CLI and the desktop
app read the key from `ALKAHEST_API_KEY`.

## 403 on pull, delete or settings

Those routes need a key with the `admin` scope when the caller is not local. A 403 on delete of a
model that came from an Ollama store is by design: that store is read-only.

## The server refuses to start on 0.0.0.0

Exposing the server without authentication is refused. Create an API key first, or, on a
network you fully trust, add `--allow-unauthenticated`.

## "engine ... is not installed"

The message names the command. Typically `alkahest engines install llama-cpp`, or
`prism-llama-cpp` for Bonsai models, `vllm` for safetensors, `whisper-cpp` for transcription. Or
enable `engines.auto_install`.

## An engine does not start with a CUDA build

Error text mentions `libcudart.so`, `libcublas.so` or "CUDA driver version is insufficient":

- the NVIDIA driver must support the CUDA version of the build (`nvidia-smi` shows the maximum);
- the CUDA runtime libraries must be present: install them system-wide or reinstall the engine
  with `--with-runtime`;
- no NVIDIA driver, or mixed driver and library versions after an upgrade: reboot, or install the
  `vulkan` or `cpu` variant.

`alkahest sysinfo` shows whether the GPU is visible through NVML.

## The model is slow, or runs on the CPU

`alkahest ps` shows `PROCESSOR`: `100% GPU`, `100% CPU` or a split. A split means the model
did not fit in video memory and some layers stayed on the CPU. Use a smaller quantisation,
reduce the context (`alkahest config model <m> set num_ctx=4096`), or quantise the KV cache
(`kv_cache_type_k=q8_0 kv_cache_type_v=q8_0`). The desktop model settings view shows the memory
estimate before you load.

## 503 "server busy" or a long wait

All memory is used by loaded models that are busy, or `scheduler.max_queue` requests are already
waiting. Lower `keep_alive` so idle models leave sooner, set `max_loaded`, or unload with
`alkahest stop <model>`.

## "model does not support chat"

The model is a completion-only model (no chat template in its metadata). Use `/api/generate` or
`/v1/completions`, or a chat variant. `alkahest ls` shows the `KIND`.

## Downloads

- *Interrupted or slow:* run the same `pull` again; it resumes. Cap the rate with
  `ALKAHEST_PULL_MAX_BPS`.
- *401/403 from Hugging Face:* the repository is gated or private. Accept its terms on the website,
  then `alkahest hf login`.
- *Checksum mismatch:* the file is discarded and downloaded again; if it persists the mirror is
  serving a modified file.
- *Behind a proxy:* the standard `HTTPS_PROXY` variables are honoured; set `hf.endpoint` (or
  `ALKAHEST_HF_ENDPOINT`) for a Hub mirror.

## Orphan or leftover processes

Stopping the server with SIGTERM or Ctrl+C stops every engine it started, and on Linux the
kernel also signals each engine when the server dies for any reason (including SIGKILL). The
server keeps a ledger of the engine processes it starts and sweeps stale ones at the next start.

## The desktop app does not open

Run `alkahest desktop --smoke-test`. On Linux the app needs a Wayland or X11 session and a Vulkan
driver (software rendering through Mesa lavapipe works). Over SSH there is no display. Without a
system tray set `ALKAHEST_DESKTOP_TRAY=0`.

## Ollama store models are missing

`alkahest config show` prints the Ollama store path in use. Set `OLLAMA_MODELS` or
`store.ollama.path` if the store is elsewhere; a system service user also needs read access
(`ollama` group). Models that are "cloud" placeholders are listed but cannot run locally.

## Still stuck

Collect `alkahest --version`, `alkahest sysinfo`, `alkahest engines ls`, the failing command with
`-vv` and the relevant log tail, then open an issue.
