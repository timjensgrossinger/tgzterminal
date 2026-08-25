#!/usr/bin/env bash
#
# make-icns.sh
#
# Builds a macOS .icns from a single square source image (PNG preferred,
# 1024x1024 or larger). A branded build points BRAND_ICON at the result to turn a
# brand symbol into a bundle icon.
#
# Usage:
#   ci/make-icns.sh <source-image> <output.icns>
#
# Regenerates only when the source is newer than the output, so repeat builds
# skip the ~10 sips invocations.
#
set -euo pipefail

SRC=${1:-}
OUT=${2:-}

if [[ -z "$SRC" || -z "$OUT" ]]; then
  echo "usage: ci/make-icns.sh <source-image> <output.icns>" >&2
  exit 2
fi

if [[ ! -f "$SRC" ]]; then
  echo "make-icns: source not found: $SRC" >&2
  exit 1
fi

if [[ -f "$OUT" && "$OUT" -nt "$SRC" ]]; then
  echo "make-icns: $OUT is up to date"
  exit 0
fi

command -v sips >/dev/null || { echo "make-icns: sips not found (macOS only)" >&2; exit 1; }
command -v iconutil >/dev/null || { echo "make-icns: iconutil not found (macOS only)" >&2; exit 1; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
SET="$WORK/icon.iconset"
mkdir -p "$SET"

# The names are fixed by iconutil; 16..512 each in 1x and 2x.
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$SRC" --out "$SET/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" "$SRC" --out "$SET/icon_${size}x${size}@2x.png" >/dev/null
done

mkdir -p "$(dirname "$OUT")"
iconutil -c icns "$SET" -o "$OUT"
echo "make-icns: wrote $OUT"
