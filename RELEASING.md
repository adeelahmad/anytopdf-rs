# Release verification

The 0.1.0 release scope is searchable PDFs from raster images, text/Markdown,
subtitles, FFmpeg video frames, and supplied audio transcripts, with optional OCR,
metadata providers and executable plugins. Future importers and analyzers remain
in `ROADMAP.md`. `AGENTS.md` was preserved byte for byte.

## Reproduce the checks

Use GNU Make and Bash with the supplied `Cargo.lock`. `make` bootstraps supported
missing build dependencies and Rust 1.88.0, then builds locally. `make providers`
separately installs FFmpeg, ExifTool, Tesseract and Poppler; see README for platform
prerequisites. Python must be 3.11+; an explicit `PYTHON` override is honored or
fails clearly. For the complete verification:

```bash
make ci SMOKE_FLAGS=--require-poppler
```

Verification checks formatting, compilation, Clippy with warnings denied, Rust
unit/integration tests with and without default features, and Python tests. The
smoke check requires Poppler (`pdfinfo`, `pdftotext`) and a Unicode system font;
it exercises text/image conversion, Unicode extraction, pagination,
font-subset size, graph output and overwrite protection.
Use `SMOKE_FLAGS="--require-poppler --strict"` when ExifTool is installed to
additionally require warning-free conversion. Minimal systems intentionally warn
about missing optional metadata. `make verify` runs just the formatting, lint and
test suite; `sh scripts/verify.sh` remains available without Make.

For provider checks, create a clearly readable image, an EXIF-rotated JPEG, a short
video with an SRT sidecar, audio with a text sidecar, and a `.example` text file.
Convert each with `--strict --dump-graph`, selecting `--ocr vision` or
`--ocr tesseract` for the image. Install the example plugin as an executable named
`anytopdf-plugin-example` on `ANYTOPDF_PLUGIN_PATH`. Inspect providers, timestamps,
source IDs and normalized OCR regions in the graphs. Extract text with `pdftotext`
and render every PDF page using `pdftoppm -png` for visual review.

## Historical local validation — 2026-10-04

The following record predates the automation changes and expanded tests. It is
retained as historical evidence, not a current candidate or publication result.

- Rust 1.88.0 on Apple Silicon macOS: formatting, workspace compilation and Clippy
  passed; 39 Rust tests passed in each feature configuration, plus three Python tests.
- Actual text, image, video/caption, audio/transcript and runtime-plugin conversions
  passed strict mode and independent text extraction. Native Apple Vision and
  Tesseract both recognized the acceptance marker with normalized bounding boxes.
- All 12 acceptance pages were rendered and visually inspected: pagination, Unicode,
  long words, image rotation, video frames, transcript pages and invisible OCR.
- Font subsetting reduced the nine-page media sample from 15,204,473 bytes to about
  100 KB while preserving searchable text. Informational image diagnostics no
  longer cause strict conversions to fail.
- FFmpeg, ffprobe, ExifTool, Tesseract and Poppler were available. docTR was not
  installed and its model-dependent execution was not validated.

## Historical optimized binary checks

| Target | Local execution verification |
| --- | --- |
| `aarch64-apple-darwin` | Native Apple Silicon; full media, Vision/Tesseract OCR, plugin and Poppler checks |
| `x86_64-apple-darwin` | Rosetta execution and provider checks |
| `aarch64-unknown-linux-musl` | Static executable in an ARM64 Alpine Linux guest; PDF, image, Unicode, pagination, graph and overwrite checks |
| `x86_64-unknown-linux-musl` | Static executable in an x86-64 Alpine Linux guest; the same checks |
| `x86_64-pc-windows-gnu` | Windows executable under Wine 11.0; PDF, image, Unicode, graph and overwrite checks |

The Windows GNU archive is compatibility-tested under Wine, not native Windows.
Native Windows hosted CI has since passed as recorded below; that CI run does
not establish release MSVC artifact publication.

