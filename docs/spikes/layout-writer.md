# Spike S2-12-T1: layout and writer for the future renderer

## Question

For backlog item 5 (Krilla/Typst renderer, PDF/A-3, tagged PDF, bookmarks, Markdown, RTL/CJK shaping), which stack should replace printpdf: Typst embedded as a library, or parley or cosmic-text for layout with krilla as the PDF writer? Specifically: API maturity and licences; whether krilla can emit invisible text for OCR overlays at glyph level; Arabic, Hebrew and CJK shaping and bidi; font fallback; binary size delta; MSRV against the pinned Rust 1.88.0; and the effect on backlog item 5 and renderer isolation (A6).

## Time box and method

- Time box: 25 minutes, desk research.
- Date: 2026-10-04 (UTC).
- Method: desk research on primary sources only (crates.io API metadata, `Cargo.toml` files on GitHub, official docs, a GitHub pull request thread, release asset listings).
- One read-only inspection: the published `krilla` 0.8.2 source archive was downloaded from crates.io to a scratch directory outside the repository and searched with grep. Nothing was compiled, built, benchmarked or run. No dependency, lockfile or code change was made.
- Versions are those current on crates.io on the date above. All size figures are estimates unless a source is cited.

## Evidence

| # | Claim | Source |
|---|-------|--------|
| E1 | krilla 0.8.2 (2026-06-04): licence `MIT OR Apache-2.0`, `rust-version = 1.92`; 0.7.0 declares 1.89; 0.6.0 and older declare none | crates.io API for krilla; krilla `Cargo.toml` |
| E2 | typst, typst-library, typst-layout, typst-pdf 0.15.1 (2026-07-17): licence `Apache-2.0`, `rust-version = 1.92` | crates.io API; typst `Cargo.toml` |
| E3 | parley 0.11.1 (2026-08-16): `Apache-2.0 OR MIT`, `rust-version = 1.88`; fontique 0.11.1 same | crates.io API |
| E4 | cosmic-text 0.19.0 (2026-04-22): `MIT OR Apache-2.0`, `rust-version = 1.89`; depends on harfrust, unicode-bidi, fontdb | crates.io API; cosmic-text `Cargo.toml` |
| E5 | harfrust 0.14.0: MIT, MSRV 1.85. rustybuzz 0.20.1: MIT, no declared MSRV | crates.io API |
| E6 | The current build uses printpdf 0.12.8 (MIT, MSRV 1.88) with `ttf-parser`; the repo pins 1.88.0 | repo `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` |
| E7 | krilla 0.8.2 `TextRenderingMode` use in `src/content.rs` covers only `Fill`, `Stroke` and `FillStroke`; there is no invisible mode (grep of the 0.8.2 archive for "invisible", "Tr 3", "render mode") | krilla 0.8.2 source archive |
| E8 | krilla PR #382 "Add invisible glyph text layer helper" (Tr 3, ISO 32000-2 9.3.6) was opened 2026-05-07 and closed unmerged on 2026-05-08. The maintainer asked "Couldn't you just set the fill opacity to 0?" | github.com/LaurenzV/krilla/pull/382 |
| E9 | `Surface::draw_glyphs` takes positioned glyphs plus the source `text: &str`. Its docs say callers must supply their own bidi, font fallback and layout | krilla 0.8.2 `src/surface.rs` |
| E10 | krilla `simple-text` feature (default on) pulls rustybuzz 0.20.1 for basic shaping; krilla's workspace also lists parley and harfrust | krilla 0.8.2 `Cargo.toml`; krilla workspace `Cargo.toml` |
| E11 | parley: shaping by HarfRust, font enumeration and fallback by fontique, bidi, segmentation and locale by ICU4X, own line breaking. MSRV may rise in patch releases. No hyphenation mentioned | github.com/linebender/parley README |
| E12 | cosmic-text: HarfRust shaping, bidirectional text, simple wrapping, custom fallback reusing browser static fallback lists; Arabic, Hindi and Simplified Chinese shown in README | github.com/pop-os/cosmic-text README |
| E13 | Typst: tries each listed family in order until one has the glyphs; `fallback` parameter; `dir` for bidi; `cjk-latin-spacing`; script auto-detection. Uses rustybuzz 0.20, unicode-bidi 0.3.18, hypher, ICU4X, and a pinned git revision of krilla for PDF | typst.app/docs text reference; typst `Cargo.toml` |
| E14 | Typst as a library is `World` trait plus `compile`; the docs do not state any semver or stability guarantee | docs.rs/typst |
| E15 | typst-as-lib 0.16.0 (third party wrapper): MIT, no declared MSRV | crates.io API |
| E16 | Typst CLI 0.15.1 release archives: 14.4 MB (aarch64-apple-darwin tar.xz) to 22.5 MB (x86_64-pc-windows-msvc zip) | api.github.com/repos/typst/typst/releases/latest |
| E17 | Current `target/release/anytopdf` in the main checkout is 8,976,624 bytes (file listing, not built by this spike) | `ls -l` of the existing artifact |

