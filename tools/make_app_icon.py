#!/usr/bin/env python3
"""
Generate AppIcon.icns for БелкаТуннель.

Renders a papal-purple rounded-square background with the white Cyrillic
"БТ" centred, produces every size macOS needs in an `.iconset/`, then
runs `iconutil` to fold it into a single `.icns` file at the path the
build script reads.

Run via `uv run --project tools python tools/make_app_icon.py`.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

# Papal purple — vivid, slightly reddish violet. Calibrated to read as
# distinctly "purple" rather than blue or indigo when scaled down to the
# 16/32px sizes Finder renders in sidebars.
PURPLE = (93, 45, 139, 255)  # #5D2D8B
WHITE = (255, 255, 255, 255)

# macOS Big Sur+ icon corner radius is roughly 22.37% of icon dimension.
CORNER_RATIO = 0.2237

# Sizes macOS uses in its standard iconset. Each entry is (px, suffix).
# The 'x2' variants are retina renditions of the same logical size.
ICONSET_SIZES = [
    (16, "icon_16x16.png"),
    (32, "icon_16x16@2x.png"),
    (32, "icon_32x32.png"),
    (64, "icon_32x32@2x.png"),
    (128, "icon_128x128.png"),
    (256, "icon_128x128@2x.png"),
    (256, "icon_256x256.png"),
    (512, "icon_256x256@2x.png"),
    (512, "icon_512x512.png"),
    (1024, "icon_512x512@2x.png"),
]

REPO_ROOT = Path(__file__).resolve().parents[1]
# This script renders the DMG file icon (papal-purple БТ), NOT the
# .app bundle icon (which is the original BelkaTunnel design). The .app
# icon at app/assets/AppIcon.icns is untouched.
OUT_ICNS = REPO_ROOT / "app" / "assets" / "DmgIcon.icns"

# Font search order. NewCenturySchoolbookC is a Cyrillic-capable serif
# face (the 'C' suffix marks the Cyrillic variant) that Pasha keeps in
# his user font folder. Bold weight reads better at 16/32 px sizes.
# Falls through to system fonts only if the preferred face is missing.
HOME = Path.home()
FONT_CANDIDATES = [
    str(HOME / "Library" / "Fonts" / "NewCenturySchoolbookC-Bold.ttf"),
    str(HOME / "Library" / "Fonts" / "NewCenturySchoolbookC.ttf"),
    "/System/Library/Fonts/SFNS.ttf",
    "/System/Library/Fonts/SFNSRounded.ttf",
    "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
    "/Library/Fonts/Arial Unicode.ttf",
]


def find_font() -> str:
    for path in FONT_CANDIDATES:
        if Path(path).exists():
            print(f"using font: {path}")
            return path
    raise SystemExit("no Cyrillic-capable system font found")


def draw_rounded_square(size: int, color: tuple[int, int, int, int]) -> Image.Image:
    """Filled rounded square the full size of the icon. Antialiased via
    4x supersample + LANCZOS downscale — `rounded_rectangle` with `radius`
    alone gives jagged corners at small sizes."""
    factor = 4
    big = Image.new("RGBA", (size * factor, size * factor), (0, 0, 0, 0))
    draw = ImageDraw.Draw(big)
    r = int(size * factor * CORNER_RATIO)
    draw.rounded_rectangle(
        (0, 0, size * factor - 1, size * factor - 1),
        radius=r,
        fill=color,
    )
    return big.resize((size, size), Image.LANCZOS)


def render(size: int) -> Image.Image:
    img = draw_rounded_square(size, PURPLE)
    draw = ImageDraw.Draw(img)

    # Pick a font size that fills ~46% of the canvas height. (Was 0.58;
    # reduced by 20% per Pasha's feedback — the serif glyphs read better
    # with more breathing room around them at 1024 px and don't get
    # crowded by the rounded-square corners at 16 px.)
    font_path = find_font()
    target_height = int(size * 0.46)

    # Binary-search the font size that yields target_height for "БТ".
    lo, hi = 8, size * 2
    chosen = lo
    while lo <= hi:
        mid = (lo + hi) // 2
        font = ImageFont.truetype(font_path, mid)
        bbox = font.getbbox("БТ")
        h = bbox[3] - bbox[1]
        if h <= target_height:
            chosen = mid
            lo = mid + 1
        else:
            hi = mid - 1
    font = ImageFont.truetype(font_path, chosen)

    text = "БТ"
    bbox = font.getbbox(text)
    text_w = bbox[2] - bbox[0]
    text_h = bbox[3] - bbox[1]
    # `getbbox` reports the ink bounds; offset by `bbox[0]` and `bbox[1]`
    # so the visible glyphs land centred in the canvas.
    x = (size - text_w) / 2 - bbox[0]
    y = (size - text_h) / 2 - bbox[1]
    draw.text((x, y), text, fill=WHITE, font=font)
    return img


def main() -> None:
    # Iconset basename has to be 'DmgIcon' so iconutil writes
    # DmgIcon.icns (the output name matches the basename — there's no
    # explicit --output flag).
    iconset = REPO_ROOT / "app" / "assets" / "DmgIcon.iconset"
    if iconset.exists():
        shutil.rmtree(iconset)
    iconset.mkdir(parents=True)

    for px, name in ICONSET_SIZES:
        img = render(px)
        img.save(iconset / name, "PNG")
        print(f"  {name:30}  {px}x{px}")

    subprocess.run(
        ["iconutil", "--convert", "icns", str(iconset)],
        check=True,
    )
    if not OUT_ICNS.exists():
        sys.exit(f"iconutil produced no output at {OUT_ICNS}")

    # Clean up the intermediate folder; PNGs aren't checked in.
    shutil.rmtree(iconset)
    print(f"\n→ {OUT_ICNS}  ({OUT_ICNS.stat().st_size // 1024} KB)")


if __name__ == "__main__":
    main()
