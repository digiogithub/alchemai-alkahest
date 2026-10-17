# FAQ

**Is it a drop-in replacement for Ollama?**
For clients, mostly yes: the Ollama native API, the `ollama` CLI verbs and the Ollama store are
supported, and the default port is 11435 so both can run at once. `ollama create` works for a
Modelfile subset (`FROM` a model or a GGUF file, `PARAMETER`, `TEMPLATE`, `SYSTEM`, `LICENSE`);
adapters, seed messages and quantisation are not supported. Models can also be built by copying
and configuring them (`alkahest cp`, `alkahest config model`).

**Does it send data anywhere?**
No telemetry. It connects to the Ollama registry, Hugging Face and GitHub (engine archives) only
when you pull a model, install an engine or search.

**Can it use my existing Ollama models?**
Yes, read-only and without copying; see [Models](guide/models.md). Stop Ollama's own server
first only if you want to free its GPU memory, not for correctness.

**Which port does it use?**
`127.0.0.1:11435`. Change it with `--host`, `ALKAHEST_HOST` or `server.port`.

**Do I need a GPU?**
No. The CPU engine runs everything (slowly for big models). GPU builds are installed with
`alkahest engines install`.

**Which GPUs are supported?**
NVIDIA (CUDA), AMD (ROCm and Vulkan), Intel and others through Vulkan, Apple Silicon (Metal).
`alkahest sysinfo` shows what was detected.

**How do I keep a model loaded all the time?**
`keep_alive = "-1"` in a request, `alkahest config model <m> set keep_alive=-1`, or globally
`scheduler.keep_alive = "-1"` (any duration works: `30m`, `24h`).

**How do I run two copies of a model, or serve many users?**
`scheduler.num_parallel` (per model) serves parallel requests from one loaded copy. Requests
beyond that queue up to `scheduler.max_queue`.

**Why is the first request slow?**
The model is loading from disk into memory. Preload it with a request that has no prompt
(`curl http://127.0.0.1:11435/api/generate -d '{"model":"<model>"}'`) or keep it loaded.

**Where are my models?**
`alkahest config show` prints the paths; by default under the data directory. Move them with
`store.models_dir`.

**Can I use a different llama.cpp?**
Yes: install another version (`--version`), build from source, or point an
[engine override](guide/engines.md#overriding-an-engine-binary) at your own binary.

**Does it support tool calling, JSON mode and structured outputs?**
Yes on the chat routes of every dialect when the model's chat template supports tools; JSON mode
and JSON schema (`format` / `response_format`) are enforced by grammar in the engine.

**Is text-to-speech available?**
Not yet: `/v1/audio/speech` answers 501. Speech-to-text and image generation are available.

**How do I back up or move an installation?**
Stop the server and copy the data directory (or the `ALKAHEST_HOME` root). The store is
content-addressed, so copying is safe at any time the server is stopped.

**How do I report a problem?**
See the end of [Troubleshooting](troubleshooting.md).
