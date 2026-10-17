# 2026-10-17 — Desktop: catalogs in Discover, Settings and Engines (ALK-US-0122)

Story [[ALK-US-0122]] (GPUI phase), spec [[ALK-SP-0011]]. Builds on the approved design
[[2026-10-17-desktop-catalog-design]] (screens 11, 11b, 15, 16, 16b, 17.15–17.18) and on the
catalog admin API of [[ALK-US-0124]] ([[2026-10-17-catalog-admin-api]]). The desktop talks to
the server only through `alkahest-client`; nothing reads catalogs in-process.

## What changed

### GPUI-free logic (unit tested)

- `crates/alkahest-desktop/src/catalog_models.rs` (new): Discover › Catalog. `group` turns
  `catalog/search` rows into one card per catalog + model (`CatalogEntry`, default tag first),
  `variant_tag` (`tag · QUANT · size · default`; safetensors "listed only"), `catalog_kind`
  (kind badge from the tags), `filter_entries` (format and "fits" filters, client side),
  `variant_rows` (radio list with fit estimate), `pull_request` (`alkahest/<ns>/<model>:<tag>`
  with `catalog`; `None` for safetensors), detail cells (`engine_text`, `template_text`,
  `params_text`, `files_text`, `origin_text`), `cli_line`, `selector_options`, and
  `CatalogDiscover` (search / detail generations, selection, chosen tag, `mark_stale` to
  re-read the open model after a sync without blanking it).
- `crates/alkahest-desktop/src/catalogs.rs` (new): Settings › Catalogs and shared catalog state.
  `status` (Up to date / Syncing… / Sync failed / Disabled / Not synced yet), `SyncJobs`
  (`catalog_sync` jobs, scope `catalogs` or `catalog:<name>`, step from the phase
  `<name>: <step>`), `source_parts`, `facts_line`, `failure_note` (snapshot in use, next try
  15 min after the error), `token_chip`, `discover_pill`, `recipe_layers`, `never_fetched` /
  `engines_offline_note` (17.18), `BannerState` (17.17: once per error, from the `catalog`
  event or an unreported listing error; dismissed errors stay dismissed; a recovery hides it),
  `AddForm` / `EditForm` / `RemoveAsk` with client-side validation (name charset, `https://` or
  `file://` without credentials, 40-hex pin, `<n>s|m|h|d` interval), `field_of_error` (server
  messages under their field), `patch_needs_sync`, `parse_size` / `format_iec` /
  `limits_patch` / `limit_of_error` for `[catalog_limits]`.
- `crates/alkahest-desktop/src/discover.rs`: `Source::All` and `Source::Catalog` (segmented
  order All, Hugging Face, Ollama library, Catalog, Paste repo / URL; Hugging Face stays the
  default), `Pick`, `DiscoverState::{catalog, all_pick, all_rows, all_pick_after_search}` —
  "All" lists catalog rows first, then Hugging Face, then Ollama (product owner decision).
- `crates/alkahest-desktop/src/engines.rs`: `ChipState::NativeStale`, `VariantChip::{native,
  install_options}` (a native chip installs with `native: true` and no variant; never
  recommended, never in "Install recommended builds"), `InstalledBuild::{from_source, native,
  native_stale, native_tag, rebuild_request}` ("Rebuild for this CPU" = `native` + `force`),
  `OriginChip` / `origin_chip` (`built-in` dashed, `catalog <name> @ <commit>`, `recipes/`,
  `user dir`), `DependencyChip` and `EngineCard::{origin, dependencies, dependency_hint}`
  (optional missing dependency → `install_engine_with` `with: [dep]` for that build).

### Views

