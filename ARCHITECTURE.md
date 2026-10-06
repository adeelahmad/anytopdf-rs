# Architecture

## Why an asset graph?

A direct `input -> PDF page` abstraction breaks as soon as an input becomes
multimodal. One MP4 can contain:

- video frames
- audio
- one or more subtitle tracks
- container metadata
- GPS/date metadata
- chapter markers

The graph represents the source and all derived units independently, then links
annotations to the exact unit/time/region they describe.

## Normalized model

```
DocumentGraph
  SourceRecord[]
    id
    path
    detected_type
    metadata
  Unit[]
    id
    source_id
    kind
    visual?
    visible_text?
    time_range?
    annotations[]
  Annotation
    kind
    text
    confidence?
    region?
    time_range?
    provider
    attributes
```

`region` is normalized top-left coordinates `[0, 1]`; renderers translate this
into their native coordinate system.

## Stages

### Discovery

Find candidate paths. Discovery is intentionally separate from probing.

URL arguments are fetched first (`crates/anytopdf-cli/src/fetch/`) into a temporary
directory that lives for the conversion: a plain download (plus a headless-browser
PDF snapshot for HTML pages) or a yt-dlp download whose captions are renamed to the
media's sidecar name. The downloaded files replace the URL in the input list, so no
importer knows about URLs; after import, each source from a URL gets `url.*` metadata.

### Probe

Every importer can score an input. The registry chooses the highest-priority
matching importer. This enables magic-byte detection, extension matching,
container sniffing, and format-specific probing without modifying core.

### Import

An importer emits normalized units. Examples:

- image -> one visual unit (camera RAW: its embedded preview or a developed render)
- video -> keyframe visual units + timestamps
- subtitle -> text/cue units
- text -> visible text unit
- JSON / JSON Lines -> one text unit per record (or an outline unit per document section)
- future `.igl` -> arbitrary page/image/text units
- email/archive -> its own units plus the units of each member file
- chat export -> text units per conversation (split at about 12 KiB), each
  followed by the units of the attachments its last message sent

Containers (emails, archives, chat exports) implement `Importer::import_with_members`. They
write members into the job workspace under sanitized single-component names and
hand each path to the pipeline's `MemberImporter`, which probes it against the
registry like a top-level input. Member units are re-parented onto the container
source and tagged with `container.member`, so the source list, `--output-dir`
grouping and manifest still describe the inputs the user named. Members must
resolve inside the workspace, nesting stops at `MAX_MEMBER_DEPTH`, and a member
that cannot be imported is an `input.members-not-imported` warning, not a failed
input. Every top-level input carries one extraction budget
(`MAX_MEMBERS_PER_INPUT`, `MAX_MEMBER_BYTES_PER_INPUT`) that extractors draw on
through `MemberImporter::charge`, so nested archives cannot multiply it.
Runtime plugins keep the plain `import` path.

Structured data (`importers/structured.rs`, model in `structured/`) parses into a
format-neutral `Node` tree, so YAML, TOML or CSV readers only need to produce a
`Node`. Shape detection then decides the units. JSON Lines, a top-level array of
objects, and (in a document too long for one outline) the longest record array
one or two object levels down, as in API responses such as `{"data": [...]}`,
become records: one text unit each, written
as `path: value` lines so a full key path and its value are searchable together,
with a `byte-range` anchor on the record's exact bytes and a
`structured.pointer` (RFC 6901). An API envelope keeps its other members in one
outline unit. Any other document becomes an indented outline, split into one
unit per top-level member when it is long. A malformed JSON Lines line is kept as
plain text with one `input.lossy-decode` warning; an invalid JSON document falls
back to plain text. Recognizers for known shapes (chat exports, logs) can be
added as importers that probe higher, without changing this one.

### Source enrichment

Adds facts that belong to the whole source:
ExifTool, ffprobe, hashes, provenance, GPS, dates, etc.

### Unit enrichment

Adds OCR, captions, transcripts, object labels, scene labels, face bounds,
dominant colours, and other annotations to individual units.

Extracted facts that have no dedicated `AnnotationKind` use `Custom` with an
`entity` attribute (for example `entity = color` with `hex`, `share`, `name`
and `family`). The renderer puts `Custom` annotations into the hidden search
layer only when they carry `entity`.

Unit enrichers run in registration order, built-ins before runtime plugins. A
late unit enricher (`Registry::register_late_unit_enricher`) runs after all of
them, so it can read their output. The built-in `text-entities` enricher is one:
it turns URLs, emails, domains and app names found in OCR, caption, transcript and
visible text into `Custom` annotations with `attributes.entity`
(`url|email|domain|app`) and `attributes.from` (`ocr|caption|transcript|text`);
URLs and domains also carry `attributes.href`, a followable absolute URL; and
dates and times into `Timestamp` annotations with `attributes.entity`
(`date|time|datetime`), `attributes.iso` (ISO 8601) and `attributes.relative` when
resolved against the source's capture date. Each keeps the time range of the cue
or keyframe it came from. OCR words are rebuilt into lines first, so multi-word
names, window titles and dates are found. The renderer adds these to the hidden
text layer and the chunks list them under `entities` instead of repeating them in
the chunk text.

