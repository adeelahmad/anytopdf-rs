#!/usr/bin/env bash
# Install missing tools using native package managers and the pinned Rust toolchain.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH:/opt/homebrew/bin:/usr/local/bin"
mode=${1:-build}
case "$mode" in build|providers|release) ;; *) echo "Unknown bootstrap mode: $mode" >&2; exit 1;; esac
platform=$(uname -s)

as_root() {
    if [[ $(id -u) == 0 ]]; then "$@"; else sudo "$@"; fi
}
install_packages() {
    # Arguments are package names for brew, apt, dnf, pacman and Chocolatey.
    case "$platform" in
        Darwin)
            if ! command -v brew >/dev/null; then
                installer=$(mktemp)
                curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh -o "$installer"
                NONINTERACTIVE=1 /bin/bash "$installer"
                rm -f "$installer"
            fi
            read -r -a packages <<< "$1"
            brew install "${packages[@]}"
            ;;
        Linux)
            if command -v apt-get >/dev/null; then
                as_root apt-get update
                read -r -a packages <<< "$2"
                as_root apt-get install -y "${packages[@]}"
            elif command -v dnf >/dev/null; then
                read -r -a packages <<< "$3"
                as_root dnf install -y "${packages[@]}"
            elif command -v pacman >/dev/null; then
                read -r -a packages <<< "$4"
                as_root pacman -S --needed --noconfirm "${packages[@]}"
            else
                echo 'Install the required tools with your package manager (supported: apt, dnf, pacman).' >&2
                exit 1
            fi
            ;;
        MINGW*|MSYS*|CYGWIN*)
            if ! command -v choco >/dev/null; then
                echo 'Use Git Bash with Chocolatey, or install Python 3.11+, Git, Rustup and Visual Studio C++ Build Tools first.' >&2
                exit 1
            fi
            read -r -a packages <<< "$5"
            choco install -y --no-progress "${packages[@]}"
            ;;
        *) echo "Unsupported bootstrap platform: $platform" >&2; exit 1;;
    esac
    hash -r
}

if [[ "$platform" == Darwin ]] && ! xcode-select -p >/dev/null 2>&1; then
    xcode-select --install
    echo 'Finish the Apple Command Line Tools installer, then rerun make.' >&2
    exit 1
fi
if [[ "$platform" == Linux ]] && ! command -v cc >/dev/null; then
    install_packages '' 'build-essential' 'gcc gcc-c++ make' 'base-devel' ''
fi
command -v curl >/dev/null || install_packages curl 'curl ca-certificates' 'curl ca-certificates' 'curl ca-certificates' curl
command -v git >/dev/null || install_packages git git git git git
if ! bash scripts/python.sh >/dev/null 2>&1; then
    if [[ -n "${PYTHON:-}" ]]; then
        bash scripts/python.sh # Explicit overrides must not silently select another interpreter.
    fi
    install_packages python python3 python3 python python
fi
bash scripts/python.sh >/dev/null
if ! command -v rustup >/dev/null; then
    installer=$(mktemp)
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o "$installer"
    sh "$installer" -y --profile minimal --default-toolchain none --no-modify-path
    rm -f "$installer"
fi
toolchain=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml)
if ! rustup run "$toolchain" rustc --version >/dev/null 2>&1; then
    rustup toolchain install "$toolchain" --profile minimal --component rustfmt --component clippy
fi
for component in rustfmt clippy; do
    if ! rustup component list --toolchain "$toolchain" --installed | grep -q "^$component-"; then
        rustup component add --toolchain "$toolchain" "$component"
    fi
done
if [[ -n "${TARGET:-}" ]]; then
    rustup target add --toolchain "$toolchain" "$TARGET"
    if [[ "$platform" == Linux && "$TARGET" == *-unknown-linux-musl ]] && ! command -v musl-gcc >/dev/null; then
        install_packages '' musl-tools musl-gcc musl ''
    fi
fi
if [[ "$mode" == providers ]]; then
    for provider in ffmpeg exiftool tesseract pdftotext; do
        if ! command -v "$provider" >/dev/null; then
            case "$provider" in
                ffmpeg) install_packages ffmpeg ffmpeg ffmpeg ffmpeg ffmpeg;;
                exiftool) install_packages exiftool libimage-exiftool-perl perl-Image-ExifTool perl-image-exiftool exiftool;;
                tesseract) install_packages tesseract tesseract-ocr tesseract tesseract tesseract;;
                pdftotext) install_packages poppler poppler-utils poppler-utils poppler poppler;;
            esac
        fi
    done
    if [[ "$platform" == Linux ]]; then
        install_packages '' fonts-dejavu-core dejavu-sans-fonts ttf-dejavu ''
    fi
fi
if [[ "$mode" == release ]] && ! command -v gh >/dev/null; then
    install_packages gh gh gh github-cli gh
fi
printf 'Dependencies ready: Rust %s, Python 3.11+, native tools (%s).\n' "$toolchain" "$mode"