macOS binaries link only Apple system libraries/frameworks. Linux guests had no
ExifTool, FFmpeg or OCR providers installed: ordinary text/image conversion
succeeded with the expected optional-metadata warnings. Their PDFs were extracted
and checked independently with host Poppler. The guests used temporary in-memory
filesystems, with no Docker or host filesystem mounts.

Cross-builds were run from Bash with Rust 1.88.0, `cargo-zigbuild` 0.23.4 and Zig
0.16.0. Native builds used `cargo build --release --locked`; Linux used:

```bash
cargo zigbuild --release --locked -p anytopdf --no-default-features \
  --target aarch64-unknown-linux-musl --target x86_64-unknown-linux-musl
```


## Packaging

```bash
make package SMOKE_FLAGS=--require-poppler
# Or select an explicit target supported by the build host:
make package TARGET=aarch64-apple-darwin
```

Packages in `dist/` contain the executable, user/developer documentation, changelog
and MIT/Apache licenses. Each archive has a SHA-256 sidecar. Windows uses ZIP; other
targets use tar.gz. Verify checksums and run the executable extracted from its
archive before distributing it.

## GitHub CI/CD

`.github/workflows/ci.yml` runs `make ci` on Linux, macOS and Windows for branch
pushes, pull requests and manual runs. Rust dependencies/builds are cached. Both
feature configurations must pass, and Linux additionally requires Poppler checks.

`.github/workflows/release.yml` uses direct shell builds on native runners for
Apple Silicon/Intel macOS, ARM64/x86-64 Linux musl, and Windows x86-64 MSVC. Linux
uses `NO_DEFAULT_FEATURES=1`; Windows uses the static CRT flags in
`.cargo/config.toml`. Every target runs `make package`, including a smoke test.

## Conventional Commits and SemVer

Use `type(scope): description` or `type: description`, with a blank line before
any body. Accepted types are `feat`, `fix`, `perf`, `refactor`, `docs`, `style`,
`test`, `build`, `ci`, `chore` and `revert`. For example:

```text
feat(ocr): retain page-local text
fix(pdf): preserve Unicode glyphs
feat(plugin)!: change response contract

BREAKING CHANGE: explain the required migration
```

Either `!` or a `BREAKING CHANGE:` / `BREAKING-CHANGE:` footer requests a major
release (including before 1.0). Features request minor; fixes, performance and
reverts request patch. Other types alone produce no automatic release after the
first release. `BUMP=patch|minor|major` can explicitly request maintenance releases
or raise the inferred level, but cannot lower it. The first automatic release
uses the declared stable workspace version; an explicit first bump increments it.
Later planning uses the highest reachable stable `vX.Y.Z` tag and requires the
workspace version to match it. Merge commits are excluded from commit policy.

`make commit-check` checks commits since that tag. CI fetches full history and
checks the push range (all reachable non-merge commits for a new branch) or the PR
title. Squash-merge PRs using the validated title. Manual/reusable runs lacking
push/PR context skip event policy while retaining all build/test checks.

## Publish a stable release

`make build-release` builds locally. **`make release` mutates version/history and
publishes to GitHub.** Start on a named branch with a clean committed worktree,
full history, a configured Git author, a single intended GitHub push URL matching
the fetch repository, and authenticated `gh` access. Confirm the destination:

```bash
git remote -v
gh auth status --hostname github.com
make commit-check
make release-plan
make release REMOTE=origin SMOKE_FLAGS=--require-poppler
```

The release target installs missing release tools (including `gh`), checks
repository access, fetches tags, and derives the version from Conventional
Commits. It preserves handwritten changelog sections, appending generated commit
history where the version already exists; otherwise it adds a new section. It
updates workspace/lockfile versions, runs `make ci package`, then creates
`chore(release): X.Y.Z` and an annotated `vX.Y.Z` tag. Branch and tag are pushed
atomically without force. The command waits for a matching tag/commit Release
workflow and verifies the published state and exact asset names.

