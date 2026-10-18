#!/usr/bin/env bash
# Renders packaging/homebrew/alkahest.rb.in for a release (ALK-US-0075).
#
#   scripts/release/homebrew-formula.sh <version> <SHA256SUMS> [base-url] > alkahest.rb
#
# <version>     the release version without the leading `v` (0.1.0, 0.2.0-rc.1)
# <SHA256SUMS>  the checksum file of the release (`<hash>  <file>` lines)
# [base-url]    where the archives are served; default
#               https://github.com/digiogithub/alchemai-alkahest/releases/download/v<version>
#
# Reads the two macOS headless archives, alkahest-<version>-aarch64-apple-darwin.tar.gz and
# alkahest-<version>-x86_64-apple-darwin.tar.gz, from the checksum file and fails when either is
# missing, so a formula is never published with a placeholder in it.
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 <version> <SHA256SUMS> [base-url]" >&2
  exit 2
fi
version="$1"
sums="$2"
base="${3:-https://github.com/digiogithub/alchemai-alkahest/releases/download/v${version}}"
template="$(dirname "$0")/../../packaging/homebrew/alkahest.rb.in"

# sha256 of the archive named $1 in the checksum file; `*` marks binary mode in some tools.
sha_of() {
  local want="$1" hash name
  while read -r hash name; do
    name="${name#\*}"
    if [[ "$name" == "$want" ]]; then
      echo "$hash"
      return 0
    fi
  done < "$sums"
  echo "error: $want is not listed in $sums" >&2
  return 1
}

arm="alkahest-${version}-aarch64-apple-darwin.tar.gz"
x64="alkahest-${version}-x86_64-apple-darwin.tar.gz"
sha_arm="$(sha_of "$arm")"
sha_x64="$(sha_of "$x64")"

sed \
  -e "s|@VERSION@|${version}|g" \
  -e "s|@URL_ARM64@|${base}/${arm}|g" \
  -e "s|@SHA_ARM64@|${sha_arm}|g" \
  -e "s|@URL_X64@|${base}/${x64}|g" \
  -e "s|@SHA_X64@|${sha_x64}|g" \
  "$template"
