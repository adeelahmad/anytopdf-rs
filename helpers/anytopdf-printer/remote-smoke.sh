#!/usr/bin/env bash
# End-to-end check of remote printing: an IPP client prints over TLS with a
# password through `anytopdf print remote` to the real anytopdf-printer helper.
# Expects a PDF owned by the signed-in user (not the name the client claimed),
# the job details in its manifest, and a receipt with the peer address.
#
#   helpers/anytopdf-printer/remote-smoke.sh [anytopdf-printer] [anytopdf]
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
printer=${1:-$here/anytopdf-printer}
anytopdf=${2:-$root/target/release/anytopdf}
python=${PYTHON:-python3}
for tool in "$printer" "$anytopdf"; do
    [ -x "$tool" ] || { echo "remote-smoke: missing executable $tool" >&2; exit 1; }
done
anytopdf=$(cd "$(dirname "$anytopdf")" && pwd)/$(basename "$anytopdf")
fixtures=$root/crates/anytopdf-print/tests/fixtures
helper_port=${SMOKE_PORT:-18641}
front_port=${SMOKE_FRONT_PORT:-18642}

work=$(mktemp -d "${TMPDIR:-/tmp}/anytopdf-remote-smoke.XXXXXX")
mkdir -p "$work/spool" "$work/out"
pids=()
cleanup() {
    for pid in "${pids[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    rm -rf "$work"
}
trap cleanup EXIT

cat > "$work/convert.sh" <<WRAPPER
#!/bin/sh
env | grep '^ANYTOPDF_PRINT_' > "$work/job.env"
exec "$anytopdf" "\$@"
WRAPPER
chmod +x "$work/convert.sh"

ANYTOPDF_BIN="$work/convert.sh" "$printer" server \
    -o server-port="$helper_port" \
    -o spool-directory="$work/spool" \
    -o output-directory="$work/out" \
    -o log-file="$work/server.log" -o log-level=info &
pids+=($!)
for _ in $(seq 1 30); do
    grep -q 'Starting system' "$work/server.log" 2>/dev/null && break
    sleep 1
done
grep -q 'Starting system' "$work/server.log" 2>/dev/null || {
    echo "remote-smoke: print helper did not start" >&2
    cat "$work/server.log" >&2 || true
    exit 1
}

echo 'correct horse' | "$anytopdf" print passwd adeel --users "$work/users.json"
"$anytopdf" print remote --listen "127.0.0.1:$front_port" --upstream "127.0.0.1:$helper_port" \
    --tls-cert "$fixtures/server.pem" --tls-key "$fixtures/server.key" \
    --users "$work/users.json" --receipts "$work/receipts.jsonl" 2> "$work/front.log" &
pids+=($!)

"$python" - "$front_port" "$fixtures/ca.pem" <<'EOF_PY'
import base64, http.client, socket, ssl, struct, sys, time, zlib

port, ca = int(sys.argv[1]), sys.argv[2]

def png():
    width, height = 850, 1100
    rows = b"".join(b"\0" + (b"\x20" if 200 <= y < 260 else b"\xff") * width for y in range(height))
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))

def attr(tag, name, value):
    name, value = name.encode(), value.encode()
    return bytes([tag]) + struct.pack(">H", len(name)) + name + struct.pack(">H", len(value)) + value

def print_job():
    body = struct.pack(">BBHI", 2, 0, 0x0002, 1) + b"\x01"
    body += attr(0x47, "attributes-charset", "utf-8")
    body += attr(0x48, "attributes-natural-language", "en")
    body += attr(0x45, "printer-uri", f"ipps://localhost:{port}/ipp/print/anytopdf")
    body += attr(0x42, "requesting-user-name", "mallory")
    body += attr(0x42, "job-name", "remote-smoke")
    body += attr(0x49, "document-format", "image/png")
    return body + b"\x03" + png()

context = ssl.create_default_context(cafile=ca)
for _ in range(30):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=1).close()
        break
    except OSError:
        time.sleep(0.5)

def post(headers):
    conn = http.client.HTTPSConnection("localhost", port, context=context, timeout=30)
    conn.request("POST", "/ipp/print/anytopdf", body=print_job(),
                 headers={"Content-Type": "application/ipp", **headers})
    response = conn.getresponse()
    return response.status, response.read()

status, _ = post({})
assert status == 401, f"expected 401 without credentials, got {status}"
token = base64.b64encode(b"adeel:correct horse").decode()
status, body = post({"Authorization": f"Basic {token}"})
assert status == 200, f"expected 200, got {status}"
ipp_status = struct.unpack(">H", body[2:4])[0]
assert ipp_status < 0x0100, f"Print-Job failed with IPP status 0x{ipp_status:04x}"
print("remote-smoke: Print-Job accepted over TLS")
EOF_PY

pdf=
for _ in $(seq 1 120); do
    pdf=$(find "$work/out" -name '*-remote-smoke.pdf' -print -quit)
    [ -n "$pdf" ] && break
    sleep 1
done
if [ -z "$pdf" ]; then
    echo "remote-smoke: no PDF after 120 seconds" >&2
    cat "$work/server.log" "$work/front.log" >&2
    exit 1
fi

fail() {
    echo "remote-smoke: $1" >&2
    cat "$work/front.log" >&2
    exit 1
}
grep -qx 'ANYTOPDF_PRINT_USER=adeel' "$work/job.env" || fail "helper did not see the signed-in user: $(cat "$work/job.env")"
"$anytopdf" extract "$pdf" --json > "$work/extract.json"
"$python" - "$work/extract.json" "$work/receipts.jsonl" <<'EOF_PY'
import json, sys
metadata = json.load(open(sys.argv[1]))["manifest"]["sources"][0]["metadata"]
assert metadata.get("print.user") == "adeel", metadata
assert metadata.get("print.job-name") == "remote-smoke", metadata
receipts = [json.loads(line) for line in open(sys.argv[2])]
assert len(receipts) == 1, receipts
receipt = receipts[0]
assert receipt["schema_version"] == "anytopdf.print-receipt/1", receipt
assert (receipt["peer"], receipt["user"], receipt["operation"], receipt["job_name"]) == (
    "127.0.0.1", "adeel", "Print-Job", "remote-smoke"), receipt
EOF_PY
echo "remote-smoke: printed $(basename "$pdf") as adeel through the TLS front"
