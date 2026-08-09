#!/usr/bin/env bash
# One-liner install for climax:
#   curl -fsSL https://raw.githubusercontent.com/luismaf/climax/master/scripts/install.sh | bash
#   pin a release:  ... | bash -s -- -v 0.4.1      (or: bash -s 0.4.1)
#
# Detects your system and picks the right method:
#   Ubuntu/Debian : .deb package via apt
#   Arch          : PKGBUILD via makepkg (the yay way, without needing the AUR)
#   macOS         : release binary into ~/.local/bin
#   Windows       : cargo install --git (needs a Rust toolchain, e.g. rustup)
#   other Linux   : release binary into ~/.local/bin
#
# It never touches your services or config: run 'climax --install' if you
# want the systemd user service (boot autorun).
#
# Env overrides (handy for testing): CLIMAX_VERSION=v0.4.1 to pin a release,
# CLIMAX_FORCE=arch|deb|mac|windows|linux to force a branch, CLIMAX_DRY_RUN=1
# to only print what would happen. CLI: '-v VERSION' or bare VERSION pins a
# release, e.g.  curl -fsSL <url> | bash -s -- -v 0.4.1
set -euo pipefail

usage() {
    sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
    echo
    echo "Options:"
    echo "  -v, --version VERSION   install a specific release instead of latest"
    echo "  -h, --help              show this help"
    echo
    echo "Env: CLIMAX_VERSION, CLIMAX_FORCE (arch|deb|mac|windows|linux), CLIMAX_DRY_RUN=1"
}

while [ $# -gt 0 ]; do
    case "$1" in
        -v|--version)
            [ $# -ge 2 ] || { echo "option $1 needs a value (e.g. -v 0.4.1)" >&2; exit 1; }
            CLIMAX_VERSION="$2"
            shift 2
            ;;
        -h|--help) usage; exit 0 ;;
        -*)
            if [[ "$1" == v* ]] || [[ "$1" == ?*.*.* ]]; then CLIMAX_VERSION="$1"; shift
            else echo "unknown option: $1 (try --help)" >&2; exit 1; fi
            ;;
        *) CLIMAX_VERSION="$1"; shift ;;
    esac
done

REPO="luismaf/climax"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

sudo_if() {
    if [ "$(id -u)" = "0" ]; then "$@"; else sudo "$@"; fi
}

VERSION="${CLIMAX_VERSION:-}"
if [ -z "$VERSION" ]; then
    echo "-> fetching latest release..."
    VERSION="$(
        curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
            | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\(v[^"]*\)".*/\1/p' \
            | head -1
    )"
fi
[ -n "$VERSION" ] || { echo "could not determine the latest version (no network?)" >&2; exit 1; }

OS="$(uname -s)"
MACH="$(uname -m)"

detect() {
    case "$OS" in
        MINGW*|MSYS*|CYGWIN*) echo "windows" ;;
        Darwin*) echo "mac" ;;
        Linux*)
            if [ -f /etc/os-release ]; then
                if grep -qi "arch" /etc/os-release; then echo "arch"
                elif grep -qi "debian\|ubuntu" /etc/os-release; then echo "deb"
                else echo "linux"; fi
            else echo "linux"; fi
            ;;
        *) echo "linux" ;;
    esac
}
BRANCH="${CLIMAX_FORCE:-$(detect)}"

install_binary() { # generic Linux/macOS: tar.gz into ~/.local/bin
    if [ "$OS" = "Darwin" ]; then
        case "$MACH" in
            arm64|aarch64) TRIPLE="aarch64-apple-darwin" ;;
            *)             TRIPLE="x86_64-apple-darwin" ;;
        esac
    else
        case "$MACH" in
            x86_64)        TRIPLE="x86_64-unknown-linux-gnu" ;;
            aarch64|arm64) TRIPLE="aarch64-unknown-linux-gnu" ;;
            *) echo "unsupported platform: $OS $MACH" >&2; exit 1 ;;
        esac
    fi
    URL="https://github.com/$REPO/releases/download/$VERSION/climax-$TRIPLE.tar.gz"
    echo "-> downloading climax $VERSION ($TRIPLE)..."
    [ "${CLIMAX_DRY_RUN:-0}" = "1" ] && { echo "-> (dry-run) would download $URL"; return 0; }
    curl -fsSL "$URL" -o "$TMP/climax.tar.gz"
    tar xzf "$TMP/climax.tar.gz" -C "$TMP"
    mkdir -p "$HOME/.local/bin"
    install -m755 "$TMP/climax" "$HOME/.local/bin/climax"
    echo "-> done: ~/.local/bin/climax ($VERSION)"
}

install_arch() { # the yay way, straight from this repo (no AUR needed)
    echo "-> Arch detected: building the PKGBUILD (yay -S climax, but from source)..."
    git clone --quiet --depth 1 --branch "$VERSION" "https://github.com/$REPO.git" "$TMP/climax"
    cd "$TMP/climax/packaging/aur"
    [ "${CLIMAX_DRY_RUN:-0}" = "1" ] && { echo "-> (dry-run) would run: makepkg -si"; return 0; }
    makepkg -si
}

install_deb() { # Ubuntu/Debian: .deb via apt (resolves dependencies)
    case "$MACH" in
        x86_64)            DEB="climax_${VERSION#v}_amd64.deb" ;;
        aarch64|arm64)     DEB="climax_${VERSION#v}_arm64.deb" ;;
        *) echo "no .deb for $MACH; falling back to the generic binary"; install_binary; return 0 ;;
    esac
    echo "-> Ubuntu/Debian detected: installing $DEB via apt..."
    curl -fsSL "https://github.com/$REPO/releases/download/$VERSION/$DEB" -o "$TMP/$DEB"
    [ "${CLIMAX_DRY_RUN:-0}" = "1" ] && { echo "-> (dry-run) would run: sudo apt-get install -y $TMP/$DEB"; return 0; }
    sudo_if apt-get install -y "$TMP/$DEB"
}

install_windows() { # Windows (git-bash): build with Rust
    echo "-> Windows detected: building with cargo (needs a Rust toolchain)."
    echo "   No Rust yet? Get it: https://rustup.rs (then run this script again)."
    if ! command -v cargo >/dev/null 2>&1; then
        echo "   cargo not found — install Rust first: https://rustup.rs" >&2
        exit 1
    fi
    [ "${CLIMAX_DRY_RUN:-0}" = "1" ] && { echo "-> (dry-run) would run: cargo install --git https://github.com/$REPO.git"; return 0; }
    cargo install --git "https://github.com/$REPO.git" --tag "$VERSION"
}

echo "-> climax $VERSION · branch: $BRANCH"
case "$BRANCH" in
    arch) install_arch ;;
    deb) install_deb ;;
    mac) install_binary ;;
    windows) install_windows ;;
    linux) install_binary ;;
    *) echo "unsupported: $BRANCH" >&2; exit 1 ;;
esac

echo
echo "Try it:        climax -s"
echo "Background:    climax --install   (systemd user service, boot autorun)"
echo "Remove it:     climax --uninstall (service) · rm ~/.local/bin/climax (binary)"
