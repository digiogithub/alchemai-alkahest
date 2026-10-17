# syntax=docker/dockerfile:1.7
#
# Headless Alkahest server image (Linux x86_64 and aarch64): the CLI/server built without the
# `desktop` feature plus the bundled CPU llama.cpp, the same contents as the release tar.gz.
#
#   docker build -t alkahest .
#   docker run -d --name alkahest -p 11435:11435 \
#     -e ALKAHEST_API_KEY="$(openssl rand -hex 32)" \
#     -v alkahest:/var/lib/alkahest alkahest
#
# The server binds 0.0.0.0 inside the container, so every request through the published port
# is remote and needs a key (ALK-SP-0006.R2). Without ALKAHEST_API_KEY (or
# ALKAHEST_API_KEY_FILE, for Docker/Kubernetes secrets) and without keys in the store, the
# server refuses to start. ALKAHEST_API_KEY_SCOPES=inference,admin also grants the admin API.
# Commands run inside the container (`docker exec alkahest alkahest pull qwen3`) are loopback
# and need no key.

ARG RUST_VERSION=1.98.1
ARG DEBIAN_RELEASE=bookworm

FROM rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p alkahest --no-default-features \
    && install -D -m 0755 target/release/alkahest /out/bin/alkahest
# The bundled engine is fetched by the product itself (as `cargo xtask bundle` does), so the
# download is checked against the recipe's SHA-256 and carries its install marker.
RUN ALKAHEST_HOME=/tmp/alkahest /out/bin/alkahest engines install llama-cpp --variant cpu \
    && mkdir -p /out/lib/alkahest \
    && cp -a /tmp/alkahest/data/engines /out/lib/alkahest/engines \
    && install -D -m 0644 LICENSE /out/share/doc/alkahest/LICENSE \
    && install -m 0644 NOTICE THIRD_PARTY_LICENSES /out/share/doc/alkahest/

FROM debian:${DEBIAN_RELEASE}-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libgomp1 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /var/lib/alkahest --create-home alkahest
# /usr/local/bin/alkahest finds the engine at ../lib/alkahest/engines (bundled_roots).
COPY --from=build /out/ /usr/local/
ENV ALKAHEST_HOME=/var/lib/alkahest \
    ALKAHEST_HOST=0.0.0.0:11435 \
    ALKAHEST_LOG_FORMAT=json
USER alkahest
VOLUME ["/var/lib/alkahest"]
EXPOSE 11435
# `alkahest ps` talks to the server over loopback (no key needed) and fails while it is down.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["alkahest", "ps"]
ENTRYPOINT ["alkahest"]
CMD ["serve"]
