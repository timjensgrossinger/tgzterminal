#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'usage: %s --version tgz-vYYYY.MM.PATCH --output DIR\n' "$0" >&2
    exit 2
}

version=''
output_dir=''
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            [[ $# -ge 2 ]] || usage
            version="$2"
            shift 2
            ;;
        --output)
            [[ $# -ge 2 ]] || usage
            output_dir="$2"
            shift 2
            ;;
        *)
            usage
            ;;
    esac
done

[[ "$version" =~ ^tgz-v[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
[[ -n "$output_dir" ]] || usage

version="${version#tgz-v}"
arch=$(dpkg-architecture -q DEB_HOST_ARCH)
target_dir="${TARGET_DIR:-target}"
package_name="tgzterminal_${version}_${arch}.deb"
root=$(mktemp -d "${TMPDIR:-/tmp}/tgzterminal-deb.XXXXXX")
trap 'rm -rf "$root"' EXIT

mkdir -p "$output_dir"
install -Dm755 "$target_dir/release/tgzterminal" "$root/usr/bin/tgzterminal"
install -Dm755 "$target_dir/release/wezterm-gui" "$root/usr/bin/wezterm-gui"
install -Dm755 "$target_dir/release/wezterm-mux-server" "$root/usr/bin/wezterm-mux-server"
install -Dm755 "$target_dir/release/strip-ansi-escapes" "$root/usr/bin/strip-ansi-escapes"
ln -s tgzterminal "$root/usr/bin/wezterm"

cat > "$root/usr/bin/open-tgzterminal-here" <<'EOF'
#!/bin/sh
exec tgzterminal start --cwd "$PWD" -- "$@"
EOF
chmod 0755 "$root/usr/bin/open-tgzterminal-here"
ln -s open-tgzterminal-here "$root/usr/bin/open-wezterm-here"

mkdir -p "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<EOF
Package: tgzterminal
Version: $version
Section: utils
Priority: optional
Architecture: $arch
Maintainer: TGZTerminal maintainers
Homepage: https://github.com/timjensgrossinger/tgzterminal
Description: TGZTerminal terminal emulator
 GPU-accelerated terminal emulator and multiplexer based on WezTerm.
Conflicts: wezterm, wezterm-nightly
Replaces: wezterm, wezterm-nightly
Provides: x-terminal-emulator
EOF

cat > "$root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    update-alternatives --install /usr/bin/x-terminal-emulator x-terminal-emulator /usr/bin/tgzterminal 20
fi
EOF
chmod 0755 "$root/DEBIAN/postinst"

cat > "$root/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = remove ]; then
    update-alternatives --remove x-terminal-emulator /usr/bin/tgzterminal || true
fi
EOF
chmod 0755 "$root/DEBIAN/prerm"

desktop="$root/usr/share/applications/org.tgzterminal.TGZTerminal.desktop"
sed \
    -e 's/^Name=WezTerm$/Name=TGZTerminal/' \
    -e 's/org\.wezfurlong\.wezterm/org.tgzterminal.TGZTerminal/g' \
    -e 's/wezterm/tgzterminal/g' \
    assets/wezterm.desktop > "$desktop"
install -Dm644 assets/icon/terminal.png \
    "$root/usr/share/icons/hicolor/128x128/apps/org.tgzterminal.TGZTerminal.png"
install -Dm644 assets/wezterm.appdata.xml \
    "$root/usr/share/metainfo/org.tgzterminal.TGZTerminal.appdata.xml"
sed -i \
    -e 's/org\.wezfurlong\.wezterm/org.tgzterminal.TGZTerminal/g' \
    -e 's/WezTerm/TGZTerminal/g' \
    "$root/usr/share/metainfo/org.tgzterminal.TGZTerminal.appdata.xml"

install -Dm644 assets/shell-integration/wezterm.sh "$root/etc/profile.d/tgzterminal.sh"
install -Dm644 assets/shell-completion/bash \
    "$root/usr/share/bash-completion/completions/tgzterminal"
install -Dm644 assets/shell-completion/zsh \
    "$root/usr/share/zsh/site-functions/_tgzterminal"

deps=$(dpkg-shlibdeps -O \
    -e "$root/usr/bin/tgzterminal" \
    -e "$root/usr/bin/wezterm-gui" \
    -e "$root/usr/bin/wezterm-mux-server" \
    -e "$root/usr/bin/strip-ansi-escapes" \
    | sed -n 's/^shlibs:Depends=//p')
[[ -n "$deps" ]] || { printf 'Could not derive shared-library dependencies\n' >&2; exit 1; }
printf 'Depends: %s\n' "$deps" >> "$root/DEBIAN/control"

dpkg-deb --build --root-owner-group "$root" "$output_dir/$package_name"
sha256sum "$output_dir/$package_name" > "$output_dir/$package_name.sha256"
printf 'Built %s\n' "$output_dir/$package_name"
