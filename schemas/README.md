# Schemas

JSON Schema contracts for every machine-readable `--json` surface.

- One file per payload: `schemas/<name>.schema.json`.
- JSON Schema draft 2020-12 (`"$schema": "https://json-schema.org/draft/2020-12/schema"`), with `"$id": "https://github.com/adeelahmad/anytopdf-rs/schemas/<name>/v<N>"`.
- A required `"schema_version": {"const": "anytopdf.<name>/<N>"}`. Any breaking change bumps `<N>` in both places.
- Only the validator subset is used (`anytopdf_core::schema::validate`): `type`, `required`, `properties`, `additionalProperties`, `items`, `enum`, `const`, `minimum`, `minItems`, `anyOf`, `oneOf`, and local `$ref`/`$defs`. Annotation keywords are ignored; any other keyword fails closed. There are no remote `$ref`s.

The repository commits exactly 14 files: convert, probe, doctor, plugins, capabilities, extract, manifest, chunks, events, job, webhook, search, index and ask. `search` (`anytopdf.search/1`) is `anytopdf search --json` and the `queue serve --search` `GET /v1/search` body, and `index` (`anytopdf.index/1`) is `anytopdf index add|list --json`. `ask` (`anytopdf.ask/1`) is the answer and cited passages from `anytopdf ask --json`. `job` (`anytopdf.job/1`) is a queue job record and `webhook` (`anytopdf.webhook/1`) the body of a `queue work --webhook` delivery. `convert --events` writes `anytopdf.events/1` NDJSON to stderr, one object per line, with a gapless `seq` and no timestamps. Under `--json`, stdout carries exactly one JSON document and every diagnostic goes to stderr.
