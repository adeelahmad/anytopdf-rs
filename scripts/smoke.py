#!/usr/bin/env python3
"""Exercise text/image conversion and, when available, independent PDF extraction."""
import argparse
import json
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tempfile
import zlib


def png(path):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    width, height = 400, 200
    rows = b"".join(b"\0" + bytes((230, 240, 250)) * width for _ in range(height))
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--require-poppler", action="store_true")
    parser.add_argument("--strict", action="store_true", help="Require warning-free conversion; needs ExifTool and a Unicode font")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"binary does not exist: {binary}")
    extractor = shutil.which("pdftotext")
    inspector = shutil.which("pdfinfo")
    if args.require_poppler and not (extractor and inspector):
        parser.error("pdftotext and pdfinfo are required for PDF acceptance checks")
    with tempfile.TemporaryDirectory(prefix="anytopdf-smoke-") as temporary:
        root = Path(temporary)
        text = root / "notes.txt"
        marker = "Searchable acceptance marker"
        long_line = "W" * 180
        text.write_text(f"{marker}\nUnicode: café résumé\n{long_line}\n" + "A readable line of content.\n" * 130, encoding="utf-8")
        visual = root / "image.png"
        png(visual)
        output = root / "result.pdf"
        graph = root / "graph.json"
        command = [str(binary), "--no-plugins", "convert", str(text), str(visual), "--ocr", "off", "-o", str(output), "--dump-graph", str(graph)]
        if args.strict:
            command.append("--strict")
        result = subprocess.run(command, capture_output=True, text=True, timeout=120)
        if result.returncode:
            raise RuntimeError(result.stderr)
        if not output.read_bytes().startswith(b"%PDF-"):
            raise AssertionError("output has no PDF header")
        if output.stat().st_size > 2 * 1024 * 1024:
            raise AssertionError("small document unexpectedly exceeds 2 MiB; check font subsetting")
        data = json.loads(graph.read_text(encoding="utf-8"))
        if len(data["sources"]) != 2 or len(data["units"]) != 2:
            raise AssertionError("conversion lost a source or unit")
        repeated = subprocess.run(command, capture_output=True, timeout=120)
        if repeated.returncode == 0:
            raise AssertionError("conversion overwrote existing output without --overwrite")
        if extractor and inspector:
            extracted = subprocess.check_output([extractor, "-layout", str(output), "-"], text=True, encoding="utf-8")
            if marker not in extracted or "café résumé" not in extracted:
                raise AssertionError("PDF text extraction lost visible or Unicode text")
            if long_line not in re.sub(r"\s+", "", extracted):
                raise AssertionError("PDF text extraction lost a long unbroken line")
            info = subprocess.check_output([inspector, str(output)], text=True)
            pages = re.search(r"^Pages:\s+(\d+)", info, re.MULTILINE)
            if not pages or int(pages.group(1)) < 4:
                raise AssertionError("expected paginated text plus image")
            print("Smoke passed: text, image, Unicode extraction, pagination, graph, and overwrite protection")
        else:
            print("Smoke passed: PDF header, graph, and overwrite protection; Poppler extraction not available")


if __name__ == "__main__":
    main()
