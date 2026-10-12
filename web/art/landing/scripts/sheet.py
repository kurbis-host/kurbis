"""Contact sheet of film frames, so a stretch of motion can be read as stills.

    python3 sheet.py OUT.png COLS FILE [FILE ...]

Each tile is labelled with its file name (the time or frame number).
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw


def main(out, cols, files):
    tiles = [Image.open(f).convert("RGB") for f in files]
    w, h = tiles[0].size
    rows = (len(tiles) + cols - 1) // cols
    sheet = Image.new("RGB", (cols * w, rows * h), (16, 16, 20))
    draw = ImageDraw.Draw(sheet)
    for i, (tile, f) in enumerate(zip(tiles, files)):
        x, y = (i % cols) * w, (i // cols) * h
        sheet.paste(tile, (x, y))
        label = Path(f).stem
        draw.rectangle((x, y, x + 8 + 7 * len(label), y + 16), fill=(0, 0, 0))
        draw.text((x + 4, y + 2), label, fill=(255, 255, 255))
    sheet.save(out)


if __name__ == "__main__":
    main(sys.argv[1], int(sys.argv[2]), sys.argv[3:])
