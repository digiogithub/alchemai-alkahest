# Alkahest website

Static site at https://alkahest.alchemai.es, published with GitHub Pages by
`.github/workflows/pages.yml`. The custom domain is set in the repository Pages settings and
resolves through a DNS-only CNAME `alkahest.alchemai.es → digiogithub.github.io` in Cloudflare.

```
pages/
  index.html        English
  es/index.html     Spanish (same markup; keep both in sync when editing)
  assets/site.css   styles, built on the Alembic tokens (design/tokens.json, dark theme)
  assets/site.js    tabs, copy buttons, OS detection, latest-release download links
  build.sh          assembles the site with screenshots, brand marks and fonts
```

Screenshots come from `design/screens/png/`, marks from `design/brand/` and the fonts (OFL-1.1)
from `crates/alkahest-desktop/assets/fonts/`; `build.sh` copies them, so they are not duplicated
here. Preview locally:

```sh
pages/build.sh
python3 -m http.server -d target/pages 8000
```

Download links are filled at run time from the GitHub API (latest release) and fall back to the
releases page, so the site does not need a rebuild for a new release.
