# Repository Guidelines

## Project Structure & Module Organization

This Rust workspace converts media and documents into searchable PDFs:

- `crates/anytopdf-core/`: normalized asset graph, plugin traits, registry, and pipeline.
- `crates/anytopdf-builtin/`: importers, OCR, captions, metadata, and media discovery.
- `crates/anytopdf-pdf/`: PDF rendering from the normalized graph.
- `crates/anytopdf-cli/`: the `anytopdf` executable and CLI arguments.
- `crates/anytopdf-index/`: cross-file SQLite search index behind `convert --index`, `index` and `search`.
- `crates/anytopdf-plugin-whisper/`: Whisper speech-to-text runtime plugin.
- `crates/anytopdf-onnx/`: shared pure-Rust (tract) ONNX helpers for vision plugins.
- `crates/anytopdf-plugin-objects/`: YOLO object detection runtime plugin.
- `crates/anytopdf-plugin-faces/`: face detection runtime plugin (embedded YuNet model).
- `examples/anytopdf-plugin-example.py`: runtime plugin example.
- `scripts/verify.sh`: local checks; `.github/workflows/release.yml`: release builds.

No dedicated test or asset directories currently exist. Consult `ARCHITECTURE.md` and `PLUGIN_PROTOCOL.md` before changing pipeline stages or plugin interfaces.

## Build, Test, and Development Commands

Use the Rust 1.92.0 toolchain pinned in `rust-toolchain.toml` (edition 2024).

- `cargo build --release`: build `target/release/anytopdf`.
- `cargo run -p anytopdf -- doctor`: inspect optional runtime providers.
- `cargo run -p anytopdf -- convert README.md -o /tmp/anytopdf-preview.pdf`: exercise text conversion locally.
- `cargo fmt --all`: format workspace code.
- `sh scripts/verify.sh`: run formatting checks, workspace checking, Clippy with warnings denied, and tests.
- `cargo test --workspace`: run workspace tests separately.

FFmpeg/ffprobe, ExifTool, Tesseract, and Python/docTR are optional runtime providers; document which were available when validating provider-dependent changes.

## Coding Style & Naming Conventions

Use rustfmt defaults, four-space indentation, `snake_case` functions/modules, `PascalCase` types/traits, and `SCREAMING_SNAKE_CASE` constants. Follow existing `anyhow::Result` and contextual error handling. Declare shared dependencies in the root `Cargo.toml` and inherit them with `workspace = true`. Name runtime executables `anytopdf-plugin-*`.

## Testing Guidelines

No tests or coverage threshold are currently configured. Use Rust's built-in test harness: place unit tests in `#[cfg(test)] mod tests` beside implementation and integration tests under the relevant crate's `tests/` directory. Give tests descriptive behavior names, such as `probe_prefers_magic_over_extension`. Cover parsing, importer selection, provenance, and optional-provider failures when changing those behaviors. Use small fixtures and temporary workspaces; visually inspect PDFs for renderer changes.

## Architecture & Plugin Boundaries

Keep format knowledge in importers. Enrichers must preserve unrelated data and record annotation provenance. Renderers consume only the normalized graph. Keep derived files inside the job workspace, isolate optional-plugin failures, and preserve the versioned JSON protocol.

## Commit & Pull Request Guidelines

This checkout contains no Git history to establish commit conventions. Use concise imperative subjects, optionally scoped, such as `builtin: handle empty subtitle cues`. Describe the change, link relevant issues, and report verification commands and results. Include PDF samples or screenshots for rendering changes, and update architecture or protocol documentation when interfaces change.