The workflow independently validates the manifest, local workspace entries in
`Cargo.lock`, changelog entry and tag. CI must pass before all five native builds;
all five packages must pass before publishing. Publication accepts exactly five
archives and their five sidecars, rejects non-files/symlinks and unsafe checksum
paths, hashes every archive, and creates `SHA256SUMS`. The resulting release has
11 assets. Its body is the exact tagged changelog section emitted by
`scripts/release.py notes`, passed through `--notes-file`.

## Interrupted release recovery

Inspect `git status`, the latest commit/tag, and the actual Actions result first.
A failure before the release commit restores only the three owned metadata files;
other unexpected changes remain for inspection. A failure after the commit/tag
retains them. Do not reset or delete published history and do not move a tag.

```bash
gh run list --repo adeelahmad/anytopdf-rs --workflow release.yml
# After resolving the reported cause (rerun a transient failed workflow if needed):
make release-resume REMOTE=origin
```

Resume validates metadata and requires the matching tag to point at HEAD. If the
tag is missing, HEAD must be the matching `chore(release)` commit; verification
runs again before creating the tag. With an existing tag, resume retries the
atomic push and waits for the matching workflow/publication; it does not itself
rerun failed Actions. An already published matching release can be confirmed
without creating another version. Fixes requiring source changes need a new
version/tag. A failed check, unavailable runner, timeout or missing asset remains
a failure until its actual cause is resolved.

## Manual prereleases and packaging-only dispatch

Automatic version planning supports stable versions. For a manual prerelease,
update the manifest and lockfile together, add its handwritten changelog section,
commit with a Conventional Commit, run `make ci package`, and validate the exact
tag before creating/pushing it:

```bash
python3 scripts/release.py validate --tag v1.2.3-rc.1
# Only when that matches the intended, verified source version:
git tag -a v1.2.3-rc.1 -m 'Release 1.2.3-rc.1'
git push --atomic origin HEAD refs/tags/v1.2.3-rc.1
```

Replace the example version with the actual version. Tags containing a prerelease
suffix publish as prereleases. Existing releases are not overwritten. Only the
publishing job receives `contents: write` with `GITHUB_TOKEN`. Manual workflow
dispatch validates and builds downloadable artifacts without publication, even
when a tag is selected. Branch dispatch checks metadata without requiring a tag.
There is no Docker, signing or crates.io publishing step.

## Hosted evidence and release acceptance

The public destination is [adeelahmad/anytopdf-rs](https://github.com/adeelahmad/anytopdf-rs).
The initial private run was prevented from starting by an account billing/spending
restriction. After the authorized move to public visibility, baseline and recovery
CI succeeded on Linux, macOS and native Windows; the latest pre-delivery run is
[37150925764](https://github.com/adeelahmad/anytopdf-rs/actions/runs/37150925764).
This is prior CI evidence. Final delivery-candidate CI, five-target packaging and
published release acceptance remain pending in this source snapshot.

Before claiming a release, record its exact commit/tag, successful matching hosted
run and published non-draft release. Download all 11 assets, check the exact
MUSL/Darwin/MSVC names, inspect archive contents and recompute SHA-256 hashes.
Compare the published body with `scripts/release.py notes` at the tagged commit.
Run extracted binaries on compatible hosts and retain native hosted smoke evidence
for other targets. Local tests and mocked recovery scenarios prove their cases;
Wine, Rosetta and cross-build checks are distinct from native hosted execution.
Record provider versions and live PDF text/visual checks separately. docTR remains
unvalidated until its actual model execution succeeds.

## Distribution scope

Signing/notarization and automatic updates are not configured. Optional OCR/media
providers and fonts remain runtime prerequisites for their respective features.
Complex-script shaping and bidirectional layout are not guaranteed. Runtime
plugins are trusted executables; time/output limits are not an OS sandbox.
