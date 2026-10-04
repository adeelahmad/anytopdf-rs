# Full-build feasibility spike (S2-12-T2)

## Question

Can anytopdf ship a second, "full" release build per target (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`) that bundles an LGPL FFmpeg, OCR engine and models, the Whisper base model with a runtime, and a Noto font subset, with no runtime downloads and with its licence-notice obligations met? What would each archive weigh, and should the project publish two builds (slim and full)? (Requirements R23 and R25 decision 1.)

## Time box and method

- Date: 2026-10-04 (UTC). Time box: about 25 minutes.
- Method: desk research only. Primary sources were the FFmpeg legal and licence pages, the GNU LGPL text, the whisper.cpp, whisper-rs, Tesseract and tessdata_fast repositories, the Hugging Face model repository, the Noto font repositories, the SIL OFL FAQ and crates.io.
- File sizes were read from the GitHub contents API and the Hugging Face model API (metadata queries, byte counts of the published files). Slim archive sizes were read from the local `dist/` directory of the v0.1.0 release build.
- No scratch experiment was run. Nothing was built, linked or measured by this spike; every build-size figure other than the quoted file sizes and the two local slim archives is an estimate and says so.

## Evidence

| # | Claim | Source |
|---|-------|--------|
| E1 | FFmpeg is LGPL v2.1+ by default; `--enable-gpl` makes the build GPL v2 and `--enable-nonfree` makes it unredistributable. | https://ffmpeg.org/legal.html |
| E2 | The FFmpeg compliance checklist recommends dynamic linking, shipping the matching FFmpeg source, the configure line, attribution, and not hiding the library names. | https://ffmpeg.org/legal.html |
| E3 | GPL-only external libraries include libx264, libx265, libxvid, librubberband, libvidstab; many filters and some x86 asm are GPL; fdk-aac and OpenSSL need `--enable-nonfree`; `--enable-version3` raises the build to LGPL v3. | https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/LICENSE.md |
| E4 | LGPL 2.1 section 6 lets a combined work be distributed if users can relink against a modified library (a shared-library mechanism, or object files for relinking), with licence text and source or a written offer. Not re-fetched in this session (HTTP 429); verify before release. | https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html |
| E5 | Published BtbN LGPL FFmpeg archives (full-featured, not minimal): linux64 about 137 MB, linuxarm64 about 116 MB, win64 about 170 MB (compressed, as of n8.1/n9.0 assets). No macOS or musl LGPL builds are published there. | https://github.com/BtbN/FFmpeg-Builds/releases/latest |
| E6 | Tesseract is Apache-2.0, depends on Leptonica (BSD 2-clause), and has Windows build configuration. | https://github.com/tesseract-ocr/tesseract |
| E7 | tessdata_fast is Apache-2.0. Sizes in bytes: eng 4,113,088; osd 10,562,727; script/Arabic 9,311,038; script/Hebrew 4,866,337; script/HanS 5,972,903; script/Latin 89,384,811. tessdata_best eng is 15,400,601. | https://github.com/tesseract-ocr/tessdata_fast (sizes via the GitHub contents API) |
| E8 | whisper.cpp is MIT. Disk sizes: tiny 75 MiB, base 142 MiB, small 466 MiB. Runs on Windows (MSVC), macOS Intel and Arm (Metal, Core ML) and Linux. | https://github.com/ggml-org/whisper.cpp |
| E9 | `ggml-base.bin` is 147,951,465 bytes; `ggml-base-q5_1.bin` is 59,707,625; `ggml-base-q8_0.bin` is 81,768,585. Repository licence tag: MIT. | https://huggingface.co/ggerganov/whisper.cpp (sizes via the Hugging Face model API) |
| E10 | whisper-rs is Unlicense, vendors whisper.cpp as a submodule, lists Windows, macOS and Linux and Metal/CUDA/Vulkan features. The GitHub repository was archived on 2025-07-30 and maintenance moved to Codeberg; crates.io lists 0.16.0 updated 2026-03-12. | https://github.com/tazz4843/whisper-rs and https://crates.io/crates/whisper-rs |
| E11 | Noto fonts are SIL OFL 1.1. Noto Sans Regular 621,572 bytes; Noto Sans Arabic Regular 234,892; Hebrew Regular 26,860; Devanagari Regular 243,520. | https://github.com/notofonts/notofonts.github.io (sizes via the GitHub contents API) |
| E12 | Noto Sans CJK: whole-family regular TTC 19,484,784 bytes; per-language subset OTF (Sans/SubsetOTF) SC Regular 8,331,336, JP Regular 4,533,028, KR Regular 4,644,748. | https://github.com/notofonts/noto-cjk (sizes via the GitHub contents API) |
| E13 | OFL: bundling with software needs the copyright statement, licence notice and licence text; the application itself need not be OFL; subsetting counts as modification, so a subset of a font with a Reserved Font Name must be renamed unless functionally equivalent; embedding in PDFs needs no licence text with the document. | https://openfontlicense.org/ofl-faq/ |
| E14 | Local slim archives, v0.1.0: aarch64-apple-darwin 3,858,364 bytes, aarch64-unknown-linux-musl 3,583,179 bytes (`dist/*.tar.gz`). The release workflow packages the five target triples; the Windows target uses a zip. | `dist/` in this repository and `.github/workflows/release.yml` |
| E15 | Today FFmpeg, ExifTool and Tesseract are optional external providers found on PATH; Apple Vision is native on macOS (`apple-vision` feature); docTR may download weights on first use; audio transcription is a plugin. | README.md in this repository |

## Findings

### Per-component feasibility

1. FFmpeg (LGPL). Feasible if built without `--enable-gpl`, `--enable-nonfree` and, to stay LGPL v2.1, without `--enable-version3` (E1, E3). Decode and demux of common video and audio (H.264, HEVC, VP9, AV1 via native or LGPL decoders, AAC, MP3, Opus, Vorbis, FLAC, PCM) does not need any GPL library. Exclude: libx264, libx265, libxvid, libfdk-aac, OpenSSL, the GPL filters, avisynth, libvidstab, librubberband (E3). The project only needs demux, decode, scene-change and keyframe extraction (E15), so a custom `--disable-everything` build with selected demuxers, decoders, the `scdet`/`select`/`scale` filters and PNG/MJPEG encoders is enough. Whether the scene-change filter used today is GPL-free is an unknown (see risks).
2. Static versus shared. FFmpeg's own checklist prefers dynamic linking (E2). LGPL 2.1 section 6 also allows static linking if relinkable objects are supplied (E4). Because the FFmpeg tools are separate executables invoked by anytopdf as providers (E15), the cleanest model is to ship `ffmpeg` and `ffprobe` as separate programs in the archive (not linked into the Rust binary). Then there is no combined-work question for anytopdf's own MIT OR Apache-2.0 code, and the LGPL duty reduces to notice, licence text and source or offer for FFmpeg itself. On Windows ship shared DLLs beside the executables; on Linux musl and macOS a statically linked FFmpeg executable is acceptable as a separate program, with the source offer. Linking libav* into the Rust binary is possible but is not recommended: it creates the relinking duty for the whole executable.
3. OCR. Tesseract (Apache-2.0) plus Leptonica (BSD 2-clause) is permissively licensed (E6) and compatible with the project's dual licence. Models: tessdata_fast eng 4.1 MB, osd 10.6 MB, Arabic 9.3 MB, Hebrew 4.9 MB, Simplified Chinese 6.0 MB (E7). Latin script is 89 MB, so avoid it; use `eng` plus selected scripts. The `best` eng model is 15.4 MB. Apple Vision is already native on macOS (E15), so macOS could skip Tesseract models, but identical content across targets keeps behaviour and support simple. docTR needs a Python environment and downloads weights, so it cannot be part of a no-download bundle (E15); keep it as an external-provider option.
4. Whisper. Base model is 142 MiB (147,951,465 bytes) at f16 and 59.7 MB at q5_1 (E8, E9); the models and whisper.cpp are MIT, and the original OpenAI weights are published under MIT (Hugging Face licence tag MIT, E9). Runtime options: (a) the `whisper-cli` executable from whisper.cpp, shipped as a separate provider, which fits the existing plugin or provider model; (b) the whisper-rs binding, which compiles whisper.cpp at build time (needs CMake and a C++ toolchain, E10) and whose upstream repository was archived in July 2025 (E10). Recommendation here is (a) for the first full build. The accuracy loss of q5_1 against f16 for the base model is not verified by this spike.
5. Noto subset. OFL 1.1 (E11, E13). Latin, Arabic, Hebrew and Devanagari regular weights total about 1.13 MB (621,572 + 234,892 + 26,860 + 243,520 bytes). Adding the Simplified Chinese subset OTF adds 8.3 MB (E12); JP and KR subsets are about 4.5 MB each. The full CJK TTC is 19.5 MB and should not be used. Subsetting further (for example with pyftsubset) is a modification and may require renaming if the font declares a Reserved Font Name (E13); shipping the unmodified regular files avoids this.

### Per-target view

| Target | FFmpeg approach | OCR runtime | Whisper runtime | Main build risk |
|--------|-----------------|-------------|-----------------|-----------------|
| x86_64-unknown-linux-musl | static `ffmpeg`/`ffprobe`, custom minimal LGPL build, built in CI (no published LGPL musl build, E5) | static Tesseract + Leptonica via musl-gcc | static `whisper-cli`, C++ with libstdc++/libc++ static | C++ on musl, CMake, ARM/AVX flags |
| aarch64-unknown-linux-musl | same, cross-compiled or on an arm64 runner | same | same, NEON | cross toolchain for C++ |
| aarch64-apple-darwin | static custom build; no published LGPL macOS build (E5) | Tesseract, or native Apple Vision (E15) | `whisper-cli` with Metal (E8) | notarization and signing of extra executables |
| x86_64-apple-darwin | static custom build | same | CPU build, Accelerate optional (E8) | Intel macOS runner availability, signing |
| x86_64-pc-windows-msvc | shared DLL build (E2); BtbN publishes a win64 LGPL build, but it is full-featured and about 170 MB compressed (E5) | MSVC build of Tesseract + Leptonica | MSVC build of `whisper-cli` (E8) | MSVC builds of FFmpeg need MSYS2 and `--toolchain=msvc`; DLL placement |

### Size per target (estimate)

Basis: slim sizes are measured for the two local archives (E14) and assumed about 3.7 to 4.0 MB for the other three. Component sizes are the byte counts in E7, E9, E11 and E12 (OCR models: eng + osd + Arabic + Hebrew + HanS = 34.9 MB; Noto: Latin, Arabic, Hebrew, Devanagari, SC subset = 9.5 MB, taken at about 8 MB archived because font and traineddata files compress only modestly; Whisper: q5_1 base 59.7 MB). Executables are estimates: minimal FFmpeg plus ffprobe about 6 MB (9 MB with Windows DLLs), Tesseract and Leptonica about 4 MB, whisper-cli about 3 to 4 MB. All sums are estimates, in MB of compressed archive; none was built.

| Target | Slim | + FFmpeg | + OCR runtime and models | + Whisper runtime and q5_1 base | + Noto | Full (estimate) | Full with f16 base (estimate) |
|--------|------|----------|--------------------------|----------------------------------|--------|-----------------|-------------------------------|
| x86_64-unknown-linux-musl | 3.7 | 6 | 39 | 63 | 8 | about 120 | about 208 |
| aarch64-unknown-linux-musl | 3.6 (measured) | 6 | 39 | 63 | 8 | about 120 | about 208 |
| aarch64-apple-darwin | 3.9 (measured) | 6 | 39 | 64 | 8 | about 121 | about 209 |
| x86_64-apple-darwin | 4.0 | 6 | 39 | 64 | 8 | about 121 | about 209 |
| x86_64-pc-windows-msvc | 4.0 | 9 | 40 | 63 | 8 | about 124 | about 212 |

For comparison, the published full-featured LGPL FFmpeg archives alone are 116 to 170 MB (E5); a minimal build is the only way to keep the full archive near 120 MB. The Whisper model is half or more of the total in every row.

### Licence-notice obligations for the release archives

- Keep `LICENSE-MIT` and `LICENSE-APACHE` for anytopdf itself, as today.
- Add a `THIRD-PARTY-NOTICES` file to the full archive covering: FFmpeg (LGPL v2.1+ text, the exact configure line, version, and the source offer or the bundled source tarball, E2, E4); Tesseract and tessdata_fast (Apache-2.0 text and NOTICE, E6, E7); Leptonica (BSD 2-clause); whisper.cpp and the Whisper weights (MIT, E8, E9); Noto fonts (OFL 1.1 text and each font's copyright line, E13).
- Publish the FFmpeg source matching the shipped binaries beside the release assets, or include it in the archive (E2).
- Name the FFmpeg binaries plainly and do not strip or obscure the licence notice or version output; do not add EULA terms that forbid reverse engineering of the LGPL parts (E2).
- State in the README and `--version` or `doctor` output which build (slim or full) is running and which provider versions are bundled.
- The slim archive remains notice-light (no third-party code beyond Rust crate licences, unchanged by this spike).

### Slim versus full

A single archive of about 120 MB would multiply the download for every user who only needs PDF conversion of text, images and documents, while a slim archive of about 4 MB is the current shipped product (E14). Two builds with no runtime download fits human decision 1 (intake, R25). The size gap is dominated by data (Whisper and OCR models), not code, so the full build should be assembled from the slim binary plus a "providers" directory, built by a separate CI job, rather than by a second Rust feature set.

## Risks and unknowns

- Scene-change detection: the current video sampling uses FFmpeg scene-change filters (README, E15). Whether the needed filters are available in a non-GPL configure is not confirmed by this spike; verify with `ffmpeg -filters` on a trial LGPL build.
- Building a custom minimal FFmpeg for five targets is new CI work; macOS, musl and MSVC toolchains each have their own pitfalls (estimate: one to two engineer-days per target to stabilise, basis: general experience, not measured).
- Static C++ (Tesseract, whisper.cpp) on musl: no primary source reviewed here confirms a clean static musl build; treat as unknown until an experiment is run.
- Accuracy of the q5_1 base model versus f16 is unverified; base is also a weak model for non-English speech.
- whisper-rs upstream was archived in July 2025 and moved (E10); depending on it adds maintenance risk. The CLI-provider approach avoids this.
- Patent exposure: LGPL licensing does not remove patent obligations for codecs such as H.264 or HEVC; this spike gives no legal advice and a legal read is advised before shipping HEVC or AAC decode.
- LGPL 2.1 section 6 wording was not re-fetched (HTTP 429) and relies on E2 and E4; re-check before release.
- Apple notarization and Windows signing of the additional executables are not researched.
- Archive size estimates ignore compression gains and the extra size of debug or Metal resources; error bars are probably 25 percent.

## Recommendation

Recommendation: Publish two builds per target, slim (current, about 4 MB) and full (estimate about 120 MB with the q5_1 Whisper base model, no runtime downloads), assembled as the unchanged Rust binary plus separately packaged provider executables and model files, with FFmpeg built in-house as an LGPL, decode-only, no-GPL/no-nonfree configuration and Whisper delivered through a whisper.cpp CLI.

Supporting decisions:
1. FFmpeg ships as separate `ffmpeg` and `ffprobe` programs (shared DLLs on Windows), never linked into the Rust binary, so anytopdf's MIT OR Apache-2.0 licence is unaffected.
2. Ship OCR models eng, osd, Arabic, Hebrew and Simplified Chinese from tessdata_fast; do not ship Latin script (89 MB) or docTR.
3. Ship Whisper base at q5_1 (59.7 MB) by default; offer the f16 model only if a measured accuracy gap justifies the extra 88 MB.
4. Ship unmodified Noto Sans Latin, Arabic, Hebrew, Devanagari and the SC subset (about 9.5 MB).
5. Before committing to this plan, run one scratch experiment on x86_64-unknown-linux-musl: build the minimal LGPL FFmpeg, confirm scene-change filters work, and build whisper.cpp and Tesseract statically.

## Backlog impact

- Confirms the intake decision of two builds (R25 decision 1) as feasible on all five targets, subject to the musl and MSVC experiments above.
- Adds backlog work after Sprint 2: a provider-bundle CI job (FFmpeg minimal LGPL build, Tesseract, whisper-cli per target), a `THIRD-PARTY-NOTICES` generator, source-offer publishing for FFmpeg, and release-policy checks that the full archive contains no GPL or nonfree FFmpeg.
- Audio transcription (currently a plugin, E15) gains a bundled default in the full build; it is not a Rust code dependency, so no change to `Cargo.toml` is needed for this route.
- macOS can optionally drop Tesseract models in favour of Apple Vision, saving about 35 MB per macOS archive (estimate, from E7).
- Does not affect the layout/writer spike (backlog item 5) beyond the font set: Noto subsets chosen here should match whichever writer is selected.

## Sources

- https://ffmpeg.org/legal.html
- https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/LICENSE.md
- https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html
- https://github.com/BtbN/FFmpeg-Builds/releases/latest
- https://github.com/tesseract-ocr/tesseract
- https://github.com/tesseract-ocr/tessdata_fast
- https://github.com/ggml-org/whisper.cpp
- https://huggingface.co/ggerganov/whisper.cpp
- https://github.com/tazz4843/whisper-rs
- https://crates.io/crates/whisper-rs
- https://github.com/notofonts/notofonts.github.io
- https://github.com/notofonts/noto-cjk
- https://openfontlicense.org/ofl-faq/
