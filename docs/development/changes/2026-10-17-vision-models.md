# Change: vision models end to end (ALK-US-0056)

Implements [[ALK-US-0056]] / the image-input part of [[ALK-SP-0005]].R2 (task ALK-T-0158).
Builds on [[2026-10-17-llama-server-adapter]] (`--mmproj`), [[2026-10-17-openai-api]],
[[2026-10-17-anthropic-messages-api]] and [[2026-10-17-openai-responses-api]]. Plan:
[[plan/alkahest-v1-plan]].

## What

Crate `alkahest-api`, new module `images`:

- `normalize_messages(&mut [Value], &ImageFetchPolicy)`: every OpenAI `image_url` part
  becomes a checked `data:` URL before it reaches llama-server. `data:` URLs must be
  `image/*`, base64 and within the size limit (`check_data_url`); `http(s)` URLs are fetched
  by the server (`fetch_as_data_url`); any other scheme is a 400. `detail` is kept.
- `fetch_as_data_url`: SSRF protection — the host is resolved first and every address must
  be public (`is_private`: loopback, RFC 1918, link-local incl. 169.254.169.254, CGNAT,
  benchmarking, reserved, IPv6 unique-local / link-local / documentation / NAT64,
  IPv4-mapped) unless `allow_private`; the request then connects to the checked address
  (`reqwest` `resolve`, no proxy), so DNS rebinding cannot redirect it; redirects are
  followed manually (at most 3) with the same check per hop; `image/*` content type
  required; size limit enforced on `Content-Length` and while streaming; one timeout over
  the whole fetch. Private targets answer 403 (in the dialect's shape), other failures 400.
- `ImageFetchPolicy { enabled, allow_private, max_bytes (20 MiB), timeout (15 s) }`, an
  optional state service; without one `ImageFetchPolicy::from_env` applies
  (`ALKAHEST_IMAGE_FETCH=0` disables fetching, `ALKAHEST_IMAGE_FETCH_ALLOW_PRIVATE=1` opens
  private networks).
- Wiring: `/v1/chat/completions`, `/v1/messages` (Anthropic `url` image sources) and
  `/v1/responses` (`input_image` URLs) normalise after the Vision capability check, so a
  model without vision answers 400 before anything is fetched. Ollama `images` were already
  base64 data URLs with the Vision check.

The projector (`mmproj`) is passed automatically by the existing launch path (`--mmproj`,
verified below).

## Decisions

- Fetch policy lives in `alkahest-api` (env + optional state service) rather than a new
  `alkahest-core` config section, per the rule that shared types change only in their own
  story; a `[server.images]` config section can map onto `ImageFetchPolicy` later.

## Verification

- `crates/alkahest-api/tests/vision.rs` (wiremock image server): remote images fetched into
  data URLs in OpenAI, Anthropic (through a redirect) and Responses, Ollama inline images;
  loopback URLs refused with 403 by default and nothing fetched; HTML, oversized,
  `file://` and non-image data URLs answer 400; models without vision refuse images in
  OpenAI, Anthropic and Ollama before fetching. Unit tests in `images` (private ranges,
  data URLs, loopback and scheme refusal).
- Real engine: opt-in `crates/alkahest/tests/serve_vision.rs` (`ALKAHEST_TEST_ENGINE=1`,
  `hf.co/ggml-org/SmolVLM-256M-Instruct-GGUF:Q8_0` pulled into the scratch `ALKAHEST_HOME`,
  175 MB + 104 MB projector, removed afterwards): llama-server started with `--mmproj`;
  Ollama `images`, OpenAI data URL and http URL (fetched from a loopback server with
  `ALKAHEST_IMAGE_FETCH_ALLOW_PRIVATE=1`), Anthropic base64 and Responses `input_image` all
  answered, with image-sized prompt token counts: passed.
- `scripts/ci-local.sh`: passed (777 tests).
