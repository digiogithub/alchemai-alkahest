# Website in pages/, published with GitHub Pages

Date: 2026-10-18. Related: [[plan/alkahest-v1-plan]], design snapshot `design/README.md`.

## What

- `pages/index.html` (English) and `pages/es/index.html` (Spanish): one-page site with hero and
  library screenshot, sources-to-API funnel (the brand mark metaphor), features, a tabbed gallery
  of the designed screens (Library, Discover, Model settings, Running, Engines, API keys,
  Catalogs), model kinds, engines, API routes with code samples, technical details, security
  and downloads.
- `pages/assets/site.css`: built on the Alembic dark tokens (`design/tokens.json`): brass for the
  one primary action, verdigris for live, kind badges, processor pills and format tags shaped
  as in the desktop app. Fonts are the embedded Schibsted Grotesk and IBM Plex Mono (OFL).
- `pages/assets/site.js`: tabs, copy buttons, scroll reveal, OS detection (only the detected
  system gets the brass download button) and download links resolved from the latest GitHub
  release, falling back to the releases page.
- `pages/build.sh` copies screenshots (`design/screens/png`), marks (`design/brand`) and fonts
  (`crates/alkahest-desktop/assets/fonts`) into the output, so they are not duplicated in git.
- `.github/workflows/pages.yml` builds and deploys on pushes to `main` that touch the site or the
  design images, and on demand.
- `typos.toml` excludes `pages/es/**` (Spanish).

## Fixes found on the way

The setup scripts, the Homebrew formula template and the crate metadata pointed at
`digiogithub/alkahest`, which does not exist; the repository is `digiogithub/alchemai-alkahest`
(as `alkahest update` already used). Fixed in `scripts/setup/install.sh`,
`scripts/setup/install.ps1`, `scripts/release/homebrew-formula.sh`,
`packaging/homebrew/alkahest.rb.in`, `Cargo.toml` and `crates/alkahest/Cargo.toml`. The
Homebrew tap `digiogithub/homebrew-tap` does not exist yet, so the site does not offer Homebrew.

## Verification

- `pages/build.sh` assembles a 2.2 MB site; served locally and captured with headless Chrome at
  1440 px (English) and 390 px (Spanish): layout, tabs and the latest-release links (v1.0.2,
  sizes) render.
- `scripts/setup/install.sh --dry-run` resolves URLs under `digiogithub/alchemai-alkahest`;
  `releases/latest` resolves to `v1.0.2`.

## Hosting

- GitHub Pages enabled with `build_type=workflow`, custom domain `alkahest.alchemai.es`
  (`gh api repos/digiogithub/alchemai-alkahest/pages`); repository homepage set to it.
- Cloudflare zone `alchemai.es`: CNAME `alkahest → digiogithub.github.io`, DNS only (not
  proxied) so GitHub can issue the certificate.
- Pages use absolute `og:image`, `og:url` and `canonical` URLs on that domain.
