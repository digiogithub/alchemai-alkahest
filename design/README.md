# Alkahest desktop design

Design hand-off for `alkahest desktop` (crate `alkahest-desktop`, GPUI via the
`gpui-kit` facade in `src/ui/`). Exported on 2026-10-17 from the Design canvas
"Alkahest Desktop Design" (claude.ai artifact `Fv6snHtv63gBKhtrzvZbKv`).
The canvas is the editable master; this folder is the snapshot the code is
built against. Re-export when the canvas changes.

```
design/
  tokens.json        Alembic design system tokens (input of `cargo xtask tokens`)
  brand/             logo mark, lockups, app icons (SVG)
  screens/*.html     standalone previews, open in any browser (file:// works)
  screens/png/       rendered PNGs of each screen
  screens/support.js tiny preview runtime used by the .html files
  source/            canvas sources (.dc.html + canvas.json), round-trip only
```

## Brand

- Name: **alkahest**, always lower case in the wordmark; "Alkahest" in prose.
- Mark: a funnel drawn as the alchemical sign for water. Wide lip = many model
  sources, solid core = the loaded model, drop through the apex = one API.
  Built on a 64-unit grid, stroke 4.5, round joins and caps.
- Sizes: `mark.svg` from 64 px up; `mark-small.svg` (drop removed, stroke 6)
  for 48 px and below; `mark-tray.svg` (stroke 8, `currentColor`) for the
  system tray and 16 px favicons, so it can be tinted as a template icon.
- `app-icon.svg` is the installer / dock icon (ink tile, brass mark);
  `app-icon-brass.svg` is the alternative for light docks.
- `lockup-*.svg` set the wordmark as live text in Schibsted Grotesk 600;
  convert the text to outlines before using them outside the app.
- Clear space around the mark: 4 units of the 64 grid on every side.

## Design system "Alembic"

Dark first, dense, instrument-like. Light theme tokens are defined for
completeness; v1 can ship dark only.

- **Brass** (`color.*.accent`) marks the one primary action of a view, focus,
  selection and slider fill. One brass button per screen.
- **Verdigris** (`color.*.ok`) means live: running, resident on GPU, toggle on.
- **Signal blue** (`info`) for loading, downloading, verifying.
- **Cinnabar** (`danger`) only for stop, kill, revoke, delete.
- **Model kind** (`kind.*`): eight kinds, each badge = glyph + label + colour
  on a 15 % tint. Never rely on colour alone.
- **Processor state** (`processorState.*`): `GPU 100%`, `GPU x% · CPU y%`,
  `CPU`, `Loading… n%`, `Not loaded`. Same pill in Library and Running.
- **Format tags**: outlined mono tags (`GGUF · Q4_K_M`, `safetensors · BF16`).
  Ternary formats (`PQ2_0`, `PTQ1_0`) use the brass border because they need
  the prism engine. Models read from the Ollama store use a dashed border
  ("read-only").
- **Type**: Schibsted Grotesk for UI, IBM Plex Mono for every number, size,
  path, quant, flag and id, so columns and meters align. Both OFL-1.1: embed
  the font files in the binary (no GPL concern, keep the licence texts).
- **Surfaces**: raised cards use a 1 px border and no shadow; only overlays
  (menus, dialogs, toasts) get `shadow.overlay`.
- **Icons**: 24-unit stroke icons, stroke 1.8, round caps (Lucide-compatible
  shapes). Nav 18 px, controls 16 px.

`tokens.json` follows the W3C design tokens draft (`$value`, `$type`,
`{alias}` references). Colours with alpha are 8-digit hex.

## Shell

- Window: default 1440×900, minimum 960×640. Client-side title bar follows
  the platform; the design shows content only.
- Left rail 232 px (`bg.sidebar`): wordmark, nav (Library, Discover, Running,
  Engines, API keys, Settings) with live counters (models, active downloads,
  loaded models, engine install jobs), and a service card at the bottom (state, bind address, service
  manager). Active item = `accent.subtle` background + `accent.text`.
