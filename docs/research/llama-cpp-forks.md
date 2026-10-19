# llama.cpp forks as opt-in engine recipes (RTX 4000 Ada)

Research for [[ALK-US-0132]], part of [[plan/alkahest-v1-plan]]. Date of the checks: 2026-10-19, GitHub API through `gh`
plus the READMEs of each fork. Target hardware: NVIDIA RTX 4000 Ada, SM 8.9, 20 GB. Scope: source builds only, never
auto-selected. Recipes follow `recipes/prism-llama-cpp.toml`; a fork recipe adds `base = "llama-cpp"` and its own `id`.

## 1. Summary

"Verified" below means read from the GitHub API or from the fork's own files at the pinned commit. Performance numbers are
the fork authors' claims, none was reproduced by us, and none was measured on Ada.

| Candidate (as named in the preliminary study) | Real repository | Licence | Last push | Stars | Pin (full SHA) | Verdict |
|---|---|---|---|---|---|---|
| openalchemy/llama.cpp (TurboQuant) | `openalchemy/llama.cpp`, branch `main` | MIT | 2026-06-03 | 0 | `50f2c1b51058a61850ba5287703b108d7fc8406f` | Do not ship (stale, superseded) |
| atomic-llama-cpp-turboquant | `AtomicBot-ai/atomic-llama-cpp-turboquant`, branch `dev` | MIT | 2026-10-12 | 424 | release `b10298-2.0.0` = `cda0f9256665845cd9d6f3a0176880883f653a3d` | Recommend recipe `llama-cpp-turboquant` |
| "Indras-Mirror fork" (CUDA MTP, fused TBQ4 FA) | `Indras-Mirror/llama.cpp-turboq-mtp`, branch `master` (the URL `Indras-Mirror/llama.cpp` does not exist) | MIT | 2026-09-24 | 96 | tag `b10738` = `8f2b24374e44ed6ea2d62e1292c9e3220d557616` | Recommend as experimental recipe `llama-cpp-mtp` |
| achyudh/llama.cpp (KV streaming) | `achyudh/llama.cpp`, branch `feature/adaptive-kv-stream-rocm` | MIT | 2026-10-16 | 45 | `9e24be0e52bb64bc2773b0193a15328966fbe2fe` | Do not ship: ROCm/RDNA 4 port, no CUDA claims |
| troed/llama.cpp-adaptive-kv-streaming | same, branch `fuss` | MIT | 2026-10-19 | 34 | `d49c762f618f770cffddd8bb0ded18c79959152d` | Do not ship now (personal derivative); revisit |
| (upstream of both KV-streaming forks) | `RaymondHuang210129/llama.cpp-adaptive-kv-streaming`, branch `feature/adaptive-kv-stream-v2` | MIT | 2026-10-18 | 449 | `db8fd5d898b0d19be74a4269c126c23d2f62c38d` | Recommend low priority, recipe `llama-cpp-kvstream` |
| llamAmpere | `JakeATX/llamAmpere` (also `drassadnikov/llamAmpere`) | MIT | 2026-10-17 | 182 | not pinned | Not applicable (Ampere SM 8.6, RTX 3090 tuning) |
| iacopPBK/llama.cpp-gfx906 | same, branch `master` | MIT | 2026-08-23 | 146 | not pinned | Not applicable (AMD gfx906; README says the fork is not updated) |

No candidate is GPL. All are MIT forks of `ggml-org/llama.cpp` and keep its LICENSE file. None of the forks with a CMake
tree was built by us; the build flags below come from their READMEs and CMake files.

## 2. Verified facts versus preliminary-study claims

| Preliminary-study claim | Result |
|---|---|
| openalchemy/llama.cpp exists, TurboQuant KV | Verified. Only `turbo3` / `turbo2`, CUDA, snapshot of 2026-06-02. |
| "atomic-llama-cpp-turboquant" | Name is right, owner was missing: `AtomicBot-ai`. Other accounts hold copies. |
| Indras-Mirror fork: CUDA MTP + fused TBQ4 flash attention, RTX 4090, Qwen3.6-27B | Repo is `Indras-Mirror/llama.cpp-turboq-mtp`. The README states these features and a 4090 table with Qwen3.6-27B Q4_K_M. Claims only, not reproduced. |
| README at `achyudh/llama.cpp` commit `9e24be0...` mentions the Indras fork | Not verified. That README has no mention of Indras, TBQ or CUDA flags beyond a ROCm build. The link to Indras came from elsewhere. |
| achyudh/llama.cpp: KV streaming for long context | The README at that commit describes a ROCm/HIP port for the RX 9070 XT (gfx1201). It credits RaymondHuang210129 as the origin. |
| troed/llama.cpp-adaptive-kv-streaming | Verified, a fork of Raymond's repository with CUDA build instructions. |

