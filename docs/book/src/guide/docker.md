# Docker

The repository's `Dockerfile` builds a headless image for Linux x86_64 and aarch64: the CLI and
server compiled without the desktop app, plus the bundled CPU llama.cpp. It holds the same files
as the release tar.gz.

```sh
docker build -t alkahest .
docker run -d --name alkahest -p 11435:11435 \
  -e ALKAHEST_API_KEY="$(openssl rand -hex 32)" \
  -v alkahest:/var/lib/alkahest \
  alkahest
```

Inside the container the server binds `0.0.0.0:11435`, so every request through the published
port is remote and needs a key. The image therefore expects one from the environment (see
[A key from the environment](serving.md#a-key-from-the-environment)); without it, and without
keys in the store on the volume, the server refuses to start.

With a Docker secret instead of a variable, so the key does not show in `docker inspect`:

```yaml
services:
  alkahest:
    image: alkahest
    ports: ["11435:11435"]
    environment:
      ALKAHEST_API_KEY_FILE: /run/secrets/alkahest_api_key
      ALKAHEST_API_KEY_SCOPES: inference,admin   # also allow pulls through the API
    secrets: [alkahest_api_key]
    volumes: [alkahest:/var/lib/alkahest]
secrets:
  alkahest_api_key:
    file: ./alkahest_api_key.txt
volumes:
  alkahest: {}
```

Commands run inside the container are loopback calls and need no key:

```sh
docker exec alkahest alkahest pull qwen3:0.6b
docker exec alkahest alkahest ps
```

| Path or variable | Value in the image |
|---|---|
| `ALKAHEST_HOME` | `/var/lib/alkahest` (models, keys, config, logs; the volume) |
| `ALKAHEST_HOST` | `0.0.0.0:11435` |
| `ALKAHEST_LOG_FORMAT` | `json` |
| user | `alkahest` (uid 10001) |
| health check | `alkahest ps` |

The image runs CPU inference only. GPU engines come from recipes (`alkahest engines install`)
and need the matching runtime in the container (for NVIDIA, the NVIDIA Container Toolkit and
`--gpus all`).