- Content padding 24/32 px, screen title `type.h1`, primary action top right.
- All data comes from the local server over HTTP (`alkahest-client`); the
  desktop never runs inference in-process.

## Screens

| # | Screen | Preview | Purpose |
|---|---|---|---|
| 00 | Brand | `screens/00-brand.html` | logo, construction, sizes, lockups |
| 01 | Design system | `screens/01-design-system.html` | tokens and components |
| 10 | Library | `screens/10-library.html` | local models |
| 11 | Discover | `screens/11-discover.html` | Hugging Face and Ollama search and download |
| 12 | Model settings | `screens/12-model-settings.html` | per-model runtime config |
| 13 | Running | `screens/13-running.html` | loaded models, hardware, live requests |
| 14 | API keys & network | `screens/14-api-keys.html` | external access, keys, endpoints, HF token |
| 15 | Engines | `screens/15-engines.html` | engine recipes, installed builds per accelerator, install jobs |
| 16 | Settings | `screens/16-settings.html` | storage, model loading, server connection, theme, logging |
| 17 | States & confirmations | `screens/17-states.html` | empty, first-run, connection, failure states; destructive dialogs |

The previews contain sample data only (model names, sizes and counters are
illustrative).

### 10 Library

- Header: count, total disk use, store path; filter field; brass "Add model"
  (goes to Discover).
- Kind filter chips with counts (All, Chat, Embeddings, Rerank, Vision, Audio,
  Image, Video, Decision) plus Source and Format dropdowns.
- Info banner when an Ollama store is detected (path, model count, read-only).
- Table columns: Model (name + source line in mono), Kind badge, Format tag,
  Engine, Size, Context, State pill, actions (Load / Unload / Cancel, settings,
  delete). Table scrolls horizontally below 980 px.

### 11 Discover

- Segmented control: Hugging Face, Ollama library, Paste repo / URL. HF token
  status pill.
- Search field (focused state shown) and filters: task, format, "fits in N GB"
  (from detected VRAM), sort.
- Result list (repo, downloads, kind, format, meta); selected item has the
  brass border.
- Detail panel: repo, licence, params; file picker per quant with size, fit
  estimate and the recommended quant; "Recommended from the model card" block
  (context, sampling, KV cache, template, engine, estimated VRAM) applied on
  download; destination folder; brass download button.
- Downloads strip at the bottom: progress, speed, ETA, sha256 verification.

### 12 Model settings

- Breadcrumb Library / model, kind badge, format tag, source tag.
- Runtime card: engine (recipe build), keep-alive, context window slider on a
  log scale 2k–128k with the model-card value marked, KV cache K and V types,
  GPU layers, parallel slots, batch size, threads, toggles (flash attention,
  tools, thinking, mmap).
- Chat template card: From GGUF (jinja) / Ollama TEMPLATE / Custom, with the
  template in a code block.
- Side column: memory estimate (weights + KV cache + compute buffers, stacked
  bar against the GPU, headroom message), "Model card says" diff with "Apply n
  differences", and the resulting launch line for the engine.
- Header actions: Reset to card defaults, brass "Save & reload".

### 13 Running

- Header: models in memory, scheduler limits; Unload idle, Stop all (danger).
- Hardware tiles: VRAM per GPU, GPU load, system RAM, CPU.
- One row per loaded model: name and kind, engine line (context, pid, layer
  split), processor pill, memory, requests (active, queued, one segment per
  parallel slot), throughput, keep-alive countdown (brass when about to
  unload), Unload and kill buttons.
- Live requests table: time, client (key prefix and name or localhost),
  endpoint, model, tokens in/out, latency (queued in brass, slow in cinnabar).

### 14 API keys & network

- "Allow access from other machines" toggle switches bind address between
  `127.0.0.1` and `0.0.0.0`; port; CORS origins; plain-HTTP warning when on.
