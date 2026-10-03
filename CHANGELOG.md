# Changelog

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
