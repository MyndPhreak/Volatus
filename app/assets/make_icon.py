"""Draws the Volatus icon (a treemap whose tiles form a V, the wings of "volatus") and writes volatus.png and volatus.ico.

Run: python make_icon.py   (needs Pillow)
"""
from pathlib import Path
from PIL import Image, ImageDraw

S = 1024  # master canvas; everything below is in 256-unit design space
K = S / 256
OUT = Path(__file__).parent


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def master():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    # Background: rounded square, deep navy to indigo diagonal gradient.
    bg = Image.new("RGBA", (S, S))
    px = bg.load()
    top, bot = (16, 22, 48), (52, 36, 110)
    for y in range(S):
        for x in range(S):
            px[x, y] = (*lerp(top, bot, (x + y) / (2 * S - 2)), 255)
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, S - 1, S - 1), radius=56 * K, fill=255)
    img.paste(bg, (0, 0), mask)

    d = ImageDraw.Draw(img)
    cyan, violet = (56, 224, 255), (176, 110, 255)
    # V of treemap tiles: rows converge on the centre and end in one tile.
    rows = 4
    for k in range(rows):
        t = k / (rows - 1)
        col = lerp(cyan, violet, t)
        y0, y1 = 44 + 40 * k, 44 + 40 * k + 34
        if k < rows - 1:
            tiles = [(36 + 22 * k, 80 + 22 * k), (176 - 22 * k, 220 - 22 * k)]
        else:
            tiles = [(102, 154)]
        for x0, x1 in tiles:
            d.rounded_rectangle((x0 * K, y0 * K, x1 * K, y1 * K), radius=7 * K, fill=(*col, 255))
    return img


m = master()
m.resize((256, 256), Image.LANCZOS).save(OUT / "volatus.png")
m.save(OUT / "volatus.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