## Findings

**Licences.** All three are compatible with the project's MIT OR Apache-2.0. krilla, parley and cosmic-text are dual-licensed (E1, E3, E4). Typst is Apache-2.0 only (E2), so a binary linking it is distributable only under Apache-2.0 terms, which narrows the project's "MIT OR Apache-2.0" for that binary. Third-party typst-as-lib is MIT (E15). harfrust and rustybuzz are MIT (E5). parley's emoji sub-crate has separate licence terms (E11).

**API maturity.** krilla is 0.8.x with an active release cadence (0.7 in March, 0.8 in May and June 2026) and a large test suite per its README. Parley is 0.11 and explicitly allows MSRV bumps in patch releases (E11). Typst's library API (`World`, `compile`) has no documented stability promise (E14), and PDF export inside Typst depends on a git revision of krilla (E13). All three are pre-1.0 or unguaranteed, so any adoption needs a pinned version and an isolation layer.

**Invisible text for OCR overlays.** krilla 0.8.2 cannot emit text render mode 3 (E7). A patch adding it was rejected by the maintainer in favour of a fill opacity of 0 (E8). The glyph-level path exists: `draw_glyphs` places individual positioned glyphs with their source text for ToUnicode mapping (E9), so an OCR word box can be written as glyph runs with `push_opacity(0)` or fill opacity 0. Open point: that approach writes a normal fill (mode 0) with a transparency state, not mode 3, so extractor behaviour (pdftotext, pdf.js, Acrobat search, ocrmypdf-style checks) is not verified here. Typst has no OCR-overlay feature and no invisible-text primitive that this research found, so it would need hidden text styled with transparent fill, with the same caveat.

**Shaping and bidi.**

| Option | Shaping engine | Bidi | Arabic / Hebrew / CJK |
|--------|----------------|------|-----------------------|
| Typst | rustybuzz 0.20 | unicode-bidi 0.3.18 | Supported by design; `dir`, script detection, `cjk-latin-spacing` (E13) |
| parley + krilla | HarfRust | ICU4X | Supported through HarfRust and ICU4X (E11) |
| cosmic-text + krilla | HarfRust | unicode-bidi | Supported; README demos Arabic and Chinese (E12) |

krilla itself does no layout: its optional rustybuzz path is basic only, and bidi and fallback are the caller's job (E9, E10). Hebrew is not shown by name in the cosmic-text or parley sources read; it follows from the engines (RTL plus HarfRust), so treat it as expected but untested.

**Font fallback.** Typst has an ordered family list plus a global fallback switch (E13). Parley delegates to fontique (E11). cosmic-text has its own fallback lists (E12). The current build embeds fonts via ttf-parser with no fallback.

**Binary size delta (estimate).** No measurement was made. Basis: the existing binary is about 9.0 MB (E17); the Typst CLI release archives are 14.4 to 22.5 MB compressed (E16) and include CLI-only extras, so an embedded Typst plus its ICU data and fonts is estimated at +10 to +30 MB uncompressed. For parley or cosmic-text plus krilla the estimate is +2 to +6 MB, basis: krilla's dependency list (E10) is of similar scale to printpdf's, and parley/ICU4X add shaping and Unicode data; this is a rough guess to be replaced by a `cargo bloat` measurement during item 5. printpdf's own dependencies (allsorts, lopdf, serde, wasm-bindgen) would be dropped, partly offsetting the delta.

