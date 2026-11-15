#!/usr/bin/env bash
# This script updates the icon files from the svg file.
# It assumes that the svg file is square and already carries the macOS icon-grid
# margin (plate ~82% of the canvas), so no border is added.
# Needs rsvg-convert (librsvg) and magick (imagemagick); macOS for the .icns.
set -ex
cd $(git rev-parse --show-toplevel)

src=assets/icon/wezterm-icon.svg
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# the linux icon
rsvg-convert -w 128 -h 128 "$src" -o assets/icon/terminal.png

# the macOS icon
rsvg-convert -w 1024 -h 1024 "$src" -o "$work/icon_1024.png"
rm -f assets/macos/WezTerm.app/Contents/Resources/terminal.icns
ci/make-icns.sh "$work/icon_1024.png" assets/macos/WezTerm.app/Contents/Resources/terminal.icns

# The Windows icon
for dim in 16 32 48 64 96 128 256 ; do
  rsvg-convert -w $dim -h $dim "$src" -o "$work/ico_$(printf %03d $dim).png"
done
magick "$work"/ico_*.png assets/windows/terminal.ico
