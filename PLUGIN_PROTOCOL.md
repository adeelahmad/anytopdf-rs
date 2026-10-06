# anytopdf runtime plugin protocol v1

The runtime protocol is deliberately file-oriented.

## Manifest

Invocation:

```bash
anytopdf-plugin-example --anytopdf-manifest
```

stdout:

```json
{
  "protocol": 1,
  "name": "example",
  "version": "1.0.0",
  "capabilities": [
    {
      "kind": "importer",
      "extensions": ["igl"],
      "mime_types": ["application/x-igl"],
      "priority": 50
    }
  ]
}
```

Capability kinds:
- `importer`
- `source-enricher`
- `graph-enricher`
- `unit-enricher`
- `renderer`

A `graph-enricher` capability may set `"phase"`: `"before-units"` (the default)
runs before unit enrichers, and `"after-units"` runs after them, for summaries
over what they found or to consume their annotations (`anytopdf-plugin-face-id`
embeds the faces a detector added). The host skips a capability with any other phase and
reports a discovery warning. Hosts that predate the field run every graph
enricher before unit enrichers; the protocol stays at `"protocol": 1`.

Optional readiness fields (both may be omitted; `ready` defaults to `true`):

```json
{ "ready": false, "detail": "whisper.cpp has no ggml model; run `anytopdf setup whisper` to download a model" }
```

`detail` says what the plugin found or still needs; `anytopdf doctor` and
`anytopdf capabilities` print it. `ready` decides whether a *bundled* plugin
runs (see Discovery below). A plugin found on `PATH` or `ANYTOPDF_PLUGIN_PATH`
is registered whatever it reports, so its own warnings explain a missing
dependency during conversion.

## Discovery

The host looks for executables named `anytopdf-plugin-*` in:

1. every directory on `PATH` and in `ANYTOPDF_PLUGIN_PATH` (platform path
   separator): always registered;
2. bundled folders: `plugins/` beside the `anytopdf` executable (release
   archives, Scoop), `../libexec/plugins` from it (Homebrew; symlinks are
   resolved first) and `<data dir>/plugins`: registered only when the manifest
   reports `ready: true`, and skipped when a plugin with the same `name` was
   found in (1).

`<data dir>` is `ANYTOPDF_DATA_DIR` when set, otherwise
`~/Library/Application Support/anytopdf` on macOS, `%LOCALAPPDATA%\anytopdf` on
Windows and `${XDG_DATA_HOME:-~/.local/share}/anytopdf` elsewhere. Plugins
inherit the host's environment, so they can read the same variable to find
per-user state such as downloaded models. `--no-plugins` disables both.

## Request

The core writes a JSON request into the job workspace:

```json
{
  "protocol": 1,
  "operation": "import",
  "workspace": "/tmp/anytopdf-job-...",
  "source": {
    "id": "...",
    "path": "/data/file.igl",
    "detected_type": "application/x-igl"
  }
}
```

Requests may carry an optional `options` object: the plugin's table from the
configuration (`[NAME]` in the config file, where `NAME` is the manifest name and
`-` matches `_`; `ANYTOPDF_NAME_KEY` or `ANYTOPDF_NAME__KEY` in the environment;
`--set NAME.KEY=VALUE` on the command line). Every capability of the plugin gets
the same table. It is omitted when no table is configured; plugins supply their
own defaults. Additive; the protocol stays at `"protocol": 1`.

```json
{
  "protocol": 1,
  "operation": "import",
  "options": {"layers": ["walls"], "dpi": 300}
}
```

Invocation:

```bash
anytopdf-plugin-example \
  --anytopdf-request /tmp/.../request.json \
  --anytopdf-response /tmp/.../response.json
```

The plugin must write the response atomically.

## Response

```json
{
  "protocol": 1,
  "ok": true,
  "warnings": [],
  "units": [
    {
      "kind": "visual",
      "visual_path": "/tmp/.../page-0001.png",
      "visible_text": null,
      "time_range": null,
      "annotations": [
        {
          "kind": "custom",
          "text": "Layer: foo",
          "provider": "example",
          "confidence": null,
          "region": null,
          "time_range": null,
          "attributes": {
            "custom_kind": "igl-layer"
          }
        }
      ]
    }
  ]
}
```

