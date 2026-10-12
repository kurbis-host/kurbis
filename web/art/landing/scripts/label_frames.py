"""Burn the part of the journey and the scroll position into rendered frames, so
notes can point at a moment.

    python3 label_frames.py FRAMES_DIR OUT_DIR SESSION_JSON

SESSION_JSON is the export/session_<name>.json that film.py writes: the scroll
position on every frame, and where the visitor rests.
"""

import json
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


def main(src, dst, session_json):
    data = json.loads(Path(session_json).read_text())
    parts = sorted(data["parts"].items(), key=lambda kv: kv[1])
    scroll, rests = data["scroll"], data["rests"]
    dst = Path(dst)
    dst.mkdir(parents=True, exist_ok=True)
    font = ImageFont.load_default(size=18)
    for path in sorted(Path(src).glob("*.jpg")):
        f = int(path.stem)
        s = scroll[min(f - 1, len(scroll) - 1)]
        part = [name for name, start in parts if start <= s + 1e-6][-1]
        resting = any(a < f < b for a, b in rests)
        label = f"scroll {s:4.2f}   {part}" + ("   resting" if resting else "")
        im = Image.open(path).convert("RGB")
        draw = ImageDraw.Draw(im)
        box = draw.textbbox((0, 0), label, font=font)
        w, h = box[2] + 16, box[3] + 12
        draw.rectangle((10, 10, 10 + w, 10 + h), fill=(0, 0, 0))  # top left: the interface owns the bottom corners
        draw.text((18, 14), label, fill=(255, 255, 255), font=font)
        # a thin bar along the bottom edge: how far down the page this is
        end = parts[-1][1]
        draw.rectangle((0, im.height - 4, im.width, im.height), fill=(0, 0, 0))
        draw.rectangle((0, im.height - 4, int(im.width * s / end), im.height), fill=(255, 196, 40))
        im.save(dst / path.name, quality=92)


if __name__ == "__main__":
    main(*sys.argv[1:4])
