# anytopdf-printer

An IPP Everywhere printer that saves every print job as a searchable PDF.

It is a small [PAPPL](https://www.msweet.org/pappl/) printer application. PAPPL
accepts JPEG, PNG, PWG Raster and Apple Raster jobs and rasterizes them at 300 dpi
(sGray or sRGB); PDF jobs are not accepted yet. The helper spools
the pages as PWG Raster and runs `anytopdf convert` on the file. Decoding, OCR and
rendering all happen in anytopdf's `print-raster` importer, so the helper has no
format knowledge of its own.

It is optional and separate from the Rust workspace (see `docs/spikes/pappl.md`):
default builds, release archives and Windows do not need PAPPL.

## Build

Linux and macOS, with PAPPL 1.3 or later:

```bash
sudo apt install libpappl-dev pkg-config   # Debian/Ubuntu
brew install pappl pkg-config              # macOS
make printer                               # from the repository root
```

## Run

```bash
ANYTOPDF_BIN=/path/to/anytopdf \
  ./anytopdf-printer server -o output-directory=$HOME/Printed
```

The first start creates one printer, `anytopdf`, at
`ipp://localhost:8631/ipp/print/anytopdf`. Add it as a normal network printer, or
print from the command line:

```bash
./anytopdf-printer submit -d anytopdf -o job-name=receipt photo.jpg
```

Each job becomes `<output-directory>/<YYYYmmdd-HHMMSS>-job<id>-<job name>.pdf`.
Existing files are never replaced. A job whose conversion fails is marked aborted
and the reason is in the log.

| `-o` option        | Environment              | Default                      |
|--------------------|--------------------------|------------------------------|
| `output-directory` | `ANYTOPDF_PRINTER_OUTPUT`| current directory            |
| `anytopdf`         | `ANYTOPDF_BIN`           | `anytopdf` on `PATH`         |
| `listen-hostname`  | `ANYTOPDF_PRINTER_LISTEN`| `localhost`                  |
| `server-port`      | `ANYTOPDF_PRINTER_PORT`  | `8631`                       |
| `spool-directory`  | `ANYTOPDF_PRINTER_SPOOL` | `$TMPDIR/anytopdf-printer-<uid>` |
| `log-file`         |                          | `-` (stderr)                 |
| `log-level`        |                          | `info`                       |

The spool directory also holds the saved printer configuration. Other PAPPL
sub-commands (`printers`, `jobs`, `cancel`, `status`, `shutdown`) work as usual;
run `./anytopdf-printer --help`.

## Security

Print jobs are untrusted input. The helper listens on localhost by default and
the web interface only allows local administration. Setting
`listen-hostname=*` exposes it to the network without a password; TLS with
authentication is a later step on the roadmap. Conversion runs as the user who
started the server, and the raster importer rejects pages larger than 2^27 pixels
before allocating them.

## Discovery

PAPPL advertises the printer over DNS-SD (Bonjour) when Avahi (Linux) or
mDNSResponder (macOS) is running; without it, add the printer by URI. The
printer speaks IPP Everywhere; it is not AirPrint or Mopria certified.

## Test

```bash
make printer-smoke
```

builds anytopdf and the helper, starts a server on port 18631 with a temporary
spool, prints a Letter page and checks the PDF's page size and embedded manifest.
It needs `pdfinfo` and `pdfdetach` from Poppler.
