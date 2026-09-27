#!/usr/bin/env bash
# Generates winget-pkgs manifests for a TGZTerminal release and pushes them to
# a winget-pkgs fork. Used ONCE, manually, to introduce the package to
# microsoft/winget-pkgs: winget moderation reviews the first submission by
# hand. Every later version is submitted automatically by the wingetcreate
# step in .github/workflows/tgzterminal-windows-release.yml, which updates the
# manifests this script introduced -- this script is NOT run per release.
#
# Usage:
#   ci/make-tgz-winget-manifests.sh <winget-pkgs-checkout> <TGZTerminal-Setup-tgz-v*.exe>
#
# <winget-pkgs-checkout> must be a clone of YOUR fork of
# microsoft/winget-pkgs with origin pointing at the fork; the branch is pushed
# to origin with the same name. Open the PR against microsoft/winget-pkgs
# afterwards (gh pr create ... from that checkout).
#
# Modeled on upstream WezTerm's ci/make-winget-pr.sh, which stays untouched
# (it serves wez.wezterm and points at wezterm/wezterm releases).
set -xe

winget_repo=$1
setup_exe=$2

# The tag rides in the asset name: TGZTerminal-Setup-tgz-v2026.09.8.exe
TAG_NAME=$(basename "$setup_exe" | sed -E 's/^TGZTerminal-Setup-(.+)\.exe$/\1/')
case "$TAG_NAME" in
  tgz-v*) ;;
  *) printf 'not a TGZTerminal setup exe: %s\n' "$setup_exe" >&2; exit 1 ;;
esac

# winget PackageVersion must be dotted-numeric with no leading zeros:
# tgz-v2026.09.8 -> 2026.9.8
PKG_VERSION=$(printf '%s' "$TAG_NAME" | sed -E 's/^tgz-v0*([0-9]+)\.0*([0-9]+)\.0*([0-9]+)$/\1.\2.\3/')
case "$PKG_VERSION" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) printf 'cannot map tag %s to a winget PackageVersion\n' "$TAG_NAME" >&2; exit 1 ;;
esac

# sha256, portable across macOS (shasum) and Linux (sha256sum)
if command -v sha256sum >/dev/null 2>&1; then
  exehash=$(sha256sum -b "$setup_exe" | cut -f1 -d' ' | tr a-f A-F)
else
  exehash=$(shasum -a 256 "$setup_exe" | cut -f1 -d' ' | tr a-f A-F)
fi

# ReleaseDate from the tagged commit when the tag is visible in this repo,
# else today. winget validates the format, not the exact value.
release_date=$(git show -s "--format=%cd" "--date=format:%Y-%m-%d" "$TAG_NAME" -- 2>/dev/null || true)
[ -n "$release_date" ] || release_date=$(date +%F)

cd "$winget_repo" || exit 1

# First sync the fork with microsoft/winget-pkgs
git remote add upstream https://github.com/microsoft/winget-pkgs.git || true
git fetch upstream master --quiet
git checkout -b "tgzterminal-$PKG_VERSION" upstream/master

pkgid=TimGrossinger.TGZTerminal
# winget-pkgs layout: first letter of the publisher, then Publisher/Name/Version
dir="manifests/t/TimGrossinger/TGZTerminal/$PKG_VERSION"
mkdir -p "$dir"

cat > "$dir/$pkgid.installer.yaml" <<-EOT
PackageIdentifier: $pkgid
PackageVersion: $PKG_VERSION
MinimumOSVersion: 10.0.17763.0
InstallerType: inno
UpgradeBehavior: install
ReleaseDate: $release_date
Installers:
- Architecture: x64
  InstallerUrl: https://github.com/timjensgrossinger/tgzterminal/releases/download/$TAG_NAME/$(basename "$setup_exe")
  InstallerSha256: $exehash
  # The Inno AppId from ci/tgzterminal-windows-installer.iss; the _is1 suffix
  # is how Inno records it in Add/Remove Programs, which is what winget matches.
  ProductCode: '{8638970E-1E70-438A-AF45-7523FCDB720F}_is1'
ManifestType: installer
ManifestVersion: 1.1.0
EOT

cat > "$dir/$pkgid.locale.en-US.yaml" <<-EOT
PackageIdentifier: $pkgid
PackageVersion: $PKG_VERSION
PackageLocale: en-US
Publisher: Tim Grossinger
PublisherUrl: https://github.com/timjensgrossinger
PublisherSupportUrl: https://github.com/timjensgrossinger/tgzterminal/issues
Author: Tim Grossinger
PackageName: TGZTerminal
PackageUrl: https://github.com/timjensgrossinger/tgzterminal
License: MIT
LicenseUrl: https://github.com/timjensgrossinger/tgzterminal/blob/main/LICENSE.md
ShortDescription: GPU-accelerated cross-platform terminal emulator and multiplexer with a built-in agent sidebar, a fork of WezTerm
Tags:
- terminal
- terminal-emulator
- cross-platform
- rust
- wezterm
- agent
ReleaseNotesUrl: https://github.com/timjensgrossinger/tgzterminal/releases/tag/$TAG_NAME
ManifestType: defaultLocale
ManifestVersion: 1.1.0
EOT

cat > "$dir/$pkgid.yaml" <<-EOT
PackageIdentifier: $pkgid
PackageVersion: $PKG_VERSION
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.1.0
EOT

git add --all
git diff --cached
git commit -m "New package: TimGrossinger.TGZTerminal version $PKG_VERSION"
git push --set-upstream origin "tgzterminal-$PKG_VERSION" --quiet

printf 'Pushed branch tgzterminal-%s. Open the PR from your winget-pkgs fork:\n' "$PKG_VERSION"
printf '  gh pr create --repo microsoft/winget-pkgs --head <you>:tgzterminal-%s \\\n' "$PKG_VERSION"
printf '    --title "New package: TimGrossinger.TGZTerminal version %s" --fill\n' "$PKG_VERSION"
