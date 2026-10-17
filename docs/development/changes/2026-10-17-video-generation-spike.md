# Change: video generation spike (ALK-US-0060)

Research note [[video-generation]] (`docs/research/video-generation.md`) for [[ALK-US-0060]]
(task ALK-T-0165) / [[ALK-SP-0005]].R4. Builds on [[2026-10-17-sd-image-generation]].

## What

Evaluation of local video engines (stable-diffusion.cpp with Wan, LTX-2.x, HunyuanVideo, LingBot;
diffusers; excluded GPL graph tools), what sd-server exposes (`/sdcpp/v1/vid_gen` job API, webm,
webp and avi output), approximate hardware needs, three API shapes, and a recommendation: no video
endpoint in v1.0; in v1.x an OpenAI-Videos-shaped asynchronous API over sd-server's job API, after
multi-file model manifests exist. No code changed.

## Verification

Read from the pinned release's docs, `api.md` and `sd-server --help`, and LocalAI's `VideoRequest`.
Hardware figures are not measured and the OpenAI Videos shape could not be re-fetched (bot
challenge); both are flagged in the note.
