# Serving and security

`alkahest serve` starts the API server. By default it listens on `127.0.0.1:11435`:
loopback only, no API key needed, nothing reachable from the network.

```sh
alkahest serve                         # 127.0.0.1:11435
alkahest serve --host :8080            # port only, loopback
alkahest serve --host 0.0.0.0:11435    # all interfaces (needs API keys, see below)
ALKAHEST_HOST=10.0.0.5:11435 alkahest serve
```

`--host` accepts `host`, `host:port`, `:port` or a URL. The same flag on the other commands says
which server they talk to, so `alkahest --host 10.0.0.5:11435 ps` inspects a remote server.

Stop the server with Ctrl+C or SIGTERM. It stops accepting requests, lets running requests
finish for up to `server.shutdown_timeout_secs` (30 s), then stops every engine process. No
engine outlives the server.

## Who needs a key

| Caller | Key needed? |
|---|---|
| Same machine (loopback) | no, unless `auth.require_on_loopback = true` |
| Another machine | **yes**, unless `auth.require_key_for_remote = false` |
| Admin routes (`/alkahest/...`, pull, delete, copy, create) from another machine | a key with the `admin` scope |

Alkahest refuses to start on a non-loopback address when remote callers would not need a key or
when no active key exists, so you cannot expose an open server by accident. Passing
`--allow-unauthenticated` overrides the refusal for a trusted network.

Exempt from authentication: `/` and `/healthz` (liveness probes). Everything else, including
`/api/version`, is authenticated for remote callers.

## API keys

```sh
alkahest keys create ci-bot                          # inference scope: chat, embeddings, ...
alkahest keys create ops --scope admin               # admin includes inference
alkahest keys create demo --model llama3.2:3b --expires 7d
alkahest keys ls                                     # name, id, scopes, last used, counters
alkahest keys revoke ci-bot
```

The key is printed **once**, in the form `alk_<id>_<secret>`; only a salted hash is stored (in a
file readable by the owner only). A revoked key stops working on the next request. Send it as
`Authorization: Bearer <key>` or `x-api-key: <key>`, so OpenAI, Anthropic and Ollama clients work
by setting their API key option.

- `--scope inference` (default) allows model calls; `--scope admin` also allows managing models,
  keys and settings.
- `--model` restricts a key to the named models (repeatable).
- `--expires` takes a duration (`12h`, `30d`, `8w`) or an RFC 3339 time.

`keys create` and `keys ls` talk to the key store of the local installation, not to the HTTP
server; the desktop app and the admin API (`/alkahest/v1/keys`) manage the same store.

Failed attempts get one generic 401 (no hint whether the key was unknown or wrong), and the key
comparison is constant time.

### A key from the environment

Containers and orchestrated servers usually receive their secrets as environment variables or
mounted files rather than running `alkahest keys create`. `alkahest serve` reads one extra key
from:

| Variable | Meaning |
|---|---|
| `ALKAHEST_API_KEY` | the key itself |
| `ALKAHEST_API_KEY_FILE` | a file holding the key (Docker and Kubernetes secrets); one trailing newline is ignored |
| `ALKAHEST_API_KEY_SCOPES` | `inference` (default) or `inference,admin` |

The value is any string of at least 32 characters without whitespace, for example
`openssl rand -hex 32`; it does not need the `alk_` form. It counts as an active key for the
startup check, works next to the keys in the store, and appears as key id `env` in the audit log
and usage counters. Setting both variables, a shorter value or an unreadable file stops the start.
Only its SHA-256 digest is kept in memory, and engine processes are started without these
variables. To rotate it, restart the server with the new value.

### Usage and audit

`alkahest keys ls` shows when each key was last used and its request and token counters. With
`auth.audit_log = true` (the default) every key-authenticated or refused request appends one JSON
line to `<log dir>/audit.jsonl`: time, client, route family, key id, status. It never contains
keys or request bodies, and local traffic is not logged.

## Behind a reverse proxy

