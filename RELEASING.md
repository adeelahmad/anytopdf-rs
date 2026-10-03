# Release verification

The 0.1.0 release scope is searchable PDFs from raster images, text/Markdown,
subtitles, FFmpeg video frames, and supplied audio transcripts, with optional OCR,
metadata providers and executable plugins. Future importers and analyzers remain
in `ROADMAP.md`. `AGENTS.md` was preserved byte for byte.

## Reproduce the checks

Use the pinned Rust 1.88.0 toolchain and the supplied `Cargo.lock`:

```bash
make ci SMOKE_FLAGS=--require-poppler
```

Verification checks formatting, compilation, Clippy with warnings denied, Rust
unit/integration tests with and without default features, and Python tests. The
smoke check requires Poppler (`pdfinfo`, `pdftotext`) and a Unicode system font;
it exercises text/image conversion, Unicode extraction, pagination,
font-subset size, graph output and overwrite protection.
Use `SMOKE_FLAGS="--require-poppler --strict"` when ExifTool is installed to
additionally require warning-free conversion. Minimal systems intentionally warn
about missing optional metadata. `make verify` runs just the formatting, lint and
test suite; `sh scripts/verify.sh` remains available without Make.

For provider checks, create a clearly readable image, an EXIF-rotated JPEG, a short
video with an SRT sidecar, audio with a text sidecar, and a `.example` text file.
Convert each with `--strict --dump-graph`, selecting `--ocr vision` or
`--ocr tesseract` for the image. Install the example plugin as an executable named
`anytopdf-plugin-example` on `ANYTOPDF_PLUGIN_PATH`. Inspect providers, timestamps,
source IDs and normalized OCR regions in the graphs. Extract text with `pdftotext`
and render every PDF page using `pdftoppm -png` for visual review.

## Validation evidence — 2026-10-04

- Rust 1.88.0 on Apple Silicon macOS: formatting, workspace compilation and Clippy
  passed; 39 Rust tests passed in each feature configuration, plus three Python tests.
- Actual text, image, video/caption, audio/transcript and runtime-plugin conversions
  passed strict mode and independent text extraction. Native Apple Vision and
  Tesseract both recognized the acceptance marker with normalized bounding boxes.
- All 12 acceptance pages were rendered and visually inspected: pagination, Unicode,
  long words, image rotation, video frames, transcript pages and invisible OCR.
- Font subsetting reduced the nine-page media sample from 15,204,473 bytes to about
  100 KB while preserving searchable text. Informational image diagnostics no
  longer cause strict conversions to fail.
- FFmpeg, ffprobe, ExifTool, Tesseract and Poppler were available. docTR was not
  installed and its model-dependent execution was not validated.

## Optimized binaries

| Target | Local execution verification |
| --- | --- |
| `aarch64-apple-darwin` | Native Apple Silicon; full media, Vision/Tesseract OCR, plugin and Poppler checks |
| `x86_64-apple-darwin` | Rosetta execution and provider checks |
| `aarch64-unknown-linux-musl` | Static executable in an ARM64 Alpine Linux guest; PDF, image, Unicode, pagination, graph and overwrite checks |
| `x86_64-unknown-linux-musl` | Static executable in an x86-64 Alpine Linux guest; the same checks |
| `x86_64-pc-windows-gnu` | Windows executable under Wine 11.0; PDF, image, Unicode, graph and overwrite checks |

The Windows GNU archive is compatibility-tested under Wine, not native Windows.
The hosted native Windows MSVC workflow is configured but has not been run here.

macOS binaries link only Apple system libraries/frameworks. Linux guests had no
ExifTool, FFmpeg or OCR providers installed: ordinary text/image conversion
succeeded with the expected optional-metadata warnings. Their PDFs were extracted
and checked independently with host Poppler. The guests used temporary in-memory
filesystems, with no Docker or host filesystem mounts.

Cross-builds were run from Bash with Rust 1.88.0, `cargo-zigbuild` 0.23.4 and Zig
0.16.0. Native builds used `cargo build --release --locked`; Linux used:

```bash
cargo zigbuild --release --locked -p anytopdf --no-default-features \
  --target aarch64-unknown-linux-musl --target x86_64-unknown-linux-musl
```


## Packaging

```bash
make package SMOKE_FLAGS=--require-poppler
# Or select an explicit target supported by the build host:
make package TARGET=aarch64-apple-darwin
```

Packages in `dist/` contain the executable, user/developer documentation, changelog
and MIT/Apache licenses. Each archive has a SHA-256 sidecar. Windows uses ZIP; other
targets use tar.gz. Verify checksums and run the executable extracted from its
archive before distributing it.

## GitHub CI/CD

`.github/workflows/ci.yml` runs `make ci` on Linux, macOS and Windows for branch
pushes, pull requests and manual runs. Rust dependencies/builds are cached. Both
feature configurations must pass, and Linux additionally requires Poppler checks.

`.github/workflows/release.yml` uses direct shell builds on native runners for
Apple Silicon/Intel macOS, ARM64/x86-64 Linux musl, and Windows x86-64 MSVC. Linux
uses `NO_DEFAULT_FEATURES=1`; Windows uses the static CRT flags in
`.cargo/config.toml`. Every target runs `make package`, including a smoke test.

To publish after committing the changes and configuring the repository remote:

1. Set the workspace version in `Cargo.toml`, refresh `Cargo.lock`, and update
   `CHANGELOG.md`. Commit these together.
2. Run `make ci SMOKE_FLAGS=--require-poppler` locally.
3. Create and push the matching tag, for example `git tag v0.1.0` followed by
   `git push origin v0.1.0`. A mismatched tag fails before building.
4. The workflow runs CI, builds all five archives, verifies their SHA-256 files,
   and creates a GitHub Release with archives, checksums and generated notes.

Tags containing a prerelease suffix are published as prereleases. Use a new
version/tag for a new release; publishing does not overwrite an existing release.
Only the publishing job has `contents: write`; it uses the built-in `GITHUB_TOKEN`.
Manual workflow dispatch builds downloadable artifacts without publishing, even
when a tag is selected. There is no Docker, signing, or crates.io publishing step.

This checkout has no Git remote, so hosted Actions execution and release publication
are not claimed by the local validation record. GitHub's
[release creation reference](https://cli.github.com/manual/gh_release_create)
describes the CLI used by the publishing job.

## Distribution scope

Signing/notarization and automatic updates are not configured. Optional OCR/media
providers and fonts remain runtime prerequisites for their respective features.
Complex-script shaping and bidirectional layout are not guaranteed. Runtime
plugins are trusted executables; time/output limits are not an OS sandbox.
