#!/usr/bin/env bash
# One-liner install for climax:
#   sh -c "$(curl -fsSL https://raw.githubusercontent.com/luismaf/climax/master/scripts/install.sh)"
# Downloads the latest release binary for your OS/arch into ~/.local/bin.
# It never touches your services or config: run 'climax --install' if you
# want the systemd user service with boot autorun.
set -euo pipefail

REPO="luismaf/climax"

echo "-> fetching latest release..."
VERSION="${CLIMAX_VERSION:-}"
if [ -z "$VERSION" ]; then
    VERSION="$(
        curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
            | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\(v[^"]*\)".*/\1/p' \
            | head -1
    )"
fi
[ -n "$VERSION" ] || { echo "could not determine the latest version (no network?)" >&2; exit 1; }

OS="$(uname -s)"
MACH="$(uname -m)"
case "$OS-$MACH" in
    Linux-x86_64)  TRIPLE="x86_64-unknown-linux-gnu" ;;
    Linux-aarch64|Linux-arm64) TRIPLE="aarch64-unknown-linux-gnu" ;;
    Darwin-x86_64) TRIPLE="x86_64-apple-darwin" ;;
    Darwin-arm64|Darwin-aarch64) TRIPLE="aarch64-apple-darwin" ;;
    *)
        echo "unsupported platform: $OS $MACH" >&2
        exit 1
        ;;
esac

URL="https://github.com/$REPO/releases/download/$VERSION/climax-$TRIPLE.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "-> downloading climax $VERSION ($TRIPLE)..."
curl -fsSL "$URL" -o "$TMP/climax.tar.gz"
tar xzf "$TMP/climax.tar.gz" -C "$TMP"

mkdir -p "$HOME/.local/bin"
install -m755 "$TMP/climax" "$HOME/.local/bin/climax"

echo "-> done: ~/.local/bin/climax ($VERSION)"
echo
echo "Try it:        climax -s"
echo "Background:    climax --install   (systemd user service, boot autorun)"
echo "Remove it:     climax --uninstall (service) · rm ~/.local/bin/climax (binary)"
