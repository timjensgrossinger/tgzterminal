#!/usr/bin/env bash
#
# build-macos-bundle.sh
#
# Builds dist/TGZTerminal.app (and optionally dist/TGZTerminal.dmg) from the
# committed bundle template at assets/macos/WezTerm.app. Adapted from
# upstream ci/deploy.sh, but rebrands the bundle to TGZTerminal and assembles
# it under dist/ instead of producing a zip.
#
# Usage:
#   ci/build-macos-bundle.sh [--universal|--native] [--dmg] [--no-build] [--help]
#
# Flags:
#   --universal   Build both x86_64-apple-darwin and aarch64-apple-darwin and
#                 combine the resulting binaries with lipo. This is the
#                 default.
#   --native      Build only for the host architecture via a plain
#                 `cargo build --release` (fast local iteration).
#   --dmg         Also produce dist/TGZTerminal.dmg via hdiutil.
#   --no-build    Skip the cargo build step entirely and just re-assemble the
#                 bundle from whatever is already present under target/.
#   --help        Print this help and exit.
#
# Environment:
#   MACOS_SIGN_IDENTITY   Codesign identity to use. Defaults to "-" (ad-hoc
#                         signing). Set to a real Developer ID identity to
#                         produce a signed, distributable bundle.
#   CARGO_TARGET_DIR      Honored if set; otherwise defaults to "target" at
#                         the repo root.
#
set -euo pipefail

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

BUILD_MODE="universal"
BUILD_DMG=0
SKIP_BUILD=0

print_help() {
  sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --universal)
      BUILD_MODE="universal"
      shift
      ;;
    --native)
      BUILD_MODE="native"
      shift
      ;;
    --dmg)
      BUILD_DMG=1
      shift
      ;;
    --no-build)
      SKIP_BUILD=1
      shift
      ;;
    --help|-h)
      print_help
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      print_help
      exit 1
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

TARGET_DIR=${CARGO_TARGET_DIR:-target}
MACOS_SIGN_IDENTITY=${MACOS_SIGN_IDENTITY:--}

APP_TEMPLATE="assets/macos/WezTerm.app"
DIST_DIR="dist"

# Branding parameters. Defaults preserve TGZTerminal behavior exactly; set the
# BRAND_* env vars to produce a differently-branded bundle.
BRAND_APP_NAME=${BRAND_APP_NAME:-TGZTerminal}
BRAND_BUNDLE_ID=${BRAND_BUNDLE_ID:-com.tgzterminal.app}
BRAND_CLI_BIN=${BRAND_CLI_BIN:-tgzterminal}
BRAND_ICON=${BRAND_ICON:-}

APP_NAME="${BRAND_APP_NAME}.app"
APP_PATH="$DIST_DIR/$APP_NAME"
DMG_PATH="$DIST_DIR/${BRAND_APP_NAME}.dmg"

BINARIES=(tgzterminal wezterm-mux-server wezterm-gui strip-ansi-escapes)
DYLIBS=(libEGL.dylib libGLESv1_CM.dylib libGLESv2.dylib)

log() {
  echo "==> $*"
}

fail() {
  echo "error: $*" >&2
  exit 1
}

if [[ ! -d "$APP_TEMPLATE" ]]; then
  fail "bundle template not found at $APP_TEMPLATE"
fi

# ---------------------------------------------------------------------------
# Build binaries
# ---------------------------------------------------------------------------

build_native() {
  log "Building release binaries (native arch)"
  cargo build --release -p wezterm -p wezterm-gui -p wezterm-mux-server -p strip-ansi-escapes
}

build_universal() {
  local targets=(x86_64-apple-darwin aarch64-apple-darwin)
  for triple in "${targets[@]}"; do
    log "Building release binaries for $triple"
    cargo build --release --target "$triple" \
      -p wezterm -p wezterm-gui -p wezterm-mux-server -p strip-ansi-escapes
  done
}

if [[ "$SKIP_BUILD" -eq 1 ]]; then
  log "Skipping cargo build (--no-build)"
