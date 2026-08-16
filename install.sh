#!/bin/sh
# sinter installer — one static binary, no dependencies.
#
#   curl -fsSL https://raw.githubusercontent.com/shellfu/sinter/main/install.sh | sh
#
# Downloads the latest release binary for this platform, verifies its
# checksum, and installs to ~/.local/bin (override with SINTER_INSTALL_DIR).
# Nothing else is touched; uninstall = delete the binary.
set -eu

REPO="shellfu/sinter"
INSTALL_DIR="${SINTER_INSTALL_DIR:-$HOME/.local/bin}"
BASE="https://github.com/$REPO/releases/latest/download"

main() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)
            case "$arch" in
                x86_64)          target="x86_64-unknown-linux-musl" ;;
                aarch64 | arm64) target="aarch64-unknown-linux-musl" ;;
                *) die "unsupported architecture: $arch (build from source: cargo install --git https://github.com/$REPO sinter-cli)" ;;
            esac ;;
        Darwin)
            case "$arch" in
                x86_64)          target="x86_64-apple-darwin" ;;
                arm64 | aarch64) target="aarch64-apple-darwin" ;;
                *) die "unsupported architecture: $arch" ;;
            esac ;;
        *) die "unsupported OS: $os (Linux and macOS binaries are published; elsewhere: cargo install --git https://github.com/$REPO sinter-cli)" ;;
    esac

    asset="sinter-$target.tar.gz"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT

    say "downloading $asset ..."
    fetch "$BASE/$asset" "$tmp/$asset"
    fetch "$BASE/$asset.sha256" "$tmp/$asset.sha256"

    say "verifying checksum ..."
    (cd "$tmp" && checksum "$asset" "$asset.sha256") \
        || die "checksum mismatch — refusing to install"

    tar -xzf "$tmp/$asset" -C "$tmp"
    [ -f "$tmp/sinter" ] || die "archive did not contain a 'sinter' binary"

    mkdir -p "$INSTALL_DIR"
    install -m 755 "$tmp/sinter" "$INSTALL_DIR/sinter"

    say "installed $("$INSTALL_DIR/sinter" --version) -> $INSTALL_DIR/sinter"
    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *) say "note: $INSTALL_DIR is not on your PATH — add: export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
    esac
    say "next: cd your-repo && sinter init"
}

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
    else
        die "need curl or wget"
    fi
}

checksum() {
    # $1 = file, $2 = file containing "<sha256>  <name>"
    want="$(awk '{print $1}' "$2")"
    if command -v sha256sum >/dev/null 2>&1; then
        got="$(sha256sum "$1" | awk '{print $1}')"
    else
        got="$(shasum -a 256 "$1" | awk '{print $1}')"
    fi
    [ "$want" = "$got" ]
}

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

main "$@"
