#!/usr/bin/env bash
# Publish or update 'climax' on the AUR.
# Requires: an AUR account (aur.archlinux.org) + SSH key configured.
# Usage: scripts/publish-aur.sh [aur-clone-dir]
set -euo pipefail

AUR_DIR="${1:-$HOME/aur/climax}"

if [ ! -d "$AUR_DIR/.git" ]; then
    echo "==> Cloning AUR repo (first time)..."
    if ! git clone ssh://aur@aur.archlinux.org/climax.git "$AUR_DIR" 2>/dev/null; then
        echo "'climax' may already be taken on the AUR — try 'climax-guard' instead." >&2
        exit 1
    fi
fi

cp packaging/aur/PKGBUILD "$AUR_DIR/PKGBUILD"
cd "$AUR_DIR"
updpkgsums
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO
if git diff --cached --quiet; then
    echo "No changes: the AUR is already up to date."
    exit 0
fi
git commit -m "climax $(grep '^pkgver=' PKGBUILD | cut -d= -f2)-$(grep '^pkgrel=' PKGBUILD | cut -d= -f2)"
git push origin master

echo "==> Published: https://aur.archlinux.org/climax/"
echo "    Install: yay -S climax"