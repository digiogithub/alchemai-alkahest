# Long model names stay inside their box (desktop)

Story [[ALK-US-0135]]. Epic: desktop application. Plan: [[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest-desktop/src/views/widgets.rs`: `model_name(id, name, lines)`. The name
  shrinks with its parent (`min_w_0`), wraps onto at most `lines` lines (GPUI breaks before
  `/`, `-`, `:` or inside one word longer than the line) and ends in an ellipsis
  (`line_clamp` + `text_ellipsis`; `truncate` for one line). A tooltip shows the whole name.
- Used in: Library rows (2 lines), Running cards (2 lines; the kind badge no longer shrinks),
  the Model settings breadcrumb (1 line) and title (2 lines), the Hugging Face and Ollama
  detail headers in Discover (2 lines; the task chip keeps its width, the breadcrumb truncates).
- Running, Live requests: client, endpoint and model columns shrink down to 64 px and end in an
  ellipsis (`column`), so the row no longer runs past the card in a narrow window.
- Discover, Hugging Face: the results column shrinks from 470 to 300 px so the detail panel
  keeps at least 340 px (`RESULTS_MIN_WIDTH`, `DETAIL_MIN_WIDTH`); at 1000 px the repository
  title used to wrap one or two characters per line.

## Why

Model names from Hugging Face (`hf.co/<user>/<repo>:<quant>`) have no spaces, so the text had
no break opportunity inside its column and pushed into the next one or past the window.

## Verification

- Screenshots under xvfb (1000×700 and 1400×800) of Library, Running, Discover and Model
  settings with `hf.co/limcheekin/CodeRankEmbed-GGUF:Q4_K_M` loaded: two lines then `…`,
  tooltip with the whole name, no overlap.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  `cargo clippy -p alkahest-desktop --features gui-test --all-targets -- -D warnings`: clean.
- `cargo nextest run --workspace --locked`: 1440 passed; `-p alkahest-desktop --features
  gui-test`: 160 passed. `cargo xtask check-deps`, `tokens --check --contrast`,
  `alkahest desktop --smoke-test`: OK.

## Not changed

At windows narrower than about 1200 px the Library table's fixed columns (kind … actions) are
wider than the card, so the Context column is clipped and columns drift by a few pixels between
rows. That is the table layout, not the names; it needs its own change (hide or stack columns
at narrow widths).
