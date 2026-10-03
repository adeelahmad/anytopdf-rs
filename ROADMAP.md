# Roadmap

Checked items describe implemented code, not release certification. See
`RELEASING.md` for validation evidence and release procedures.

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

## 0.2 — runtime plugin host
- [x] PATH / `ANYTOPDF_PLUGIN_PATH` executable discovery
- [x] runtime importer adapter
- [x] runtime source/graph/unit enricher adapters
- [x] plugin timeouts and bounded captured output/response size
- [ ] OS sandbox, descendant process containment, and hard CPU/memory/disk quotas
- [x] capability allow/deny registration policy and discovery opt-out

## 0.3 — richer media semantics
- [ ] pluggable Whisper/whisper.cpp transcription
- [ ] neutral face detection: presence/count/bounds
- [ ] object detection provider (YOLO/DETR plugin)
- [ ] scene classification provider
- [ ] barcode/QR extraction
- [ ] audio chapter / speaker-turn annotations
- [ ] OCR-text-aware video frame retention

## 0.4 — more "anything"
- [ ] PDF importer
- [ ] Office documents
- [ ] email / mbox / EML
- [ ] HTML / URL snapshot provider
- [ ] archives
- [ ] CAD / image stacks
- [ ] IGL plugin once the file format/parser is specified
- [ ] generic command-adapter plugin

## 0.5 — RAG packaging
- [ ] embed normalized manifest as PDF associated data when renderer supports it
- [ ] deterministic chunk IDs
- [ ] source hashes
- [ ] optional JSONL sidecar
- [ ] semantic page/chunk headings
- [ ] provenance graph export
