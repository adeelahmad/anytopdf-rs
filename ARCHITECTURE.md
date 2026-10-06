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

### Probe

Every importer can score an input. The registry chooses the highest-priority
matching importer. This enables magic-byte detection, extension matching,
container sniffing, and format-specific probing without modifying core.

### Import

An importer emits normalized units. Examples:

- image -> one visual unit
- video -> keyframe visual units + timestamps
- subtitle -> text/cue units
- text -> visible text unit
- future `.igl` -> arbitrary page/image/text units
- email/archive -> its own units plus the units of each member file

Containers (emails, archives) implement `Importer::import_with_members`. They
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

### Source enrichment

Adds facts that belong to the whole source:
ExifTool, ffprobe, hashes, provenance, GPS, dates, etc.

### Unit enrichment

Adds OCR, captions, transcripts, object labels, scene labels, face bounds, and
other annotations to individual units.

Graph enrichers run before unit enrichers by default (for example, Whisper adds a
transcript unit that later enrichers can see). A graph enricher whose `phase()`
is `GraphPhase::AfterUnits` (runtime capability `"phase": "after-units"`) runs
after them instead, so it can summarize their annotations; `anytopdf-plugin-vlm`
writes video and scene summaries this way.

### Planning

The current default is one visual unit per visual PDF page plus visible text
pages for text-only units. A future planner plugin can group contact sheets,
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

Discovery:
- `PATH`: `anytopdf-plugin-*`
- future config dirs:
  - Linux: `$XDG_CONFIG_HOME/anytopdf/plugins`
  - macOS: `~/Library/Application Support/anytopdf/plugins`
  - Windows: `%APPDATA%\anytopdf\plugins`

Protocol:
- `plugin --anytopdf-manifest`
- `plugin --anytopdf-request request.json --anytopdf-response response.json`

Large data is exchanged through workspace file paths rather than base64 JSON.

## Future plugin examples

- `anytopdf-plugin-igl`
- `anytopdf-plugin-office`
- `anytopdf-plugin-cad`
- `anytopdf-plugin-email`
- `anytopdf-plugin-archive`
- `anytopdf-plugin-whisper` (shipped in `crates/anytopdf-plugin-whisper`)
- `anytopdf-plugin-vlm` (shipped in `crates/anytopdf-plugin-vlm`)
- `anytopdf-plugin-yolo`
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
