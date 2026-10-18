#!/usr/bin/env bash
# Alkahest installer for Linux and macOS.
#
# Detects OS, architecture and GPU, installs the `alkahest` binary (from a release archive
# whose SHA-256 is verified against the release checksum file, or from a local build),
# puts it on PATH and installs the recommended llama.cpp engine build.
#
# Safety: nothing downloaded is ever executed or piped into a shell before its checksum
# matches. Run `install.sh --dry-run` to see every step without changing anything.
#
# Archives and SHA256SUMS are produced by .github/workflows/release.yml (ALK-US-0074); the
# repository below is the public home; override it with --base-url for a mirror or fork.
set -euo pipefail

# Release location; override with --base-url or ALKAHEST_RELEASE_BASE_URL.
DEFAULT_BASE_URL="https://github.com/digiogithub/alchemai-alkahest/releases/download"
DEFAULT_VERSION="latest"

DRY_RUN=0
VARIANT=""
PREFIX=""
NO_ENGINE=0
SERVICE=0
VERSION="${ALKAHEST_VERSION:-$DEFAULT_VERSION}"
BASE_URL="${ALKAHEST_RELEASE_BASE_URL:-$DEFAULT_BASE_URL}"
FROM_LOCAL=""
ASSUME_YES=0

usage() {
  cat <<'USAGE'
Usage: install.sh [options]

  --dry-run          print what would happen; change nothing, download nothing
  --variant NAME     engine accelerator build (cpu, cuda12.8, vulkan, rocm, metal, ...);
                     the best one for this machine when omitted
  --prefix DIR       install the binary into DIR/bin (default: ~/.local, or /usr/local
                     when run as root; a prefix that is not writable uses sudo)
  --no-engine        do not run `alkahest engines install llama-cpp`
  --service          also run `alkahest service install --user`
  --version X.Y.Z    release to install (default: latest)
  --base-url URL     release base URL (default: placeholder, see header)
  --from-local PATH  use an existing alkahest binary instead of downloading
  -y, --yes          do not ask before using sudo
  -h, --help         this text
USAGE
}

die() { echo "install.sh: error: $*" >&2; exit 1; }
info() { echo "==> $*"; }

# run prints the command in dry-run mode and executes it otherwise.
run() {
  if [ "$DRY_RUN" = 1 ]; then
    echo "[dry-run] $*"
  else
    "$@"
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --variant) [ $# -ge 2 ] || die "--variant needs a value"; VARIANT="$2"; shift ;;
    --prefix) [ $# -ge 2 ] || die "--prefix needs a value"; PREFIX="$2"; shift ;;
    --no-engine) NO_ENGINE=1 ;;
    --service) SERVICE=1 ;;
    --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION="$2"; shift ;;
    --base-url) [ $# -ge 2 ] || die "--base-url needs a value"; BASE_URL="$2"; shift ;;
    --from-local) [ $# -ge 2 ] || die "--from-local needs a value"; FROM_LOCAL="$2"; shift ;;
    -y|--yes) ASSUME_YES=1 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
  shift
done

# --- detection -------------------------------------------------------------------------

detect_os() {
  case "$(uname -s)" in
    Linux) echo linux ;;
    Darwin) echo macos ;;
    *) die "unsupported OS $(uname -s); use install.ps1 on Windows" ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo x86_64 ;;
    aarch64|arm64) echo aarch64 ;;
    *) die "unsupported architecture $(uname -m)" ;;
  esac
}

# detect_gpu prints a short human description; it never fails.
detect_gpu() {
  local os="$1" arch="$2"
  if [ "$os" = macos ]; then
    if [ "$arch" = aarch64 ]; then echo "Apple Silicon (Metal)"; else echo "none (Intel Mac)"; fi
    return
  fi
  if command -v nvidia-smi >/dev/null 2>&1 \
     && nvidia-smi -L 2>/dev/null | grep -q '^GPU'; then
    echo "NVIDIA (CUDA)"
  elif [ -e /dev/kfd ] || command -v rocminfo >/dev/null 2>&1; then
    echo "AMD (ROCm)"
  elif command -v vulkaninfo >/dev/null 2>&1 \
     && vulkaninfo --summary 2>/dev/null | grep -qi 'deviceName'; then
    echo "Vulkan-capable GPU"
  else
    echo "none detected (CPU)"
  fi
}

OS="$(detect_os)"
ARCH="$(detect_arch)"
GPU="$(detect_gpu "$OS" "$ARCH")"
info "platform: $OS/$ARCH, GPU: $GPU"

TARGET=""
case "$OS-$ARCH" in
  linux-x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
  linux-aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
  macos-x86_64) TARGET="x86_64-apple-darwin" ;;
  macos-aarch64) TARGET="aarch64-apple-darwin" ;;
esac

# --- prefix and privileges ---------------------------------------------------------------

USE_SUDO=0
if [ -z "$PREFIX" ]; then
  if [ "$(id -u)" = 0 ]; then PREFIX="/usr/local"; else PREFIX="$HOME/.local"; fi
fi
BIN_DIR="$PREFIX/bin"
DEST="$BIN_DIR/alkahest"

# nearest_existing prints the closest existing ancestor of a path.
nearest_existing() {
  local p="$1"
  while [ ! -e "$p" ] && [ "$p" != "/" ]; do p="$(dirname "$p")"; done
  echo "$p"
}