- `views/catalogs.rs` (new) `CatalogsView`: one per connection, created by the shell and shared
  by Discover, Engines and Settings. Reads `GET /alkahest/v1/catalogs`, follows `job`
  (`catalog_sync`) and `catalog` events, emits `CatalogsEvent::{ShowGeneral, Open, Changed}`.
  Draws the Catalogs tab (16b): header with secondary "Sync all" and brass "Add catalog",
  General / Catalogs tabs (count, cinnabar dot after a failed sync), one card per catalog in
  layer order (position, name, token chip, status pill, Sync now, Edit…, Remove — not for
  `official` —, verdigris enable toggle, source line with a truncating URL, facts line, sync
  progress bar and step in mono, failure block, warnings disclosure, 72 % opacity when
  disabled), "Advanced · snapshot limits" disclosure (three IEC fields, defaults, "Apply
  limits", server errors under the field), side cards "Recipe layers" and "Private catalogs".
  Dialogs: Add catalog 17.15 (480 px; masked token field; on 409 "token of another host" the
  primary becomes "Replace token & add" → `replace_token: true`), Edit (PATCH of the changed
  fields; a changed ref/subdir/pin starts a sync), token (Replace… / Set token…), Remove
  17.16 (alternative "Disable instead"; for a keyring token, a check box "Also delete its
  token from the keyring" → `delete_token`). All actions apply at once. `banner_element`
  renders 17.17 for other views ("Set token…" when 401/403, Retry, dismiss).
- `views/discover.rs`: All and Catalog sources. Catalog: search field, Catalog selector (only
  with more than one enabled catalog), client-side Format and "Fits in N GB", result cards per
  11b, detail panel (path, mono name, kind badge, description, licence/tags/homepage, variant
  radio list with fit and Default, "From the catalog entry" grid, CLI line, brass
  `Pull <tag> · size`; safetensors → disabled "Listed only"), header pill (enabled catalogs,
  "<name> sync failed") opening Settings › Catalogs, banner 17.17, empty / offline block 17.18
  (secondary Sync now and Catalog settings, no brass). All: one query for every source (copied
  into the Ollama and Catalog fields), source chips on Hugging Face and Ollama cards, the detail
  panel follows the open row. HF and Ollama result cards were factored into `hf_card` /
  `ollama_card` for reuse.
- `views/engines.rs`: origin chip next to the recipe tag, native tags on build rows,
  "Rebuild for this CPU", dependencies row with "Install", stale native chip (dashed brass),
  17.17 banner, 17.18 info strip with "Sync now", footnote with the layer order and a link to
  Settings › Catalogs; a finished catalog sync reloads the listing.
- `views/settings.rs`: General / Catalogs tabs; the Catalogs tab shows `CatalogsView`.
- `views/shell.rs`: builds `CatalogsView`, passes it to the screens, routes
  `CatalogsEvent::Open` to Settings › Catalogs.
- `views/widgets.rs`: `status_pill`, `section_tab`.
- `alkahest-client`: `PullRequest.catalog` (`Option<String>`, skipped when `None`), the field
  the server's pull API already accepted but the typed request lacked.

## Token handling

The token fields are masked kit inputs. The value is read once when sent and the field is
cleared before the request leaves; the string lives only inside the request future and is
dropped after `PUT .../token`. Nothing displays, logs or stores it in the app. Add & sync adds
the catalog without a sync, stores the token, then starts the sync, so a private catalog's
first sync already has its credentials.

## Verification

- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean; desktop job
  `cargo clippy -p alkahest-desktop -p alkahest --features alkahest-desktop/gui-test,alkahest/desktop --all-targets --locked -- -D warnings`: clean.
- `cargo nextest run --workspace --locked`: 1359 passed, 1 skipped.
- `cargo test -p alkahest-desktop --features gui-test --locked`: 150 passed (new GUI test
  `views::catalogs::tests::catalogs_tab_renders_and_dialogs_validate`).
- `cargo xtask check-deps`: OK. `cargo xtask tokens --check --contrast`: up to date, WCAG OK.
- `cargo build -p alkahest --no-default-features --locked`: ok.
- `xvfb-run -a -s "-screen 0 1440x900x24" target/debug/alkahest desktop --smoke-test`: first
  frame in 650 ms.
- `cargo deny check`: bans, licences, sources ok; advisories fail only on the pre-existing
  zbus / zvariant unmaintained advisories (no new dependency).
- Manual run against a scratch server (`ALKAHEST_HOME`, `file://` catalogs `local` and a
  failing `lab`, official not fetched) under Xvfb with screenshots compared to the design PNGs:
  Settings tabs with count and dot, Catalogs cards (Up to date / Sync failed / Not synced yet),
  failure note, limits disclosure, Add dialog (local name validation; a valid `file://` add
  synced to Up to date), Remove dialog with Disable instead (remove confirmed on the server),
  Discover › Catalog (pill "3 catalogs · lab sync failed", selector, card, detail, brass
  Pull), All (catalog card first, Hugging Face rows with source chips), Engines (17.18 strip,
  origin chips `catalog local @ dir-250c`, native chip "native · on request").

## Deviations from the design

- The downloads strip does not show "mirror n of m" for catalog pulls: the pull job carries no
  mirror field today.
- Hugging Face stays the default Discover source (the design draws All first in the control,
  not as the default); All searches every source on each switch.
- Private catalogs side card lists every non-official catalog with "Set token…" / "Replace…"
  (the design only draws catalogs with a token), and offers "Delete" for a token of another
  host (`token_source = mismatch`).
- The token dialog after "Set token…" in a banner opens on Settings › Catalogs (dialogs live in
  that view).
- The banner from a listing is offered only for errors no command reported
  (`error_reported = false`); errors reported by a sync job show on the card and the tab dot.

## Open points

- The 17.17 banner path was exercised by unit tests (`banner_once_per_error`), not live: the
  background refresh was off in the manual run.
- Live token set / 409 replace were not exercised against a real keyring in this session (unit
  and GUI tests cover the local paths; the server side is covered by ALK-US-0124 tests).
