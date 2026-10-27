# Plan: speech models (Phonon) — ALK-US-0139 … ALK-US-0143

Status: in progress (2026-10-27). Decision: [[0008-phonon-tts-engine]] (supersedes
[[0003-tts-engine]]). Parity target: Ollama 0.40.11 speech (`/v1/audio/speech`, `voices` in
`/api/show`, `ollama run phonon --voice`). Plan: [[plan/alkahest-v1-plan]].

| Phase | Story | Crates | Depends on | Test |
|---|---|---|---|---|
| 1 | [[ALK-US-0139]] cargo builds + `ptts-openai-server` recipe | alkahest-recipes, xtask, recipes/, packaging/ | — | recipe parse, cargo argv, `engines install ptts-openai-server` |
| 1 | [[ALK-US-0141]] Ollama tensor layers → checkpoint folder; HF Phonon pull | alkahest-store, alkahest-ollama, alkahest-hf, alkahest-gguf | — | synthetic manifest, read-only Ollama test, `pull phonon` |
| 1 | [[ALK-US-0140]] engine adapter + TTS selection | alkahest-engine, alkahest-sched, alkahest-store (meta) | — | argv, selection |
| 2 | [[ALK-US-0142]] `/v1/audio/speech` proxy, voices | alkahest-api, alkahest-client | 0140 | fake engine, real engine smoke |
| 3 | [[ALK-US-0143]] `alkahest run` speech | alkahest | 0142 | splitter, player, WAV |

End-to-end acceptance: `alkahest engines install ptts-openai-server`, `alkahest pull phonon`,
`curl /v1/audio/speech` → WAV, `alkahest run phonon --voice Hugh "…" > out.wav`.

Evidence gathered before coding (2026-10-27): `ptts-openai-server` v0.5.0 builds with cargo in
~1 min; a Python prototype rebuilt `phonon:latest` from its 223 tensor layers into a checkpoint
folder that the server loads (`/health` ok, 38 voices, streamed WAV).
