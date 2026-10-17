# Local video generation: engines, hardware and API shape

Status: spike for [[ALK-US-0060]] (task ALK-T-0165), 2026-10-17. Spec: [[ALK-SP-0005]].R4
(modality endpoints), engine rules [[ALK-SP-0003]].R2. Plan: [[plan/alkahest-v1-plan]]. Builds on
[[2026-10-17-sd-image-generation]].

## Question

Which local video generation engines are viable for v1.x, what do they need, and what should the
API look like? The `video` model kind already exists in the classifier and selects
stable-diffusion.cpp; nothing generates video yet.

## Sources

- stable-diffusion.cpp release `master-970-2f016e8`: `docs/wan.md`, `docs/ltx2.md`,
  `docs/hunyuan_video.md`, `docs/lingbot_video.md`, `examples/server/api.md` (sections `vid_gen`,
  job model, `sd_cpp_extra_args`), and `sd-server --help` of the Linux x64 archive.
- LocalAI `core/schema/localai.go` (`VideoRequest`) and `docs/content/features/video-generation.md`
  (MIT) in `/www/MCP/Alchemai/Alkahest-refs/LocalAI`.
- OpenAI's video API (`POST /v1/videos`, an asynchronous job API) is described from memory:
  platform.openai.com answered the fetch tool with a bot challenge, so the shape below is not
  re-verified in this session. Check it before building.
- Hardware figures come from the model cards and the upstream documents above and were **not
  measured** here; treat them as order-of-magnitude. Only image generation was run
  ([[2026-10-17-sd-image-generation]]).

## Engines

| Engine | Models | Licence | Notes |
|---|---|---|---|
| **stable-diffusion.cpp** (`sd-server`, already pinned) | Wan 2.1 (T2V 1.3B and 14B, I2V, FLF2V, VACE), Wan 2.2 (TI2V 5B, T2V/I2V A14B with two experts, S2V), LTX-2.3 and 2.5 (22B-class, with audio), HunyuanVideo 1.5, LingBot-Video, MiniMax-H3, AnimateDiff for SD 1.5 | MIT code; model licences vary (Wan: Apache-2.0; LTX and Hunyuan carry their own community terms, to be checked per model) | Same binary as image generation, GGUF and safetensors, CPU/CUDA/Vulkan/Metal. Video mode is selected per request on the server (`supported_modes` in `/sdcpp/v1/capabilities`), not by a launch flag. |
| diffusers / PyTorch (LocalAI's `diffusers` backend) | everything on the Hub | Apache-2.0 | A Python venv engine like vLLM's; heavy install (several GB), best model coverage, CUDA-centric. Option for models sd.cpp lacks. |
| ComfyUI-style graphs | any | GPL-3.0 | Excluded by the GPL rule. |
| vLLM-Omni, MLX-Video (Apple) | Wan, LTX-2 | Apache-2.0 / MIT | New and platform-specific; revisit after v1.x. |

Result for sd.cpp: it is the only engine that fits the product shape (one small subprocess
server, no Python) and it covers the open models that matter. Everything else costs a second
runtime.

## What sd-server exposes for video

- Native asynchronous contract: `POST /sdcpp/v1/vid_gen` returns `202` with a job id; poll
  `GET /sdcpp/v1/jobs/{id}` (`queued`, `generating`, `completed`, `failed`, `cancelled`; a
  `preview` PNG of the current step), `POST /sdcpp/v1/jobs/{id}/cancel`. A finished job carries
  `result.output_format` and the encoded video as base64.
- Request fields: `prompt`, `negative_prompt`, `width`, `height`, `video_frames`, `fps`, `seed`,
  `init_image`, `end_image` (first/last frame), `control_frames`, `sample_params` (steps, sampler,
  `flow_shift`, guidance), `high_noise_sample_params` and `moe_boundary` for Wan 2.2's two experts,
  `output_format` (`webm` default, `webp` animated, `avi` as MJPG; webm needs a build with libwebm,
  which the pinned archives have) and `output_compression`.
- The OpenAI-compatible route of sd-server (`/v1/images/generations`) is image-only.
- Frame counts must fit the model's temporal compression (Wan and LingBot: 33, 49, 81 ...).
- Wan needs several component files next to the diffusion model (`--vae`, `--t5xxl` umt5-xxl,
  `--clip_vision` for I2V). Alkahest tracks one weights file per model, so a video model is a
  multi-file model: the same gap as Flux (see Open issues).

## Hardware needs (approximate)

- Wan 2.1 T2V 1.3B at 480p: the only video model that is comfortable on consumer GPUs (about 8 GB
  of VRAM, quantised text encoder, VAE tiling); minutes per clip.
- Wan 2.2 TI2V 5B (720p, 24 fps) needs a 24 GB-class card, or CPU offload with much slower runs.
- Wan 14B / A14B and LTX-2.x 22B plus a 12B text encoder: 24 GB and up at Q4-Q8, or heavy
  `--offload-to-cpu` with 64 GB of RAM; tens of minutes for a few seconds of video.
- The Wan VAE is the VRAM peak; `--vae-tiling` and the tiny `taesd` decoder trade quality for it.
- Disk: 10 to 40 GB per model with its encoders.
- CPU-only is possible and impractical (hours).

The development machine (20 GB RTX 4000 Ada) can run Wan 1.3B and probably the 5B with offload.

## API shape options

1. **OpenAI Videos API** (`POST /v1/videos` job, `GET /v1/videos/{id}`, `GET
   /v1/videos/{id}/content`, `DELETE`): asynchronous by design, which fits minutes-long jobs and
   maps onto sd-server's job model one to one. SDK support exists in the official clients. Fields
   (`model`, `prompt`, `seconds`, `size`, `input_reference`) cover the basics; Alkahest
   extensions (`seed`, `steps`, `negative_prompt`, `fps`, `end_image`) follow the image route.
2. **LocalAI-style `POST /video`**: one synchronous call that returns `url` or `b64_json`;
   simple, but a minutes-long HTTP request breaks proxies and SDK timeouts.
3. **Pass-through of `/sdcpp/v1/vid_gen`**: exact, no mapping work, but a native
   and unstable schema that clients do not know.

Video must be asynchronous because of run time. Options 1 and 3 can coexist.

## Recommendation

1. v1.0: no video endpoint. The kind is classified and listed, `video` models select the sd.cpp
   engine, and a request to a video model on an image route answers 400 ("this model generates
   video").
2. v1.x, one story after multi-file model support: implement option 1 over sd-server's
   `vid_gen` job API with webm output, first Wan 2.1 T2V 1.3B and Wan 2.2 TI2V 5B (Apache-2.0).
   Job state lives in the scheduler's lease (a lease must stay open until the job ends, and
   `keep_alive` must not unload the model mid-job).
3. Prerequisites: multi-file model manifests (diffusion model, VAE, text encoder, vision encoder as
   named layers, passed to the engine as flags), memory estimates for diffusion models (VAE peak),
   a result store with expiry for the `content` endpoint, and a pull path that understands the
   Comfy-Org/QuantStack repository layouts.
4. Do not add a second engine (diffusers) for video before demand is shown.

## Open issues

- Multi-file model support is needed by Flux, SD3 and every video model; the story belongs to
  the model store.
- Today `kind_capabilities` gives the `video` kind the `Image` capability, so an images request to
  a video model would reach sd-server instead of the 400 recommended above; split the capability
  when video is implemented.
- Licences of LTX and HunyuanVideo weights need a review before they are recommended.
- Confirm the OpenAI Videos shape when the documentation is reachable.
