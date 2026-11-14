#!/usr/bin/env python3
"""Draw the Kubernaut app icon and write every size and format the app needs.

Output goes to crates/kubernaut-ui/assets/: icon.png (1024), icon.icns, icon.ico
and a few PNG sizes. The drawing is code rather than a binary so the icon can be
changed and regenerated; it needs Pillow and, for the .icns, macOS's iconutil.

    python3 scripts/make-icons.py
"""

import math
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

OUT = Path(__file__).resolve().parent.parent / "crates/kubernaut-ui/assets"
MASTER = 4096  # drawn large and shrunk, which is what makes the edges smooth


def gradient(size, top, bottom):
    """A vertical gradient, top colour to bottom colour."""
    strip = Image.new("RGB", (1, size))
    for y in range(size):
        t = y / (size - 1)
        strip.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)))
    return strip.resize((size, size))


def rounded_mask(size, radius):
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, size - 1, size - 1), radius, fill=255)
    return mask


def heptagon(cx, cy, r):
    """Seven points, one straight up: the number of sides Kubernetes' own mark has."""
    return [
        (cx + r * math.sin(2 * math.pi * i / 7), cy - r * math.cos(2 * math.pi * i / 7))
        for i in range(7)
    ]


def stroke_polygon(draw, points, width, fill):
    """A closed outline with round joins, which ImageDraw.line alone does not give."""
    loop = points + [points[0]]
    draw.line(loop, fill=fill, width=width, joint="curve")
    for x, y in points:
        draw.ellipse((x - width / 2, y - width / 2, x + width / 2, y + width / 2), fill=fill)


def star(cx, cy, long, short):
    """A four-pointed compass star: a navigator's mark."""
    points = []
    for i in range(8):
        angle = math.pi * i / 4 - math.pi / 2
        r = long if i % 2 == 0 else short
        points.append((cx + r * math.cos(angle), cy + r * math.sin(angle)))
    return points


def draw_master():
    s = MASTER
    c = s / 2

    # The tile: deep space blue, a little lighter at the top.
    tile = gradient(s, (36, 52, 120), (14, 20, 52))
    # Inset 10% on each side, as macOS app icons are, so the shadow has room.
    inset = round(s * 0.098)
    body = s - 2 * inset
    base = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    shadow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle(
        (inset, inset + s * 0.012, s - inset, s - inset + s * 0.012),
        round(body * 0.2237),
        fill=(0, 0, 0, 120),
    )
    shadow = shadow.filter(ImageFilter.GaussianBlur(s * 0.012))
    base.alpha_composite(shadow)
    tile_body = tile.resize((body, body)).convert("RGBA")
    tile_body.putalpha(rounded_mask(body, round(body * 0.2237)))
    base.alpha_composite(tile_body, (inset, inset))

    art = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(art)

    # A soft glow behind the mark.
    glow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse(
        (c - s * 0.3, c - s * 0.3, c + s * 0.3, c + s * 0.3), fill=(70, 140, 255, 90)
    )
    art.alpha_composite(glow.filter(ImageFilter.GaussianBlur(s * 0.06)))

    ring_r = body * 0.335
    stroke_polygon(d, heptagon(c, c, ring_r), round(body * 0.07), (234, 241, 255, 255))

    # The star is a gradient clipped to its own shape, cyan at the top to blue.
    star_mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(star_mask).polygon(star(c, c, body * 0.215, body * 0.062), fill=255)
    paint = gradient(s, (110, 232, 255), (52, 120, 255)).convert("RGBA")
    paint.putalpha(star_mask)
    art.alpha_composite(paint)

    # A small bright dot at the centre, so the star reads as a point of light.
    r = body * 0.026
    d.ellipse((c - r, c - r, c + r, c + r), fill=(255, 255, 255, 255))

    base.alpha_composite(art)
    return base


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    master = draw_master()
    resize = lambda n: master.resize((n, n), Image.LANCZOS)

    resize(1024).save(OUT / "icon.png")
    for n in (32, 64, 128, 256, 512):
        resize(n).save(OUT / f"{n}x{n}.png")

    resize(256).save(
        OUT / "icon.ico", sizes=[(n, n) for n in (16, 24, 32, 48, 64, 128, 256)]
    )

    if shutil.which("iconutil"):
        with tempfile.TemporaryDirectory() as tmp:
            iconset = Path(tmp) / "icon.iconset"
            iconset.mkdir()
            for n in (16, 32, 128, 256, 512):
                resize(n).save(iconset / f"icon_{n}x{n}.png")
                resize(n * 2).save(iconset / f"icon_{n}x{n}@2x.png")
            subprocess.run(
                ["iconutil", "-c", "icns", str(iconset), "-o", str(OUT / "icon.icns")],
                check=True,
            )
    else:
        print("iconutil not found: icon.icns was not rebuilt", file=sys.stderr)

    for path in sorted(OUT.iterdir()):
        print(f"{path.name:14} {path.stat().st_size:>8} bytes")


if __name__ == "__main__":
    main()
