# Changelog

## 0.3.0

Visual and audio analysis, cross-file search, and many new inputs.

### Behaviour changes

- URLs, email addresses, domains and app names found in OCR, captions, transcripts
  and text become `custom` entity annotations, and dates and times become
  `timestamp` annotations with an ISO 8601 value (relative ones such as "last
  Friday" resolved against the capture date). Both are searchable in the hidden text
  layer and listed per chunk under the new optional `entities` field of
  `anytopdf.chunks/1`. App names come from a gazetteer, window titles and URL
  domains, never bare capitalized words. `--no-entities` turns extraction off and
  `--date-order dmy|mdy` sets how `03/04/2024` is read.
- Images and video keyframes carry up to five dominant colours in the hidden search
  layer and the chunks JSON (`color navy blue #141e64 31%`), so searching "red" or
  "blue" finds the frames those colours dominate. `--colors off` disables it.
  Runtime plugins' `custom` annotations with an `entity` attribute are now searchable too.
- JSON and JSON Lines inputs (`.json`, `.jsonl`, `.ndjson`, or sniffed). Each record of a
  JSON Lines file, top-level array or long API response (`{"data": [...]}`) becomes its own
  searchable chunk of `path: value` lines anchored to its exact bytes, and records share
  pages instead of taking one each. Other documents render as an indented outline. A
  malformed line is kept as text with one `input.lossy-decode` warning.
- Camera RAW photos (CR2, CR3, NEF, ARW, DNG, RAF, ORF, RW2, PEF and more) import as
  image pages from their embedded camera preview, with no external tool. A missing or
  small preview is developed by `sips`, `dcraw_emu`, `dcraw` or ImageMagick
  (`--raw-decode auto|preview|develop`, `ANYTOPDF_RAW_DECODE`). TIFF-based RAW files no
  longer fall through to the TIFF importer, which decoded only a thumbnail.
- Photographed pages are flattened before OCR: the sheet is found, its perspective
  corrected and skewed text lines straightened (`--scan-mode auto|on|off`,
  `ANYTOPDF_SCAN_MODE`, default `auto`). `auto` only changes photos that clearly show a
  page with text on a distinct background, or a page-filling scan with skewed lines;
  the unit records `scan.page` corners and `scan.deskew-degrees`.
- `convert` accepts `http://` and `https://` URLs. Web pages become readable text plus
  a headless Chrome/Chromium/Edge snapshot (`--url-snapshot auto|on|off`), video and
  podcast links are fetched with yt-dlp along with their captions and chapters, and other links are
  imported by content type. Sources record `url.source`, `url.final` and `url.fetched`,
  and the provenance page lists the URL. `--url-mode`, `--url-sub-langs`,
  `--url-max-height`, `--url-max-mb`, `--url-timeout` and `--url-allow-private` (each
  also an `ANYTOPDF_URL_*` variable) tune fetching; loopback and private addresses are
  refused by default. `doctor` lists `yt-dlp` and `chrome`.
- `convert --links FILE` converts every link in a text list, a browser bookmark export
  (Netscape HTML) or Chrome's `Bookmarks` JSON; bookmark folders become nested PDF/A
  bookmarks (`outline.title`, `outline.folders`, kept under `--profile share`) and an
  unreachable link is skipped with `input.unreadable`.
- The HTML importer keeps only the page's single `<main>` (or `<article>`) element
  when one exists and records it as `html.content`.
- The new `anytopdf-plugin-objects` runtime plugin detects objects in images and video
  keyframes with a YOLOv8/YOLO11/YOLOv5 ONNX model (`ANYTOPDF_OBJECTS_MODEL`) on a
  pure-Rust ONNX runtime. Detections become searchable `object` annotations with label,
  confidence, box and frame time, plus a per-frame count such as `objects: 2 person, 1 tie`.
- The new `anytopdf-plugin-clip` runtime plugin embeds every image, page and video
  keyframe with CLIP (ViT-B/32 on the CPU through the pure-Rust `tract` ONNX runtime)
  and tags it with zero-shot scene labels such as `screenshot`, `document`, `outdoors`
  or `beach`. Tags are `scene` annotations with `entity = scene-tag` and land in the
  PDF's hidden text layer, so searching a PDF for "beach" finds beach photos; other
  `scene` annotations (keyframe selection notes) stay out of it. The embedding is
  kept in unit metadata (`clip.embedding`) for the search index and never written
  into the PDF. `anytopdf-plugin-clip --fetch-model` downloads the model with pinned
  SHA-256 checksums. The shared `anytopdf-onnx` crate holds the ONNX helpers.
- `anytopdf-plugin-sentiment`, a bundled opt-in runtime plugin, labels transcript
  segments, caption cues, OCR blocks and text paragraphs `positive`, `negative` or
  `neutral` and tags them `question`, `complaint` or `urgent` (`formal`/`informal`
  with an LLM). Labels are `custom` annotations with `entity` `sentiment`, `tone` or
  `sentiment-overall` and the segment's time range or region, written to the hidden
  text layer so "negative" or "complaint" is searchable. English works offline
  through a built-in VADER lexicon port; `ANYTOPDF_SENTIMENT_LLM_URL` and
  `ANYTOPDF_SENTIMENT_LLM_MODEL` use a local OpenAI-compatible endpoint instead,
  falling back to the lexicon with a warning. Text only: nothing is inferred from
  faces or voices.
- Every option can be set three equivalent ways, rclone-style: `[video] interval` in a
  TOML config file (`~/.config/anytopdf/config.toml`, or `--config PATH`),
  `ANYTOPDF_VIDEO_INTERVAL`, or `--video-interval`, with `--set TABLE.KEY=VALUE` on top;
  later layers win. Each built-in type has a table (`[image]`, `[video]`, `[ocr]`,
  `[captions]`) whose options are generated as `--TABLE-KEY` flags, grouped by type in
  `anytopdf convert --help`; older flag names stay as aliases. Runtime plugins receive
  their table in a new optional `options` request field (protocol stays 1).
  `anytopdf config` shows the effective settings and where each came from; new
  `anytopdf.config/1` and config-file schemas.
- `anytopdf capture screen` records the screen through FFmpeg (avfoundation on macOS,
  gdigrab or ddagrab on Windows, x11grab on Linux, or any `--input-format`/`--input`)
  until `--duration` or Ctrl-C, then converts the recording with the video importer's
  interval, scene-change and dedupe sampling. `anytopdf doctor` adds a "Screen capture"
  section (`capture` in `--json`) with the grabber and, on macOS, the Screen Recording
  permission.
- WhatsApp, Telegram, Slack and iMessage (`imessage-exporter` text) chat exports become
  conversation pages with a `time sender: text` line per message; attachments in the export
  follow the message that sent them. New `--chat-date-order` and `--no-chat-attachments`.
- Location enrichment (`--location on|gps|off`, default `on`). A GPS fix from EXIF,
  XMP, QuickTime `GPSCoordinates` or an ISO 6709 `location` tag becomes a `location`
  annotation reverse geocoded offline to "City, Region, Country" from an embedded
  GeoNames `cities1000` table (CC BY 4.0). Place names in OCR text, captions,
  transcripts and text pages become `location` annotations with
  `attributes.source = text` and are searchable in the PDF's hidden layer.
  GPS-derived locations never enter the hidden layer and `--profile share` drops
  them, as before.
- `anytopdf search` searches every PDF in a local SQLite (FTS5) index, printing the PDF,
  page, time, kind, matching text and source file (`--kind`, `--person`, `--collection`,
  `--limit`, `--json` with the new `anytopdf.search/1` schema). `convert --index`
  records an output with every annotation; `anytopdf index add|list|remove` manages
  existing PDFs from their embedded chunks (`anytopdf.index/1`). The index lives at
  `ANYTOPDF_INDEX` or in the user data directory (`--index-db` overrides) and is never
  written into a PDF. The MCP server gains a `search` tool and `queue serve --search`
  a `GET /v1/search` endpoint.
- `anytopdf ask <pdf-or-directory> "<question>"` and the MCP `ask` tool answer a
  question from converted PDFs. Chunks are ranked with BM25 and returned as numbered
  passages citing PDF, pages, time range and original file. With `ANYTOPDF_LLM_URL`
  set to an OpenAI-compatible server the answer cites them as `[n]`; an unreachable
  endpoint adds an `ask.llm-failed` warning and falls back to the passages. New
  `anytopdf.ask/1` schema.
- `anytopdf setup whisper [--model NAME] [--from FILE] [--base-url URL]` turns on
  speech-to-text in one step: it downloads a whisper.cpp ggml model (default `base`)
  into the user data folder (`ANYTOPDF_DATA_DIR`, or the platform's per-user data
  folder), verifies the SHA-1 whisper.cpp publishes, records the model with its SHA-256
  in `whisper/setup.json`, and says what else transcription still needs.
- Bundled runtime plugins (`plugins/` beside the binary, Homebrew's `libexec/plugins`,
  `<data dir>/plugins`) are found without `ANYTOPDF_PLUGIN_PATH` and run once their
  manifest reports the new optional `ready: true`; `detail` explains what is missing.
  The Whisper plugin is ready when an engine, a model and FFmpeg are all present.
  `anytopdf doctor` gains a Transcription section (`transcription` in `--json`), and
  `capabilities` lists idle bundled plugins with the step that turns them on.
  `install.sh` now puts plugins in `<data dir>/plugins`.
- `convert --draw-boxes[=KINDS]` draws labelled vector boxes for annotation regions
  (`objects`, `faces`, `ocr`, or `all`; bare `--draw-boxes` means `objects,faces`) over
  image and video-frame pages in both renderers. Face boxes show the matched person's
  name when a recognizer supplies one. The source image is untouched and the overlay
  is a PDF/A artifact, so the text layer and page count are unchanged.
- OCR words in the PDF/A text layer are stretched to their OCR boxes, so search and
  selection highlights cover the word in the image instead of the font's natural width.
- An OCR provider that fails during `--ocr auto` fallback is reported by its last error
  line (for docTR, `ModuleNotFoundError: No module named 'doctr'`) instead of a full
  Python traceback.
- `install.sh` installs the latest release on macOS and Linux
  (`curl -fsSL https://raw.githubusercontent.com/adeelahmad/anytopdf-rs/main/install.sh | sh`),
  verifying the archive's SHA-256 checksum first.
- `--plugin-sandbox off|contain|strict` (default `off`) confines runtime plugins.
  `contain` kills every process a plugin starts when its call ends or times out;
  `strict` also limits writes to the job workspace and blocks network access (Landlock and
  seccomp on Linux, `sandbox-exec` on macOS) and exits 2 where it cannot be enforced.
  `--plugin-sandbox-allow-read PATH` adds readable paths under `strict`.
  `anytopdf queue` defaults to `contain`, so no process a plugin starts outlives a
  queued job; pass `--plugin-sandbox off` or `strict` to choose another level.
- `anytopdf-plugin-vlm` asks a local OpenAI-compatible vision model (Ollama,
  llama.cpp, LM Studio) or the Moondream API about every image and video keyframe:
  a caption, what it shows and the activities in it, plus optional open-vocabulary
  detection. An after-units graph enricher adds a summary page with a category and
  topics for each video, audio or subtitle source and per-scene summaries for videos.
  Graph enrichers may declare phase `after-units`; the protocol stays 1.
- `anytopdf-plugin-faces` finds faces in images and keyframes with an embedded YuNet
  model and records neutral presence, count, bounds and landmarks only.
  `anytopdf-plugin-face-id` embeds each detected face with a user-supplied ONNX model,
  and `--recognize-faces` matches the embeddings against a local face index so face
  boxes and `--person` searches carry the matched name. No age, gender, emotion or
  other trait is estimated.
- `anytopdf-plugin-audio-events` adds an "Audio events" page per audio or video
  source listing speech, music, noise and silence segments, raised voices and, with an
  AudioSet model, events such as laughter, applause or sirens, with their time ranges.
- `anytopdf watch imap` gains `--allow-from` and `--require-dmarc` sender checks,
  `--auth xoauth2` (`ANYTOPDF_IMAP_OAUTH_TOKEN` or `--oauth-token-file`) and
  `--queue DIR` hand-off; the `imap` feature is now default and ships in the musl
  release archives and the container.
- Security hardening from the threat model: panics in importers, enrichers and the
  renderer skip that input instead of aborting the run; directory scans skip symlinks;
  `--profile share` redacts plugin directories from diagnostics; FFmpeg and ffprobe
  run with `-protocol_whitelist file`; `--overwrite` through a hard link replaces only
  the link; `extract` exits 3 on cyclic PDFs.
- Release archives, Homebrew, Scoop and the container now bundle every workspace
  runtime plugin (`audio-events`, `clip`, `face-id`, `faces`, `objects`, `sentiment`,
  `vlm`, `whisper`); each stays idle until it reports `ready` or is put on
  `ANYTOPDF_PLUGIN_PATH`.

### Commit history

#### Features

- imap: add sender allowlist, XOAUTH2, queue hand-off and ship in releases (#29) (b6e66d91)
- core: add opt-in runtime plugin sandbox (#12) (346fab37)
- queue: run queued jobs under --plugin-sandbox contain by default (#31) (af680a34)
- dist: serve the v0.2.0 Homebrew formula and Scoop manifest from this repository (#33) (23ba3474)
- launch kit with install.sh, README rewrite and word-aligned OCR highlights (#32) (545d7831)
- pdf: draw labelled annotation boxes on visual pages with --draw-boxes (#34) (3f0ccad1)
- vlm: describe keyframes and summarize videos with a local vision model (#41) (a783263e)
- cli: add `anytopdf capture screen` for live screen recording (#35) (f1e44982)
- builtin: annotate images and keyframes with their dominant colours (#40) (a1a5c323)
- plugins: detect objects in images and video keyframes with YOLO (#48) (b83f6f0f)
- builtin: extract URLs, app names, dates and times from OCR, captions and transcripts (#36, landed in #48) (b83f6f0f)
- builtin: import JSON and JSON Lines as per-record searchable chunks (#45, landed in #48) (b83f6f0f)
- builtin: import WhatsApp, Telegram, Slack and iMessage chat exports (#46, landed in #48) (b83f6f0f)
- builtin: import camera RAW photos and flatten photographed pages before OCR (#52, landed in #48) (b83f6f0f)
- cli: convert URLs, link lists and browser bookmark exports (#47, landed in #48) (b83f6f0f)
- builtin: reverse geocode GPS fixes and extract place names offline (#39, landed in #48) (b83f6f0f)
- index: search across all converted files with a SQLite index (#38, landed in #48) (b83f6f0f)
- cli: answer questions about converted PDFs with anytopdf ask and an MCP tool (#43, landed in #48) (b83f6f0f)
- whisper: one-step transcription setup (#37, landed in #48) (b83f6f0f)
- cli: rclone-style options from one definition: config file, env and flags (#49) (30e62c7c)
- faces: add face detection runtime plugin with embedded YuNet (#42, landed in #49) (30e62c7c)
- clip: embed images with CLIP for search by meaning and zero-shot scene tags (#44, landed in #49) (30e62c7c)
- plugin: add sentiment and tone runtime plugin (#51, landed in #49) (30e62c7c)
- plugins: add audio-events plugin for speech, music, raised voices and sound events (#50, landed in #49) (30e62c7c)
- faces: recognize people from a local face index (#53, landed in #49) (30e62c7c)

#### Fixes

- security: resolve threat model open questions with tests and fixes (#6) (0e848585)

#### Maintenance

- security: model the opt-in plugin sandbox and the IMAP watcher (#30) (b73c6dfd)

## 0.2.0

Searchable-PDF fidelity, identity, and CLI contract release.

### Behaviour changes

- `anytopdf queue` (`add`, `work`, `status`, `secret`) adds a folder-backed job queue
  with a watched inbox and Standard Webhooks (`job.received`, `job.completed`,
  `job.failed`) signed with HMAC-SHA256 and retried from a durable outbox; new
  `anytopdf.job/1` and `anytopdf.webhook/1` schemas. Other commands are unchanged
  and open no sockets.
- `anytopdf queue serve` accepts authenticated HTTP uploads as queue jobs and serves
  their status and PDFs: loopback by default, TLS required off loopback, a bearer
  token from `ANYTOPDF_QUEUE_TOKEN`, and `--max-upload-mb` (default 100).
- `extract` exits 3 (input) when a version-matched manifest or chunks file
  does not match its schema, naming the document and the first failing JSON path;
  other `schema_version` values still warn and exit 0.
- `--events` streams NDJSON progress events (`anytopdf.events/1`) to stderr; failing runs
  end with one failed `run.finished` and print no human `error:` line.
- The invisible text layer is content-only; provenance moves out of it (supersedes the
  earlier provenance-in-text-layer behaviour).
- A visible back-matter provenance page is added; disable it with `--no-provenance-page`.
- `--profile archive` and `--profile share` select output profiles.
- Sources carry content-derived IDs, SHA-256 and size.
- Multi-frame TIFF and GIF inputs import every frame as a page with a `frame` region anchor; `--max-image-frames N` caps the count (0 = unlimited) and warns `input.frames-not-imported`.
- HTML files (`.html`, `.htm`, `.xhtml`, or any file opening with an HTML doctype) import
  as one text page; scripts, styles and markup are dropped and the title is kept.
- Email files (`.eml`, `.mbox`, or header-sniffed) import as one text page per message
  (subject, sender, recipients, date, body; HTML-only bodies converted). Attachments are
  written into the job workspace under sanitized names and imported by whichever importer
  matches; ones that cannot be imported warn `input.members-not-imported`.
- PDF inputs import page by page. With Poppler, `pdftoppm` renders each page and the
  PDF's text layer (`pdftotext -bbox-layout` lines, else the built-in extractor) becomes
  the page's searchable annotations, marked as a native text layer so OCR skips the page.
  Without Poppler, page text becomes text pages and `provider.missing` is reported.
- Builds with the `imap` cargo feature add `anytopdf watch imap`, which converts each new
  message in one IMAP mailbox into a PDF through the email importer. TLS is required off
  loopback, the password never reaches the child `convert`, and progress is kept in a
  state file keyed by UIDVALIDITY with bounded retries and a `failed/` folder.
- HEIC/HEIF/AVIF photos import as image pages (OCR included) when `sips` (macOS),
  `heif-convert` (libheif) or ImageMagick is installed; otherwise they fail with a message
  naming those tools.
- Zip, tar and gzipped tar archives import as an index page plus each member through its
  own importer. Extraction is bounded (512 MiB per member, 1 GiB per archive, 10,000
  entries, 200:1 zip ratio, 2 GiB and 10,000 members per input across nesting); names
  are reduced to one safe component and links are skipped. Zip-based documents (OOXML,
  ODF, EPUB, JAR/APK) are not treated as archives.
- Importers can expand containers through `Importer::import_with_members`; nesting stops
  at `MAX_MEMBER_DEPTH` (4).
- Units carry anchors (time span, region, byte range) and page ranges (`unit_pages`).
- PWG Raster and Apple Raster (URF) print jobs import one page per unit and keep their
  paper size through the `visual.dpi` unit value; the optional `anytopdf-printer` PAPPL
  helper turns IPP print jobs into searchable PDFs.
- Output is reproducible; `SOURCE_DATE_EPOCH` fixes embedded timestamps.
- The graph dump no longer contains workspace paths.
- Diagnostics use typed codes (for example `plugin.warning`).
- `--strict` ignores informational notices.
- Exit codes 0-7 classify failures; batch conversion tolerates failures unless `--fail-fast`.
- `convert` is implicit, `-o` may appear anywhere, and default output naming is derived from the input.
- `--json` output with published schemas, and a `capabilities` command.
- `capabilities` without `--json` prints a table of available, partial and missing
  capabilities in this environment with hints to enable them; use `--json` for the JSON document.
- The PDF embeds a manifest and chunks; `extract` recovers attachments.
- Input fidelity: video frames are validated, file types are sniffed, lossy decoding is
  reported, and `--transcript` supplies audio transcripts.
- `--output-dir` selects the batch output directory.
- Office documents (Word, Excel, PowerPoint, OpenDocument, RTF) import through
  headless LibreOffice and Poppler: one page image per page, with the document's
  positioned text from `pdftotext` and no OCR on those pages. `doctor` reports
  `soffice`, `pdftoppm` and `pdftotext`.
- The `anytopdf-plugin-whisper` runtime plugin transcribes audio and video with
  whisper.cpp or an OpenAI-compatible Whisper CLI into timed transcript pages.
- Updated help text.
- Release builds enable LTO and strip symbols.
- `--renderer pdfa` writes tagged PDF/A-3a through krilla: embedded fonts, XMP
  metadata, an sRGB output intent, a structure tree, bookmarks per source, and the
  manifest and chunks as associated files. Lines are reordered with the bidi
  algorithm and shaped, and characters fall back to further fonts (`ANYTOPDF_FONT`
  may list several). The hidden layer uses fill opacity 0 instead of text rendering
  mode 3. `extract` now reads compressed attachments.
- `--renderer pdfa` is the default; `--renderer pdf` keeps the printpdf output. When
  `ANYTOPDF_FONT` is unset, `pdfa` embeds a bundled DejaVu Sans (Bitstream Vera
  licence, `crates/anytopdf-pdf/fonts/LICENSE-DejaVu.txt`).
- The Rust toolchain is 1.92.0.

### Commit history

#### Features

- builtin: detect provider tool versions (fe76de14)
- core: typed diagnostics with stable codes and severities (7130ae54)
- core: content-derived source and unit IDs with SHA-256 (e714b197)
- cli: coded diagnostics output and severity-based strict mode (b7ff3632)
- builtin: warn when multi-frame images import only the first frame (ddaabcf4)
- builtin: sniff text content and decode non-UTF-8 lossily (afcf848f)
- cli: distinct exit codes per failure class (06f401fd)
- core: kind-appropriate source anchors on every unit (3679f260)
- cli: skip failing inputs by default with a run summary; add --fail-fast (287294c8)
- pdf: byte-reproducible output with deterministic IDs and dates (77a23ce9)
- cli: convert without a subcommand; accept -o and --output anywhere (a31f3c5d)
- core: fail-closed JSON Schema subset validator (0cc6f3aa)
- pdf: provenance back-matter page and per-unit page map (c5837acf)
- pdf: deterministic embedded file attachments (501c8c3e)
- cli: --json for probe, doctor and plugins with published schemas (4cf04aec)
- core: archive and share privacy profiles (537951d8)
- core: versioned manifest and chunk set with JSON schemas (553ec066)
- cli: --profile, --no-provenance-page and SOURCE_DATE_EPOCH reproducibility (73cc3796)
- cli: embed manifest and chunks in every converted PDF (93df0d4b)
- cli: extract embedded manifest and chunks as JSON (a37c933f)
- cli: default output naming with numbered no-clobber fallback (ad5924db)
- cli: convert --json result payload with run summary (55579aa0)
- cli: capabilities command listing codes, profiles and schemas (165e55d9)
- cli: --output-dir writes one PDF per input (6c4116c2)
- cli: help text on every subcommand and flag; list --ocr choices (9acb9337)
- core: add an optional frame index to region anchors (86dbcb52)
- schemas: add anytopdf.events/1 schema and list it in capabilities (16320efe)
- builtin: import every TIFF page and GIF frame as its own unit (1056711a)
- cli: implement NDJSON event writer (9ba4af46)
- core: emit pipeline events from ingest_observed (f4dc7da5)
- cli: add --events NDJSON progress stream (ef6632e3)
- cli: end failing --events runs with one failed run.finished and document --events (79ef2d70)
- cli: add --max-image-frames to cap multi-frame TIFF/GIF import (97121777)
- builtin: import HTML pages as text (#1) (77eb87ab)
- cli: print a capabilities table for this environment (#3) (f284e2ea)
- builtin: import email messages and their attachments (#8) (3f553d5c)
- add MCP server and package manager distribution (#7) (8e4862d1)
- pdf: add krilla PDF/A-3b renderer behind --renderer pdfa (#11) (82aa1963)
- builtin: import Office documents through LibreOffice (#4) (0e925900)
- print: remote printing front with TLS, users and discovery (#10) (fda18c0d)
- cli: add folder-backed job queue with signed webhooks (#5) (71261a6c)
- pdf: tagged PDF/A-3a with bookmarks, bidi shaping and font fallback (#18) (d9587863)
- print: stamp signed-in users, write receipts and report printing in doctor (#17) (b9bae4c2)
- plugin: add Whisper transcription runtime plugin (#16) (f3889f35)
- builtin: import zip and tar archives with extraction limits (#13) (c300a03b)
- cli: accept authenticated HTTP uploads into the job queue (#20) (16cfa75c)
- print: add IPP print server helper and print-raster importer (#14) (af6b0379)
- cli: make the tagged PDF/A-3a renderer the default (#21) (5839b22f)
- builtin: import HEIC/HEIF/AVIF photos and existing PDFs (#23) (3787e375)
- imap: watch an IMAP mailbox and convert each new message (#9) (9a003182)

#### Fixes

- pdf: keep the hidden text layer content-only (936017a3)
- builtin: associate or flag --transcript with several media sources (0d7db2ce)
- cli: protect sources on Windows when resolving output names (f8bbc661)
- cli: redact absolute paths from diagnostics under the share profile (8cc611a3)
- cli: derive provider exit class from a typed exhaustion marker (17111780)
- cli: unconditional dead_code expect on events module (5b4b7abe)
- cli: reject manifests and chunks that fail their schema in extract (bae069ed)
- cli: build census relative names with forward slashes (b5c24c0f)

#### Maintenance

- record verified v0.1.0 release acceptance (2fa92fac)
- spikes: full-build feasibility findings (af6eb509)
- spikes: PAPPL printer feasibility findings (bb86d6ec)
- spikes: layout and PDF writer findings (94eb382a)
- enable LTO and symbol stripping for release builds (8e483b4e)
- add roadmap section to README (b8d9d9ae)
- sync changelog, architecture and plugin protocol with sprint 2 (47b16f64)
- cli: doctor text output uses shared provider detection (221daff6)
- canonicalize temp roots for Windows short and verbatim paths (1f9aaddb)
- pdf: add source layout tests for the lib.rs split (55ad8691)
- add CLI source layout tests (red) (4082cdff)
- pdf: split lib.rs into provenance, layout and fonts modules (a4d32c72)
- cli: split main.rs into cli, convert, commands and publish modules (51599199)
- cli: split main.rs into cli, convert, commands and publish modules (fc477130)
- pdf: split lib.rs into provenance, layout and fonts modules (689406b8)
- core: red tests for optional frame on region anchors and single-frame golden (c1510182)
- promote frame field on Anchor::Region to canonical definition (7c31916a)
- protocol: note optional frame on region anchors (3373fd40)
- basename helper usage and filename guards (7ce74dfd)
- schemas: add RED tests for the events schema and capabilities listing (3506d8e4)
- builtin: use shared basename helper for source names (ac025274)
- pin typed provider-exhaustion marker (RED) (7fff7f30)
- scaffold provider-class symbols for S3-04-T1 (bf1dc2df)
- builtin: multi-frame image import RED (5b584a1c)
- core: add failing pipeline observer tests (52acca83)
- add failing NDJSON event writer tests (e1d43dce)
- cli: scaffold NDJSON event writer stubs (7da12295)
- builtin: scaffold ImageImporter frame cap and max_image_frames option (2f412db0)
- core: relocate pipeline observer tests to integration test (f1acf4ef)
- scaffold pipeline events stubs (d4489077)
- cli: add failing --events stream tests (869cf8b4)
- red tests for failed-run terminal events, broken pipe and docs (20082bd7)
- red tests for --max-image-frames and multi-frame conversion (a10c0fbc)
- cli: red test for core sha256_hex in manifest and profiles (c5878d24)
- cli: hash with anytopdf_core::sha256_hex in manifest and profiles tests (347e0b45)
- cli: red tests for extract schema validation (fd531f7e)
- keep extract schema phrase on one line in changelog (ef0a1d83)
- cli: red census for single PNG fixture tree and byte-pin guards (f9079f69)
- cli: one shared PNG fixture tree under tests/common (2b686fe3)
- cli: red census for shared schema, JSON and repo-file helpers (74322923)
- cli: share schema, JSON and repository-file test helpers (4e483563)
- cli: red census for shared command and stderr helpers (fcbc1373)
- cli: share command and stderr process helpers (bc4425d9)
- cli: red census for shared event, page-count and string helpers (76747cf8)
- cli: share event, page-count and string test helpers (753b42e4)
- sign off sprint 4, refresh roadmap, add security policy (4c245a88)
- bump the Rust toolchain from 1.88.0 to 1.92.0 (#15) (3e269ce2)
- bring roadmap up to date and flag unmodeled surfaces (#19) (b8c95dc1)
- ship the Whisper plugin in release archives, packages and the container (#22) (e1f39da8)
- record queue upload, archives and Whisper in roadmap and threat model (#24) (5b608685)
- print: print over TLS through the remote front to the real helper (#25) (61f71d73)
- record the krilla PDF/A-3a default renderer and tick shipped roadmap items (#26) (bf7665af)

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

### Commit history

#### Features

- establish searchable media PDF converter and release tooling (a7096d09)

#### Fixes

- bootstrap: verify prerequisites before reporting readiness (a49898dd)
- release: validate workspace lock versions before publication (0bdf0f7a)
- release: validate published assets and recover interrupted releases (7993aec1)

#### Maintenance

- enforce release policy and verify publication inventory (775e9472)
- resolve Bash explicitly for Windows workflow fixtures (99785443)

