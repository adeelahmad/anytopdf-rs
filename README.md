# anytopdf

`anytopdf` is a pluggable media/document ingestion engine whose canonical output
is a searchable, RAG-friendly PDF.

It is deliberately not implemented as "a list of file extensions plus converters".
The architecture is closer to a driver framework:

```
sources
  -> discover/probe
  -> importer
  -> asset graph
  -> source enrichers
  -> unit enrichers
  -> page planner
  -> renderer
  -> searchable PDF
```

A source may produce any number of derived units. A video can produce keyframes,
audio segments, subtitle cues and metadata. An image generally produces one visual
unit. A transcript produces text units. Future importers can produce whatever
representation makes sense for formats such as `.igl`, CAD, email, Office,
archives, proprietary exports, or remote sources.

## Search model

Every fact becomes an `Annotation` with provenance:

- OCR text and bounding boxes
- captions / closed captions
- transcript text and time ranges
- file / EXIF / XMP / ffprobe metadata
- date/time and GPS/location metadata
- scene/keyframe changes
- object labels from an object-analysis plugin
- neutral face presence/count/bounds from a face-analysis plugin
- arbitrary future annotations

The PDF renderer paints the visual page normally and emits searchable annotations
using PDF text rendering mode 3 (invisible). The hidden layer carries content only
(OCR, captions, transcripts, objects, barcodes, time ranges); source paths and file
metadata are never written into it. Text/transcript units become normal
visible text pages.

The built-in project intentionally limits face enrichment to neutral facts such
as presence, count and bounds. It does **not** infer gender identity, emotion,
age or other sensitive/demographic traits from a face. The plugin model supports
adding other non-sensitive semantic analyzers without changing the core.

## Plugin model

There are two kinds of plugins.

### 1. Built-in Rust plugins

These implement Rust traits and are compiled into the executable. This is what
you use for fast, portable core functionality.

### 2. Runtime executable plugins

Executables named `anytopdf-plugin-*` are discovered from `PATH` and directories
in `ANYTOPDF_PLUGIN_PATH` (using the platform path separator). They speak the versioned JSON protocol documented in
`PLUGIN_PROTOCOL.md`.

This is the compatibility boundary for future/proprietary formats. A runtime
plugin can be written in Rust, Go, Python, Swift, C++, etc.; there is no Rust
dynamic-library ABI dependency.

A future `.igl` plugin can therefore be shipped independently:

```
anytopdf-plugin-igl
```

and register itself as an importer without modifying the core binary.

## Current built-ins

Importers:
- raster images
- video through FFmpeg
- audio container placeholder units
- text / Markdown
- SRT / VTT captions

Enrichment:
- ExifTool metadata
- ffprobe media metadata
- OCR provider chain:
  - Apple Vision on macOS when built with `apple-vision`
  - docTR through local Python when available
  - Tesseract CLI
- sidecar captions / transcripts
- video timestamps and scene-selection provenance

Rendering:
- searchable PDF via `printpdf`

External plugins are the intended route for model-heavy enrichers such as:
- YOLO / DETR object detection
- scene classification
- speech-to-text engines
- format-specific decoders
- proprietary document systems

## Quick start

Unpack the archive for your operating system and run `./anytopdf` (Windows:
`anytopdf.exe`). The executable needs no Rust or Python installation for text and
image conversion; provider-specific dependencies are listed below.

```bash
./anytopdf --version
./anytopdf notes.txt photo.jpg -o out.pdf
./anytopdf --no-plugins convert notes.txt --ocr off -o notes.pdf
./anytopdf doctor
```

Keep `Cargo.lock` when building from source. For video, install FFmpeg; for OCR,
use native Apple Vision on macOS or install Tesseract. Audio transcription requires
a supplied transcript or a plugin. PDF/Office/HTML importers are future work.

## CLI

```bash
anytopdf convert . -o archive.pdf
anytopdf convert photo.jpg meeting.mp4 transcript.srt -o searchable.pdf
anytopdf convert meeting.mp4 --transcript meeting.vtt -o meeting.pdf
anytopdf convert . --filter 'invoice|receipt' -o receipts.pdf

anytopdf doctor
anytopdf plugins
anytopdf probe some.igl
```

Video defaults combine interval sampling and FFmpeg scene-change sampling and
then perceptually deduplicate frames.

```bash
anytopdf convert meeting.mp4 \
  --video-interval 5 \
  --scene-threshold 0.30 \
  -o meeting.pdf
```

## Roadmap

