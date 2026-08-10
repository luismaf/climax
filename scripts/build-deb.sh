#!/usr/bin/env bash
# Builds a .deb without depending on dpkg-deb: uses ar + tar (available on
# any system). Usage:
#   scripts/build-deb.sh <version> <arch: amd64|arm64> <binary> <output.deb>
set -euo pipefail

VERSION="${1:?missing version}"
ARCH="${2:?missing architecture (amd64|arm64)}"
BIN="${3:?missing binary}"
OUT="${4:?missing output file}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

PKG="$TMP/pkg"
mkdir -p "$PKG/usr/bin" "$PKG/DEBIAN" "$PKG/usr/share/licenses/climax"

cp "$BIN" "$PKG/usr/bin/climax"
chmod 755 "$PKG/usr/bin/climax"
if [ -f LICENSE ]; then
    cp LICENSE "$PKG/usr/share/licenses/climax/LICENSE"
fi

cat > "$PKG/DEBIAN/control" <<EOF
Package: climax
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Maintainer: LuisMa <luismaf@gmail.com>
Depends: libc6 (>= 2.17)
Description: Claude Code quota guard: JSON statusLine hook + auto-resume over herdr
 Watches the Claude Code 5-hour rate-limit window through the official JSON
 statusLine hook, warns before the hard limit (optional delegation) and at
 the reset automatically unblocks the agent(s) via herdr. No UI scraping.
 Linux/systemd: 'climax --install' sets up the service.
EOF

(
    cd "$PKG"
    tar czf "$TMP/control.tar.gz" ./DEBIAN
    tar czf "$TMP/data.tar.gz" ./usr
)
printf '2.0\n' > "$TMP/debian-binary"

ar rcs "$OUT" "$TMP/debian-binary" "$TMP/control.tar.gz" "$TMP/data.tar.gz"
echo "-> $OUT ($(du -h "$OUT" | cut -f1))"