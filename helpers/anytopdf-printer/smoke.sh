#!/usr/bin/env bash
# End-to-end check: start anytopdf-printer, print a page, expect a searchable
# Letter-sized PDF in the output directory.
#
#   helpers/anytopdf-printer/smoke.sh [anytopdf-printer] [anytopdf]
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
printer=${1:-$here/anytopdf-printer}
anytopdf=${2:-$here/../../target/release/anytopdf}
python=${PYTHON:-python3}
for tool in "$printer" "$anytopdf"; do
    [ -x "$tool" ] || { echo "smoke: missing executable $tool" >&2; exit 1; }
done
anytopdf=$(cd "$(dirname "$anytopdf")" && pwd)/$(basename "$anytopdf")

work=$(mktemp -d "${TMPDIR:-/tmp}/anytopdf-printer-smoke.XXXXXX")
mkdir -p "$work/spool" "$work/out"
server=
cleanup() {
    if [ -n "$server" ]; then
        kill "$server" 2>/dev/null || true
        wait "$server" 2>/dev/null || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT

# A Letter page at 100 dpi with dark bars, written with the standard library only.
"$python" - "$work/page.png" <<'EOF'
import struct, sys, zlib
width, height = 850, 1100
rows = []
for y in range(height):
    dark = 200 <= y < 260 or 400 <= y < 430
    rows.append(b"\0" + (b"\x20" if dark else b"\xff") * width)
def chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
png = b"\x89PNG\r\n\x1a\n"
png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(b"".join(rows)))
png += chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
EOF

# Record the job metadata the helper hands to the converter.
cat > "$work/convert.sh" <<WRAPPER
#!/bin/sh
env | grep '^ANYTOPDF_PRINT_' > "$work/job.env"
exec "$anytopdf" "\$@"
WRAPPER
chmod +x "$work/convert.sh"

ANYTOPDF_BIN="$work/convert.sh" "$printer" server \
    -o server-port="${SMOKE_PORT:-18631}" \
    -o spool-directory="$work/spool" \
    -o output-directory="$work/out" \
    -o log-file="$work/server.log" -o log-level=info &
server=$!

# Client sub-commands start a default server when none answers, so wait for
# this one before running any of them.
for _ in $(seq 1 30); do
    grep -q 'Starting system' "$work/server.log" 2>/dev/null && break
    kill -0 "$server" 2>/dev/null || break
    sleep 1
done
grep -q 'Starting system' "$work/server.log" 2>/dev/null || {
    echo "smoke: server did not start" >&2
    cat "$work/server.log" >&2 || true
    exit 1
}
"$printer" printers | grep -qx anytopdf

"$printer" submit -d anytopdf -o job-name=smoke-test "$work/page.png"

pdf=
for _ in $(seq 1 120); do
    pdf=$(find "$work/out" -name '*-job1-smoke-test.pdf' -print -quit)
    [ -n "$pdf" ] && break
    sleep 1
done
if [ -z "$pdf" ]; then
    echo "smoke: no PDF after 120 seconds" >&2
    cat "$work/server.log" >&2
    exit 1
fi

info=$(pdfinfo "$pdf")
echo "$info" | grep -E '^(Pages|Page size):'
echo "$info" | grep -Eq '^Page size: +612 x 792 pts' || { echo "smoke: expected a Letter page" >&2; exit 1; }
pdfdetach -list "$pdf" | grep -q anytopdf-manifest.json || { echo "smoke: manifest missing" >&2; exit 1; }
[ -z "$(find "$work/spool" -name '*.pwg')" ] || { echo "smoke: spool file left behind" >&2; exit 1; }
for expected in ANYTOPDF_PRINT_JOB_NAME=smoke-test ANYTOPDF_PRINT_FORMAT=image/png; do
    grep -qx "$expected" "$work/job.env" || {
        echo "smoke: converter did not receive $expected" >&2
        cat "$work/job.env" >&2
        exit 1
    }
done
echo "smoke: printed $(basename "$pdf")"