anytopdf is meant to produce an evidence file: one PDF that is both the human
rendition and the machine index (embedded manifest, chunks, provenance and hashes),
works offline, and is ready for agents to read. `- [x]` is merged on the sprint 2
branch, `- [ ] (in progress, sprint 2)` is being built now, and `- [ ]` is planned.
Per-release detail is in [ROADMAP.md](ROADMAP.md).

### Searchable text and diagnostics
- [x] Content-only hidden text layer (no paths or metadata, no per-page duplication)
- [x] Typed diagnostics with stable codes and INFO/WARNING severities
- [x] `--strict` ignores missing optional providers
- [x] Provider version detection
- [x] Warning for multi-frame TIFF/GIF input (imports frame 1, warns "imported 1 of N frames")
- [ ] (in progress, sprint 2) Content-sniffed text importer (csv, json, log, code; lossy for non-UTF-8)
- [ ] (in progress, sprint 2) `--transcript` is never silently ignored

### CLI and automation
- [ ] (in progress, sprint 2) Simple form `anytopdf <inputs...> -o out.pdf`, no subcommand, `-o` anywhere
- [ ] (in progress, sprint 2) Automatic `<stem>.pdf` naming, numbered and never clobbering
- [ ] (in progress, sprint 2) `--output-dir` writes one PDF per input
- [ ] (in progress, sprint 2) Distinct exit codes
- [ ] (in progress, sprint 2) Batch continues past failed inputs by default, `--fail-fast` to stop
- [ ] (in progress, sprint 2) `--json` for convert, probe, doctor and plugins, with capabilities and published JSON Schemas
- [ ] (in progress, sprint 2) Help text on every flag
- [ ] NDJSON progress events

### Evidence file and provenance
- [x] Content-derived source and unit IDs with SHA-256 and size
- [ ] (in progress, sprint 2) Source anchors (time span, bounding box, byte range) and a page map
- [ ] (in progress, sprint 2) Embedded versioned manifest and chunks (or sidecar), plus `anytopdf extract --json`
- [ ] (in progress, sprint 2) Byte-reproducible output with `SOURCE_DATE_EPOCH` and recorded provider versions
- [ ] (in progress, sprint 2) Archive and share privacy profiles
- [ ] (in progress, sprint 2) Provenance page as the last page (`--no-provenance-page` to omit)
- [ ] Deterministic chunk IDs and semantic page/chunk headings
- [ ] Provenance graph export
- [ ] Incremental index mode
- [ ] PDF/A-3, tagged PDF and bookmarks

### Rendering
- [x] Spike: layout and writer options
- [ ] New renderer (parley layout, krilla writer, Rust 1.92 toolchain bump, invisible text via fill opacity)
- [ ] Rendered Markdown
- [ ] Arabic, Hebrew and CJK shaping

### Input formats
- [ ] PDF input (keep the text layer, OCR only textless pages)
- [ ] HTML and URL snapshot
- [ ] EML and mbox
- [ ] Archives (zip, tar)
- [ ] HEIC
- [ ] Office documents (structure first, LibreOffice when present)
- [ ] CAD, image stacks, IGL plugin and a generic command-adapter plugin

### Media enrichment
- [ ] Whisper transcription (pluggable whisper.cpp)
- [ ] Face presence, count and bounds
- [ ] Object detection and scene classification providers
- [ ] Barcode and QR extraction
- [ ] Audio chapters and speaker turns
- [ ] OCR-text-aware video frame retention

### Intake channels
- [ ] Webhooks (Standard Webhooks: job.received, job.completed, job.failed, HMAC signature, retries)
- [ ] Shared job queue with watched folder and HTTP upload inputs
- [ ] IMAP watcher (IDLE and polling, Paperless-ngx style rules, OAuth, DKIM/SPF sender allowlist, quarantine)
- [ ] Email-to-print

### Printing
- [x] Spike: PAPPL printer feasibility
- [ ] Network printer via IPP Everywhere, AirPrint and Mopria, built on PAPPL as an optional helper process
- [ ] IPP over TLS with a password, localhost by default, print receipts on the provenance page
- [ ] Remote printing over Tailscale or WireGuard with DNS-based discovery
- [ ] Microsoft Universal Print investigation

### Security and plugins
- [ ] Untrusted-input handling shipped with the intake channels: sandboxed conversion without network, size and page caps, zip-bomb rejection, per-sender budgets
- [ ] OS sandbox, descendant process containment and hard CPU, memory and disk quotas for runtime plugins

### Builds and distribution
- [x] Release build with LTO and strip (8.58 MB to 6.25 MB on macOS arm64)
- [x] Spike: slim and full build shapes
- [ ] Slim and full builds (full bundles LGPL decode-only ffmpeg, OCR models, Whisper base, Noto fonts)
- [ ] Homebrew, winget, scoop, cargo binstall, `curl | sh`, and npx/uvx wrappers
- [ ] Signing and notarization
- [ ] MCP server mode
- [ ] Agent skill and `llms.txt`

