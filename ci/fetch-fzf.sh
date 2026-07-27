#!/usr/bin/env bash
#
# fetch-fzf.sh
#
# Vendors a pinned fzf release binary so the worktree/file-browser feature
# (wezterm-gui/src/termwindow/mod.rs::file_browser_script) always has a real
# fzf UI to run, on every platform we ship, without requiring the end user to
# separately `brew install fzf` / `winget install fzf` first. The binary is
# downloaded once per (os, arch) into a cache dir and copied to $DEST on every
# call, so repeat local builds don't re-hit the network.
#
# Usage:
#   ci/fetch-fzf.sh <os> <arch> <dest-path>
#     os:   darwin | windows
#     arch: amd64 | arm64
#
# Environment:
#   FZF_CACHE_DIR   Override the download cache directory. Defaults to
#                   "$CARGO_TARGET_DIR/vendor/fzf" (or "target/vendor/fzf").
#
set -euo pipefail

OS="${1:?usage: fetch-fzf.sh <os> <arch> <dest-path>}"
ARCH="${2:?usage: fetch-fzf.sh <os> <arch> <dest-path>}"
DEST="${3:?usage: fetch-fzf.sh <os> <arch> <dest-path>}"

# Pinned release. Bump the version and re-derive checksums together:
#   curl -fsSL https://github.com/junegunn/fzf/releases/download/v<ver>/fzf_<ver>_checksums.txt
FZF_VERSION="0.74.1"

case "$OS-$ARCH" in
  darwin-amd64)
    ASSET="fzf-${FZF_VERSION}-darwin_amd64.tar.gz"
    SHA256="642f29fb2800690385efb176a209b14d9f593795f0f70ee12c919ee15472e439"
    BIN_NAME="fzf"
    ;;
  darwin-arm64)
    ASSET="fzf-${FZF_VERSION}-darwin_arm64.tar.gz"
    SHA256="849d1d33b050f04dd6b765665e417da151b0e4654dbed8f55c60fd8e23f3ba20"
    BIN_NAME="fzf"
    ;;
  windows-amd64)
    ASSET="fzf-${FZF_VERSION}-windows_amd64.zip"
    SHA256="d83a94a68a9203f6366754123c0e4d7a61e16be18a5d845f4838664b330c4f5f"
    BIN_NAME="fzf.exe"
    ;;
  *)
    echo "error: unsupported os/arch combination for fzf: $OS/$ARCH" >&2
    exit 1
    ;;
esac

CACHE_DIR="${FZF_CACHE_DIR:-${CARGO_TARGET_DIR:-target}/vendor/fzf}"
ARCH_CACHE_DIR="$CACHE_DIR/$OS-$ARCH"
CACHED_BIN="$ARCH_CACHE_DIR/$BIN_NAME"

log() {
  echo "==> $*"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

if [[ ! -f "$CACHED_BIN" ]]; then
  URL="https://github.com/junegunn/fzf/releases/download/v${FZF_VERSION}/${ASSET}"
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT

  log "Downloading fzf ${FZF_VERSION} for ${OS}/${ARCH}"
  curl -fsSL --retry 4 --retry-delay 5 "$URL" -o "$TMP/$ASSET"

  ACTUAL_SHA256=$(sha256_of "$TMP/$ASSET")
  if [[ "$ACTUAL_SHA256" != "$SHA256" ]]; then
    echo "error: checksum mismatch for $ASSET" >&2
    echo "  expected: $SHA256" >&2
    echo "  actual:   $ACTUAL_SHA256" >&2
    exit 1
  fi

  mkdir -p "$TMP/extracted"
  case "$ASSET" in
    *.zip) unzip -o -q "$TMP/$ASSET" -d "$TMP/extracted" ;;
    *.tar.gz) tar -xzf "$TMP/$ASSET" -C "$TMP/extracted" ;;
  esac

  mkdir -p "$ARCH_CACHE_DIR"
  cp "$TMP/extracted/$BIN_NAME" "$CACHED_BIN"
  chmod +x "$CACHED_BIN"
  rm -rf "$TMP"
  trap - EXIT
else
  log "Using cached fzf ${FZF_VERSION} for ${OS}/${ARCH} at $CACHED_BIN"
fi

mkdir -p "$(dirname "$DEST")"
cp "$CACHED_BIN" "$DEST"
chmod +x "$DEST"
log "fzf staged at $DEST"