- Keys table: name, created, masked prefix, scopes (kinds, models,
  endpoints), last used, 24 h count, Revoke.
- New key panel: key shown once with copy button; only the hash is stored.
- Endpoint toggles: Ollama native `/api/*`, OpenAI `/v1/*`, Anthropic
  `/v1/messages`, decisions `/v1/systemone` and `/v1/decisions`.
- Hugging Face token (stored in the system keyring) with Replace.

### 15 Engines

Data: the engine catalog and installed builds (`alkahest engines ls` /
`engines info`, admin engines API), install jobs (admin install job API).

- Header: engine count, installed builds, engines folder and disk use;
  secondary "Reload recipes"; brass "Install recommended builds" installs the
  `recommended` variant of every engine that has none installed (the one
  primary action of the view).
- "This machine" strip: detected accelerators with the facts variant
  selection uses (GPU, VRAM, driver, maximum CUDA version; Vulkan; CPU
  features), the preference order Metal › CUDA › ROCm › Vulkan › CPU, and the
  `engines.auto_install` toggle (off by default, it downloads executables).
- Install jobs card (one per running job, signal blue): state pill
  (`Downloading` with bytes/speed, `Verifying sha256`, `Building from source`
  with the cmake step counter), progress bar, last log lines in a code block
  with the log path, Cancel (secondary; cancel kills a source build).
- One card per engine id (`llama-cpp`, `prism-llama-cpp`, `vllm`, later
  whisper.cpp and stable-diffusion.cpp): name, id in mono, what it serves,
  pinned recipe version tag, "Override…" (opens a sheet for
  `engines.overrides.<id>`: custom executable path and extra args).
  - Installed builds table: `version · variant` in mono, tag pill (`in use` =
    verdigris when a loaded model runs on it, `bundled` = neutral), origin
    (`prebuilt · sha256 verified`, `prebuilt · tofu`, `built from source`),
    size, models using it, Info and Remove (cinnabar outline; no Remove on the
    bundled CPU build). Remove opens confirmation 17.13.
  - Not installed: one muted line with the CLI equivalent.
  - Variants row, one chip per recipe variant, state by style: installed
    (verdigris border), recommended (brass border, note with download size),
    available (neutral), building/downloading (signal blue), source only or
    install on request (dashed), unsupported on this host (dashed, faint,
    reason as the note). A click on an available chip installs that variant;
    a menu on it offers "Build from source".
- Footnote: models use the newest installed build unless pinned in model
  settings (`engine = "llama-cpp@b11669"`); recipe directories.

### 16 Settings

Data: the server configuration (admin settings API over `config.toml`) and
the desktop's own `desktop.toml`. Network settings (bind, port, CORS,
endpoints) and the HF token stay on screen 14; Settings links there.

