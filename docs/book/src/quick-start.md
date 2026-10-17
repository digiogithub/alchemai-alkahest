# Quick start

Five minutes from nothing to an answer. The commands assume the binary is on `PATH`.

## 1. Install an engine

If your package did not bundle one:

```sh
alkahest engines install llama-cpp
```

This picks the best build for the machine (CUDA, Vulkan, ROCm, Metal or CPU). Add
`--variant cpu` to force the CPU build.

## 2. Download a model

```sh
alkahest pull llama3.2:3b                       # Ollama registry
alkahest pull hf.co/ggml-org/tiny-llamas:Q4_0   # Hugging Face, GGUF quantisation Q4_0
alkahest ls
```

If an Ollama installation exists on the machine, its models already appear in `alkahest ls`
with the source `ollama`. Nothing is copied and nothing is written there.

## 3. Start the server

```sh
alkahest serve
```

It listens on `127.0.0.1:11435` (one above Ollama's port, so both can run side by side). Leave
it running, or [install it as a service](guide/service.md).

## 4. Talk to it

In another terminal:

```sh
alkahest run llama3.2:3b "Why is the sky blue?"     # one-shot answer
alkahest run llama3.2:3b                            # interactive chat, /bye to leave
alkahest ps                                         # what is loaded, memory, expiry
alkahest stop llama3.2:3b                           # unload now
```

The first request loads the model (a few seconds); it stays loaded for five minutes after the
last request, then the engine process exits.

## 5. Use it from an application

Any Ollama, OpenAI or Anthropic client works by changing the base URL:

```sh
curl http://127.0.0.1:11435/api/generate -d '{"model":"llama3.2:3b","prompt":"Hello","stream":false}'
curl http://127.0.0.1:11435/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"llama3.2:3b","messages":[{"role":"user","content":"Hello"}]}'
```

```python
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:11435/v1", api_key="unused-on-loopback")
print(client.chat.completions.create(model="llama3.2:3b",
      messages=[{"role": "user", "content": "Hello"}]).choices[0].message.content)
```

Requests from the same machine need no API key. To serve other machines you must create keys
first: see [Serving and security](guide/serving.md).

## 6. The desktop app

`alkahest desktop` opens a window to browse, download and configure models and to watch what is
running. See [Desktop app](guide/desktop.md).
