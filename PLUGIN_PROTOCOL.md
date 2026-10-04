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
non-finite anchors fail validation.

Units may also carry an optional `unit_pages` field (additive; the host fills it with the
page range a unit occupies in the rendered PDF). Plugins must ignore it on requests and
the protocol stays at `"protocol": 1`.

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

## Host validation and execution policy

Importer responses may omit unit `id` and `source_id`; the host generates unit IDs
and assigns the importing source ID. Supplied source IDs must match the request.
Sources may carry optional `sha256` (lowercase hex of the file bytes) and `size`
(bytes). After all enrichers run, the host re-derives every source and unit ID,
so plugins may omit these fields and any IDs they supply are transient:

- `source.id = UUIDv8(first 16 bytes of SHA-256("anytopdf/source/v1\0" + sha256_hex + "\0" + occurrence))`,
  where `occurrence` counts earlier sources with the same digest.
- `unit.id = UUIDv8(first 16 bytes of SHA-256("anytopdf/unit/v1\0" + source_id + "\0" + position))`,
  where `position` is the unit's index among that source's units in graph order.

Enrichment responses must preserve existing source and unit identities. Graph
responses must retain existing sources and keep valid, unique IDs and references.
Annotations must have a provider, finite coordinates, confidence between 0 and 1
when supplied, and finite, ordered nonnegative time ranges.

Visual paths must be absolute existing paths to the original input or inside the
canonical job workspace. The host resolves symlinks before validating the path.
This validation does not prevent an executable from accessing other files itself.

- `--no-plugins`: bypass runtime discovery entirely.
- `--plugin-timeout SECONDS`: bound each manifest/request invocation (default 60).
- `--allow-plugin-kind KIND`: register only specified capabilities; repeatable.
- `--deny-plugin-kind KIND`: deny capabilities; overrides the allow list.
- `convert --renderer runtime:NAME`: select a registered runtime PDF renderer.

Capability filters control registration, not executable permissions: manifest
commands still run during discovery. Plugins remain trusted native processes,
without filesystem/network isolation or hard CPU/memory quotas. A timeout kills
the direct child; independently spawned descendants are not guaranteed to stop.
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