Unit enrichers run in registration order. The `scan` enricher runs before OCR
and is the one built-in enricher that replaces a unit's image: for still photos
it writes a flattened, deskewed derivative into the job workspace and records the
original sheet corners (`scan.page`) and rotation (`scan.deskew-degrees`) on the
unit, so OCR boxes and the rendered page refer to the same corrected image.

The built-in `location` enricher is a late unit enricher (it runs after runtime
plugins' unit enrichers) so it also reads captions that runtime unit enrichers
add. It turns a source's GPS fix into one `location` annotation on the source's
first unit, reverse geocoded against the embedded GeoNames table, and place names
found in OCR text, captions, transcripts and text pages into `location`
annotations with `attributes.source = text`.

Graph enrichers run before unit enrichers by default (for example, Whisper adds a
transcript unit that later enrichers can see). A graph enricher whose `phase()`
is `GraphPhase::AfterUnits` (runtime capability `"phase": "after-units"`) runs
after them instead, so it can summarize their annotations; `anytopdf-plugin-vlm`
writes video and scene summaries this way.

### Face recognition

Detection, embedding and identity are separate. A detector plugin adds `face`
annotations with five-point `landmarks`. `anytopdf-plugin-face-id` (graph
enricher in the `after-units` phase) aligns each face onto the ArcFace template, embeds it with
an ONNX model through tract, tags the annotation with `face.ref` and writes the
vector to `face-id/` in the job workspace, never into the graph. With
`--recognize-faces` the CLI reads those vectors after the pipeline, matches them
against the local face index (`anytopdf-faces`, SQLite), clusters faces that
match nobody as `person-N`, records sightings, sets `attributes.person` and the
annotation text, and appends a "People in …" text unit per source. Face
annotations are part of the hidden search layer.

### Planning

The current default is one visual unit per visual PDF page plus visible text
pages for text-only units. A text unit whose metadata sets `layout.flow` to
`continuous` continues on the page where the previous text unit of the same
source ended (after a blank row) instead of starting a page; a unit that fits on
one page moves to the next page rather than being split. Record importers use it
so each record stays its own chunk without costing a page, and chunks of flowed
units may share page numbers. A future planner plugin can group contact sheets,
storyboards, or source-specific layouts.

### Rendering

The renderer sees only the normalized graph.

For visual pages:
1. paint the original/derived image;
2. map OCR regions to PDF coordinates;
3. write OCR as invisible text;
4. write metadata/captions/semantic annotations invisibly;
5. preserve source/timestamp/provider provenance in searchable text.

With `convert --draw-boxes` (graph metadata `anytopdf.draw-boxes`), both renderers
also draw a vector overlay between steps 1 and 2: a stroked rectangle per `Object`,
`Face` or `Ocr` annotation region of the selected kinds, with a filled label for faces
(`attributes.person` when set, else the annotation text) and objects (text and
confidence). `boxes.rs` computes the geometry once, with a stable colour per kind; the
image file is never modified, and `pdfa` tags the overlay as an artifact.

Two built-in renderers share the layout helpers in `anytopdf-pdf` (`layout.rs`,
`provenance.rs`) and therefore produce the same pages and `unit_pages`:

- `pdfa` (default): krilla 0.8, tagged PDF/A-3a validated by krilla at write time,
  with a structure tree, bookmarks, bidi reordering, rustybuzz shaping and
  per-character font fallback (`pdfa_text.rs`). Its primary font is the bundled
  DejaVu Sans (`fonts/`) unless `ANYTOPDF_FONT` names one. It builds the manifest
  and chunks itself from the graph and its own render report and stores them as
  PDF/A-3 associated files. The CLI detects attachments that already match and
  does not rewrite the file. krilla has no text rendering mode 3, so the hidden layer
  uses a fill opacity of 0.
- `pdf`: printpdf, with text rendering mode 3. The CLI adds the manifest and chunks
  afterwards with lopdf.

## Print jobs

PWG Raster and Apple Raster (URF) are importers like any other: each page becomes
a visual unit with a `frame` anchor and a `visual.dpi` metadata value, and the
renderer sizes that page from its resolution so a 300 dpi Letter job yields a
Letter page. The optional `anytopdf-printer` helper (C, on PAPPL) is a separate
process: it accepts IPP jobs, spools them as PWG Raster and runs
`anytopdf convert`. It is not linked into the Rust binary (see
`docs/spikes/pappl.md`), so default builds and Windows are unaffected, and other
front ends such as remote printing can feed the same importer.

## Extension strategy

### Built-ins

Built-in plugins implement Rust traits. They can be individually feature-gated
to keep static distributions small.

### Runtime plugins

Runtime plugins are executable processes, not `.so`/`.dylib` Rust trait objects.
This keeps the ABI stable and makes plugins language-agnostic.

Discovery (details in `PLUGIN_PROTOCOL.md`):
- `PATH` and `ANYTOPDF_PLUGIN_PATH`: `anytopdf-plugin-*`, always registered
- bundled folders, registered only once the plugin's manifest reports
  `ready: true` (so a shipped plugin with a missing dependency stays quiet):
  - `plugins/` beside the executable, and `../libexec/plugins` (Homebrew)
  - `<data dir>/plugins`, where `<data dir>` is `ANYTOPDF_DATA_DIR` or the
    per-user data folder (`~/Library/Application Support/anytopdf`,
    `%LOCALAPPDATA%\anytopdf`, `${XDG_DATA_HOME:-~/.local/share}/anytopdf`)

`anytopdf setup whisper` installs a checksummed whisper.cpp model into
`<data dir>/whisper/` and records it in `setup.json`; the bundled Whisper plugin
reads that record, so engine + model + FFmpeg present is all it takes to turn
transcription on.

Protocol:
- `plugin --anytopdf-manifest`
- `plugin --anytopdf-request request.json --anytopdf-response response.json`

Large data is exchanged through workspace file paths rather than base64 JSON.

### Configuration

`anytopdf-cli/src/config/` layers built-in defaults, a TOML file, `ANYTOPDF_*`
variables, flags and `--set` into one document shaped like
`schemas/config-file.schema.json`, validates it, and writes the global values back
into the parsed command line. The schema is the single definition of every
built-in option: its title, description, type and default generate the
`--TABLE-KEY` flag (grouped by type in `convert --help`) and the
`ANYTOPDF_TABLE_KEY` variable, so the file key, variable and flag cannot drift.
Top-level tables become `anytopdf_core::PluginOptions`: built-ins read typed
structs from `anytopdf-builtin/src/options.rs` (a test keeps their defaults equal
to the schema's), and runtime plugins get the table named after them as the
request's `options`. Child conversions (queue, MCP, mail watcher) receive the same
`--config`, `--no-config` and `--set` flags.

## Future plugin examples

- `anytopdf-plugin-igl`
- `anytopdf-plugin-office`
- `anytopdf-plugin-cad`
- `anytopdf-plugin-email`
- `anytopdf-plugin-archive`
- `anytopdf-plugin-whisper` (shipped in `crates/anytopdf-plugin-whisper`)
- `anytopdf-plugin-clip` (shipped in `crates/anytopdf-plugin-clip`; CLIP embeddings
  and scene tags, on the pure-Rust tract runtime)
- `anytopdf-plugin-faces` (shipped in `crates/anytopdf-plugin-faces`; see `docs/faces.md`)
- `anytopdf-plugin-objects` (shipped in `crates/anytopdf-plugin-objects`; YOLO ONNX
  models on the shared pure-Rust runtime in `crates/anytopdf-onnx`)
- `anytopdf-plugin-sentiment` (shipped in `crates/anytopdf-plugin-sentiment`)
- `anytopdf-plugin-audio-events` (shipped in `crates/anytopdf-plugin-audio-events`)
- `anytopdf-plugin-vlm` (shipped in `crates/anytopdf-plugin-vlm`)
- `anytopdf-plugin-paddleocr`
- `anytopdf-plugin-cloud-vision`
- `anytopdf-plugin-sharepoint`

## Static distribution

The executable plugin protocol solves an important conflict:

- the core can be delivered as one static Linux binary;
- optional proprietary/model-heavy providers do not have to be linked into it;
- users can add capabilities later without replacing the core;
- external plugins may carry their own native dependencies if needed.

The `doctor` command reports which optional providers are actually usable on the
current host.

## Failure and publication boundaries

The pipeline canonicalizes inputs and its workspace before invoking providers.
It validates imported graph identities and annotation values. Source, graph and
unit enrichers roll back their in-memory changes on failure. Unit providers read
one consistent snapshot per provider pass, avoiding a full graph clone per unit.

The CLI renders into job-local staging, evaluates warnings in strict mode, and
publishes output through a temporary file beside the destination. Source paths
remain protected. JSON graph dumps retain diagnostic workspace paths rather than
copying derived assets. Runtime plugin policy and timeouts are documented in
`PLUGIN_PROTOCOL.md`. OS-level sandboxing is opt-in through `--plugin-sandbox`
(`src/sandbox.rs` in core, `docs/design/plugin-sandbox.md`); by default plugins run
unconfined, except under `queue`, which defaults to `contain`.

## Search index

`crates/anytopdf-index` is a consumer of rendered output, not a pipeline stage.
It keeps one SQLite database (bundled, FTS5) with `documents` (one per indexed
PDF, with its SHA-256, origin, profile and optional collection), `sources`,
`entries` and `embeddings`. `convert --index` records the graph each PDF was
rendered from, after the output profile filter, plus its render report: one
`chunk` entry per unit (the chunk text) and one entry per annotation with kind,
provider, confidence, region, frame, time range, page range and attributes.
`index add` records a PDF from its embedded manifest and chunks (chunk entries
only). Entries are indexed by an external-content FTS5 table kept in sync by
triggers; deleting a document cascades. `embeddings` stores one vector per unit
and model for semantic search, ranked by cosine similarity. `PRAGMA
user_version` versions the layout: an index from a newer anytopdf, or a database
that is not an index, is refused rather than changed.

## Intake: mail watcher

`crates/anytopdf-imap` is an intake channel, not a pipeline stage. It watches one
mailbox, spools each new message as a raw `.eml` file and hands it to a
`MessageSink`. The CLI's sink (feature `imap`, `anytopdf watch imap`) runs
`anytopdf convert` in a child process per message, so a crash or hang on one
message cannot stop the watcher. A job-queue sink can replace it without
changing the watcher. The watcher does no MIME parsing: format knowledge stays
in whichever importer claims RFC 822 input. Progress is a JSON state file keyed by
UIDVALIDITY and written atomically after every message.

## Diagnostics

Every warning or notice is a typed Diagnostic with a stable code (plugin-supplied warning
strings map to `plugin.warning`). Informational diagnostics never fail `--strict`.

## Identity and anchors

Source IDs are content-derived from the file SHA-256 plus an occurrence index, so the same
bytes yield the same ID. Units carry an anchor (time span, region, or byte range) filled by
the host when a plugin omits it.

## Profiles and channels

Output profiles (`archive`, `share`) choose which channels are embedded. The invisible text
layer holds content only; provenance is a visible back-matter page.

## Manifest and attachments

The PDF embeds a manifest and attachment chunks describing the sources. The `extract`
command recovers them.

`ask` reads those chunks back (through `extract`), ranks them against a question
with BM25, and optionally sends the top passages to an OpenAI-compatible chat
endpoint (`ANYTOPDF_LLM_URL`). It is a consumer of the normalized output, like a
RAG client, and never touches the pipeline; every passage keeps its PDF, page
range and time-span anchor so answers stay traceable.

## CLI contract

Failures map to exit codes 0-7. `--events` attaches an observer to the pipeline that writes NDJSON progress to stderr; the CLI emits the terminal `run.finished`. `--json` output follows published schemas, and the
`capabilities` command reports which importers, enrichers, renderers, providers and runtime
plugins this environment supports (`--json` gives the static contract).

## Job queue and webhooks

`anytopdf queue` sits outside the pipeline: it never calls the pipeline in-process.
Each job runs the same `convert` command as a child process with `--events --json`,
so exit codes, NDJSON events and schemas are unchanged and a crashing conversion or
plugin cannot take the worker down. The queue is a directory: job records
(`anytopdf.job/1`) change state by atomic rename between `jobs/pending`, `running`,
`done` and `failed`, which lets several workers share it without locks, and a
running job carries a lease after which another worker requeues it. The watched
inbox needs no listener. `queue serve` is the opt-in HTTP intake: it only writes
uploads into job work directories and enqueues them, binds loopback unless TLS is
configured, and checks a bearer token on every request. Because queued inputs come
from folders and uploads rather than an operator, `queue` defaults runtime plugins
to `--plugin-sandbox contain`; an explicit level overrides it.

Webhook messages (`anytopdf.webhook/1`) are written to `webhooks/pending/` before
they are sent, signed per Standard Webhooks with HMAC-SHA256, and retried with
backoff, giving at-least-once delivery that survives a worker restart. Payloads use
file names and queue-relative paths only.

## Remote printing

`anytopdf-print` sits in front of the print helper and never parses document
data. It terminates TLS (rustls), drops peers outside the CIDR allowlist before
the handshake, checks HTTP Basic credentials against Argon2id hashes, and
forwards each HTTP request to the helper on loopback with the IPP
`requesting-user-name` replaced by the signed-in user; a receipts log records
the peer address the helper never sees. The helper passes job details to
`convert` as `ANYTOPDF_PRINT_*` variables, which become `print.*` source
metadata (manifest and provenance page, dropped by `share`). A guard refuses
non-loopback listeners without users and an allowlist. The same DNS-SD
description feeds the unicast zone snippet and the mDNS advertisement. Jobs
still enter the pipeline through the helper and the normal importers.
