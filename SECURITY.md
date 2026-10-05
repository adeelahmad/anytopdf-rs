# Security Policy

anytopdf is a local CLI: one person converting their own media and documents
into searchable PDFs on their own machine, with their own permissions. It is not
a conversion service and opens no listening socket. This policy summarizes the
threat model in [`docs/threat-model/threat-model.md`](docs/threat-model/threat-model.md),
which is authoritative where the two differ. The model is under maintainer
review; its open questions (§1.18) are not yet decided.

## Reporting a vulnerability

Report privately through GitHub:
<https://github.com/adeelahmad/anytopdf-rs/security/advisories/new>

Do not open a public issue for a suspected vulnerability. Include:

- `anytopdf --version` and the platform;
- the exact command line and the profile (`archive` or `share`);
- a minimal input file, if one is needed to reproduce;
- `anytopdf doctor` output and any installed `anytopdf-plugin-*` executables;
- which property below you believe is broken.

Reports are triaged against the threat model as it stood at the reported
version. Only the latest release and `main` receive fixes; fixes are noted in
`CHANGELOG.md`.

## What counts as a vulnerability

A report is in scope when it breaks a property the project provides (threat
model §1.11). The security-critical ones are:

- **P1** With `--no-plugins`, no runtime plugin executable runs.
- **P2** No file anytopdf writes replaces a discovered input or an explicit
  `--transcript` file, even with `--overwrite`.
- **P7** With `--profile share`, absolute local paths are removed from the
  manifest, chunks, `--dump-graph`, `--json`, events, the summary and
  diagnostics, and metadata is cut to an allow-list.

Correctness properties are also tracked there, including plugin response
validation (P4), subprocess timeouts and 16 MiB capture caps (P5), the
content-only hidden text layer (P8), `extract` exiting 0 or 3 on any hostile PDF
(P9, P17) and the CLI exit-code and event contract (P10).

In-scope adversaries are the author of an input file, the author of a PDF passed
to `extract`, the author of a plugin's response JSON (the response only), and
someone reading outputs shared under `--profile share`.

## Out of scope

These close by design (threat model §1.3, §1.10, §1.12):

- **Plugin code.** An installed plugin is trusted native code running as you.
  There is no OS sandbox, network isolation or CPU/memory/disk quota for plugins
  or providers. `--allow-plugin-kind`/`--deny-plugin-kind` filter registration,
  not execution; only `--no-plugins` stops plugins running. Processes a timed-out
  plugin spawns are not guaranteed to stop.
- **Resource exhaustion from input size or shape.** There are no default size,
  pixel, page, frame, depth or decompression-bomb limits for `convert` or
  `extract` inputs (a subprocess outliving its timeout is still a bug).
- **Privacy under the default `archive` profile**, and personal data in text
  content under `share`. `share` redacts paths and metadata; it is not
  anonymization.
- **Output authenticity.** SHA-256 values identify source bytes; they do not
  sign the PDF, and `extract` does not verify that a manifest matches its PDF.
- **Terminal escapes** in human stderr lines (JSON and NDJSON are escaped).
- **Symlinked caption sidecars**, which are followed. The operator controls the
  input directory layout.
- **Hostile multi-user or service deployment.** Network intake channels are not
  shipped.
- **The operator and the local environment**: flags, `PATH`,
  `ANYTOPDF_PLUGIN_PATH`, `ANYTOPDF_FONT`, `TMPDIR`.
- **External providers' own bugs** (FFmpeg, ffprobe, ExifTool, Tesseract, docTR,
  Apple Vision) when used as documented. Report those upstream.
- `examples/anytopdf-plugin-example.py`, release and CI tooling under
  `scripts/` and `.github/`, test code, and design documents under `docs/`.

## Using anytopdf safely

From the threat model's downstream responsibilities (§1.13):

- Install only trusted plugins, keep `PATH` and `ANYTOPDF_PLUGIN_PATH`
  directories writable only by trusted users, and use `--no-plugins` otherwise.
- Convert untrusted media inside a sandbox or container with resource limits,
  and set `--max-image-frames` and `--max-video-frames` to non-zero values.
- Use `--profile share` before giving outputs to someone else, and review the
  visible and OCR text yourself.
- Sign outputs with your own tooling if you need tamper evidence; treat
  `extract` results as unverified when `origin` is `sidecar` or a
  `extract.version-mismatch` warning appears.
- Escape text from PDFs, `--json`, `extract` and stderr before putting it into
  HTML, terminals, shells or databases; treat file names as attacker-chosen.
- Keep outputs outside the directories you scan, and do not run two conversions
  to the same output at once.
- Install FFmpeg, ExifTool, Tesseract, Python and docTR from trusted sources.
  docTR may download model weights on first use; pre-install them or use
  `--ocr tesseract`/`--ocr off` on offline hosts.
