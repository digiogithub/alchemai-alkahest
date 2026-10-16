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
  Engines, API keys) with live counters (models, active downloads, loaded
  models), and a service card at the bottom (state, bind address, service
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

### Not designed yet

- Engines screen (recipes: installed builds per accelerator, download or
  build, version pins).
- Settings (store paths, default keep-alive, max loaded models, theme).
- Empty, error and first-run states; delete and kill confirmations.

## Previews

Open `screens/10-library.html` in a browser. Nav links between screens work.
Fonts load from Google Fonts, so the first open needs network. To re-render
the PNGs:

```sh
cd design/screens
google-chrome --headless=new --hide-scrollbars --virtual-time-budget=4000 \
  --window-size=1440,1080 --screenshot=png/10-library.png "file://$PWD/10-library.html"
```