## Dependencies

Building from source:
- GNU Make and Bash to start the bootstrap (Git Bash on Windows).
- Python 3.11+ for build verification and release tooling.
- Rust 1.88.0 (pinned in `rust-toolchain.toml`); packaged binaries do not require Rust.
- `Cargo.lock` pins dependencies compatible with this toolchain.

Optional runtime providers:
- `ffmpeg` / `ffprobe`: video/audio demuxing and keyframes
- `exiftool`: rich metadata
- `tesseract`: OCR fallback
- Python + `doctr`: docTR OCR fallback

On macOS the `apple-vision` Cargo feature uses native Vision directly from Rust.

## Build

```bash
make
./target/release/anytopdf doctor
# Optional: install media/OCR/PDF inspection providers separately
make providers
```

Default `make` installs missing supported build tools, the pinned Rust toolchain,
rustfmt and Clippy, then builds the optimized CLI. Bootstrap uses Homebrew on
macOS, apt/dnf/pacman on Linux, or Chocolatey from Git Bash on Windows. Package
installation may need administrator access and network access. GNU Make and Bash
must already be available to start it. On macOS, finish the Apple Command Line
Tools installer if prompted and rerun. Windows needs Visual Studio C++ Build
Tools for MSVC; bootstrap does not install that compiler or Git Bash.

`make deps` prepares build tools only. `make providers` additionally installs
FFmpeg, ExifTool, Tesseract and Poppler (plus DejaVu fonts on Linux); it does not
install docTR models or perform audio transcription. Missing providers remain
optional for ordinary conversions. `make doctor` reports actual availability.

Python selection honors `PYTHON=/absolute/path/to/python` (quote paths containing
spaces), otherwise tries Python 3.11+ candidates. An invalid explicit override
fails instead of silently choosing another interpreter. Once tools are ready,
`cargo build --release --locked` remains available directly.

Linux fully-static (run on Linux with `musl-tools` installed):

```bash
rustup target add x86_64-unknown-linux-musl
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
  cargo build --release --locked -p anytopdf --no-default-features --target x86_64-unknown-linux-musl
```

Windows MSVC builds use static CRT flags from `.cargo/config.toml`.

macOS cannot fully statically link Apple system frameworks; the application
binary itself remains a single executable and uses the system Vision framework.

## Design invariants

1. Core orchestration never switches on individual file extensions.
2. Importers own format knowledge.
3. Enrichers append annotations; they do not rewrite unrelated data.
4. Every annotation records provider/provenance.
5. Temporary derived artifacts live inside one job workspace.
6. Renderer consumes only the normalized graph, never format-specific objects.
7. Plugin failures are isolated and reported; one failed optional enricher does
   not invalidate the entire job.
8. Runtime plugins use paths/JSON, not Rust ABI structs.
9. The PDF is the canonical portable artifact; the graph can also be serialized
   as JSON for debugging or future indexing.

## Reliability and operation

Running without a subcommand shows help. `probe FILE` reports the selected importer
without decoding media, extracting frames, or running enrichers.

`probe`, `doctor` and `plugins` accept `--json` and then write exactly one versioned JSON
document to stdout (diagnostics go to stderr); contracts live in `schemas/`.

```bash
anytopdf --no-plugins convert notes.txt --ocr off -o notes.pdf
anytopdf convert notes.txt -o notes.pdf --overwrite
anytopdf convert recordings/ --strict -o ../archive.pdf --dump-graph ../archive.json
anytopdf --plugin-timeout 30 --allow-plugin-kind importer plugins
anytopdf --deny-plugin-kind renderer convert document.example -o result.pdf
```

Existing outputs require `--overwrite`; input files and explicit transcripts are
protected even with that flag. Put outputs outside input directories so subsequent
directory scans do not ingest them. Writes are staged and atomically published.
PDF and JSON outputs are separate file transactions. `--strict` refuses to publish
when ingestion or rendering produces warnings; normal mode reports warnings and
keeps usable content. `--quiet` suppresses the success summary, not warnings.

### Exit codes

| Code | Class | Meaning |
|---|---|---|
| 0 | success | The PDF was published. |
| 1 | internal | Unexpected failure. |
| 2 | usage | Invalid option or value. |
| 3 | input | Missing, unreadable or unusable input. |
| 4 | provider | An explicitly requested provider (for example `--ocr tesseract`) is unavailable. |
| 5 | strict | `--strict` stopped on warnings before publishing. |
| 6 | render | Rendering failed; nothing was published. |
| 7 | fail-fast | `--fail-fast` stopped on a skipped input; nothing was published. |