# A prefix we cannot write to needs sudo; ask first unless --yes.
if [ ! -w "$(nearest_existing "$BIN_DIR")" ]; then
  USE_SUDO=1
  if [ "$DRY_RUN" = 0 ]; then
    command -v sudo >/dev/null 2>&1 || die "$BIN_DIR is not writable and sudo is not available"
    if [ "$ASSUME_YES" = 0 ]; then
      printf '%s is not writable; use sudo to install there? [y/N] ' "$BIN_DIR"
      read -r reply
      case "$reply" in y|Y|yes) ;; *) die "aborted; choose another --prefix" ;; esac
    fi
  else
    info "$BIN_DIR is not writable: a real run would ask to use sudo"
  fi
fi

# priv runs a command with sudo when the prefix needs it.
priv() {
  if [ "$USE_SUDO" = 1 ]; then run sudo "$@"; else run "$@"; fi
}

# --- obtain the binary -------------------------------------------------------------------

# sha256_of prints the SHA-256 of a file with whichever tool the platform has.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  else die "need sha256sum or shasum to verify the download"; fi
}

SRC_BIN=""
TMP=""
cleanup() { if [ -n "$TMP" ]; then rm -rf "$TMP"; fi; }
trap cleanup EXIT

if [ -n "$FROM_LOCAL" ]; then
  [ -x "$FROM_LOCAL" ] || die "--from-local: $FROM_LOCAL is not an executable file"
  SRC_BIN="$FROM_LOCAL"
  info "using local binary $SRC_BIN"
else
  [ -n "$TARGET" ] || die "no release archive for $OS/$ARCH"
  ARCHIVE="alkahest-$VERSION-$TARGET.tar.gz"
  if [ "$VERSION" = latest ]; then
    URL_DIR="${BASE_URL%/download}/latest/download"
  else
    URL_DIR="$BASE_URL/v$VERSION"
  fi
  info "release archive: $URL_DIR/$ARCHIVE"
  info "checksum file:   $URL_DIR/SHA256SUMS"
  if [ "$DRY_RUN" = 1 ]; then
    echo "[dry-run] download archive and SHA256SUMS, verify the SHA-256, extract"
  else
    command -v curl >/dev/null 2>&1 || die "curl is required to download the release"
    TMP="$(mktemp -d)"
    if [ "$VERSION" = latest ]; then
      # Archives are named after the version, so resolve what `latest` points to first.
      tag="$(curl -fsSLI --proto '=https' --tlsv1.2 -o /dev/null -w '%{url_effective}' \
        "${BASE_URL%/download}/latest")" || die "could not resolve the latest release"
      resolved="${tag##*/}"
      ARCHIVE="alkahest-${resolved#v}-$TARGET.tar.gz"
      info "latest release: ${resolved}"
    fi
    curl -fsSL --proto '=https' --tlsv1.2 -o "$TMP/SHA256SUMS" "$URL_DIR/SHA256SUMS" \
      || die "could not fetch the checksum file (is the release published? try --base-url or --from-local)"
    curl -fsSL --proto '=https' --tlsv1.2 -o "$TMP/$ARCHIVE" "$URL_DIR/$ARCHIVE" \
      || die "could not fetch $ARCHIVE"
    want="$(awk -v f="$ARCHIVE" '$2 == f || $2 == "*" f {print $1}' "$TMP/SHA256SUMS")"
    [ -n "$want" ] || die "$ARCHIVE is not listed in SHA256SUMS"
    got="$(sha256_of "$TMP/$ARCHIVE")"
    [ "$want" = "$got" ] || die "checksum mismatch for $ARCHIVE (expected $want, got $got)"
    info "checksum verified"
    mkdir "$TMP/x"
    tar -xzf "$TMP/$ARCHIVE" -C "$TMP/x"
    SRC_BIN="$(find "$TMP/x" -type f -name alkahest | head -n 1)"
    [ -n "$SRC_BIN" ] || die "archive does not contain an alkahest binary"
  fi
fi

# --- install (idempotent) ----------------------------------------------------------------

if [ "$DRY_RUN" = 0 ] && [ -f "$DEST" ] && [ -n "$SRC_BIN" ] && cmp -s "$SRC_BIN" "$DEST"; then
  info "$DEST is already up to date"
else
  info "installing alkahest into $BIN_DIR"
  priv mkdir -p "$BIN_DIR"
  priv install -m 0755 "${SRC_BIN:-alkahest}" "$DEST"
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) info "note: $BIN_DIR is not on PATH; add: export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

# --- engine and service ------------------------------------------------------------------

ALK="$DEST"
if [ "$DRY_RUN" = 1 ]; then ALK="alkahest"; fi

if [ "$NO_ENGINE" = 0 ]; then
  info "installing the llama.cpp engine"
  if [ -n "$VARIANT" ]; then
    run "$ALK" engines install llama-cpp --variant "$VARIANT"
  else
    run "$ALK" engines install llama-cpp
  fi
else
  info "skipping engine install (--no-engine)"
fi

if [ "$SERVICE" = 1 ]; then
  info "installing the per-user service"
  run "$ALK" service install --user
fi

info "done. Try: alkahest pull <model> && alkahest run <model>"
