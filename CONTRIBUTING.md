# Contributing to anytopdf

Thanks for helping. Bug reports with a sample file, new importers, runtime plugins
and documentation fixes are all welcome.

## Before you start

- Read [ARCHITECTURE.md](ARCHITECTURE.md) before changing pipeline stages, and
  [PLUGIN_PROTOCOL.md](PLUGIN_PROTOCOL.md) before changing the plugin interface.
- Format knowledge belongs in importers. Enrichers add annotations with provenance
  and leave unrelated data alone. Renderers read only the normalized graph.
- A new format that needs a heavy dependency or a model is usually better as an
  `anytopdf-plugin-*` executable than as a built-in; see
  [examples/anytopdf-plugin-example.py](examples/anytopdf-plugin-example.py).
- For larger changes, open an issue first so we can agree on the shape.

## Build and test

The toolchain is pinned in `rust-toolchain.toml` (Rust 1.92, edition 2024).

```bash
make                   # install build tools if missing, then build
sh scripts/verify.sh   # fmt check, cargo check, Clippy with warnings denied, tests
make ci                # what GitHub CI runs on Linux, macOS and Windows
anytopdf doctor        # which optional providers this machine has
```

FFmpeg, ExifTool, Tesseract, Poppler, LibreOffice and docTR are optional. When a
change depends on one, say in the pull request which providers you had installed.

- Put unit tests in a `#[cfg(test)] mod tests` beside the code and integration
  tests in the crate's `tests/` folder. Name tests after the behaviour, for example
  `probe_prefers_magic_over_extension`.
- Python tooling tests live in `tests/` and run with
  `python3 -m unittest discover -s tests`.
- For renderer changes, open the produced PDF and look at it, and attach a
  screenshot to the pull request.

## Commits and pull requests

- Every commit and pull request title is a
  [Conventional Commit](https://www.conventionalcommits.org/), for example
  `fix(builtin): handle empty subtitle cues`. CI checks this, and pull requests are
  squash-merged with their title.
- Describe what changes for a user, link the issue, and list the checks you ran.
- Update the README, ARCHITECTURE.md or PLUGIN_PROTOCOL.md when behaviour or an
  interface changes, and add a line to CHANGELOG.md.

## Security

Please do not open public issues for vulnerabilities. Follow
[SECURITY.md](SECURITY.md) instead.

## License

By contributing you agree that your contributions are dual-licensed under the MIT
and Apache-2.0 licenses, like the rest of the project, without additional terms.
