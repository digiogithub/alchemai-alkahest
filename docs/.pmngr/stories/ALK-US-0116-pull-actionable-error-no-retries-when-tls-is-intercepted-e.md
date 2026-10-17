---
id: ALK-US-0116
type: story
title: "Pull: actionable error (no retries) when TLS is intercepted, e.g. ISP block of Cloudflare R2 IPs"
status: triage
priority: medium
author: mcp
labels: [bug, fetch, tls]
created: 2026-10-17T13:26:28Z
updated: 2026-10-17T14:00:03Z
inbox:
  status: pending
  source: mcp
  received: 2026-10-17T13:26:28Z
---

## Report

On 2026-10-17, `alkahest pull qwen3:0.6b` fails both on the host (`target/debug/alkahest`, fresh `ALKAHEST_HOME`) and inside the Docker image:

```
ERROR rustls_platform_verifier::verification::others: failed to verify TLS certificate: invalid peer certificate: Other(OtherError(CaUsedAsEndEntity))
error: pulling qwen3:0.6b: downloading application/vnd.docker.container.image.v1+json: part 0 failed after 7 attempts: http error: error sending request for url (https://<id>.r2.cloudflarestorage.com/ollama/docker/registry/v2/blobs/...)
```

The manifest fetch from the registry succeeds; the blob redirect to the R2 host fails TLS verification. Not caused by the env API key / Docker change ([[ALK-US-0114]], [[ALK-US-0115]]).

## To find out

- Whether the certificate presented on this network is the real R2 chain or a middlebox / proxy certificate (curl or openssl from the same machine and from another network).
- If genuine: whether `rustls-platform-verifier` 0.7.1 / webpki rejects a chain that Ollama (Go crypto) accepts; a regression test with the chain.