- Header: which files are written; dirty state ("2 unsaved changes · 1 needs
  a restart", brass dot); Discard (secondary) and brass "Save" — the label
  becomes "Save & restart server" when a changed field needs a restart.
- Changed fields carry a brass `changed` tag (`changed · restart` when the
  server must restart) and the focus ring.
- Storage: models folder and engines folder (mono path input + "Choose…"),
  free disk; a changed models folder explains that existing models are not
  moved. "Use models from the Ollama store" toggle (verdigris) with the
  detected path, model count and the read-only promise; "Other path…".
- Model loading: default keep-alive as segmented presets (After each request
  = `0`, `5m`, `30m`, `1h`, Forever = `-1`) plus a custom field accepting the
  Ollama duration syntax; max loaded models, parallel requests per model
  (`0` is shown as "Auto"), queue limit.
- A field set by an environment variable (`ALKAHEST_*`) is read-only: dashed
  border, muted value and a lock note naming the variable.
- Side column: server connection (URL this app talks to; API key for a
  remote server, kept in the keyring), Appearance (Dark / Light / System
  segmented, applies at once, `desktop.toml`), Logging (level, format, log
  folder + rotation, "Open folder"), and "Loaded from" listing the
  configuration sources in precedence order.

### 17 States & confirmations

A reference sheet (fixed 1440 px wide, not a page). Frames show the content
area; rail and service card are as on the other screens.

- 17.1 Library empty: dashed drop zone, outline mark, "No models yet", brass
  "Discover models", secondary "Point to an Ollama store" (Settings), CLI
  hint `alkahest pull …`. The empty result of a filter (not drawn) reuses
  the block with "No models match" and a "Clear filters" secondary, no brass.
- 17.2 Running empty: hardware tiles still live, dashed block "Nothing
  loaded", secondary "Open Library" (no brass: nothing to do here).
- 17.3 First run (no server answered and no models): rail items disabled,
  grey service card, three-step checklist: 1 Start the local server (brass,
  the active step has the brass border), 2 engine build (recommended GPU
  variant from the Engines data), 3 get a model (Ollama store count or
  Discover). Footer link to connect to a remote server. Steps turn verdigris
  with a check when done; the view is replaced by Library when 1 and 3 are.
- 17.4 Server stopped (local URL): grey dot "Service not running", the
  error in mono, Start server (secondary in the card, brass in the content);
  lists keep their last data greyed and actions are disabled.
- 17.5 Connection lost (remote URL, Start not offered): brass warning banner
  with retry countdown, attempt and last seen time; raw error in mono
  cinnabar text; "Change server" and brass "Retry now". Backoff as in
  `connection.rs` (0.5 s doubling to 10 s).
- 17.6 Unauthorized (401/403): lock, explanation, password field for the
  key, keyring note, "Use local server" and brass "Connect".
- 17.7 Download failures in the Discover downloads strip: `Interrupted`
  (kept bytes, Resume / Discard), `Checksum mismatch` (bytes deleted,
  Retry), `Gated repo` (brass lock pill, link to add the HF token).
- 17.8 Engine and load failures: `Build failed` (log tail, Open full log,
  Install prebuilt instead), `Load failed` for memory (needed vs free,
  Unload others and retry, Lower context → model settings), `Engine missing`
  (Go to Engines; shown when auto-install is off).
- 17.9–17.14 Confirmation dialogs (overlay surface, `shadow.overlay`, 380 px,
  backdrop `bg.code` at 72 %): title as a question naming the object, the
  object's facts in mono, consequences in one or two sentences, buttons right
  aligned: Cancel (default focus, brass focus ring), optional safer
  alternative (secondary), confirming action (cinnabar). Esc and the backdrop
  cancel. Dialogs: delete model; delete refused for an Ollama-store model
  (information only, "Copy command" for `ollama rm`); kill engine (alternative
  "Unload instead"); revoke key (type the key name to enable Revoke when the
  key was used in the last 24 h); remove engine build (fallback named); stop
  all models (alternative "Unload idle").
- Colour rule recap: cinnabar fills only the confirming button of a
  destructive action, the destructive outline buttons, and failure pills/text;
  warnings use the brass subtle banner; nothing in these states adds a second
  brass button to a view.

### Not designed yet

- Tray menu and notifications (ALK-US-0073).
- Light theme renders of the screens (tokens exist; v1 may ship dark only).

## Previews

Open `screens/10-library.html` in a browser. Nav links between screens work.
Fonts load from Google Fonts, so the first open needs network. To re-render
the PNGs:

```sh
cd design/screens
google-chrome --headless=new --hide-scrollbars --virtual-time-budget=4000 \
  --window-size=1440,1080 --screenshot=png/10-library.png "file://$PWD/10-library.html"
```

`17-states.html` is a tall sheet: render it with `--window-size=1440,3100`.
The exported `.html` files differ from the canvas sources only by the
template tag (`<x-dc>` becomes `<script type="text/x-dc-template">`), the root
`min-height: 100vh` and the link targets (`Library.dc.html` becomes
`10-library.html`).
