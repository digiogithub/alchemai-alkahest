# Configuration

Alkahest works with no configuration. When you need to change something, settings are layered,
lowest precedence first:

1. built-in defaults;
2. the system file (`/etc/alkahest/config.toml` on Linux);
3. the user file (`config.toml` in the user configuration directory);
4. an explicit file: `--config FILE` or `ALKAHEST_CONFIG`;
5. `ALKAHEST_*` environment variables;
6. command-line flags (`--host`, `--log-level`, ...).

Files merge key by key, so a file only lists what it changes. **Unknown keys are rejected** so a
typo is an error, not a silently ignored setting.

```sh
alkahest config path     # the file locations and whether each exists
alkahest config show     # the effective configuration and which layer set each part
alkahest config show --json
```

A small example:

```toml
# ~/.config/alkahest/config.toml
[server]
host = "0.0.0.0"
port = 11435

[scheduler]
keep_alive = "30m"       # unload idle models after 30 minutes
max_loaded = 2

[store.ollama]
enabled = true           # reuse an existing Ollama store (read-only)

[logging]
level = "info"
format = "json"
```

The full list of keys, types, defaults and the environment variable that overrides each is in
the [configuration reference](../reference/configuration.md).

## Where things live

| What | Default (Linux) | Override |
|---|---|---|
| Models | `<data dir>/models` | `store.models_dir`, `ALKAHEST_MODELS` |
| Engines | `<data dir>/engines` | `engines.dir`, `ALKAHEST_ENGINES_DIR` |
| API key store | `<data dir>/keys.db` | `auth.key_store` |
| Per-model settings | `<data dir>/model-config/` | |
| Logs | the platform log directory | `logging.dir`, `ALKAHEST_LOG_DIR` |

`ALKAHEST_HOME` moves everything under one root: data, configuration, cache and logs. It is how
containers, tests and portable installs are set up, and how a system service gets its own state
directory (`/var/lib/alkahest`).

## Environment variables beyond configuration keys

| Variable | Meaning |
|---|---|
| `ALKAHEST_HOME` | relocate all Alkahest directories under one root |
| `ALKAHEST_CONFIG` | an explicit configuration file |
| `ALKAHEST_API_KEY` | key the CLI and the desktop app send to a remote server |
| `ALKAHEST_OLLAMA_LIBRARY` | base URL of the Ollama library used by `search` and the desktop browser |
| `ALKAHEST_IMAGE_FETCH=0` | do not let the server fetch image URLs in vision requests |
| `ALKAHEST_IMAGE_FETCH_ALLOW_PRIVATE=1` | allow fetching image URLs that resolve to private networks (off by default to prevent server-side request forgery) |
| `ALKAHEST_DESKTOP_TRAY=0` | start the desktop app without the system tray icon |
| `HF_TOKEN` | Hugging Face token (never stored in a configuration file; a file containing one is refused) |
| `OLLAMA_MODELS` | location of an Ollama store to reuse |

## Secrets

No secret lives in a configuration file. The Hugging Face token is read from `HF_TOKEN` or the
OS keyring; API keys are stored as salted hashes and shown once.

## Changing settings from the desktop app or the API

The desktop Settings view and `PUT /alkahest/v1/settings/server` (admin scope) write the
configuration file. Most settings apply after a restart; the response says so. A value forced by
an environment variable is shown as locked.
