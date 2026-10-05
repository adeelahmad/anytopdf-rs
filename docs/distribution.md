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

## Homebrew tap

One-time setup: create a `homebrew-tap` repository next to this one.

After each release, download the `package-manifests` artifact from the release
workflow run (or regenerate it from the published checksums) and commit the
formula to the tap:

```bash
gh release download v0.2.0 --pattern SHA256SUMS
python scripts/distribution.py --version 0.2.0 --checksums SHA256SUMS --output packaging
cp packaging/anytopdf.rb ../homebrew-tap/Formula/anytopdf.rb
```

Users then install with:

```bash
brew install adeelahmad/tap/anytopdf
brew install ffmpeg exiftool tesseract   # optional providers
```

The macOS archives keep the Apple Vision OCR feature; the Linux formula installs
the static musl binary.

## Scoop bucket

One-time setup: create a `scoop-bucket` repository. Copy `packaging/anytopdf.json`
to `bucket/anytopdf.json` there after each release. The manifest carries
`checkver` and `autoupdate`, so Scoop's `checkver -u` can also bump it from the
release's `.sha256` sidecar.

```powershell
scoop bucket add anytopdf https://github.com/adeelahmad/scoop-bucket
scoop install anytopdf
```

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

`docker build -t anytopdf .` builds from source with the pinned toolchain. The
release workflow passes `--build-arg BINARY=prebuilt` with the musl binaries
staged in `dist/docker/<arch>/anytopdf`, so the image ships the exact binary that
was smoke-tested and checksummed. The image is published as a private GHCR
package by default; make it public in the package settings once.

## MCP server

`anytopdf mcp` is part of every channel above; see the README's "MCP server"
section for client configuration. With the container, register
`docker run --rm -i -v <folder>:/work ghcr.io/adeelahmad/anytopdf-rs mcp` as the
server command and give the agent paths under `/work`.
