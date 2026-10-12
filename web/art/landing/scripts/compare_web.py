"""Lay the browser's frames beside the previs's, position by position.

    python3 compare_web.py <previs frames dir> <browser frames dir> <out.jpg> [wide|tall]

Frames are named <shape>_<scroll position>.<ext> in both directories (the previs's are
pulled from its videos; the browser's come from web_frames.mjs). Each cell of the sheet
is one position: the previs on the left (or on top), the browser beside it.
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw


def sheet(previs, browser, out, shape="wide"):
    pairs = []
    for frame in sorted(Path(browser).glob(f"{shape}_*.png")):
        position = frame.stem.split("_")[1]
        other = Path(previs) / f"{shape}_{position}.jpg"
        if other.exists():
            pairs.append((position, other, frame))
    tall = shape == "tall"
    w, h = (200, 356) if tall else (480, 270)
    across = 4 if tall else 2
    rows = (len(pairs) + across - 1) // across
    cell = (w * 2 + 6, h + 20)
    page = Image.new("RGB", (across * cell[0] + (across - 1) * 14, rows * cell[1]), (18, 16, 20))
    draw = ImageDraw.Draw(page)
    for i, (position, a, b) in enumerate(pairs):
        x, y = (i % across) * (cell[0] + 14), (i // across) * cell[1]
        draw.text((x + 4, y + 4), f"{position}   previs | browser", fill=(230, 220, 200))
        for k, path in enumerate((a, b)):
            page.paste(Image.open(path).convert("RGB").resize((w, h), Image.LANCZOS), (x + k * (w + 6), y + 20))
    page.save(out, quality=88)
    return page.size


if __name__ == "__main__":
    print(sheet(*sys.argv[1:5]))
