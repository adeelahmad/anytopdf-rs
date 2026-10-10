# Distribution

Every channel below installs the same archives the release workflow builds,
smoke-tests and checksums for the five targets. Nothing is published to a
registry automatically except the GitHub release itself and, for tag pushes,
the container image on GHCR.

| Channel | Source | What the release workflow does |
| --- | --- | --- |
| GitHub release | `make package` archives + `SHA256SUMS` | Publishes them on a `v*` tag |
| Homebrew (macOS, Linux) | `scripts/distribution.py` → `anytopdf.rb` | Renders the formula from `SHA256SUMS`, uploads it as the `package-manifests` workflow artifact |
| Scoop (Windows) | `scripts/distribution.py` → `anytopdf.json` | Same artifact as the formula |
| cargo-binstall | `[package.metadata.binstall]` in `crates/anytopdf-cli/Cargo.toml` | Nothing; binstall reads the release archives directly |
| cargo install | Source build | Nothing |
| Container | `Dockerfile` | Builds `linux/amd64` and `linux/arm64` from the musl release binaries; pushes `ghcr.io/<owner>/<repo>:<version>` (and `latest` for non-prereleases) on a `v*` tag, builds only on manual runs |

## Bundled plugins

Every archive carries the workspace's runtime plugins (`PLUGINS` in the
Makefile, currently `anytopdf-plugin-audio-events`, `anytopdf-plugin-clip`,
`anytopdf-plugin-face-id`, `anytopdf-plugin-faces`, `anytopdf-plugin-objects`,
`anytopdf-plugin-sentiment`, `anytopdf-plugin-tika`, `anytopdf-plugin-vlm` and `anytopdf-plugin-whisper`) in a
`plugins/` folder. anytopdf
finds them there (and in Homebrew's `libexec/plugins` and `<data dir>/plugins`)
without `ANYTOPDF_PLUGIN_PATH`, but runs a bundled plugin only once its manifest
reports `ready: true`: an enabled Whisper plugin without an engine would warn on
every audio or video conversion and fail `--strict` runs. `make package` checks
that each plugin answers `--anytopdf-manifest` on the build runner before
archiving.

| Channel | Plugin location | Turned on by |
| --- | --- | --- |
| Archive | `<archive>/plugins/` | found automatically; `anytopdf setup whisper` |
| `install.sh` | `<data dir>/plugins` | found automatically; `anytopdf setup whisper` |
| Homebrew | `$(brew --prefix anytopdf)/libexec/plugins` | found automatically; `anytopdf setup whisper` |
| Scoop | `$(scoop prefix anytopdf)\plugins` | found automatically; `anytopdf setup whisper` |
| Container | `/opt/anytopdf/plugins` | `-e ANYTOPDF_PLUGIN_PATH=/opt/anytopdf/plugins`, or the `WHISPER=cpp` build |
| cargo-binstall | not installed | `cargo install --git … anytopdf-plugin-whisper` |

Whisper also needs an engine and a model: `brew install whisper-cpp` (or
whisper.cpp's Windows release zip) plus `anytopdf setup whisper`, which
downloads a checksummed ggml model into the user data folder. The container's
`WHISPER=cpp` build includes the engine; mount a model as described below.
`anytopdf doctor` lists whatever is still missing.

The Homebrew caveats and Scoop notes point at `anytopdf setup whisper` and
`anytopdf capabilities`; setting `ANYTOPDF_PLUGIN_PATH` to the plugins folder still
runs every bundled plugin regardless of readiness.

## Homebrew tap

This repository is its own tap: `Formula/anytopdf.rb` is the formula for the
latest release. Because the repository is not named `homebrew-*`, users tap it
by URL once:

```bash
brew tap adeelahmad/anytopdf https://github.com/adeelahmad/anytopdf-rs
brew install adeelahmad/anytopdf/anytopdf
brew install ffmpeg exiftool tesseract   # optional providers
```

The macOS archives keep the Apple Vision OCR feature; the Linux formula installs
the static musl binary.

## Scoop bucket

This repository is also the Scoop bucket: `bucket/anytopdf.json` is the manifest
for the latest release.

```powershell
scoop bucket add anytopdf https://github.com/adeelahmad/anytopdf-rs
scoop install anytopdf/anytopdf
```

The manifest carries `checkver` and `autoupdate`, so Scoop's `checkver -u` can
also bump it from the release's `.sha256` sidecar.

## Updating the formula and manifest after a release

The release workflow renders both files from the published `SHA256SUMS` and
uploads them as the `package-manifests` artifact. Commit them in a PR after each
release (or regenerate them from the published checksums):

```bash
gh release download v0.5.0 --pattern SHA256SUMS
python scripts/distribution.py --version 0.5.0 --checksums SHA256SUMS --output packaging
cp packaging/anytopdf.rb Formula/anytopdf.rb
cp packaging/anytopdf.json bucket/anytopdf.json
```

`tests/test_distribution.py` fails if either file is edited by hand or the two
disagree on the version.

## cargo-binstall and cargo install

`cargo binstall` downloads the release archive for the host target (Linux hosts
fall back to the musl archive) instead of compiling:

```bash
cargo binstall --git https://github.com/adeelahmad/anytopdf-rs anytopdf
```

Building from source needs the pinned toolchain from `rust-toolchain.toml`:

```bash
cargo install --locked --git https://github.com/adeelahmad/anytopdf-rs anytopdf
```

The crates are not on crates.io yet; publishing would need every workspace crate
published in dependency order, which is a separate decision.

## Container

The image bundles FFmpeg, ExifTool, Tesseract (English) and DejaVu fonts on
Debian bookworm and runs as an unprivileged user in `/work`:

```bash
docker run --rm -v "$PWD:/work" ghcr.io/adeelahmad/anytopdf-rs notes.txt photo.jpg -o out.pdf
docker run --rm -i -v "$PWD:/work" ghcr.io/adeelahmad/anytopdf-rs mcp
```

`docker build -t anytopdf .` builds from source with the pinned toolchain.
`docker build --build-arg WHISPER=cpp -t anytopdf:whisper .` also compiles
whisper.cpp's `whisper-cli` (tag `WHISPER_CPP_REF`, default `v1.9.4`) and puts
the Whisper plugin on `PATH`; mount a ggml model at `/models/ggml-base.en.bin` or
set `ANYTOPDF_WHISPER_MODEL`. The release image does not include whisper.cpp:
compiling it for arm64 under QEMU would dominate the release job, and the model
has to be supplied separately either way. The release workflow passes
`--build-arg BINARY=prebuilt` with the musl binaries staged in
`dist/docker/<arch>/`, so the image ships the exact binaries that were
smoke-tested and checksummed. The image is published as a private GHCR
package by default; make it public in the package settings once.

## MCP server

`anytopdf mcp` is part of every channel above; see the README's "MCP server"
section for client configuration. With the container, register
`docker run --rm -i -v <folder>:/work ghcr.io/adeelahmad/anytopdf-rs mcp` as the
server command and give the agent paths under `/work`.