Units may carry an optional `anchor` (tagged by `kind`: `time-span` with
`start_seconds`/`end_seconds`, `region` with `x`/`y`/`width`/`height`, or
`byte-range` with `start`/`end`). The host fills a default anchor when it is
absent (time range, else full-frame region for visual units, else the whole
source byte range) and never overwrites one a plugin supplies. Inverted or
non-finite anchors fail validation. `region` anchors may carry an optional 0-based
`frame` for multi-frame images; protocol version unchanged.

Units may also carry an optional `unit_pages` field (additive; the host fills it with the
page range a unit occupies in the rendered PDF). Plugins must ignore it on requests and
the protocol stays at `"protocol": 1`.

Units whose positioned text came from the document itself (not OCR) may set the
unit metadata entry `"text-layer": "native"`; the built-in OCR enricher skips
those units. The Office importer sets it for pages with extractable text.

Text units may set the unit metadata entry `"layout.flow": "continuous"` to
continue on the page where the previous text unit of the same source ended
instead of starting a new page; use it for many small records (rows, messages,
log entries). Protocol version unchanged.

Annotations of kind `scene` are searchable content (written to the PDF's hidden
text layer) only when they carry the attribute `"entity": "scene-tag"`, as the
CLIP plugin's zero-shot tags do; other `scene` annotations record how a keyframe
was selected and stay out of it.

Unit metadata is never written into the PDF. The CLIP plugin stores each visual
unit's embedding there as `clip.embedding` (base64 of little-endian `f32`, unit
length), with `clip.model` (an identifier that changes with the model files) and
`clip.dim`; a cross-file search index may read these keys.

Plugins may create derived files only under the supplied workspace unless the
user explicitly configured otherwise.

## Compatibility

- Unknown response fields must be ignored.
- Plugins must ignore unknown request fields.
- The protocol number changes only for incompatible wire changes.
- Semantic plugin changes use the plugin's own version.


## Enrichment operations

Runtime plugins may receive:

- `source-enrich`: return an updated `source`
- `graph-enrich`: return an updated `graph`
- `unit-enrich`: return an updated `unit` or append-only `annotations`
- `render`: receive the normalized `graph` plus `output`, and return `render_report`

This allows model-heavy providers (Whisper, YOLO, scene classifiers, proprietary
decoders) to ship independently while still emitting the same normalized
annotation model.

Built-in unit enrichers (such as OCR) run before runtime unit enrichers, and graph
enrichers (such as Whisper) before both, so a unit enricher sees OCR, caption and
transcript annotations. `anytopdf-plugin-sentiment` is one: it answers
`unit-enrich` with append-only `custom` annotations whose `attributes` carry
`entity` (`sentiment`, `tone` or `sentiment-overall`), `label`, a signed `score`,
and `from` (`transcript`, `caption`, `ocr` or `text`). Annotations with an `entity`
attribute are written to the hidden text layer. It reads settings from the
request's `options` object when present (keys such as `llm_model`), falling back
to its `ANYTOPDF_SENTIMENT_*` environment variables.

## Host validation and execution policy

Importer responses may omit unit `id` and `source_id`; the host generates unit IDs
and assigns the importing source ID. Supplied source IDs must match the request.
Sources may carry optional `sha256` (lowercase hex of the file bytes) and `size`
(bytes). The host recomputes both for every source whose path is a readable file
and uses supplied values only for sources it cannot read, such as virtual sources. After all enrichers run, the host re-derives every source and unit ID,
so plugins may omit these fields and any IDs they supply are transient:

- `source.id = UUIDv8(first 16 bytes of SHA-256("anytopdf/source/v1\0" + sha256_hex + "\0" + occurrence))`,
  where `occurrence` counts earlier sources with the same digest.
- `unit.id = UUIDv8(first 16 bytes of SHA-256("anytopdf/unit/v1\0" + source_id + "\0" + position))`,
  where `position` is the unit's index among that source's units in graph order.

Enrichment responses must preserve existing source and unit identities. Graph
responses must retain existing sources and keep valid, unique IDs and references.
Annotations must have a provider, finite coordinates, confidence between 0 and 1
when supplied, and finite, ordered nonnegative time ranges.

