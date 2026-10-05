# Changelog

## 0.2.0

Searchable-PDF fidelity, identity, and CLI contract release.

### Behaviour changes

- `extract` exits 3 (input) when a version-matched manifest or chunks file
  does not match its schema, naming the document and the first failing JSON path;
  other `schema_version` values still warn and exit 0.
- `--events` streams NDJSON progress events (`anytopdf.events/1`) to stderr; failing runs
  end with one failed `run.finished` and print no human `error:` line.
- The invisible text layer is content-only; provenance moves out of it (supersedes the
  earlier provenance-in-text-layer behaviour).
- A visible back-matter provenance page is added; disable it with `--no-provenance-page`.
- `--profile archive` and `--profile share` select output profiles.
- Sources carry content-derived IDs, SHA-256 and size.
- Multi-frame TIFF and GIF inputs import every frame as a page with a `frame` region anchor; `--max-image-frames N` caps the count (0 = unlimited) and warns `input.frames-not-imported`.
- HTML files (`.html`, `.htm`, `.xhtml`, or any file opening with an HTML doctype) import
  as one text page; scripts, styles and markup are dropped and the title is kept.
- Email files (`.eml`, `.mbox`, or header-sniffed) import as one text page per message
  (subject, sender, recipients, date, body; HTML-only bodies converted). Attachments are
  written into the job workspace under sanitized names and imported by whichever importer
  matches; ones that cannot be imported warn `input.members-not-imported`.
- Zip, tar and gzipped tar archives import as an index page plus each member through its
  own importer. Extraction is bounded (512 MiB per member, 1 GiB per archive, 10,000
  entries, 200:1 zip ratio, 2 GiB and 10,000 members per input across nesting); names
  are reduced to one safe component and links are skipped. Zip-based documents (OOXML,
  ODF, EPUB, JAR/APK) are not treated as archives.
- Importers can expand containers through `Importer::import_with_members`; nesting stops
  at `MAX_MEMBER_DEPTH` (4).
- Units carry anchors (time span, region, byte range) and page ranges (`unit_pages`).
- Output is reproducible; `SOURCE_DATE_EPOCH` fixes embedded timestamps.
- The graph dump no longer contains workspace paths.
- Diagnostics use typed codes (for example `plugin.warning`).
- `--strict` ignores informational notices.
- Exit codes 0-7 classify failures; batch conversion tolerates failures unless `--fail-fast`.
- `convert` is implicit, `-o` may appear anywhere, and default output naming is derived from the input.
- `--json` output with published schemas, and a `capabilities` command.
- `capabilities` without `--json` prints a table of available, partial and missing
  capabilities in this environment with hints to enable them; use `--json` for the JSON document.
- The PDF embeds a manifest and chunks; `extract` recovers attachments.
- Input fidelity: video frames are validated, file types are sniffed, lossy decoding is
  reported, and `--transcript` supplies audio transcripts.
- `--output-dir` selects the batch output directory.
- Updated help text.
- Release builds enable LTO and strip symbols.
- `--renderer pdfa` writes PDF/A-3b through krilla: embedded fonts, XMP metadata, an
  sRGB output intent, and the manifest and chunks as associated files. The hidden
  layer uses fill opacity 0 instead of text rendering mode 3. `extract` now reads
  compressed attachments.

## 0.1.0

Initial searchable-media release: raster images, plain text/Markdown, SRT/VTT,
FFmpeg video frames, audio transcript pages, native Apple Vision, Tesseract/docTR
adapters, metadata enrichment, and executable plugins.

### Reliability fixes

- Pin Rust-compatible dependencies in `Cargo.lock`; repair Apple Vision features
  and bindings and the PDF image-reader import.
- Normalize images by content and EXIF orientation before rendering or OCR.
- Preserve complete transcripts, numeric subtitle dialogue, and caption timing;
  avoid duplicate sidecars and incorrect source attribution.
- Detect audio-only media containers and reject failed or invalid frame extraction.
- Subset and embed Unicode fonts, paginate measured text, wrap long words, and isolate the
  invisible search layer from visible page graphics.
- Keep informational PDF diagnostics out of warnings so strict image conversions succeed.
- Stage PDF output, protect sources and existing destinations, and support strict
  conversion that refuses to publish on warnings.
- Validate graph identities, provenance, coordinates, and runtime-plugin paths;
  roll back failed enrichment and bound subprocess time and captured output.
- Add plugin opt-out and capability filters, a working example importer, regression
  tests, independent PDF smoke checks, and release archives with SHA-256 checksums.

### Current scope

Markdown remains plain text. GIF/TIFF import uses the first frame/page. Audio needs
a transcript or speech-recognition plugin. PDF/Office/HTML import, complex-script
layout, OS-level plugin sandboxing, signing/notarization, and automatic updates are
future work; see `ROADMAP.md` and `RELEASING.md`.

### Commit history

#### Features

- establish searchable media PDF converter and release tooling (a7096d09)

#### Fixes

- bootstrap: verify prerequisites before reporting readiness (a49898dd)
- release: validate workspace lock versions before publication (0bdf0f7a)
- release: validate published assets and recover interrupted releases (7993aec1)

#### Maintenance

- enforce release policy and verify publication inventory (775e9472)
- resolve Bash explicitly for Windows workflow fixtures (99785443)

