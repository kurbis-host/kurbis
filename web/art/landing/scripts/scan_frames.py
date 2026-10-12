"""Find hard cuts and glitches in rendered frames: how much each frame differs from
the one before, and where that jumps far above its neighbours.

    python3 scan_frames.py FRAMES_DIR SESSION_JSON [TOP]

A take that is meant to be continuous should show no lone spike. Covered set
changes (white-out, portal swirl) are expected to be quiet.
"""

import json
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageStat


def main(src, session_json, top=14):
    scroll = json.loads(Path(session_json).read_text())["scroll"]
    files = sorted(Path(src).glob("*.jpg"))
    small = [Image.open(f).convert("RGB").resize((160, 90)) for f in files]
    diffs = []
    for i in range(1, len(small)):
        stat = ImageStat.Stat(ImageChops.difference(small[i - 1], small[i]))
        diffs.append((sum(stat.mean) / 3, int(files[i].stem)))
    values = [d for d, _ in diffs]
    spikes = []
    for i, (d, f) in enumerate(diffs):
        near = sorted(values[max(i - 4, 0) : i] + values[i + 1 : i + 5])
        median = near[len(near) // 2] if near else 0.0
        spikes.append((d / max(median, 1.0), d, median, f))
    print(f"{len(files)} frames, mean change {sum(values) / len(values):.2f}, largest {max(values):.2f}")
    print("largest changes against their neighbours (ratio, change, neighbours' median, frame, scroll):")
    for ratio, d, median, f in sorted(spikes, reverse=True)[: int(top)]:
        print(f"  x{ratio:4.1f}  {d:6.2f}  {median:6.2f}  frame {f:4d}  scroll {scroll[min(f - 1, len(scroll) - 1)]:.2f}")
    still = [f for d, f in diffs if d < 0.05]
    print(f"frames that do not change at all: {len(still)}", still[:20])


if __name__ == "__main__":
    main(*sys.argv[1:4])
