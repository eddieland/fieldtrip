#!/bin/sh
# Install fieldtrip from a GitHub release.
#
#   curl -fsSL https://github.com/eddieland/fieldtrip/releases/latest/download/install.sh | sh
#
# Environment:
#   FIELDTRIP_VERSION      Version to install, e.g. 0.1.0. Defaults to the latest release.
#   FIELDTRIP_INSTALL_DIR  Where to put the binary. Defaults to ~/.local/bin.
#
# Supports Linux (x86_64, aarch64) and macOS (Intel, Apple silicon). On Windows, use install.ps1.

set -eu

REPO="eddieland/fieldtrip"

say() {
    printf 'fieldtrip-install: %s\n' "$1" >&2
}

fail() {
    say "error: $1"
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || fail "this script needs '$1', which was not found"
}

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux) os_part="unknown-linux-musl" ;;
        Darwin)
            os_part="apple-darwin"
            # A shell under Rosetta reports x86_64 on Apple silicon; prefer the native build.
            if [ "$arch" = "x86_64" ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = "1" ]; then
                arch="arm64"
            fi
            ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            fail "on Windows, run in PowerShell: irm https://github.com/$REPO/releases/latest/download/install.ps1 | iex"
            ;;
        *) fail "unsupported operating system: $os" ;;
    esac

    case "$arch" in
        x86_64 | amd64) arch_part="x86_64" ;;
        aarch64 | arm64) arch_part="aarch64" ;;
        *) fail "unsupported architecture: $arch" ;;
    esac

    printf '%s-%s' "$arch_part" "$os_part"
}

download() {
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2" || fail "could not download $1; check that the release exists"
    elif command -v wget >/dev/null 2>&1; then
        wget --https-only -q "$1" -O "$2" || fail "could not download $1; check that the release exists"
    else
        fail "this script needs curl or wget"
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        fail "this script needs sha256sum or shasum to verify the download"
    fi
}

main() {
    need uname
    need tar
    need mkdir
    need mktemp

    target="$(detect_target)"
    archive="fieldtrip-$target.tar.gz"

    version="${FIELDTRIP_VERSION:-}"
    if [ -n "$version" ]; then
        base="https://github.com/$REPO/releases/download/v${version#v}"
    else
        base="https://github.com/$REPO/releases/latest/download"
    fi

    install_dir="${FIELDTRIP_INSTALL_DIR:-$HOME/.local/bin}"

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' INT TERM

    say "downloading $archive (${version:-latest})"
    download "$base/$archive" "$tmp/$archive"
    download "$base/SHA256SUMS" "$tmp/SHA256SUMS"

    expected="$(grep " $archive\$" "$tmp/SHA256SUMS" | cut -d ' ' -f 1)"
    [ -n "$expected" ] || fail "no checksum for $archive in SHA256SUMS"
    actual="$(sha256 "$tmp/$archive")"
    [ "$expected" = "$actual" ] || fail "checksum mismatch for $archive: expected $expected, got $actual"

    tar -xzf "$tmp/$archive" -C "$tmp"
    mkdir -p "$install_dir"
    # Copy to a temporary name and rename, so a running fieldtrip is replaced rather than overwritten in place.
    cp "$tmp/fieldtrip-$target/fieldtrip" "$install_dir/.fieldtrip.tmp"
    chmod 755 "$install_dir/.fieldtrip.tmp"
    mv -f "$install_dir/.fieldtrip.tmp" "$install_dir/fieldtrip"

    say "installed $("$install_dir/fieldtrip" --version) to $install_dir/fieldtrip"

    case ":$PATH:" in
        *":$install_dir:"*) ;;
        *) say "$install_dir is not on your PATH; add it, or use the full path in your MCP client config" ;;
    esac
}

# Everything runs from main, so a download cut off midway through this script runs nothing.
main "$@"