elif [[ "$BUILD_MODE" == "native" ]]; then
  build_native
else
  build_universal
fi

# Resolve the path to a built binary, combining per-arch outputs with lipo
# when in universal mode. Writes the result to $1 (destination path).
place_binary() {
  local bin_name="$1"
  local dest="$2"

  if [[ "$BUILD_MODE" == "native" ]]; then
    local src="$TARGET_DIR/release/$bin_name"
    [[ -f "$src" ]] || fail "expected binary not found: $src (did the build succeed?)"
    cp "$src" "$dest"
  else
    local srcs=()
    for triple in x86_64-apple-darwin aarch64-apple-darwin; do
      local candidate="$TARGET_DIR/$triple/release/$bin_name"
      [[ -f "$candidate" ]] || fail "expected binary not found: $candidate (did the build succeed?)"
      srcs+=("$candidate")
    done
    lipo "${srcs[@]}" -output "$dest" -create
  fi
}

# ---------------------------------------------------------------------------
# Verify expected binaries exist before assembling
# ---------------------------------------------------------------------------

log "Verifying expected binaries are present"
for bin in "${BINARIES[@]}"; do
  if [[ "$BUILD_MODE" == "native" ]]; then
    [[ -f "$TARGET_DIR/release/$bin" ]] || fail "missing $TARGET_DIR/release/$bin"
  else
    for triple in x86_64-apple-darwin aarch64-apple-darwin; do
      [[ -f "$TARGET_DIR/$triple/release/$bin" ]] || fail "missing $TARGET_DIR/$triple/release/$bin"
    done
  fi
done

# ---------------------------------------------------------------------------
# Assemble the bundle
# ---------------------------------------------------------------------------

log "Assembling $APP_PATH"
mkdir -p "$DIST_DIR"
rm -rf "$APP_PATH"
cp -r "$APP_TEMPLATE" "$APP_PATH"

# Rebrand the copied bundle's Info.plist (never touch the committed template).
PLIST="$APP_PATH/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleName $BRAND_APP_NAME" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName $BRAND_APP_NAME" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $BRAND_BUNDLE_ID" "$PLIST"
# CFBundleExecutable stays wezterm-gui; CFBundleIconFile stays terminal.icns
# (the icon file itself may be overridden below via BRAND_ICON).

mkdir -p "$APP_PATH/Contents/MacOS"
mkdir -p "$APP_PATH/Contents/Resources"

# Optionally override the bundle icon while keeping CFBundleIconFile=terminal.icns.
if [[ -n "$BRAND_ICON" ]]; then
  [[ -f "$BRAND_ICON" ]] || fail "BRAND_ICON set but file not found: $BRAND_ICON"
  log "Overriding bundle icon from $BRAND_ICON"
  cp "$BRAND_ICON" "$APP_PATH/Contents/Resources/terminal.icns"
fi

# Mesa dylibs ship at the template's .app root; move them into Contents/MacOS
# and remove the copies left at the bundle root, mirroring upstream
# ci/deploy.sh (`rm $zipdir/WezTerm.app/*.dylib`).
log "Relocating dylibs into Contents/MacOS"
for dylib in "${DYLIBS[@]}"; do
  src="$APP_PATH/$dylib"
  [[ -f "$src" ]] || fail "expected dylib not found in template copy: $src"
  mv "$src" "$APP_PATH/Contents/MacOS/$dylib"
