# Change: GGUF metadata reader (ALK-US-0005)

Implements [[ALK-US-0005]] / [[ALK-SP-0010]].R2. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-gguf`: streaming GGUF v2/v3 parser (`Gguf::read`, `Gguf::read_file`) over any
`Read`; metadata KV with all value types and arrays; tensor info table; ggml type table
(`GgmlType`) including `Q2_0` (42), `PQ2_0` (142) and `PTQ1_0`; `general.file_type` names;
typed accessors (architecture, context length, block count, heads, chat template, tokenizer,
pooling, mmproj detection, tools/thinking detection, parameter count, dominant quant, engine
family).

## Files and symbols

`crates/alkahest-gguf/src/{lib,reader,value,ggml,accessors,error}.rs`,
`tests/gguf_reader.rs`, `examples/dump.rs`.

## Decisions and deviations

- Streaming `BufReader` instead of mmap: only the header region is read, no `unsafe` needed.
- Safety: `Limits` bound counts, string and array sizes and nesting; no allocation from
  untrusted counts; arrays above `keep_array` keep only their length.
- Verified on `/www/Pips/llm/.../Ternary-Bonsai-2-9B-PQ2_0.gguf`: tensors of type id 142
  confirmed as PQ2_0, dominant quant PQ2_0, family PrismMl; block geometry (256 elems, 68
  bytes) fitted so the estimated size matches the file (2.38 GB of 2.42 GB).
- UNCONFIRMED: the `PTQ1_0` type id (143 is a placeholder, constant `GgmlType::PTQ1_0_ID`);
  no PTQ1_0 file or fork source was available offline. Inbox item filed.

## Verification

fmt, clippy `-D warnings`, nextest, `cargo deny check`, `check-deps`, headless build all pass.
Real-file test via `ALKAHEST_TEST_GGUF=<path>`.
