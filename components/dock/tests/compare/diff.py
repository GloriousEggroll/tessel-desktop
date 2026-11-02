"""Pixel comparison of python vs rust screenshots: python3 -I diff.py OUT scenario...

A pixel differs when any channel differs by more than 24 (antialiasing/font
hinting noise). Prints the differing fraction and the bounding box, and writes
<scenario>-diff-<shot>.png (differences in red over a dimmed python frame).
"""
import sys
from pathlib import Path
from PIL import Image, ImageChops

out = Path(sys.argv[1])
report = []
for scenario in sys.argv[2:]:
    for shot in ("panel", "menu", "after-menu", "context", "applets", "preview"):
        a, b = out / f"{scenario}-python-{shot}.png", out / f"{scenario}-rust-{shot}.png"
        if not (a.exists() and b.exists()):
            continue
        pa, pb = Image.open(a).convert("RGB"), Image.open(b).convert("RGB")
        if pa.size != pb.size:
            report.append(f"{scenario}/{shot}: size differs {pa.size} vs {pb.size}")
            continue
        delta = ImageChops.difference(pa, pb).convert("L").point(lambda v: 255 if v > 24 else 0)
        count = sum(1 for v in delta.getdata() if v)
        box = delta.getbbox()
        fraction = count / (pa.size[0] * pa.size[1])
        overlay = Image.blend(pa, Image.new("RGB", pa.size, (0, 0, 0)), 0.6)
        overlay.paste((255, 0, 0), mask=delta)
        overlay.save(out / f"{scenario}-diff-{shot}.png")
        report.append(f"{scenario}/{shot}: {count} px differ ({fraction:.4%}) bbox={box}")
print("\n".join(report))
(out / "diff.txt").write_text("\n".join(report) + "\n")
