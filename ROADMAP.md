# Roadmap

Checked items describe implemented code on `main`, not release certification. See
`RELEASING.md` for validation evidence and release procedures, `CHANGELOG.md` for
per-release behaviour changes, and the README roadmap for the feature-level view.

Status: sprint 4 closed on 2026-10-05; the work listed under "After sprint 4" merged the same day and shipped in 0.2.0. The work under "Media analysis and more inputs" merged on 2026-10-06 and shipped in 0.3.0. Word boxes in chunks, `--html-render` and the 0.3.0 test fixes ship in 0.4.0.

## 0.1 — architecture + searchable media core
- [x] staged registry
- [x] normalized asset/annotation graph
- [x] image importer
- [x] video keyframes / scene changes / perceptual dedupe
- [x] audio unit
- [x] text/Markdown
- [x] SRT/VTT
- [x] ExifTool / ffprobe metadata
- [x] Vision -> docTR -> Tesseract OCR abstraction
- [x] invisible searchable PDF annotation layer
- [x] graph JSON dump
- [x] external plugin wire protocol
- [x] static Linux / static CRT Windows release shape

## Release readiness
- [x] atomic output publication and overwrite protection
- [x] subset embedded fonts and keep informational diagnostics out of strict-mode warnings
- [x] failed-enricher rollback and graph validation
- [x] regression tests and media/provider smoke checks
- [x] CI and archive/checksum packaging scripts
- [x] resolve and retain Cargo.lock with Rust 1.88 compatible dependencies
- [x] full workspace checks with default and no-default features
- [x] independently inspect generated PDF appearance and searchable Unicode text
- [x] Apple Silicon/Intel macOS and ARM64/x86-64 Linux release execution
- [x] Windows GNU cross-build and Wine execution smoke test
- [x] hosted native Windows MSVC release matrix verification
- [x] hosted CI on Linux, macOS and Windows; five release targets

## 0.2 — runtime plugin host, evidence file and CLI contract
- [x] PATH / `ANYTOPDF_PLUGIN_PATH` executable discovery
- [x] runtime importer adapter
- [x] runtime source/graph/unit enricher adapters
- [x] plugin timeouts and bounded captured output/response size
- [x] capability allow/deny registration policy and discovery opt-out
- [x] content-only invisible text layer; visible provenance page
- [x] multi-frame TIFF/GIF import with `--max-image-frames`
- [x] content-derived source/unit IDs with SHA-256 and size; source anchors and page map
- [x] embedded versioned manifest and chunks (sidecar fallback) and `anytopdf extract --json`
- [x] `extract` rejects schema-invalid manifest/chunks with exit 3
- [x] archive and share privacy profiles
- [x] reproducible output with `SOURCE_DATE_EPOCH`
- [x] typed diagnostics, `--strict`, exit codes 0-7, `--fail-fast`, `--output-dir`
- [x] `--json` output with published schemas, `capabilities`, `doctor`
- [x] NDJSON `--events` progress stream
- [x] sprint 4 cleanup: shared test helpers, portable census paths on Windows