## 3. Per-fork details

### 3.1 AtomicBot-ai/atomic-llama-cpp-turboquant (recommended)

- URL: https://github.com/AtomicBot-ai/atomic-llama-cpp-turboquant. Default branch `dev`, MIT (API `license.spdx_id`),
  fork of `TheTom/llama-cpp-turboquant`, 424 stars, not archived, last push 2026-10-12.
- Pin: release tag `b10298-2.0.0` (published 2026-10-05) resolves to `cda0f9256665845cd9d6f3a0176880883f653a3d`. Branch
  head of `dev` at the time of the check was `3cde9e4f83c000627f5e6e7d50b7db7cc473427b` (2026-10-05, PR #91). Other
  tags: `b10269-1.7.0`, `b10269-1.6.0`, `b10269-1.5.1`. The release note says the work was rebased onto the TurboQuant+
  base `tqp-v0.4.0` of TheTom's fork.
- Build: standard llama.cpp CMake tree (`tools/server` present), `-DGGML_CUDA=ON`. The fork's
  `ggml/src/ggml-cuda/CMakeLists.txt` (read at the pinned commit) builds `75-virtual 80-virtual 86-real 89-real 90-virtual`
  plus Blackwell targets by default, so SM 8.9 is covered without extra flags. `GGML_NATIVE=ON` with CMake >= 3.24 and
  CUDA >= 11.6 selects `native`. `GGML_CUDA_FA_ALL_QUANTS` is optional and makes the build much longer; it is needed
  for native-kernel KV streaming with every K/V type (docs/build.md of the fork). No extra dependencies beyond mainline.
- Runtime flags (README of the fork): `-ctk turbo3 -ctv turbo3 -fa on` (turbo2 ~6.4x, turbo3 ~4.3x, turbo4 ~3.8x KV
  compression versus F16); `--spec-type nextn --spec-draft-n-max 2 --spec-draft-n-min 1` with `-m X_MTP.gguf -md X_MTP.gguf -ngl 99 -ngld 99`
  for Qwen 3.6 NextN; `--spec-type mtp --mtp-head <assistant.gguf>` for Gemma 4; `--kv-stream-arena-mib N` (block KV
  streaming, experimental, `-np 1`, loses prompt cache and context shift, docs/kv-stream.md); `--moe-cache`;
  `llama-quantize` types `TQ3_1S` / `TQ4_1S` (weights). CUDA support per README table: `turbo3` and `turbo4` full,
  `turbo2` through a reference path, `TQ4_1S` mat-vec only.
- Models needed: for NextN a combined `*_MTP.gguf` (the fork publishes `AtomicChat/Qwen3.6-35B-A3B-UDT-MTP-GGUF`; also an
  Unsloth `Qwen3.6-27B-MTP-GGUF` is referenced); for Gemma 4 MTP a `gemma-4-*-it-assistant-GGUF`. Plain GGUFs load as in mainline.
- Binary releases: yes, 19 assets for `b10298-2.0.0`, with `digest` in the release API. Linux x64 CUDA:
  `llama-turboquant-linux-x64-cuda-12.4.tar.gz` (525069456 bytes, sha256 `e3d4720d0d65c679f63f9664da8d17415c0318372ef4e5ee593c4658931ef3e9`),
  `llama-turboquant-linux-x64-cuda-13.3.tar.gz` (559369682 bytes, sha256 `be33c1e28ac7f44c618c8d725f5e9d8cfe859381c6551445e0045f2b455adb34`).
  Also CPU, Vulkan, ROCm, arm64 CUDA 13.3, macOS arm64, Windows. Not used by the draft recipe (source only requested), but
  usable later as `assets`. Base URL: `https://github.com/AtomicBot-ai/atomic-llama-cpp-turboquant/releases/download/b10298-2.0.0/<name>`.
  Whether the CUDA archives contain SM 8.9 code is not documented in the README; the CMake default list says they should.
- Ada / SM 89: no Ada benchmark. Claims in the README are Apple M4 Max Metal tables (Qwen 3.6 27B dense +5-7 %, 35B-A3B MoE
  +24-36 % with NextN). Support for SM 8.9 follows from the CMake default arch list (verified in source), not from a test.
- Verdict: recommend. Most active, most complete (TurboQuant KV, NextN, Gemma MTP, KV streaming) and release-tagged.
  One recipe covers the TurboQuant and MTP needs of the plan.

### 3.2 Indras-Mirror/llama.cpp-turboq-mtp (experimental)

- URL: https://github.com/Indras-Mirror/llama.cpp-turboq-mtp. Default branch `master`, MIT (`LICENSE` present), not a GitHub
  fork object, 96 stars, last push 2026-09-24. Description: Qwen4-Exp SWA + MTP + PLE, TBQ4 KV, DSpark, RotorQuant, RTX 4090.
- Pin: tag `b10738` = `8f2b24374e44ed6ea2d62e1292c9e3220d557616` (2026-09-01), the head of `master` and also of `qwen4-swa`.
  The README text is inconsistent about which branch is "default"; the API says `master`. Older Qwen35 build: branch
  `qwen35` = `aaa66a5e06590754bdc279fe8fe2147b0fc8ec7e` (tag `b10276`). Other repos of the same author:
  `llama.cpp-qwen4-swa`, `llama.cpp-TurboQuant-DSV4`, `tbq4-vllm`.
- Build (README): `cmake -B build -DGGML_CUDA=ON -DCMAKE_BUILD_TYPE=Release -DCMAKE_CUDA_ARCHITECTURES=89`; RotorQuant types add
  `-DGGML_CUDA_FA=ON -DGGML_CUDA_FA_ALL_QUANTS=ON`. `common/arg.cpp` at the pin lists `GGML_TYPE_TBQ3_0`, `TBQ4_0`,
  `PLANAR3_0`, `ISO3_0` as KV types and `COMMON_SPECULATIVE_TYPE_DRAFT_MTP` (verified in source). `tools/server` exists.
- Runtime flags (README): `-ctk tbq4_0 -ctv tbq4_0` (or `tbq3_0`, `-ctkd/-ctvd` for the draft cache), `--flash-attn on`
  (required for the fused kernel), `--spec-type mtp` (Qwen 3.6, custom 3-token MTP) or `--spec-type draft-mtp` (Qwen 3.8
  embedded head), `--override-kv qwen35.attention.sliding_window=int:4096,...swa_global_layers=int:8`, `--no-kv-offload`.
- Models: Qwen 3.6 / 3.8 GGUFs that carry MTP tensors. The README does not name a specific Qwen 3.6 MTP GGUF.
- Known issues stated by the README: MTP needs `--parallel 1`; vision + MTP crashes; 7B models crash with TBQ4 (head
  geometry), 27B works; qwen4exp needs `LLAMA_ATTN_ROT_DISABLE=1`.
- Binary releases: none (0 releases; 8 plain tags).
- Ada / SM 89: the README gives a `sm_89` build line and benchmarks on an RTX 4090 (24 GB, Ada), Qwen3.6-27B Q4_K_M: custom MTP
  82-93 t/s versus 71.5 t/s for upstream MTP, 73-98 % acceptance. These are the author's claims, not reproduced. The RTX 4000 Ada has
  20 GB and roughly a third of the 4090 memory bandwidth, so absolute numbers will differ.
- Verdict: recommend only as an experimental opt-in. Single maintainer, no releases, history is rebased and the branch split
  is confusing; fine for benchmarking, not for a default. Upstream llama.cpp has since merged MTP (PR #22673 per the README),
  so mainline may close the gap; re-evaluate after benchmarking.

### 3.3 RaymondHuang210129/llama.cpp-adaptive-kv-streaming (KV streaming, low priority)

- URL: https://github.com/RaymondHuang210129/llama.cpp-adaptive-kv-streaming. Default branch `feature/adaptive-kv-stream-v2`,
  MIT, 449 stars, last push 2026-10-18, no releases, 3 tags. Pin `db8fd5d898b0d19be74a4269c126c23d2f62c38d` (2026-10-17).
- This is the CUDA original. Build (README): `cmake -S . -B build-v2 -DGGML_CUDA=ON -DGGML_CUDA_FA_QUANTS=all -DCMAKE_BUILD_TYPE=Release`,
  `--target llama-server`; selected pairs such as `f16-f16;q8_0-q4_0;q8_0-q8_0;q5_1-q5_1` are allowed.
- Flags: `--shared-device-memory-mib N` (alias `--kv-stream-arena-mib`), `--parallel 1 --flash-attn on --fit off --batch-size 256
  --ubatch-size 256 -ctk q8_0 -ctv q4_0`, `--spec-type draft-mtp --spec-draft-n-max 3` or `draft-dflash --model-draft sidecar.gguf`.
  Do not pass `--spec-draft-type-k/v`. `benchmarks/find-max-arena.py` finds the arena size.
- Models: Qwen3.8-27B (arch `qwen35`), reference `UD-IQ4_XS`; embedded MTP needs a GGUF with MTP tensors.
- Ada / SM 89: README table lists "CUDA SM61, SM75, SM86, SM89: isolated-target build and selected forward-JIT/operator/model
  qualification on the 5070 Ti" and says forward-JIT does not reproduce the performance of older cards. Reference
  hardware is an RTX 5070 Ti (16 GB, SM 120). Claims: 128 Ki context 26.1 t/s target-only versus 2.5 t/s for stock UVM.
- Fit for our GPU: the technique targets cards with 16 GB where weights leave little KV room; streaming speed depends on
  PCIe (troed's README says PCIe 5.0 is needed for good performance; the RTX 4000 Ada is PCIe 4.0). With 20 GB the benefit is
  smaller. Single request, single GPU only. Atomic's fork already carries a similar CUDA arena (`--kv-stream-arena-mib`).
- Verdict: recipe is cheap to ship but low priority; keep it opt-in and label it experimental.

### 3.4 achyudh/llama.cpp and troed/llama.cpp-adaptive-kv-streaming

- `achyudh/llama.cpp`: https://github.com/achyudh/llama.cpp, default branch `feature/adaptive-kv-stream-rocm`, MIT, 45 stars, fork of
  `ggml-org/llama.cpp`, head `9e24be0e52bb64bc2773b0193a15328966fbe2fe` (2026-10-16), no releases. README: Adaptive KV streaming
  and RDNA 4 (gfx1201) ROCm optimisations, build with `-DGGML_HIP=ON -DAMDGPU_TARGETS=gfx1201`, flags `--kv-stream
  --kv-resident-mib 450 --kv-resident-mib-decode 2100`, benchmarks on RX 9070 XT. Nothing about CUDA. Do not ship.
- `troed/llama.cpp-adaptive-kv-streaming`: https://github.com/troed/llama.cpp-adaptive-kv-streaming, default branch `fuss`, MIT,
  34 stars, head `d49c762f618f770cffddd8bb0ded18c79959152d` (2026-10-19), no releases. Merge of Raymond's v2 (`db8fd5d89`) and upstream
  `620d2225c`, adds `--spec-type ngram-simple,draft-mtp`, MTP depth up to 5. CUDA build: `-DGGML_NATIVE=ON -DGGML_CUDA=ON
  -DGGML_CUDA_FA_QUANTS=q8_0-q4_0,q8_0-q8_0,q4_0-q4_0` (README warns not to drop the extra pairs). Moves daily; a personal
  derivative. Not shipped; its benefit over Raymond's repository can be judged after a benchmark.

### 3.5 openalchemy/llama.cpp

- https://github.com/openalchemy/llama.cpp, branch `main`, MIT, 0 stars, not a GitHub fork, last push 2026-06-02, 7 tags,
  head `50f2c1b51058a61850ba5287703b108d7fc8406f`. Adds `GGML_TYPE_TURBO3` / `TURBO2` KV types, CUDA only, `-fa 1` required, head
  dimension must be a multiple of 128; README measures RTX 5080 (Qwen2.5-Coder-14B, -4.4 GB VRAM at 32k, "gen 1.47x").
  Frozen snapshot, no releases, no stars; the Atomic fork supersedes it (turbo2/3/4, same lineage, active). Do not ship.

### 3.6 Not applicable

- llamAmpere: `JakeATX/llamAmpere` (MIT, 182 stars) tunes for RTX 3090 / 3090 Ti (Ampere SM 8.6) per its description; the `drassadnikov` copy
  says the same. Ada is a different architecture and no claim covers it. Not applicable.
- `iacopPBK/llama.cpp-gfx906`: AMD gfx906 (MI50/MI60), based on build 7924, README says the fork is not updated and points to `mxxm-t/mx-llama.cpp`. Not applicable.

## 4. Licence notes

- All checked repositories report MIT through the GitHub API and carry the upstream llama.cpp LICENSE; none is GPL. Source builds do
  not redistribute anything, but if a later recipe ships a fork's prebuilt archive, keep the fork's licence text and list it in NOTICE.
- TurboQuant lineage credits (Atomic README): TheTom/llama-cpp-turboquant (MIT, 2431 stars, tags `tqp-v0.1.1` to `tqp-v0.4.0`; `tqp-v0.4.0` =
  `bcb85fc3ae85efa0f5f392c6c880dfc524923860`). The algorithm is from the TurboQuant paper (Google Research, ICLR 2026); we did not check patent status.
- Model licences (Qwen, Gemma, Unsloth/AtomicChat re-quantisations) are separate and unchecked here.

## 5. Draft recipes

Drafts live next to this document: `llama-cpp-turboquant.toml`, `llama-cpp-mtp.toml`, `llama-cpp-kvstream.toml`. Each has two source-only
variants, `cuda` (`-DGGML_CUDA=ON`) and `cuda-native` (adds `-DCMAKE_CUDA_ARCHITECTURES=native`, `native = true`), the pinned full SHA as
`rev`, and the cmake flag set of the prism recipe. Open points: the recipe schema requires `cuda = "major.minor"` for CUDA variants, drafts use
`"12.4"`, which is a guess at the minimum toolkit (Atomic ships 12.4 and 13.3 builds); the `version` strings are chosen by us.
All three parse as TOML; none was built.

## 6. Benchmarking procedure against mainline

Goal: decide per fork whether it earns a place. Run on the RTX 4000 Ada, one GPU, nothing else on it, persistence mode on, fixed clocks if possible.

1. Builds: mainline recipe `llama-cpp` (CUDA variant) and the fork (`cuda-native`), same CUDA toolkit. Also build `llama-bench` for both
   (`cmake --build build --target llama-bench`; the recipes build only `llama-server`).
2. Same model file for both (for MTP forks also the MTP-carrying GGUF; mainline gets the same file with its own `--spec-type` if it
   supports it), same context, same `-ngl 99 -fa on`, `--parallel 1`, `-b/-ub` equal.
3. Throughput with `llama-bench`: `llama-bench -m model.gguf -ngl 99 -fa 1 -p 512 -n 128 -d 0,4096,16384,32768 -ctk f16,q8_0 -ctv f16,q8_0 -r 5 -o json`
   (fork types such as `-ctk turbo3 -ctv turbo3` or `tbq4_0` only on the fork binary). Record prompt t/s (pp) and decode t/s (tg) per depth `-d`.
4. Server-level run (needed for speculative decoding, which `llama-bench` does not exercise): start `llama-server` with the exact flags, then send
   the same fixed prompts to `/completion` with `"seed": 42, "temperature": 0, "top_k": 1, "n_predict": 256, "cache_prompt": false`. Read
   `timings.prompt_per_second`, `timings.predicted_per_second` and, for MTP, the draft acceptance rate from the log. Time to first token =
   wall time to the first streamed token (or `timings.prompt_ms`). Repeat each point 5 times after one warm-up and keep the median.
5. VRAM: poll `nvidia-smi --query-gpu=memory.used --format=csv -lms 200` during the run (or NVML) and keep the peak; also record host RSS for
   KV streaming and `--no-kv-offload`.
6. Quality guard: `llama-perplexity -m model.gguf -f wiki.test.raw -c 4096 -ngl 99 -fa 1 -ctk <type> -ctv <type>` for each KV type, and a
   long-context recall test (key planted early in a 32k-100k prompt). With temperature 0 the speculative output should equal the non-speculative
   output; compare the token sequences and flag any difference.
7. Matrix: context 4k, 16k, 32k, 64k, 128k (to the extent that VRAM allows); KV `f16`, `q8_0`, fork type; speculation off/on. Report decode t/s,
   prompt t/s, TTFT, peak VRAM, acceptance, and the ratio versus mainline. A fork passes if it wins on at least one of decode speed at a useful
   context or context reachable in 20 GB, without a perplexity regression the user would not accept.

## 7. Uncertain / not verified

- No fork was built or run; flags come from READMEs and, for CUDA arch lists and KV type names, from CMake and `common/arg.cpp` at the pins.
- All benchmark figures are authors' claims on other hardware (M4 Max, RTX 4090, RTX 5070 Ti/5080/5060 Ti, RX 9070 XT).
- Whether the Atomic CUDA release archives include SM 8.9 code; the Atomic README does not list Linux CUDA archives in its table although the release has them.
- Which Qwen 3.6 MTP GGUF suits the Indras fork; its README only says the target GGUF must carry MTP tensors.
- The origin of the link from the achyudh README to the Indras fork; the README at the cited commit does not contain it.