Batch behaviour (request Amendment 1): by default a failing input (corrupt,
unsupported or unreadable) is skipped with a warning naming the file, the rest of
the batch continues, and the run exits 0 if a PDF was published. stderr ends with
`Summary: N converted, M skipped` and one `skipped <input>: [<code>] <reason>` line
per skipped input. `--fail-fast` aborts without publishing and exits 7; `--strict`
still exits 5 on any warning; if no input is usable the run exits 3.

### Diagnostics and strict mode

Each notice prints to stderr as `INFO [code]: message` or `WARNING [code]: message`.
Informational codes (`provider.missing`, `ocr.fallback`, `manifest.sidecar`) report
optional capabilities or fallbacks and never fail `--strict`. Every other code is a
warning (for example `input.unsupported`, `import.failed`, `provider.failed`,
`render.warning`) and makes `--strict` stop before publishing.

Plugin invocations default to a 60-second timeout. Captured stdout, stderr and
plugin response JSON each have a 16 MiB limit. Metadata providers have 30-second
timeouts, OCR subprocesses 180 seconds, subtitle extraction 120 seconds and each
video extraction pass 300 seconds. `--max-video-frames` also bounds extracted frames
per pass; `0` means no frame-count limit. These limits are safeguards, not an OS
sandbox: plugins run with your account's permissions. Install only trusted plugins
or use `--no-plugins`; see `PLUGIN_PROTOCOL.md` for policy details.

Images are decoded by content, normalized to PNG in the temporary workspace, and
rotated according to EXIF orientation. OCR coordinates refer to that normalized
image. GIF and multi-page TIFF inputs currently use their first frame/page.

Fonts are loaded from the system, subset to the required glyphs, and embedded in the PDF. Set `ANYTOPDF_FONT` to a
TTF file for a particular script or on minimal Linux installations. Missing glyphs
produce warnings. Complex text shaping and bidirectional layout are not guaranteed.
Markdown is rendered as plain text. Audio requires sidecar/explicit transcripts or
a plugin for speech recognition; docTR may download model weights on first use.

`--dump-graph` is a diagnostic sidecar, not a portable media bundle: visual paths
into the temporary workspace are removed. `--profile archive|share` (default
`archive`) selects metadata detail; `share` also strips local paths.
`--no-provenance-page` omits the provenance page. `SOURCE_DATE_EPOCH` fixes the
creation time for reproducible output; an invalid value exits 2.

## Development and release checks

Use GNU Make, Bash, the pinned Rust toolchain, and Python 3.11+:

```bash
make help
make verify
make ci SMOKE_FLAGS=--require-poppler
make package
```

`make ci` runs formatting, checks, Clippy, Rust tests in both feature configurations,
Python tests, a release build and PDF smoke checks. `make package` builds and
smoke-tests the CLI, then writes its archive and SHA-256 checksum to `dist/`.
Individual targets include `build`, `build-release`, `fmt`, `check`, `lint`, `test`,
`test-no-default`, `test-python` and `doctor`. `make clean` keeps release archives.
**`make release` publishes to GitHub**: it versions, verifies, commits, tags,
pushes and waits for publication. Use `make build-release` for a local binary,
`make release-plan` for a version/notes preview, and `make commit-check` to check
Conventional Commits. See [release and recovery procedures](RELEASING.md).

Select a build target with `TARGET=<triple>`; Linux musl builds also use
`NO_DEFAULT_FEATURES=1`. Set `PYTHON=python` on Windows (Git Bash and GNU Make are
required), `CARGO_TARGET_DIR=<path>` for a separate build cache, or `DIST_DIR=<path>`
for a separate archive directory. Verification runs on the host; cross-compiled
smoke/package targets need an executable that can run on that host.

The smoke test verifies PDF structure, graph output and overwrite protection. With
Poppler (`pdftotext`, `pdfinfo`), it also checks extracted Unicode text and pagination.
`SMOKE_FLAGS=--require-poppler` requires those tools; add `--strict` when ExifTool
and a Unicode-capable font are installed.

GitHub CI runs `make ci` on Linux, macOS and Windows for branch pushes and pull
requests. Full-history push checks enforce Conventional Commits; PR checks enforce
the title for squash merging. Dispatch/reusable calls without push/PR context do
not assume event fields. Pushing a `v*` tag matching the Cargo version and lockfile
runs the checks, packages five native targets, verifies the exact archive/checksum
inventory and publishes a GitHub Release using that changelog section. Manual
release runs upload workflow artifacts only. See `RELEASING.md` for the release
procedure and validation evidence. All builds use direct shell commands.
