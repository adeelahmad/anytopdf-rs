#!/usr/bin/env python3
"""Regenerate the README demo from a real anytopdf run.

    cargo build --release -p anytopdf
    python3 docs/demo/record.py [--bin target/release/anytopdf]

Needs Pillow, Tesseract and Poppler (pdftoppm, pdftotext). It draws a phone
photo of a receipt, converts it with the anytopdf binary, then renders:

- receipt.jpg: the input photo
- before-after.png: the photo next to the produced PDF page, with the words a
  search for "total" finds in the PDF's text layer highlighted
- demo.gif: a terminal recording whose output is the commands' real output
"""

import argparse
import random
import re
import shlex
import shutil
import subprocess
import tempfile
import textwrap
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
FONTS = Path("/usr/share/fonts/truetype/dejavu")
MONO = FONTS / "DejaVuSansMono.ttf"
MONO_BOLD = FONTS / "DejaVuSansMono-Bold.ttf"

RECEIPT = [
    ("NORTHWIND HARDWARE", "bold", 44),
    ("118 Harbour Road, Portsmouth", "", 26),
    ("", "", 16),
    ("INVOICE 4471   2026-09-18", "", 28),
    ("--------------------------------", "", 28),
    ("Brass hinges x6   18.00", "", 28),
    ("Wood screws 4x40   6.40", "", 28),
    ("Oak board 1800mm   42.00", "", 28),
    ("Wall anchors x20   9.60", "", 28),
    ("Router bit 12mm   38.00", "", 28),
    ("--------------------------------", "", 28),
    ("SUBTOTAL   114.00", "", 28),
    ("VAT 20%   14.40", "", 28),
    ("TOTAL   GBP 128.40", "bold", 30),
    ("", "", 16),
    ("Paid: VISA **** 4821", "", 26),
    ("Thank you for shopping local!", "", 26),
]


