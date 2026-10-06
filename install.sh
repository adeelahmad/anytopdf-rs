#!/bin/sh
# Install the latest anytopdf release on macOS or Linux:
#
#   curl -fsSL https://raw.githubusercontent.com/adeelahmad/anytopdf-rs/main/install.sh | sh
#
# Environment overrides:
#   ANYTOPDF_VERSION       release to install, e.g. 0.2.0 (default: latest)
#   ANYTOPDF_INSTALL_DIR   where the binary goes (default: $HOME/.local/bin)
#   ANYTOPDF_PLUGIN_DIR    where bundled plugins go (default: the plugins folder in
#                          anytopdf's data folder, which anytopdf searches by itself:
#                          ~/Library/Application Support/anytopdf/plugins on macOS,
#                          ${XDG_DATA_HOME:-~/.local/share}/anytopdf/plugins elsewhere)
#   ANYTOPDF_TARGET        Rust target triple to download (default: detected)
#   ANYTOPDF_DOWNLOAD_URL  base URL holding v<version>/<archive> (default: GitHub releases)
set -eu

REPO="adeelahmad/anytopdf-rs"
BASE_URL="${ANYTOPDF_DOWNLOAD_URL:-https://github.com/$REPO/releases/download}"
INSTALL_DIR="${ANYTOPDF_INSTALL_DIR:-$HOME/.local/bin}"
# Mirrors anytopdf's user_data_dir(), so the binary finds these plugins unaided.
if [ -n "${ANYTOPDF_DATA_DIR:-}" ]; then
    DATA_DIR="$ANYTOPDF_DATA_DIR"
elif [ "$(uname -s)" = Darwin ]; then
    DATA_DIR="$HOME/Library/Application Support/anytopdf"
else
    case "${XDG_DATA_HOME:-}" in
        /*) DATA_DIR="$XDG_DATA_HOME/anytopdf" ;;
        *) DATA_DIR="$HOME/.local/share/anytopdf" ;;
    esac
fi
PLUGIN_DIR="${ANYTOPDF_PLUGIN_DIR:-$DATA_DIR/plugins}"

say() { printf 'anytopdf-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

fetch() { # fetch <url> <file>
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https,file' -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        die "curl or wget is required"
    fi
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$arch" in
        x86_64 | amd64) arch=x86_64 ;;
        arm64 | aarch64) arch=aarch64 ;;
        *) die "unsupported CPU architecture: $arch" ;;
    esac
    case "$os" in
        Linux) echo "$arch-unknown-linux-musl" ;;
        Darwin) echo "$arch-apple-darwin" ;;
        *) die "unsupported OS: $os (on Windows use: scoop bucket add anytopdf https://github.com/$REPO)" ;;
    esac
}

latest_version() {
    command -v curl >/dev/null 2>&1 || die "set ANYTOPDF_VERSION, or install curl to look up the latest release"
    url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
        || die "could not look up the latest release"
    tag=${url##*/}
    case "$tag" in
        v[0-9]*) echo "${tag#v}" ;;
        *) die "no published release found at https://github.com/$REPO/releases" ;;
    esac
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "sha256sum or shasum is required to verify the download"
    fi
}

target="${ANYTOPDF_TARGET:-$(detect_target)}"
version="${ANYTOPDF_VERSION:-$(latest_version)}"
version=${version#v}
name="anytopdf-$version-$target"
archive="$name.tar.gz"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $archive"
fetch "$BASE_URL/v$version/$archive" "$tmp/$archive" || die "download failed: $BASE_URL/v$version/$archive"
fetch "$BASE_URL/v$version/$archive.sha256" "$tmp/$archive.sha256" || die "checksum download failed"

expected=$(cut -d' ' -f1 "$tmp/$archive.sha256")
actual=$(sha256_of "$tmp/$archive")
[ "$expected" = "$actual" ] || die "checksum mismatch for $archive (expected $expected, got $actual)"

tar -xzf "$tmp/$archive" -C "$tmp"
[ -f "$tmp/$name/anytopdf" ] || die "archive does not contain $name/anytopdf"

mkdir -p "$INSTALL_DIR"
cp "$tmp/$name/anytopdf" "$INSTALL_DIR/anytopdf.new"
chmod 755 "$INSTALL_DIR/anytopdf.new"
mv -f "$INSTALL_DIR/anytopdf.new" "$INSTALL_DIR/anytopdf"
say "installed $INSTALL_DIR/anytopdf ($version, $target)"

if [ -d "$tmp/$name/plugins" ]; then
    mkdir -p "$PLUGIN_DIR"
    cp "$tmp/$name/plugins/"* "$PLUGIN_DIR/"
    say "bundled plugins are in $PLUGIN_DIR"
    say "transcription: run \`anytopdf setup whisper\` (anytopdf 0.2.0 needs ANYTOPDF_PLUGIN_PATH=\"$PLUGIN_DIR\" instead)"
fi

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) say "add $INSTALL_DIR to PATH, e.g. export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
say "next: anytopdf doctor"
