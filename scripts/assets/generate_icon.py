"""Generate Ludd's Blessing's original survey-orbit application icon."""

from pathlib import Path

from PIL import Image, ImageDraw


ROOT = Path(__file__).resolve().parents[2]
ICON_DIR = ROOT / "src-tauri" / "icons"
CANVAS = 1024


def build_icon() -> Image.Image:
    image = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)

    draw.rounded_rectangle(
        (38, 38, CANVAS - 38, CANVAS - 38),
        radius=210,
        fill="#0d141b",
        outline="#2a3c46",
        width=28,
    )
    draw.ellipse((270, 270, 754, 754), outline="#52d7d2", width=30)
    draw.ellipse((350, 350, 674, 674), outline="#1d7776", width=18)

    orbit = Image.new("RGBA", image.size, (0, 0, 0, 0))
    orbit_draw = ImageDraw.Draw(orbit)
    orbit_draw.ellipse((120, 370, 904, 654), outline="#52d7d2", width=24)
    orbit = orbit.rotate(-24, resample=Image.Resampling.BICUBIC, center=(512, 512))
    image.alpha_composite(orbit)

    draw = ImageDraw.Draw(image)
    draw.ellipse((735, 244, 817, 326), fill="#f0b65c", outline="#f3eddf", width=12)

    # A compact survey ledger at the center: warm paper strokes on a cyan spine.
    draw.rounded_rectangle((389, 345, 635, 679), radius=34, fill="#121d26", outline="#aeb7b8", width=18)
    draw.line((438, 408, 438, 616), fill="#52d7d2", width=24)
    for y, length in ((420, 140), (486, 118), (552, 140), (618, 96)):
        draw.line((470, y, 470 + length, y), fill="#f3eddf", width=20)
    draw.polygon(((512, 178), (554, 246), (512, 314), (470, 246)), fill="#f0b65c")
    return image


def main() -> None:
    ICON_DIR.mkdir(parents=True, exist_ok=True)
    icon = build_icon()
    icon.save(ICON_DIR / "icon.png", optimize=True)
    icon.save(
        ICON_DIR / "icon.ico",
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )


if __name__ == "__main__":
    main()
