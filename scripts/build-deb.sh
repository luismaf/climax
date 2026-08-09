#!/usr/bin/env bash
# Arma un .deb sin depender de dpkg-deb: usa ar + tar (presente en cualquier
# sistema). Uso:
#   scripts/build-deb.sh <versión> <arch: amd64|arm64> <binario> <salida.deb>
set -euo pipefail

VERSION="${1:?falta version}"
ARCH="${2:?falta arquitectura (amd64|arm64)}"
BIN="${3:?falta binario}"
OUT="${4:?falta archivo de salida}"

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
Description: Monitor de cuota de Claude Code via JSON hook + auto-resume sobre herdr
 Vigila la cuota de la ventana de 5 horas de Claude Code a traves del hook
 JSON del statusLine, avisa antes del hard limit (delegacion opcional) y al
 reset destraba automaticamente el/los agentes mediante herdr. Sin tmux, sin
 scraping de UI. Linux/systemd: 'climax --install-service' arma el servicio.
EOF

(
    cd "$PKG"
    tar czf "$TMP/control.tar.gz" ./DEBIAN
    tar czf "$TMP/data.tar.gz" ./usr
)
printf '2.0\n' > "$TMP/debian-binary"

ar rcs "$OUT" "$TMP/debian-binary" "$TMP/control.tar.gz" "$TMP/data.tar.gz"
echo "-> $OUT ($(du -h "$OUT" | cut -f1))"