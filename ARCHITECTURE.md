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

### Source enrichment

Adds facts that belong to the whole source:
ExifTool, ffprobe, hashes, provenance, GPS, dates, etc.

### Unit enrichment

Adds OCR, captions, transcripts, object labels, scene labels, face bounds, and
other annotations to individual units.

### Planning

The current default is one visual unit per visual PDF page plus visible text
pages for text-only units. A future planner plugin can group contact sheets,
storyboards, or source-specific layouts.

### Rendering

The renderer sees only the normalized graph.

For visual pages:
1. paint the original/derived image;
2. map OCR regions to PDF coordinates;
3. write OCR with text rendering mode 3;
4. write metadata/captions/semantic annotations invisibly;
5. preserve source/timestamp/provider provenance in searchable text.

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
- `anytopdf-plugin-whisper`
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
`PLUGIN_PROTOCOL.md`; the host does not claim OS-level sandboxing.

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

## CLI contract

Failures map to exit codes 0-7. `--events` attaches an observer to the pipeline that writes NDJSON progress to stderr; the CLI emits the terminal `run.finished`. `--json` output follows published schemas, and the
`capabilities` command reports available providers.
