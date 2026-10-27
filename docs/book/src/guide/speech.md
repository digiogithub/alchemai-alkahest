# Speech

Alkahest speaks text with [Phonon](https://huggingface.co/gradium/phonon-31m-en), Gradium's small
text-to-speech models, the same models Ollama 0.40.11 runs. Where Ollama runs them on macOS only,
Alkahest runs them on Linux, macOS and Windows through Gradium's `ptts-openai-server`.

## Setup

```shell
# Build the engine once (needs a Rust toolchain, git, pkg-config, libopus and libmp3lame)
sudo apt-get install libopus-dev libmp3lame-dev pkg-config   # Debian, Ubuntu
brew install opus lame pkg-config                            # macOS
alkahest engines install ptts-openai-server

# Download the model: Ollama's build, with 37 English voices and `default`
alkahest pull phonon
# or Gradium's own repository (q8 weights by default)
alkahest pull hf.co/gradium/phonon-31m-en
```

`alkahest show phonon` lists the voices. A `phonon` already pulled by Ollama is reused: Alkahest
reads its files and builds its own copy, never changing Ollama's store.

## Speak from the terminal

```shell
alkahest run phonon "Hello from Alkahest."
alkahest run phonon --voice Zoey "Hi, this is Zoey."
echo "Text from another program works too." | alkahest run phonon
alkahest run llama3.2 "Tell me a fun fact about owls in two sentences." | alkahest run phonon
alkahest run phonon "Save this to a file." > speech.wav
```

Piped text is spoken as it arrives, a few sentences at a time, so a chat model's answer starts
playing before it is finished. Without text, `alkahest run phonon` opens a `>>>` prompt: each
line is spoken, Ctrl+C stops speaking, `/bye` or Ctrl+D exits. Audio plays with `afplay` on
macOS, the first of `paplay`, `pw-play`, `aplay` or `ffplay` on Linux, and PowerShell on
Windows; when stdout is redirected, one WAV file is written instead.

## API

OpenAI-compatible `POST /v1/audio/speech`, streamed while the model speaks:

```shell
curl http://localhost:11435/v1/audio/speech \
  -H "Content-Type: application/json" \
  -d '{"model": "phonon", "input": "Hello from Alkahest.", "voice": "Hugh"}' \
  --output speech.wav
```

| Field | Behaviour |
|---|---|
| `model`, `input` | required; `input` at most 4096 characters |
| `voice` | a voice of the model, any case; omitted means `default`; unknown voices are a 400 that lists the voices |
| `response_format` | `wav` (default; 16-bit mono 24 kHz, size fields at their maximum because it streams), `pcm`, `mp3`, `opus` |
| `speed` | `1` only |
| `instructions`, `stream_format` other than `audio` | 400 |

`GET /v1/audio/voices?model=phonon` and the `voices` field of `/api/show` list the voices.
