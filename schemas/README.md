# Schemas

JSON Schema contracts for every machine-readable `--json` surface.

- One file per payload: `schemas/<name>.schema.json`.
- JSON Schema draft 2020-12 (`"$schema": "https://json-schema.org/draft/2020-12/schema"`), with `"$id": "https://github.com/adeelahmad/anytopdf-rs/schemas/<name>/v<N>"`.
- A required `"schema_version": {"const": "anytopdf.<name>/<N>"}`. Any breaking change bumps `<N>` in both places.
- Only the validator subset is used (`anytopdf_core::schema::validate`): `type`, `required`, `properties`, `additionalProperties`, `items`, `enum`, `const`, `minimum`, `minItems`, `anyOf`, `oneOf`, and local `$ref`/`$defs`. Annotation keywords are ignored; any other keyword fails closed. There are no remote `$ref`s.

The sprint commits exactly 8 files: convert, probe, doctor, plugins, capabilities, extract, manifest and chunks. Under `--json`, stdout carries exactly one JSON document and every diagnostic goes to stderr.