The renderer writes `ocr`, `caption`, `transcript`, `object` and `barcode`
annotations into the hidden search layer. A `custom` annotation joins them only
when its `attributes` include `entity` (for example the built-in dominant-colour
enricher's `entity: "color"`); other custom annotations stay in the chunks JSON.

A `custom` annotation with an `entity` attribute (for example
`{"kind":"custom","text":"https://example.com","attributes":{"entity":"url"}}`)
is a structured entity: it is written to the hidden text layer and listed in the
unit's chunk `entities`; so is a `timestamp` annotation with an `entity`
attribute, listed by its `iso` attribute when present. The built-in
`text-entities` enricher runs after runtime unit enrichers, so URLs, app names and
dates in plugin captions and transcripts are extracted too.

Visual paths must be existing paths to the original input or inside the canonical
job workspace. Send absolute paths: a relative path is resolved against anytopdf's
working directory, and the containment check, not absoluteness, is what the host
enforces. The host resolves symlinks before validating the path.
This validation does not prevent an executable from accessing other files itself.

- `--no-plugins`: bypass runtime discovery entirely.
- `--plugin-timeout SECONDS`: bound each manifest/request invocation (default 60).
- `--allow-plugin-kind KIND`: register only specified capabilities; repeatable.
- `--deny-plugin-kind KIND`: deny capabilities; overrides the allow list.
- `--plugin-sandbox off|contain|strict`: confine plugin processes (default `off`;
  `contain` under `anytopdf queue`).
- `--plugin-sandbox-allow-read PATH`: extra readable path under `strict`; repeatable.

### Sandbox levels

The sandbox applies to every plugin execution, including `--anytopdf-manifest`
during discovery.

- `off`: plugins run as ordinary child processes with your permissions.
- `contain`: every process a plugin starts is killed when its call ends or times
  out (a process group on Unix, a job object on Windows). On macOS a descendant
  that calls `setsid` escapes; on Windows a child created in the instant before
  job assignment can escape.
- `strict`: `contain`, plus writes only inside the job workspace and `/dev/null`,
  no network access (including connecting to Unix-domain sockets), and `TMPDIR` pointing at the
  workspace. Reads are limited to system locations, the plugin's own directory,
  the source file, the workspace and `--plugin-sandbox-allow-read` paths. Linux
  enforces this with Landlock and seccomp (x86-64 and AArch64), which also block
  `setsid` and `setpgid`. macOS uses `sandbox-exec`, denies reading file contents
  under `/Users`, `/Volumes` and the home directory outside the allowed paths, and
  does not restrict Mach IPC. Windows has no `strict` level. Where `strict` cannot be
  enforced the command exits 2 before any plugin runs. A renderer plugin writes
  its PDF inside the workspace and the host moves it into place.

Under `strict`, plugins that need an interpreter, virtual environment or model
cache in the home directory must be given it with `--plugin-sandbox-allow-read`.
Design notes: `docs/design/plugin-sandbox.md`.
- `convert --renderer runtime:NAME`: select a registered runtime PDF renderer.

Capability filters control registration, not executable permissions: manifest
commands still run during discovery. With the default `--plugin-sandbox off`,
plugins remain trusted native processes, without filesystem/network isolation or
hard CPU/memory quotas, and a timeout kills only the direct child; independently
spawned descendants are not guaranteed to stop.
Captured output and response JSON are limited to 16 MiB each. Optional plugin
failures become warnings; failed enrichers roll back graph mutations.

Protocol v1 warning strings are unchanged on the wire. The host reports every
plugin-supplied warning string as the diagnostic code `plugin.warning`, even if
it imitates a built-in code, and the protocol number stays 1.

## Working example

On Unix, copy `examples/anytopdf-plugin-example.py` into a trusted plugin directory,
make it executable, and add that directory to `ANYTOPDF_PLUGIN_PATH`:

```bash
mkdir -p ./local-plugins
cp examples/anytopdf-plugin-example.py ./local-plugins/anytopdf-plugin-example
chmod +x ./local-plugins/anytopdf-plugin-example
printf 'Hello from a runtime plugin.\n' > /tmp/document.example
ANYTOPDF_PLUGIN_PATH="$PWD/local-plugins" cargo run -p anytopdf -- \
  convert /tmp/document.example -o /tmp/example.pdf
```

The example imports UTF-8 `.example` documents and writes responses atomically.
It is not an IGL decoder. Windows plugins must be directly executable (for example,
a compiled `.exe`); a Python script alone is not a Windows executable.