done
rm -f "$APP_PATH"/*.dylib

# Place binaries. `wezterm` (the CLI) is renamed to $BRAND_CLI_BIN, with a
# `wezterm` symlink kept for compatibility.
log "Placing binaries into Contents/MacOS"
place_binary tgzterminal "$APP_PATH/Contents/MacOS/$BRAND_CLI_BIN"
chmod +x "$APP_PATH/Contents/MacOS/$BRAND_CLI_BIN"
ln -sf "$BRAND_CLI_BIN" "$APP_PATH/Contents/MacOS/wezterm"

place_binary wezterm-gui "$APP_PATH/Contents/MacOS/wezterm-gui"
chmod +x "$APP_PATH/Contents/MacOS/wezterm-gui"

place_binary wezterm-mux-server "$APP_PATH/Contents/MacOS/wezterm-mux-server"
chmod +x "$APP_PATH/Contents/MacOS/wezterm-mux-server"

place_binary strip-ansi-escapes "$APP_PATH/Contents/MacOS/strip-ansi-escapes"
chmod +x "$APP_PATH/Contents/MacOS/strip-ansi-escapes"

# Vendor fzf so the worktree/file-browser feature (sidebar.rs's Worktree pane)
# always has a real fzf UI, without requiring the user to `brew install fzf`
# themselves. See ci/fetch-fzf.sh for the pinned version + checksums.
log "Vendoring fzf into Contents/MacOS/fzf"
FZF_DEST="$APP_PATH/Contents/MacOS/fzf"
if [[ "$BUILD_MODE" == "native" ]]; then
  case "$(uname -m)" in
    arm64) HOST_ARCH=arm64 ;;
    x86_64) HOST_ARCH=amd64 ;;
    *) fail "unsupported host architecture for fzf vendoring: $(uname -m)" ;;
  esac
  "$SCRIPT_DIR/fetch-fzf.sh" darwin "$HOST_ARCH" "$FZF_DEST"
else
  FZF_TMP=$(mktemp -d)
  "$SCRIPT_DIR/fetch-fzf.sh" darwin amd64 "$FZF_TMP/fzf-amd64"
  "$SCRIPT_DIR/fetch-fzf.sh" darwin arm64 "$FZF_TMP/fzf-arm64"
  lipo "$FZF_TMP/fzf-amd64" "$FZF_TMP/fzf-arm64" -output "$FZF_DEST" -create
  rm -rf "$FZF_TMP"
fi
chmod +x "$FZF_DEST"

# Shell integration, completions, and terminfo, matching upstream deploy.sh.
log "Copying shell integration, completions, and terminfo"
if [[ -d "assets/shell-integration" ]]; then
  cp -r assets/shell-integration/* "$APP_PATH/Contents/Resources/"
fi
if [[ -d "assets/shell-completion" ]]; then
  cp -r assets/shell-completion "$APP_PATH/Contents/Resources/"
fi
if command -v tic >/dev/null 2>&1; then
  tic -xe wezterm -o "$APP_PATH/Contents/Resources/terminfo" termwiz/data/wezterm.terminfo
else
  echo "warning: tic not found on PATH; skipping terminfo generation" >&2
fi

# ---------------------------------------------------------------------------
# Codesign
# ---------------------------------------------------------------------------

log "Codesigning with identity: $MACOS_SIGN_IDENTITY"
codesign --force --deep --sign "$MACOS_SIGN_IDENTITY" "$APP_PATH"

log "Verifying signature"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"

# ---------------------------------------------------------------------------
# Optional .dmg
# ---------------------------------------------------------------------------

if [[ "$BUILD_DMG" -eq 1 ]]; then
  log "Building $DMG_PATH"
  STAGING_DIR=$(mktemp -d)
  trap 'rm -rf "$STAGING_DIR"' EXIT

  cp -r "$APP_PATH" "$STAGING_DIR/"
  ln -s /Applications "$STAGING_DIR/Applications"

  rm -f "$DMG_PATH"
  hdiutil create -volname "$BRAND_APP_NAME" -srcfolder "$STAGING_DIR" -ov -format UDZO "$DMG_PATH"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

APP_SIZE=$(du -sh "$APP_PATH" | cut -f1)
log "Bundle ready: $APP_PATH ($APP_SIZE)"
if [[ "$BUILD_DMG" -eq 1 ]]; then
  DMG_SIZE=$(du -sh "$DMG_PATH" | cut -f1)
  log "DMG ready: $DMG_PATH ($DMG_SIZE)"
fi
