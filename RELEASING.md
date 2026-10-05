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

## Sprint 4 final gate — 2026-10-05

Sprint 4 is signed off on `main` at 9fa5a91 plus documentation-only changes
(ROADMAP, SECURITY and the threat model). Run in a Linux x86-64 container with
Rust 1.88.0:

- `sh scripts/verify.sh` passed: formatting, `cargo check`, Clippy with warnings
  denied, 251 Rust tests with default features and 251 with
  `--no-default-features`, and 29 Python tests.
- `scripts/smoke.py --require-poppler` passed against the release binary: text,
  image, Unicode extraction, pagination, graph and overwrite protection.
- FFmpeg, ffprobe and Poppler were available; ExifTool, Tesseract, docTR and Apple
  Vision were not, so provider-dependent paths rely on hosted CI and earlier
  records.
- Hosted CI on Linux, macOS and Windows passed on 9fa5a91.

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

## Verified v0.1.0 release — 2026-10-04 (Australia/Melbourne)

[Release v0.1.0](https://github.com/adeelahmad/anytopdf-rs/releases/tag/v0.1.0)
is published as a stable, non-draft release from immutable commit
[`e283c57fc20af0b7f2a777812e4c25f7686a4ff8`](https://github.com/adeelahmad/anytopdf-rs/commit/e283c57fc20af0b7f2a777812e4c25f7686a4ff8).
[Release run 37152498275](https://github.com/adeelahmad/anytopdf-rs/actions/runs/37152498275)
succeeded in all 10 jobs: version validation, three operating-system verification
jobs, five native package jobs and publication. Earlier corrected-candidate
[CI run 37152086874](https://github.com/adeelahmad/anytopdf-rs/actions/runs/37152086874)
passed Linux, macOS and Windows at commit `9978544`. The release run independently
verified the released commit. Both `make release` and subsequent
`make release-resume` completed successfully without duplicate release history.

| Released target | Native hosted package and smoke result |
| --- | --- |
| `aarch64-apple-darwin` | PASS on Apple Silicon macOS |
| `x86_64-apple-darwin` | PASS on Intel macOS |
| `aarch64-unknown-linux-musl` | PASS on ARM64 Linux, including Poppler extraction |
| `x86_64-unknown-linux-musl` | PASS on x86-64 Linux, including Poppler extraction |
| `x86_64-pc-windows-msvc` | PASS on native Windows MSVC |

All five package jobs executed their binaries and checked PDF structure, graph
output and overwrite protection. Hosted macOS/Windows jobs did not have Poppler;
those jobs do not establish independent text extraction. The downloaded Apple
Silicon binary separately reported `anytopdf 0.1.0` and passed strict smoke with
host Poppler, including Unicode extraction and pagination.

All 11 published assets were downloaded and independently verified: the five
`anytopdf-0.1.0-<target>` archives (tar.gz for Darwin/MUSL, ZIP for MSVC), their five
`.sha256` sidecars, and `SHA256SUMS`. Every archive hash matched both its sidecar
and aggregate entry. Each archive contained exactly nine regular files beneath
the expected version/target directory: the executable, README, RELEASING,
ROADMAP, ARCHITECTURE, PLUGIN_PROTOCOL, CHANGELOG and both licenses. Paths were
safe, tar entries contained no links, and the ZIP CRC check passed. Published
release notes matched the changelog section at the tagged commit.

The corrected local candidate passed the complete matrix with 43 Rust tests per
default/minimal feature configuration and 29 Python tests, with no skips. Live
acceptance independently extracted searchable text and visually reviewed 18 PDF
pages: two recursive-filter image pages, three patterned video/caption pages,
one independent plugin page and 12 supplementary media/OCR pages. Image OCR and
metadata stayed on their own pages; excluded images were absent and source hashes
were unchanged. Video scenes, timestamps, captions and provenance agreed.
Supplied audio transcripts, Unicode, rotation, long tokens and the external
`.example` plugin were also exercised. These results demonstrate existing media
and plugin behavior, not automatic speech recognition, IGL decoding or remote
storage support.

Native Apple Vision and Tesseract were invoked successfully. Recorded providers
were FFmpeg/ffprobe 6.1.2, ExifTool 12.76, Tesseract 5.5.3 and Poppler 26.08.0,
with an installed Arial font. **docTR model execution remains unvalidated.**
Historical Wine, Rosetta and cross-build results above remain separate from
these native hosted and downloaded-binary checks.

The initial private CI run was blocked before execution by account billing;
public hosted runs subsequently proceeded. Candidate run `37151534301` exposed
a Windows test-helper Bash lookup problem; explicit executable resolution fixed
it and the corrected run above passed. These earlier failures are retained as
history, not successful validation. Publication/download verification also used
a temporary network proxy and isolated GitHub CLI configuration for the active
account; stored accounts were preserved. Default authentication can still report
an invalid inactive account, so this record does not claim that configuration
was repaired.

This evidence update is a separate documentation commit after publication.
The tag and packaged source remain at `e283c57`; documentation bundled in the
archives reflects that earlier snapshot. Future release acceptance should repeat
the matching-run, asset inventory/hash/contents, notes and executable checks.

## Distribution scope

Signing/notarization and automatic updates are not configured. Optional OCR/media
providers and fonts remain runtime prerequisites for their respective features.
Complex-script shaping and bidirectional layout are not guaranteed. Runtime
plugins are trusted executables; time/output limits are not an OS sandbox.

## Release binary size

Measured locally on macOS arm64 (host triple `aarch64-apple-darwin`), not
Linux or Windows proof, with `cargo build --release` at commit `4ccdcce` on
2026-10-04. Enabling `lto = true` and `strip = true` in `[profile.release]`:

- before: 8584064 bytes
- after: 6246336 bytes