A proxy on the same machine looks like loopback to Alkahest, which would give every client free
access. Alkahest therefore ignores `X-Forwarded-For` and `Forwarded` unless the immediate peer is
listed in `auth.trusted_proxies`:

```toml
[auth]
trusted_proxies = ["127.0.0.1", "::1"]
```

With the proxy listed, the client address is the one the proxy reports, and the usual rules
apply to it. Without it, a loopback peer that sends forwarding headers loses its loopback trust.
Alternatively set `auth.require_on_loopback = true` and give the proxy a key.

## TLS

The server can speak HTTPS itself (rustls, TLS 1.2 and 1.3, HTTP/1.1 and HTTP/2). It is off by
default. For a quick start with a self-signed certificate:

```sh
alkahest certs generate --name alkahest.lan --name 10.0.0.5   # writes cert, key and CA to <data dir>/tls/
```

then enable it:

```toml
[server.tls]
enabled = true
# cert = "/etc/alkahest/tls/cert.pem"   # defaults: <data dir>/tls/{cert,key,ca}.pem
# key  = "/etc/alkahest/tls/key.pem"
reload_interval_secs = 60
```

`certs generate` creates a throw-away CA and a server certificate signed by it (valid 397 days
by default, `--days`; `--force` overwrites). The private key is written readable by its owner
only and the CA key is never stored. Give clients the CA (`ca.pem`) to trust: the CLI uses the
`ca.pem` next to the configured certificate, or the file named by `ALKAHEST_CA_CERT`; other
clients need it in their trust store or option (`curl --cacert ca.pem`, `verify=` in Python).

For a real certificate (an internal CA, Let's Encrypt) put its files at the configured paths. The
server picks up renewed files without a restart: on `SIGHUP` and when the files change (checked
every `reload_interval_secs`). A broken new certificate is refused and the old one keeps serving.
With TLS enabled the server refuses to start if the files are missing, and never falls back to
plain HTTP. A plain-HTTP server on a network address prints a warning at start.

Alternatively terminate TLS in a reverse proxy (nginx, Caddy, Traefik) and keep Alkahest on
loopback or a private network.

## Choosing what is served

Each API family can be switched off; a disabled family answers 404 in its own error format:

```toml
[server.endpoints]
ollama = true
openai = true
anthropic = true
decisions = false
```

## Cross-origin browsers

CORS headers are not sent by default, so browsers block cross-origin calls. Allow specific
origins (or `*`) with `server.cors_origins = ["https://app.example"]`.

## Limits

| Setting | Default | Effect |
|---|---|---|
| `server.max_body_bytes` | 64 MiB | larger bodies get 413 |
| `server.request_timeout_secs` | 0 (off) | whole-request timeout; off because streams run for minutes |
| `scheduler.max_queue` | 512 | requests waiting for a model before new ones get 503 |
| `scheduler.num_parallel` | 0 (automatic) | parallel requests per loaded model |
| `scheduler.max_loaded` | 0 (automatic) | models loaded at once |

## Loading, keep-alive and memory

A request for a model that is not loaded starts its engine and waits. A loaded model stays for
`scheduler.keep_alive` (default `5m`) after its last request, then unloads. A request can carry
its own `keep_alive` (Ollama semantics: `5m`, `1h`, `0` to unload at once, `-1` to keep forever).
Before loading, the scheduler estimates the memory the model needs; if it does not fit it unloads
idle models first, and queues or refuses (503) when it still cannot. `alkahest ps` shows the
models, memory, queue and time to expiry; `alkahest stop <model>` unloads one now.

## Monitoring

- `GET /healthz`: liveness, always open.
- `GET /metrics`: Prometheus metrics (scheduler, requests, tokens), admin scope for remote callers.
- `GET /alkahest/v1/ps`, `/alkahest/v1/events` (server-sent events), `/alkahest/v1/info`.
- Logs: the server logs to a rotating file in the log directory (`alkahest config show` prints it)
  and to the console; under a service manager use `journalctl`. `--log-level`, `-v` and
  `ALKAHEST_LOG` choose the level (`ALKAHEST_LOG_FORMAT=json` for structured lines).
