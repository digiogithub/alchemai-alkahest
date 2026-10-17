# Change: estimator for hybrid and encoder-only models, per-process VRAM, ggml profiles

Implements [[ALK-US-0085]] (with task [[ALK-T-0223]]) and the memory-profile part of
[[ALK-US-0100]]. Refs [[ALK-SP-0004]].R3, [[ALK-SP-0010]].R5. Builds on
[[2026-10-17-memory-estimator]], [[2026-10-17-estimator-calibration]] and
[[2026-10-17-scheduler-admission-eviction]]. Evidence: [[research/memory-estimator-calibration]].

## What

`alkahest-sched/src/memory.rs`
- `ModelProfile` gains `attention_blocks` (empty = every block attends), `causal`,
  `recurrent_state_bytes` and `output_tied_bytes`, with `has_kv(i)` / `has_recurrent_state(i)`.
  `from_gguf` fills them from `<arch>.attention.recurrent_layers`, a per-block
  `attention.head_count_kv` with zeros, or `full_attention_interval`; recurrent state from
  `ssm.conv_kernel / inner_size / state_size / group_count`; `attention.causal`;
  no `output.weight` means tied embeddings.
- `estimate`: KV only in attention blocks, recurrent state per slot in the others (counted in
  `kv_bytes`), no cache at all for non-causal models, placement cost per block uses the state of
  that block; a tied output head puts a GPU copy of the embedding in VRAM (RAM unchanged,
  `weights_bytes` unchanged). Encoder-only compute buffer: micro-batch = context, 19 F32 per
  embedding column and token.
- `estimate_ggml(GgmlWorkload, weights, on_gpu)`: whisper (`1.3 w + 200 MiB`), diffusion image
  (`1.15 w + 1 GiB`), video (`+ 3 GiB`). `admission::placement` uses it for the whisper.cpp and
  stable-diffusion.cpp engines (previously "file size as RAM, 0 VRAM").

`alkahest-sysinfo`
- `MemoryMonitor` (`memory.rs`): keeps NVML open (`nvidia::Session`), `snapshot() ->
  MemoryPools` (RAM from the OS, VRAM from NVML or amdgpu sysfs, Apple unified memory) with no
  CPU/Vulkan/ROCm/driver probing, and `process_vram(pid)`:
  `nvmlDeviceGetComputeRunningProcesses_v3` summed over devices (`NVML_VALUE_NOT_AVAILABLE`
  ignored, size-query retry loop), else on Linux the DRM `fdinfo` `drm-memory-vram` of the pid
  (`fdinfo_vram`, one count per `drm-client-id`).

`alkahest-sched/src/admission.rs`: `SystemMemoryProbe` wraps an `Arc<MemoryMonitor>`
(`SystemMemoryProbe::new()`; it was a unit struct), so the snapshot before every load is the
memory-only refresh and `process_vram` returns the driver's figure; `Accounted::vram` and the
`alkahest ps` metrics already used the hook.

Harness: `crates/alkahest/tests/calibration.rs` compares KV plus RS buffers;
`crates/alkahest/tests/gpu_vram.rs` (opt-in `ALKAHEST_TEST_GPU=1`) runs the CUDA llama-server
and compares NVML process VRAM with the estimate.

## Results

KV plus recurrent state for tev1:0.8b now match llama-server to 0.00 MiB; nomic-embed-text has
no KV and its compute buffer is within +4 to +8% (flash attention). On the GPU the estimate
never under-estimates any more (tev1 was -14% before the tied-embedding fix; now +11%); the
CUDA context costs 182 to 187 MiB against a 300 MiB constant (left conservative). Tables in
[[research/memory-estimator-calibration]].

## Not done

- [[ALK-US-0100]] stays open: multi-file Flux/SD3/video models, video vs image capability,
  `/v1/images/edits`, url responses. Diffusion overheads are floors, not measured (no diffusion
  model on the development host).
- AMD per-process VRAM (`fdinfo`) is unit tested against fake trees only; Vulkan on NVIDIA
  reports graphics processes, which are not queried.
- GPU overhead constants were measured on one GPU (Ada, CUDA 12.8).

## Verification

`scripts/ci-local.sh` and `cargo machete` pass. New tests: `alkahest-sched/tests/memory.rs`
(hybrid, slot scaling, explicit flags, encoder-only, tied embeddings, whisper table, GPU vs CPU
ggml, placement), `alkahest-sysinfo` (`fdinfo`, monitor), the calibration harness
(`ALKAHEST_TEST_ENGINE=1`) and `gpu_vram` (`ALKAHEST_TEST_GPU=1`, passed on the RTX 4000 SFF Ada).
