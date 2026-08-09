#!/usr/bin/env bash
# Publica o actualiza climax en el AUR.
# Requisito: cuenta de aur.archlinux.org + clave SSH configurada para
# aur@aur.archlinux.org. Uso:
#   scripts/publish-aur.sh [dir-temporal-aur]
set -euo pipefail

AUR_DIR="${1:-$HOME/aur/climax}"

if [ ! -d "$AUR_DIR/.git" ]; then
    echo "Clonando el repo AUR (primera vez)..."
    git clone ssh://aur@aur.archlinux.org/climax.git "$AUR_DIR"
    if [ ! -d "$AUR_DIR/.git" ]; then
        echo "El nombre 'climax' puede estar ocupado en el AUR: probá 'climax-guard'." >&2
        exit 1
    fi
fi

cp packaging/aur/PKGBUILD "$AUR_DIR/PKGBUILD"
cd "$AUR_DIR"
updpkgsums
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO
if git diff --cached --quiet; then
    echo "Sin cambios: el AUR ya está al día."
    exit 0
fi
git commit -m "climax $(grep '^pkgver=' PKGBUILD | cut -d= -f2)-$(grep '^pkgrel=' PKGBUILD | cut -d= -f2)"
git push origin master
echo "==> Publicado: https://aur.archlinux.org/climax/"
echo "    Instalar: yay -S climax"