**MSRV against 1.88.0.** Only parley and fontique (1.88) fit as published (E3). cosmic-text (1.89), krilla 0.7.0 (1.89), and krilla 0.8.x and all Typst crates (1.92) do not (E1 to E4). krilla 0.6.0 and older declare no MSRV and are unverified on 1.88. Therefore the krilla writer needs either a toolchain bump to 1.92 or a pin to an old krilla, which would also mean old dependencies. Bumping `rust-toolchain.toml` is a build-policy change outside sprint 2 scope.

**Impact on backlog item 5.** The item bundles renderer, PDF/A-3, tagged PDF, bookmarks, Markdown and RTL/CJK. krilla advertises PDF/A and PDF/UA validation, tagged PDF, outlines and annotations in its README, covering most of that list without Typst. Typst would give Markdown-like markup layout but is a document compiler, not a text-box placer, and does not fit our "place hidden words over an image" pages.

**Impact on renderer isolation (A6).** Because every candidate is pre-1.0 and the MSRV forces a toolchain decision, item 5 should start by putting the renderer behind our own layout model (pages of positioned runs and image placements). The existing renderer consumes only the normalized graph already; A6 stays "do only if a sprint 2 story needs it" and no sprint 2 story needs it.

## Risks and unknowns

- Invisible text via opacity 0 is unverified against real extractors; a scratch prototype in item 5 must check pdftotext, pdf.js and a PDF/A validator.
- krilla needs Rust 1.92 (E1); bumping from 1.88 may affect CI, the release matrix and contributors.
- Version alignment between parley's HarfRust/skrifa and krilla's skrifa 0.42 was not checked.
- Size estimates are unmeasured. Typst's real size depends on ICU data and font features.
- Hebrew is untested in all sources read.
- Typst's licence (Apache-2.0 only) and unstable API are the reasons it is the weaker option, not performance.
- Upstream facts may have changed after 2026-10-04.

## Recommendation

Recommendation: Do not adopt Typst; plan item 5 as parley (layout, bidi, fallback) plus krilla (writer), gated on a deliberate Rust 1.92 toolchain bump and a scratch check of opacity-0 hidden text, behind an owned layout model.

Reasoning: Typst is Apache-2.0 only, has no stated API stability, targets document compilation rather than hidden-text overlays, and is the largest size delta (E2, E14, E16). The parley plus krilla pair covers Arabic, Hebrew and CJK shaping through HarfRust and ICU4X, fallback through fontique, and PDF/A and tagging through krilla. The cost is the MSRV bump. cosmic-text is the fallback choice to parley if its API fits better, with the same MSRV problem.

## Backlog impact

- Backlog item 5: unblocked as a plan, with prerequisites: (1) decide the Rust 1.92 bump, (2) prototype hidden text with opacity 0 and verify extraction, (3) measure size with `cargo bloat` on all five release targets, (4) introduce the owned layout model first (A6 in practice). No sprint 2 change.
- Backlog item 7 (full build): the size delta here is small next to ffmpeg, OCR models and Whisper, so it should not drive that decision.
- Backlog items 3 and 4 (PAPPL, receipts on the manifest page): independent; receipts rendering would benefit from the item 5 shaping stack.
- No sprint 2 story needs a code, Cargo or toolchain change from this spike.

## Sources

- https://crates.io/crates/krilla
- https://github.com/LaurenzV/krilla
- https://raw.githubusercontent.com/LaurenzV/krilla/main/Cargo.toml
- https://github.com/LaurenzV/krilla/pull/382
- https://crates.io/crates/typst
- https://raw.githubusercontent.com/typst/typst/main/Cargo.toml
- https://docs.rs/typst/latest/typst/
- https://typst.app/docs/reference/text/text/
- https://api.github.com/repos/typst/typst/releases/latest
- https://github.com/linebender/parley
- https://crates.io/crates/parley
- https://github.com/pop-os/cosmic-text
- https://raw.githubusercontent.com/pop-os/cosmic-text/main/Cargo.toml
- https://crates.io/crates/harfrust
- https://crates.io/crates/typst-as-lib
