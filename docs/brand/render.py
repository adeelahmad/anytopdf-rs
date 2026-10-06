#!/usr/bin/env python3
"""Render the README banners and the GitHub social preview.

    python3 docs/brand/render.py

Needs Pillow and DejaVu fonts. Writes PNGs next to this script.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
FONTS = Path("/usr/share/fonts/truetype/dejavu")
SCALE = 2  # draw at 2x, downsample for clean edges

THEMES = {
    "dark": {
        "bg": "#0b1020",
        "bg2": "#141b33",
        "ink": "#f4f1ea",
        "muted": "#9aa3bd",
        "chip": "#1d2645",
        "chip_ink": "#d6dbea",
        "page": "#f6f4ef",
        "page_line": "#c9c4b8",
        "accent": "#ff7a3d",
        "hit": "#ffd24d",
        "wire": "#3a4670",
    },
    "light": {
        "bg": "#fbfaf7",
        "bg2": "#f1eee6",
        "ink": "#121a33",
        "muted": "#5b6380",
        "chip": "#ffffff",
        "chip_ink": "#29314d",
        "page": "#ffffff",
        "page_line": "#d7d2c6",
        "accent": "#e8571c",
        "hit": "#ffd24d",
        "wire": "#c8c2b4",
    },
}

INPUTS = [".jpg", ".cr3", ".pdf", ".mp4", ".mp3", "chat.txt", ".eml", ".docx", "https://", ".zip"]


def font(name, size):
    return ImageFont.truetype(str(FONTS / name), size * SCALE)


def s(*values):
    return [v * SCALE for v in values]


def gradient(size, top, bottom):
    w, h = size
    base = Image.new("RGB", size, top)
    overlay = Image.new("RGB", size, bottom)
    mask = Image.linear_gradient("L").resize((w, h))
    return Image.composite(overlay, base, mask)


def draw_mark(draw, x, y, size, t):
    """A page with a folded corner and a highlighted search hit."""
    w, h = size * 0.78, size
    fold = size * 0.24
    page = [(x, y), (x + w - fold, y), (x + w, y + fold), (x + w, y + h), (x, y + h)]
    draw.polygon([(px * SCALE, py * SCALE) for px, py in page], fill=t["accent"])
    corner = [(x + w - fold, y), (x + w - fold, y + fold), (x + w, y + fold)]
    draw.polygon([(px * SCALE, py * SCALE) for px, py in corner], fill=t["page"])
    lx, lw, lh = x + size * 0.12, w - size * 0.24, max(2, size * 0.06)
    for i, frac in enumerate([0.55, 0.9, 0.75]):
        ly = y + size * (0.42 + i * 0.16)
        colour = t["hit"] if i == 1 else t["page"]
        draw.rounded_rectangle(s(lx, ly, lx + lw * frac, ly + lh), radius=lh * SCALE / 2, fill=colour)


def draw_page(draw, x, y, w, h, t, hit_row=3):
    fold = w * 0.18
    pts = [(x, y), (x + w - fold, y), (x + w, y + fold), (x + w, y + h), (x, y + h)]
    draw.polygon([(px * SCALE, py * SCALE) for px, py in pts], fill=t["page"], outline=t["page_line"], width=SCALE)
    draw.polygon(
        [(px * SCALE, py * SCALE) for px, py in [(x + w - fold, y), (x + w - fold, y + fold), (x + w, y + fold)]],
        fill=t["page_line"],
    )
    draw.text(s(x + 16, y + 14), "PDF/A-3a", font=font("DejaVuSansMono-Bold.ttf", 11), fill=t["accent"])
    widths = [0.8, 0.62, 0.74, 0.5, 0.7, 0.58, 0.66, 0.44]
    for i, frac in enumerate(widths):
        ly = y + 44 + i * 18
        if ly + 8 > y + h - 12:
            break
        if i == hit_row:
            draw.rounded_rectangle(s(x + 12, ly - 4, x + 16 + (w - 32) * frac + 4, ly + 10), radius=3 * SCALE, fill=t["hit"])
        draw.rounded_rectangle(s(x + 16, ly, x + 16 + (w - 32) * frac, ly + 6), radius=3 * SCALE, fill=t["page_line"])


def chip(draw, x, y, label, t, f):
    tw = draw.textlength(label, font=f) / SCALE
    draw.rounded_rectangle(s(x, y, x + tw + 20, y + 26), radius=13 * SCALE, fill=t["chip"], outline=t["wire"], width=SCALE)
    draw.text(s(x + 10, y + 5), label, font=f, fill=t["chip_ink"])
    return x + tw + 20


def flow(draw, t, left, top, right_page_x, page_y, page_h, rows):
    """Input chips on the left wired into a PDF page on the right."""
    f = font("DejaVuSansMono.ttf", 13)
    ends = []
    per_row = (len(INPUTS) + rows - 1) // rows
    for r in range(rows):
        x = left
        y = top + r * 38
        for label in INPUTS[r * per_row:(r + 1) * per_row]:
            x = chip(draw, x, y, label, t, f) + 8
        ends.append((x - 4, y + 13))
    target_x, target_y = right_page_x - 6, page_y + page_h / 2
    for ex, ey in ends:
        mid = (ex + target_x) / 2
        pts = []
        for i in range(41):
            u = i / 40
            # cubic bezier ex,ey -> mid,ey -> mid,target_y -> target
            bx = (1 - u) ** 3 * ex + 3 * (1 - u) ** 2 * u * mid + 3 * (1 - u) * u**2 * mid + u**3 * target_x
            by = (1 - u) ** 3 * ey + 3 * (1 - u) ** 2 * u * ey + 3 * (1 - u) * u**2 * target_y + u**3 * target_y
            pts.append((bx * SCALE, by * SCALE))
        draw.line(pts, fill=t["wire"], width=2 * SCALE)
    tx = target_x * SCALE
    ty = target_y * SCALE
    a = 7 * SCALE
    draw.polygon([(tx, ty), (tx - a, ty - a * 0.7), (tx - a, ty + a * 0.7)], fill=t["accent"])


def banner(theme):
    t = THEMES[theme]
    w, h = 1280, 320
    img = gradient((w * SCALE, h * SCALE), t["bg"], t["bg2"])
    draw = ImageDraw.Draw(img)
    draw_mark(draw, 64, 92, 76, t)
    draw.text(s(140, 86), "anytopdf", font=font("DejaVuSansMono-Bold.ttf", 60), fill=t["ink"])
    draw.text(s(66, 192), "Turn anything into a searchable PDF.", font=font("DejaVuSans-Bold.ttf", 25), fill=t["ink"])
    draw.text(
        s(66, 232),
        "Photos, video, voice notes, chats, email, Office  \u2192  searchable, askable, offline.",
        font=font("DejaVuSans.ttf", 16),
        fill=t["muted"],
    )
    page_x, page_y, page_w, page_h = 1080, 60, 150, 200
    flow(draw, t, 690, 92, page_x, page_y, page_h, rows=3)
    draw_page(draw, page_x, page_y, page_w, page_h, t)
    img = img.resize((w, h), Image.LANCZOS)
    img.save(HERE / f"banner-{theme}.png", optimize=True)


def social_preview():
    t = THEMES["dark"]
    w, h = 1280, 640
    img = gradient((w * SCALE, h * SCALE), t["bg"], t["bg2"])
    draw = ImageDraw.Draw(img)
    draw_mark(draw, 80, 96, 96, t)
    draw.text(s(176, 92), "anytopdf", font=font("DejaVuSansMono-Bold.ttf", 76), fill=t["ink"])
    draw.text(s(82, 228), "Turn anything into a", font=font("DejaVuSans-Bold.ttf", 44), fill=t["ink"])
    draw.text(s(82, 284), "searchable PDF.", font=font("DejaVuSans-Bold.ttf", 44), fill=t["accent"])
    draw.text(
        s(84, 362),
        "OCR, transcripts, objects, scenes and places in an invisible\ntext layer. Then search and ask across all of it, offline.",
        font=font("DejaVuSans.ttf", 22),
        fill=t["muted"],
        spacing=8 * SCALE,
    )
    f = font("DejaVuSansMono.ttf", 18)
    cmd = "$ anytopdf ask inbox.pdf \"How much was the Northwind receipt?\""
    draw.rounded_rectangle(s(80, 470, 1200, 530), radius=12 * SCALE, fill=t["chip"], outline=t["wire"], width=SCALE)
    draw.text(s(104, 487), cmd, font=f, fill=t["chip_ink"])
    draw.text(
        s(84, 566),
        "Rust · single binary · macOS / Linux / Windows · MCP server · MIT / Apache-2.0",
        font=font("DejaVuSans.ttf", 18),
        fill=t["muted"],
    )
    page_x, page_y, page_w, page_h = 1000, 90, 190, 250
    draw_page(draw, page_x, page_y, page_w, page_h, t)
    img = img.resize((w, h), Image.LANCZOS)
    img.save(HERE / "social-preview.png", optimize=True)


if __name__ == "__main__":
    banner("light")
    banner("dark")
    social_preview()
    print("wrote", ", ".join(p.name for p in sorted(HERE.glob("*.png"))))
