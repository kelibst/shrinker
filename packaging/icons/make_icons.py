"""Draw the Shrinker icon and write it in every format the installers need.

    python packaging/icons/make_icons.py

Writes shrinker.svg, shrinker-256.png, shrinker-1024.png, shrinker.ico and
shrinker.icns next to this script. Needs Pillow (pip install pillow).
"""
from pathlib import Path
from PIL import Image, ImageDraw

HERE = Path(__file__).parent
A, B = (124, 108, 255), (47, 214, 196)  # brand gradient, matches the page
# The arrows from the page's logo, on a 24-unit grid.
STROKES = [[(4, 14), (10, 14), (10, 20)], [(20, 10), (14, 10), (14, 4)], [(14, 10), (21, 3)], [(3, 21), (10, 14)]]

SVG = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
  <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0" stop-color="#7c6cff"/><stop offset="1" stop-color="#2fd6c4"/></linearGradient></defs>
  <rect width="24" height="24" rx="5.5" fill="url(#g)"/>
  <g transform="translate(3.6 3.6) scale(.7)" fill="none" stroke="#0b0b14" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">
    <path d="M4 14h6v6M20 10h-6V4M14 10l7-7M3 21l7-7"/>
  </g>
</svg>
"""


def draw(size: int = 1024) -> Image.Image:
    s = 4  # draw big, then shrink for smooth edges
    n = size * s
    grad = Image.new("RGB", (n, n))
    px = grad.load()
    for y in range(0, n, s):
        for x in range(0, n, s):
            t = (x + y) / (2 * n)
            c = tuple(round(A[i] + (B[i] - A[i]) * t) for i in range(3))
            for dy in range(s):
                for dx in range(s):
                    px[x + dx, y + dy] = c
    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, n - 1, n - 1], radius=round(n * 5.5 / 24), fill=255)
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    img.paste(grad, (0, 0), mask)
    d = ImageDraw.Draw(img)
    unit = n * 0.7 / 24
    off = n * 0.15
    w = round(2.4 * unit)
    for pts in STROKES:
        xy = [(off + x * unit, off + y * unit) for x, y in pts]
        d.line(xy, fill=(11, 11, 20, 255), width=w, joint="curve")
        for x, y in xy:  # round caps
            d.ellipse([x - w / 2, y - w / 2, x + w / 2, y + w / 2], fill=(11, 11, 20, 255))
    return img.resize((size, size), Image.LANCZOS)


if __name__ == "__main__":
    big = draw(1024)
    big.save(HERE / "shrinker-1024.png")
    big.resize((256, 256), Image.LANCZOS).save(HERE / "shrinker-256.png")
    big.save(HERE / "shrinker.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    big.save(HERE / "shrinker.icns")
    (HERE / "shrinker.svg").write_text(SVG, encoding="utf-8")
    print("icons written to", HERE)