def make_photo(path):
    """A receipt lying slightly askew on a desk, as a phone would shoot it."""
    rng = random.Random(7)
    paper = Image.new("RGB", (760, 980), "#f3efe4")
    d = ImageDraw.Draw(paper)
    y = 60
    for text, weight, size in RECEIPT:
        f = ImageFont.truetype(str(MONO_BOLD if weight else MONO), size)
        x = (760 - d.textlength(text, font=f)) / 2 if weight or size < 28 else 60
        d.text((x, y), text, font=f, fill="#2b2a28")
        y += size + 22
    # paper grain
    px = paper.load()
    for _ in range(60000):
        x, yy = rng.randrange(760), rng.randrange(980)
        r, g, b = px[x, yy]
        k = rng.randint(-14, 8)
        px[x, yy] = (max(0, r + k), max(0, g + k), max(0, b + k))
    paper = paper.filter(ImageFilter.GaussianBlur(0.6))

    desk = Image.new("RGB", (1000, 1200), "#5a4634")
    dd = ImageDraw.Draw(desk)
    for i in range(0, 1200, 6):  # wood grain
        shade = 78 + int(10 * ((i * 37) % 23) / 23)
        dd.line([(0, i), (1000, i + rng.randint(-8, 8))], fill=(shade + 12, shade - 6, shade - 30), width=3)
    desk = desk.filter(ImageFilter.GaussianBlur(2))

    rotated = paper.rotate(-1.2, resample=Image.BICUBIC, expand=True, fillcolor=(0, 0, 0, 0))
    mask = Image.new("L", paper.size, 255).rotate(-1.2, resample=Image.BICUBIC, expand=True)
    shadow = Image.new("RGBA", desk.size, (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 150), (130, 130), mask)
    shadow = shadow.filter(ImageFilter.GaussianBlur(18))
    desk = Image.alpha_composite(desk.convert("RGBA"), shadow).convert("RGB")
    desk.paste(rotated, (105, 95), mask)

    # uneven light from a desk lamp
    light = Image.radial_gradient("L").resize(desk.size).point(lambda v: 255 - v // 3)
    desk = Image.composite(desk, Image.new("RGB", desk.size, "#1a140e"), light)
    desk.save(path, quality=88)


def run(cmd, cwd):
    proc = subprocess.run(cmd, cwd=cwd, shell=True, capture_output=True, text=True)
    out = (proc.stdout + proc.stderr).rstrip("\n")
    if proc.returncode != 0:
        raise SystemExit(f"{cmd!r} failed ({proc.returncode}):\n{out}")
    return out


def search_boxes(pdf, needle, cwd):
    """Word boxes from the PDF's own text layer that match needle."""
    html = run(f"pdftotext -f 1 -l 1 -bbox {shlex.quote(str(pdf))} -", cwd)
    page = re.search(r'<page width="([\d.]+)" height="([\d.]+)"', html)
    words = re.findall(
        r'<word xMin="([\d.]+)" yMin="([\d.]+)" xMax="([\d.]+)" yMax="([\d.]+)">([^<]*)</word>', html
    )
    hits = [tuple(map(float, w[:4])) for w in words if needle in w[4].lower()]
    return float(page.group(1)), float(page.group(2)), hits


def before_after(photo, pdf, cwd, out):
    run(f"pdftoppm -f 1 -l 1 -r 110 -png {shlex.quote(str(pdf))} page", cwd)
    page_png = next(Path(cwd).glob("page*.png"))
    page = Image.open(page_png).convert("RGB")
    pw, ph, hits = search_boxes(pdf, "total", cwd)
    if not hits:
        raise SystemExit("the PDF text layer has no 'total'; OCR did not run")
    sx, sy = page.width / pw, page.height / ph
    overlay = Image.new("RGBA", page.size, (0, 0, 0, 0))
    od = ImageDraw.Draw(overlay)
    for x0, y0, x1, y1 in hits:
        od.rectangle([x0 * sx - 3, y0 * sy - 3, x1 * sx + 3, y1 * sy + 3], fill=(255, 210, 77, 140), outline=(232, 87, 28, 255), width=3)
    page = Image.alpha_composite(page.convert("RGBA"), overlay).convert("RGB")

    h = 620
    left = Image.open(photo).convert("RGB")
    left = left.resize((int(left.width * h / left.height), h), Image.LANCZOS)
    right = page.resize((int(page.width * h / page.height), h), Image.LANCZOS)
    gap, pad, head = 90, 32, 64
    canvas = Image.new("RGB", (pad * 2 + left.width + gap + right.width, h + pad * 2 + head), "#0b1020")
    d = ImageDraw.Draw(canvas)
    label = ImageFont.truetype(str(MONO_BOLD), 22)
    d.text((pad, pad), "receipt.jpg  (phone photo)", font=label, fill="#f4f1ea")
    d.text((pad + left.width + gap, pad), 'receipt.pdf  (search: "total")', font=label, fill="#f4f1ea")
    canvas.paste(left, (pad, pad + head))
    canvas.paste(right, (pad + left.width + gap, pad + head))
    ax, ay = pad + left.width + 18, pad + head + h // 2
    d.line([(ax, ay), (ax + gap - 40, ay)], fill="#ff7a3d", width=5)
    d.polygon([(ax + gap - 30, ay), (ax + gap - 46, ay - 11), (ax + gap - 46, ay + 11)], fill="#ff7a3d")
    canvas.save(out, optimize=True)


class Terminal:
    W, H, PAD, LINE = 1000, 560, 22, 24

    def __init__(self):
        self.font = ImageFont.truetype(str(MONO), 17)
        self.bold = ImageFont.truetype(str(MONO_BOLD), 17)
        self.lines = []
        self.frames = []

    def frame(self, ms, cursor=False):
        img = Image.new("RGB", (self.W, self.H), "#0b1020")
        d = ImageDraw.Draw(img)
        d.rectangle([0, 0, self.W, 34], fill="#141b33")
        for i, c in enumerate(["#ff5f57", "#febc2e", "#28c840"]):
            d.ellipse([16 + i * 22, 11, 28 + i * 22, 23], fill=c)
        d.text((self.W / 2 - 40, 8), "anytopdf", font=self.font, fill="#9aa3bd")
        visible = self.lines[-((self.H - 50) // self.LINE):]
        y = 46
        for kind, text in visible:
            if kind == "cmd":
                d.text((self.PAD, y), "$ ", font=self.bold, fill="#ff7a3d")
                d.text((self.PAD + 22, y), text, font=self.bold, fill="#f4f1ea")
            else:
                colour = "#ffd24d" if kind == "hit" else "#c9cfdf"
                d.text((self.PAD, y), text, font=self.font, fill=colour)
            y += self.LINE
        if cursor:
            kind, text = visible[-1] if visible else ("cmd", "")
            x = self.PAD + 22 + d.textlength(text, font=self.bold)
            d.rectangle([x + 2, y - self.LINE + 3, x + 11, y - 3], fill="#f4f1ea")
        self.frames.append((img, ms))

    def type(self, cmd):
        self.lines.append(("cmd", ""))
        for i in range(0, len(cmd) + 1, 2):
            self.lines[-1] = ("cmd", cmd[:i])
            self.frame(45, cursor=True)
        self.lines[-1] = ("cmd", cmd)
        self.frame(500, cursor=True)

    def output(self, text, highlight=None, ms=1400):
        for line in text.splitlines():
            for chunk in textwrap.wrap(line, 86) or [""]:
                kind = "hit" if highlight and highlight.lower() in chunk.lower() else "out"
                self.lines.append((kind, chunk))
        self.frame(ms)

    def save(self, path):
        imgs = [f for f, _ in self.frames]
        durations = [ms for _, ms in self.frames]
        palette = imgs[-1].quantize(colors=64, method=Image.Quantize.MEDIANCUT)
        frames = [im.quantize(palette=palette, dither=Image.Dither.NONE) for im in imgs]
        frames[0].save(path, save_all=True, append_images=frames[1:], duration=durations, loop=0, optimize=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bin", default=str(ROOT / "target" / "release" / "anytopdf"))
    args = parser.parse_args()
    binary = Path(args.bin).resolve()
    for tool in ("tesseract", "pdftoppm", "pdftotext"):
        if not shutil.which(tool):
            raise SystemExit(f"{tool} is required to record the demo")

    photo = HERE / "receipt.jpg"
    make_photo(photo)
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        shutil.copy(photo, work / "receipt.jpg")
        bindir = work / "bin"
        bindir.mkdir()
        (bindir / "anytopdf").symlink_to(binary)
        env = f"PATH={shlex.quote(str(bindir))}:$PATH SOURCE_DATE_EPOCH=1790000000"

        term = Terminal()
        term.frame(700, cursor=False)
        steps = [
            ("anytopdf receipt.jpg -o receipt.pdf", None, 1600),
            ("pdftotext -layout receipt.pdf - | grep -i total", "total", 2000),
            ("anytopdf extract receipt.pdf --json | jq -r '.manifest.sources[0].sha256'", None, 4000),
        ]
        for cmd, highlight, ms in steps:
            out = run(f"{env} sh -c {shlex.quote(cmd)}", work)
            term.type(cmd)
            term.output(out, highlight=highlight, ms=ms)
        term.save(HERE / "demo.gif")
        before_after(photo, work / "receipt.pdf", work, HERE / "before-after.png")
    print("wrote receipt.jpg, before-after.png, demo.gif")


if __name__ == "__main__":
    main()