## After sprint 4 — intake, more inputs and outputs (merged 2026-10-05)
- [x] HTML importer: readable text, title and alt text, no network fetches
- [x] email importer (`.eml`, `.mbox`): one text page per message; attachments imported by their own importers, nested at most 4 deep
- [x] Office importer (Word, Excel, PowerPoint, OpenDocument, RTF) through headless LibreOffice and Poppler
- [x] `capabilities` table of available, partial and missing capabilities with hints
- [x] `anytopdf queue`: folder-backed job queue with a watched inbox and signed Standard Webhooks (`job.received`, `job.completed`, `job.failed`) retried from a durable outbox
- [x] `anytopdf queue serve`: HTTP upload intake with a bearer token, localhost by default, TLS for any other address, 100 MB default upload cap
- [x] archive importer (zip, tar, tar.gz) with path-traversal and zip-bomb limits
- [x] Whisper transcription runtime plugin
- [x] Audio events runtime plugin (speech/music/silence, raised voices; laughter, applause and other sounds with an AudioSet ONNX model)
- [x] `anytopdf watch imap`: IMAP watcher with IDLE or polling, a sender allowlist with a DMARC check, XOAUTH2 and queue hand-off
- [x] `--plugin-sandbox off|contain|strict`: opt-in process containment for runtime plugins on every platform, plus a filesystem and network sandbox on Linux and macOS
- [x] PDF importer (Poppler page images plus the PDF's own text as the hidden layer; OCR only for textless pages) and HEIC/HEIF/AVIF importer
- [x] `helpers/anytopdf-printer`: optional IPP Everywhere printer built on PAPPL, with a PWG/Apple raster importer
- [x] `anytopdf print remote`: TLS print front for Tailscale/WireGuard with user passwords, a peer allowlist and DNS-SD discovery
- [x] `--draw-boxes`: labelled object, face and OCR boxes over visual pages in both renderers
- [x] krilla renderer, now the default: tagged PDF/A-3a with bookmarks, bidi shaping and font fallback, manifest and chunks as associated files
- [x] `anytopdf mcp`: MCP server over stdio exposing convert, extract, probe and capabilities
- [x] Homebrew formula, Scoop manifest, cargo-binstall metadata and a container image
- [x] Rust toolchain 1.92.0
- [x] URL, email, domain, app-name and date/time entities from OCR, captions, transcripts and text
- [ ] people, organisation and keyword entities (NER runtime plugin)
- [x] cross-file search index: `convert --index`, `anytopdf index add|list|remove`, `anytopdf search` (kind, person and collection filters), an MCP `search` tool and `queue serve --search`

## Media analysis and more inputs (merged 2026-10-06, 0.3.0)
- [x] JSON and JSON Lines importer: one searchable chunk per record
- [x] WhatsApp, Telegram, Slack and iMessage chat exports
- [x] camera RAW photos and phone-photo page flattening (`--scan-mode`)
- [x] URL inputs, link lists and browser bookmark exports (`--links`)
- [x] dominant colours per image and keyframe (`--colors`)
- [x] offline reverse geocoding and place names in text (`--location`)
- [x] `anytopdf ask`: cited answers from converted PDFs, optionally through a local LLM
- [x] `anytopdf capture screen`: record the screen and convert the recording
- [x] one definition per option: TOML config file, `ANYTOPDF_*` variables and flags; `anytopdf config`
- [x] `anytopdf setup whisper` and bundled plugins that turn on once their manifest reports `ready`
- [x] `install.sh` installer and Homebrew tap and Scoop bucket served from this repository
- [x] sentiment and tone of text (`anytopdf-plugin-sentiment`) and audio events (`anytopdf-plugin-audio-events`)
- [x] every workspace runtime plugin bundled in release archives, packages and the container

## Testing 0.3.0 (merged 2026-10-07, 0.4.0)
- [x] per-word boxes in chunks and in PDF/Office text layers
- [x] `--html-render`: local HTML printed offline by headless Chrome into image pages
- [x] YOLOX object models; image enrichment for email attachments and archive members
- [x] face-id feeds raw pixels to SFace and ArcFace R100; borderline matches no longer enroll
- [x] one Whisper setup command per system; Linux arm64 smoke test in CI

## Apache Tika importer (merged 2026-10-10, 0.5.0)
- [x] any other format Apache Tika reads, through `anytopdf-plugin-tika` (Tika server or `tika-app` jar)

## Backlog

Priority order as of sprint 4 sign-off, with status on `main`.

1. Webhooks and a job queue: done, including the HTTP upload input.
2. IMAP mail watcher: done (`anytopdf watch imap`: IDLE or polling, sender allowlist with DMARC check, OAuth2 XOAUTH2 tokens, job-queue hand-off, shipped in release builds); richer rules and a quarantine folder still open.
3. PAPPL print server: done as the optional `helpers/anytopdf-printer` IPP Everywhere helper with a print-raster importer; AirPrint and Mopria certification still open.
4. Remote printing: done, including signed-in user stamps and print receipts.
5. New renderer and PDF/A-3: done. Tagged PDF/A-3a through krilla is the default renderer, with bookmarks, bidi shaping, font fallback and a bundled DejaVu Sans font; `--renderer pdf` keeps printpdf.
6. More inputs: done (HTML, email, zip/tar archives, PDF with its own text layer, HEIC/HEIF/AVIF).
7. Office documents and Whisper transcription: done (Whisper as a runtime plugin).
8. Distribution and an MCP server: MCP server, package manifests, the tap and bucket (served from this repository) and the `install.sh` installer done; winget, npx/uvx wrappers, signing and notarization still open.

## Security
- [x] `SECURITY.md` with private reporting through GitHub, and the threat model in `docs/threat-model/`
- [x] opt-in OS sandbox (`--plugin-sandbox strict`, Linux and macOS) and descendant process containment (`contain`) for runtime plugins
- [ ] hard CPU/memory/disk quotas for runtime plugins, sandboxing for external providers, Windows `strict`, and a decision on making `contain` the default (threat model Q18)
- [ ] untrusted-input handling for intake channels (no network, size/page caps, zip-bomb rejection)
- [ ] threat model revision for the post-sprint-4 surfaces (job queue, HTTP upload and webhooks, IMAP watcher, remote print front, PAPPL printer helper, MCP server, new importers)
- [x] resolve the threat model's open maintainer questions (§1.18: Q4, Q6, Q8, Q9, Q11, Q13, Q14, Q15, Q17) with tests and fixes (#6); Q18 stays open

## Later media semantics
- [x] neutral face detection: presence/count/bounds (`anytopdf-plugin-faces`)
- [x] object detection provider (`anytopdf-plugin-objects`, YOLO ONNX on a pure-Rust runtime)
- [x] scene classification provider (zero-shot CLIP tags in `anytopdf-plugin-clip`)
- [x] CLIP image embeddings for search by meaning (`anytopdf-plugin-clip`; `search --semantic` follows the cross-file index)
- [x] face recognition against a local, user-enrolled face index: `--recognize-faces`, `anytopdf faces enroll|import|list|name|rename|merge|forget|find`, `anytopdf-plugin-face-id`
- [x] keyframe captions, questions and activities, video and scene summaries, and a video category (`anytopdf-plugin-vlm`)
- [ ] barcode/QR extraction
- [ ] audio chapter / speaker-turn annotations
- [ ] OCR-text-aware video frame retention
- [ ] deterministic chunk IDs, semantic page/chunk headings, provenance graph export
- [ ] CAD / image stacks, IGL plugin once the format is specified, generic command-adapter plugin
