# Roadmap

Checked items describe implemented code on `main`, not release certification. See
`RELEASING.md` for validation evidence and release procedures, `CHANGELOG.md` for
per-release behaviour changes, and the README roadmap for the feature-level view.

Status: sprint 4 closed on 2026-10-05. Everything below "Backlog" is not started.

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

## Backlog (not started)

Priority order as of sprint 4 sign-off.

1. Webhooks and a shared job queue (watched folder, HTTP upload)
2. IMAP mail watcher
3. PAPPL print server (IPP Everywhere / AirPrint / Mopria helper process)
4. Remote printing (Tailscale / WireGuard)
5. New renderer (krilla / Typst) and PDF/A-3
6. More inputs: PDF, HTML, email, archives, HEIC
7. Office documents and Whisper transcription
8. Distribution (Homebrew, winget, scoop, binstall, signing) and an MCP server

## Security
- [x] `SECURITY.md` with private reporting through GitHub, and the threat model in `docs/threat-model/`
- [ ] OS sandbox, descendant process containment, and hard CPU/memory/disk quotas for runtime plugins
- [ ] untrusted-input handling for intake channels (no network, size/page caps, zip-bomb rejection)
- [ ] resolve the threat model's open maintainer questions (§1.18: Q4, Q6, Q8, Q9, Q11, Q13, Q14, Q15, Q17)

## Later media semantics
- [ ] neutral face detection: presence/count/bounds
- [ ] object detection provider (YOLO/DETR plugin)
- [ ] scene classification provider
- [ ] barcode/QR extraction
- [ ] audio chapter / speaker-turn annotations
- [ ] OCR-text-aware video frame retention
- [ ] deterministic chunk IDs, semantic page/chunk headings, provenance graph export
- [ ] CAD / image stacks, IGL plugin once the format is specified, generic command-adapter plugin
