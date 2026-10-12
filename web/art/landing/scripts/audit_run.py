"""Run the legibility audit headless on a saved session, with no limit on its time.

    Blender -b blend/film_<session>.blend -P scripts/audit_run.py -- export/session_<session>.json [SCALE]

It looks at three moments of every rest of the session (just after arriving, half
way, just before leaving) and prints what `audit.summary` finds. SCALE is the size
of the renders the contrast is measured on, in percent of the blend's own frame
(default 100): small type needs them large.
"""

import json
import sys
from pathlib import Path

import bpy

sys.path.insert(0, str(Path(__file__).resolve().parent))
import audit  # noqa: E402

args = sys.argv[sys.argv.index("--") + 1 :]
data = json.loads(Path(args[0]).read_text())
scale = int(args[1]) if len(args) > 1 else 100
parts = sorted(data["parts"].items(), key=lambda kv: kv[1])
stops = []
for first, last in data["rests"]:
    s = data["scroll"][min(first, len(data["scroll"]) - 1)]
    part = [name for name, start in parts if start <= s + 1e-6][-1]
    span = last - first
    if span < 36:
        continue
    stops += [(f"{part} at {s:.2f} +{round(k / data['fps'], 1)}s", first + k) for k in (22, span // 2, span - 7)]
scene = bpy.context.scene
report = audit.run(scene, stops, contrast_scale=scale)
print("\n".join(["AUDIT " + line for line in audit.summary(report)]))
low = sorted((f["contrast"], stop, name) for stop, found in report.items() for name, f in found.items() if "contrast" in f)
print("AUDIT lowest contrast:", ", ".join(f"{name} {ratio} ({stop})" for ratio, stop, name in low[:8]))
