"""Transcribes the source document's 10-column diagrams into cell grids.

    python -I tools/diagrams.py show image62 image63 ...

Every embedded diagram is 10 cells wide; the cell size is the image width
divided by ten (often 14.4 px, so cells are sampled at their centres). Cell
classes: '.' empty (transparent), '#' gray board context drawn by an earlier
stage, '+' light gray, and the piece letters for the seven piece colours.
Rows are returned bottom-up (y = 0 is the floor), matching
data/ijlo/diagram_cells.json.
"""

import base64
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pngread import read_png  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCE = os.path.join(ROOT, "Algebruh's 2nd Methods.md")

# Reference colours measured from the embedded images.
PALETTE = {
    "I": (48, 168, 216),
    "J": (0, 96, 168),
    "L": (240, 120, 24),
    "O": (240, 192, 48),
    "S": (72, 168, 72),
    "T": (144, 48, 144),
    "Z": (216, 72, 96),
    "#": (120, 120, 120),
    "+": (168, 168, 168),
}


def load_images(path=SOURCE):
    """Returns {image name: PNG bytes} for every embedded image definition."""
    text = open(path, encoding="utf-8").read()
    found = re.findall(r"^\[(image\d+)\]:\s*<?data:image/png;base64,([A-Za-z0-9+/=]+)", text, re.M)
    return {name: base64.b64decode(data) for name, data in found}


def classify(pixel):
    r, g, b, a = pixel
    if a < 128:
        return "."
    best, distance = None, None
    for code, (pr, pg, pb) in PALETTE.items():
        d = (r - pr) ** 2 + (g - pg) ** 2 + (b - pb) ** 2
        if distance is None or d < distance:
            best, distance = code, d
    return best


def transcribe(png):
    """Returns (rows bottom-up as strings, notes). Each cell is classified by
    the majority of a 3x3 sample around its centre, so antialiased edges and
    face glyphs do not decide the class."""
    width, height, pixels = read_png(png)
    cell = width / 10
    rows_count = round(height / cell)
    notes = []
    if abs(rows_count * cell - height) > cell * 0.25:
        notes.append(f"height {height} is not a whole number of {cell:.1f}px cells")
    grid = []
    for row in range(rows_count):
        line = []
        for column in range(10):
            cx = (column + 0.5) * cell
            cy = (row + 0.5) * cell
            votes = {}
            for dx in (-0.25, 0, 0.25):
                for dy in (-0.25, 0, 0.25):
                    x = min(width - 1, int(cx + dx * cell))
                    y = min(height - 1, int(cy + dy * cell))
                    code = classify(pixels[y][x])
                    votes[code] = votes.get(code, 0) + 1
            code, count = max(votes.items(), key=lambda item: item[1])
            if count < 5:
                notes.append(f"cell x={column} y={rows_count - 1 - row} is mixed: {votes}")
            line.append(code)
        grid.append("".join(line))
    grid.reverse()
    return grid, notes


def show(names):
    images = load_images()
    for name in names:
        grid, notes = transcribe(images[name])
        print(name)
        for line in reversed(grid):
            print("   ", line)
        for note in notes:
            print("    !", note)


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[1] == "show":
        show(sys.argv[2:])
    else:
        sys.exit(__doc__)
