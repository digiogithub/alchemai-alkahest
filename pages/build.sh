#!/usr/bin/env bash
# Assembles the static website into an output directory (default: target/pages).
#
# The pages in this folder reference screenshots, brand marks and fonts that live elsewhere in
# the repository (design/ and the desktop crate), so they are copied in here rather than
# duplicated in git. The GitHub Pages workflow (.github/workflows/pages.yml) runs this script.
#
#   pages/build.sh [OUT_DIR]
#   python3 -m http.server -d target/pages 8000   # preview
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${1:-$root/target/pages}"

rm -rf "$out"
mkdir -p "$out/assets/screens" "$out/assets/brand" "$out/assets/fonts" "$out/es"

cp "$root/pages/index.html" "$out/"
cp "$root/pages/es/index.html" "$out/es/"
cp "$root/pages/assets/site.css" "$root/pages/assets/site.js" "$out/assets/"

for screen in 10-library 11b-discover-catalog 12-model-settings 13-running 14-api-keys \
  15-engines 16b-settings-catalogs; do
  cp "$root/design/screens/png/$screen.png" "$out/assets/screens/"
done
cp "$root/design/brand/mark-small.svg" "$root/design/brand/app-icon.svg" "$out/assets/brand/"

fonts="$root/crates/alkahest-desktop/assets/fonts"
cp "$fonts"/SchibstedGrotesk-{Regular,Medium,SemiBold,Bold}.ttf "$fonts"/IBMPlexMono-{Regular,Medium}.ttf \
  "$fonts"/OFL-*.txt "$out/assets/fonts/"

# Served as-is: no Jekyll processing.
touch "$out/.nojekyll"
echo "site assembled in $out"
