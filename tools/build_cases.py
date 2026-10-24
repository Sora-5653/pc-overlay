"""Builds setup/solve data for every case in the source documents (`SOURCES`).

    python -I tools/build_cases.py report        # summary of every case
    python -I tools/build_cases.py report IJLO   # one case in detail
    python -I tools/build_cases.py write         # data/cases/*.json

Pipeline, per case section of each document (see `sections`):
1. Every image reference is transcribed into a cell grid (tools/diagrams.py).
2. An image with no gray context is a setup; the images after it, up to the
   next setup, are its solves. A solve image whose gray context equals the
   cells drawn so far continues the previous solve (the source splits some
   solves into stages); one whose context is just the setup starts a new
   solve.
3. Coloured cells are split into tetrominoes per colour. Rows that the image
   draws completely in gray were cleared earlier, so a piece may straddle
   them; such a piece is checked with those rows collapsed.
4. Each setup + solve must fill a 10-wide rectangle (a perfect clear), and
   some placement order must be reachable with SRS 90-degree movement
   (tools/srs.py). Failing solves are kept but marked review_required; the
   source warns about skims and 180-degree spins.
Which setup to build for a queue is not read from the prose: the runtime
checks whether a solve can be played for the observed queue. The prose next
to each setup is kept as a note.
"""

import itertools
import json
import os
import re
import sys
from functools import lru_cache

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from diagrams import SOURCE, load_images, transcribe  # noqa: E402
from srs import SHAPES  # noqa: E402
from srs import reachable as _reachable  # noqa: E402
from srs import reachable_plus as _reachable_plus  # noqa: E402


@lru_cache(maxsize=None)
def _cached_reachable(piece, board, target, plus=False):
    return (_reachable_plus if plus else _reachable)(piece, set(board), list(target))


def reachable(piece, board, target):
    return _cached_reachable(piece, frozenset(board), frozenset(target))


def reachable_plus(piece, board, target):
    """TETR.IO's SRS+, with 180-degree rotation (src/finder.rs)."""
    return _cached_reachable(piece, frozenset(board), frozenset(target), True)

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# (document, style): the 2nd PC methods, the odd-JL no-T 2nd PCs those leave
# out ("Bad 2nd"), and the 7th PC, whose case is three pieces.
SOURCES = [
    ("Algebruh's 2nd Methods.md", "2nd"),
    ("Algebruh's Bad 2nd.md", "bad2nd"),
    ("Algebruh's 7th.md", "7th"),
]
PIECES = "IJLOSTZ"
CONTEXT = "#+"


def normalise(cells):
    min_x = min(x for x, _ in cells)
    min_y = min(y for _, y in cells)
    return frozenset((x - min_x, y - min_y) for x, y in cells)


@lru_cache(None)
def orientations(piece):
    shapes = set()
    cells = SHAPES[piece]
    for _ in range(4):
        shapes.add(normalise(cells))
        cells = [(y, -x) for x, y in cells]
    return shapes


def grid_cells(grid):
    return {(x, y): code for y, row in enumerate(grid) for x, code in enumerate(row) if code != "."}


# --- document structure --------------------------------------------------------


def sections(path=SOURCE, style="2nd"):
    """Yields (title, line number, [(line number, text, [image names])]).
    A case section starts at a `###` heading in the 2nd document and at a
    `##` heading in Bad 2nd. The 7th document names its cases in headings of
    any level and in plain lines ("**OJZ, ILZ, OLS, IJS**"); see `case_list`."""
    title, start, body = None, 0, []
    for number, line in enumerate(open(path, encoding="utf-8"), start=1):
        if line.startswith("[image"):
            break
        heading = re.match(r"^(#{2,4})\s+(.*)", line)
        names = re.findall(r"!\[\]\[(image\d+)\]", line)
        marker = style == "7th" and not heading and not names and case_list(line)
        if heading or marker:
            if title is not None:
                yield title, start, body
            text = heading.group(2).strip() if heading else line.strip()
            if style == "7th":
                title = text if case_list(text) else None
            elif style == "bad2nd":
                title = text if heading.group(1) == "##" else None
            else:
                title = text if heading.group(1) == "###" else None
            start, body = number, []
            continue
        if title is not None:
            body.append((number, line.rstrip("\n"), names))
    if title is not None:
        yield title, start, body


def clean_title(title):
    return re.sub(r"!\[\]\[image\d+\]", "", title).replace("\\", "").replace("*", "").strip()


def case_of(title):
    """'IJLS/IJLZ-alt' -> ('IJLS', 'IJLZ', 'alt'); 'IJLO (T)' -> ('IJLO', None, 'T').
    A success rate in parentheses ('IJOS/ILOZ (99.95% / 100%)') is not a variant."""
    clean = re.sub(r"\(\s*[\d.%/ ]+\)", "", clean_title(title)).strip()
    match = re.match(r"^(X-)?([IJLOSTZ]{4})(?:/([IJLOSTZ]{4}))?\s*(?:-\s*)?(.*)$", clean)
    if not match:
        return None, None, clean
    variant = match.group(4).strip(" -()")
    return match.group(2), match.group(3), variant or "main"


CASE_LIST = re.compile(r"^[IJLOSTZ]{3}(?:\s*[/,]\s*[IJLOSTZ]{3})*$")


def case_list(title):
    """7th cases named by a title, after its last ':', '-' or '(':
    'LO Locked - TIJ/TIL, TLJ, TLO/TJO' -> ['TIJ', 'TIL', 'TLJ', 'TLO', 'TJO'].
    None when the title is not such a list."""
    tail = re.split(r"[:(\-]", clean_title(title))[-1].strip(" )")
    if not CASE_LIST.match(tail):
        return None
    return list(dict.fromkeys(re.findall(r"[IJLOSTZ]{3}", tail)))


def mirror_case(case):
    return case.translate(MIRROR)


# --- decomposition -------------------------------------------------------------


def candidates(cells, piece):
    """Every placement of `piece` inside `cells`. The source draws the PC
    without line clears, so a piece placed after a row cleared appears split
    around that row: a candidate may skip rows between its own rows."""
    rows = sorted({y for _, y in cells})
    found = set()
    for shape in orientations(piece):
        height = max(y for _, y in shape) + 1
        for picked in itertools.combinations(rows, height):
            for dx in range(-3, 10):
                part = frozenset((x + dx, picked[y]) for x, y in shape)
                if part <= cells:
                    found.add(part)
    # Prefer unsplit pieces: try them first.
    return sorted(found, key=lambda part: max(y for _, y in part) - min(y for _, y in part))


def tilings(cells, piece, limit=16):
    """Exact covers of `cells` by `piece` candidates (at most `limit`)."""
    options = candidates(cells, piece)
    results = []

    def cover(left, chosen):
        if len(results) >= limit:
            return
        if not left:
            results.append(list(chosen))
            return
        anchor = min(left, key=lambda c: (c[1], c[0]))
        for part in options:
            if anchor in part and part <= left:
                chosen.append(part)
                cover(left - part, chosen)
                chosen.pop()

    if len(cells) % 4 == 0:
        cover(frozenset(cells), [])
    return results


def tile_with(cells, pieces):
    """Exact cover of `cells` by some of `pieces` (a string, repeats allowed)."""
    if not cells:
        return []
    anchor = min(cells, key=lambda c: (c[1], c[0]))
    for index, piece in enumerate(pieces):
        if piece in pieces[:index]:
            continue
        for part in candidates(cells, piece):
            if anchor in part and max(y for _, y in part) - min(y for _, y in part) < 4 \
                    and len({y for _, y in part}) == max(y for _, y in part) - min(y for _, y in part) + 1:
                rest = tile_with(cells - part, pieces[:index] + pieces[index + 1:])
                if rest is not None:
                    return [(piece, part)] + rest
    return None


def split_pieces(colour_cells):
    """Splits {cell: piece} into tetromino placements. Returns
    (list of alternative placement lists, problems)."""
    per_piece, problems = [], []
    for piece in sorted(set(colour_cells.values())):
        cells = {c for c, p in colour_cells.items() if p == piece}
        found = tilings(cells, piece)
        if not found:
            problems.append(f"{piece} cells {sorted(cells)} are not whole {piece} pieces")
            continue
        per_piece.append([[(piece, part) for part in tiling] for tiling in found])
    alternatives = [[]]
    for options in per_piece:
        alternatives = [a + b for a in alternatives for b in options][:64]
    return alternatives, problems


# --- grouping images into setups and solves -------------------------------------


class Setup:
    def __init__(self, image, line, grid):
        self.image = image
        self.line = line
        self.cells = grid_cells(grid)
        alternatives, self.problems = split_pieces(self.cells)
        self.placements = alternatives[0] if alternatives else []
        self.implicit = False
        self.solves = []
        self.notes = []

    @classmethod
    def from_context(cls, image, line, context, pieces):
        """A section that draws its setup only as gray context (TJOO's PCO+O):
        tile the gray cells with the case pieces."""
        setup = cls.__new__(cls)
        setup.image, setup.line, setup.implicit = image, line, True
        setup.cells = {c: "#" for c in context}
        setup.solves, setup.notes, setup.problems = [], [], []
        setup.placements = tile_with(frozenset(context), pieces)
        if setup.placements is None:
            setup.placements = []
            setup.problems.append(f"gray context of {image} is not a tiling of {pieces}")
        return setup

    def occupied(self):
        return set(self.cells)


class Solve:
    def __init__(self, setup):
        self.setup = setup
        self.images = []
        self.alternatives = [[]]
        self.placements = []
        self.problems = []
        self.drawn = set(setup.occupied())
        self.stages = []  # (image, grid) in order
        self.states = [set(self.drawn)]  # drawn cells before each stage, then after the last

    def add_gray_stage(self, image, placements):
        """Pieces the image draws in gray although no earlier image placed them
        (a 3-piece setup's leftover case piece, drawn as context)."""
        self.images.append(f"{image}(gray)")
        self.alternatives = [a + placements for a in self.alternatives]
        self.placements = self.alternatives[0]
        for _, cells in placements:
            self.drawn |= set(cells)
        self.stages.append((None, placements))
        self.states.append(set(self.drawn))

    def branch(self, stages):
        """A new solve that shares this solve's first `stages` stages."""
        other = Solve(self.setup)
        for image, grid in self.stages[:stages]:
            if image is None:
                other.add_gray_stage(self.images[0].split("(")[0], grid)
            else:
                other.add_stage(image, grid)
        return other

    def add_stage(self, image, grid):
        cells = grid_cells(grid)
        colour = {c: p for c, p in cells.items() if p in PIECES}
        alternatives, problems = split_pieces(colour)
        self.images.append(image)
        self.alternatives = [a + b for a in self.alternatives for b in alternatives][:64]
        self.placements = self.alternatives[0] if self.alternatives else []
        self.problems.extend(f"{image}: {p}" for p in problems)
        self.drawn |= set(colour)
        self.stages.append((image, grid))
        self.states.append(set(self.drawn))


MIRROR = str.maketrans("JLSZ", "LJZS")


def mirrored_grid(grid):
    return [row[::-1].translate(MIRROR) for row in grid]


def gray_pieces(extra, setup, pieces):
    """Tiles extra gray cells with the case pieces the setup did not use."""
    left = list(pieces)
    for piece, _ in setup.placements:
        if piece in left:
            left.remove(piece)
    return tile_with(frozenset(extra), "".join(left)) if left else None


def clone_solve(solve, setup, mirror=False):
    """`solve`'s stages replayed on another setup, mirrored if asked."""
    other = Solve(setup)
    for image, grid in solve.stages:
        if image is None:
            other.add_gray_stage(solve.images[0].split("(")[0],
                                 mirror_placements(grid) if mirror else grid)
        else:
            other.add_stage(image, mirrored_grid(grid) if mirror else grid)
    other.shared_from = solve.setup.image
    return other


def share_solves(setups, any_pieces=False):
    """A setup drawn without its own solves gets those of a setup with the
    same cells and pieces (TJLS image361/362, coloured for different queues)
    or of its mirror image (JLSZ image159 is image160 mirrored). With
    `any_pieces` (the 7th, whose solve is a whole bag whatever the setup is
    built from: LO Locked image16-20) the cells alone decide. Repeated until
    nothing changes, so a setup can take solves another one just shared."""
    changed = True
    while changed:
        changed = False
        for setup in setups:
            if not setup.solves:
                _share_into(setup, setups, any_pieces)
                changed |= bool(setup.solves)


def _share_into(setup, setups, any_pieces):
    cells = setup.occupied()
    used = sorted(p for p, _ in setup.placements)
    for other in setups:
        if other is setup or not other.solves:
            continue
        if other.occupied() == cells and (any_pieces or sorted(p for p, _ in other.placements) == used):
            setup.solves = [clone_solve(so, setup) for so in other.solves]
        elif {(9 - x, y) for x, y in other.occupied()} == cells and \
                sorted(p.translate(MIRROR) for p, _ in other.placements) == used:
            setup.solves = [clone_solve(so, setup, mirror=True) for so in other.solves]
        if setup.solves:
            setup.notes.append(f"Solves shared from setup {other.image}.")
            break


def collapsed(cells):
    """`cells` with every full row removed, as drawn after the clear."""
    full = {y for _, y in cells if all((x, y) in cells for x in range(10))}
    return {(x, y - sum(1 for r in full if r < y)) for x, y in cells if y not in full}, sorted(full)


def uncollapse(grid, full):
    """Reinserts the rows `full` (as gray context) into a grid drawn after
    they cleared."""
    rows = list(grid)
    for y in full:
        rows.insert(y, "#" * 10)
    return rows


def group(title, body, images, pieces="", style="2nd"):
    """Returns ([Setup], [problems]) for one section."""
    setups, problems = [], []
    current = None
    for number, text, names in body:
        if not names:
            if current is not None and text.strip():
                current.notes.append(text.strip())
            continue
        for name in names:
            grid, notes = transcribe(images[name])
            problems.extend(f"{name}: {n}" for n in notes)
            cells = grid_cells(grid)
            context = {c for c, p in cells.items() if p in CONTEXT}
            if current is not None and context and context == {(9 - x, y) for x, y in current.occupied()}:
                # The solve of the mirrored setup: read it mirrored back.
                grid = mirrored_grid(grid)
                cells = grid_cells(grid)
                context = {c for c, p in cells.items() if p in CONTEXT}
                problems.append(f"{name} (line {number}): drawn for the mirrored setup; read mirrored")
            if not context:
                extended = current is not None and not current.solves and \
                    all(cells.get(c) == p for c, p in current.cells.items())
                if extended:
                    # Same setup, drawn with more pieces (a queue-based step).
                    current.image_stages = getattr(current, "image_stages", [current.image]) + [name]
                    replacement = Setup(name, current.line, grid)
                    replacement.notes = current.notes
                    replacement.image_stages = current.image_stages
                    setups[-1] = current = replacement
                    continue
                current = Setup(name, number, grid)
                setups.append(current)
                continue
            if style == "7th" and current is not None and context != current.occupied():
                # A queue-based setup that completes a row, its solve drawn
                # after the clear (JSZ image112/113): put the row back.
                shrunk, full = collapsed(current.occupied())
                if full and context == shrunk:
                    grid = uncollapse(grid, full)
                    cells = grid_cells(grid)
                    context = {c for c, p in cells.items() if p in CONTEXT}
            if current is None:
                if style == "7th":
                    # The shape alone, before the coloured setups for each
                    # case (LO Locked image15, ST/ZT image26).
                    continue
                current = Setup.from_context(name, number, context, pieces)
                setups.append(current)
            last = current.solves[-1] if current.solves else None
            if last is not None and context == last.drawn:
                last.add_stage(name, grid)
                continue
            # A new solve: from the setup, or branching after some stage of an
            # earlier solve (the latest one first).
            branched = None
            for solve in reversed(current.solves):
                for stages, state in enumerate(solve.states[1:-1], start=1):
                    if context == state:
                        branched = solve.branch(stages)
                        break
                if branched:
                    break
            if branched is not None:
                branched.add_stage(name, grid)
                current.solves.append(branched)
            elif context == current.occupied():
                solve = Solve(current)
                solve.add_stage(name, grid)
                current.solves.append(solve)
            elif context > current.occupied() and gray_pieces(context - current.occupied(), current, pieces):
                solve = Solve(current)
                solve.add_gray_stage(name, gray_pieces(context - current.occupied(), current, pieces))
                solve.add_stage(name, grid)
                current.solves.append(solve)
            else:
                # Gray context that matches neither: report how far off it is.
                expected = current.occupied()
                problems.append(
                    f"{name} (line {number}): gray context differs from setup {current.image} "
                    f"(+{len(context - expected)} / -{len(expected - context)} cells)")
    share_solves(setups, any_pieces=style == "7th")
    return setups, problems


# --- checks ---------------------------------------------------------------------


def frame_check(setup, solve):
    """Returns problems: the pieces must fill whole rows exactly once."""
    pieces = setup.placements + solve.placements
    seen = {}
    problems = []
    for piece, cells in pieces:
        for c in cells:
            if c in seen:
                problems.append(f"cell {c} drawn by both {seen[c]} and {piece}")
            seen[c] = piece
    height = max(y for _, y in seen) + 1
    missing = [(x, y) for y in range(height) for x in range(10) if (x, y) not in seen]
    if missing:
        problems.append(f"{len(missing)} cells of the {height}-row PC are empty, e.g. {missing[:3]}")
    if len(pieces) * 4 != 10 * height:
        problems.append(f"{len(pieces)} pieces do not make a {height}-line PC")
    return problems


def reachable_order(setup, solve):
    """True if some order places every piece, with completed rows clearing."""
    pieces = setup.placements + solve.placements
    height = max(y for _, cells in pieces for _, y in cells) + 1
    goal = frozenset(range(len(pieces)))

    @lru_cache(None)
    def state(placed):
        cells = {c for i in placed for c in pieces[i][1]}
        cleared = [y for y in range(height) if all((x, y) in cells for x in range(10))]
        live = {(x, y - sum(1 for r in cleared if r < y)) for x, y in cells if y not in cleared}
        return live, cleared

    @lru_cache(None)
    def go(placed):
        if placed == goal:
            return True
        live, cleared = state(placed)
        for i in goal - placed:
            piece, cells = pieces[i]
            if any(y in cleared for _, y in cells):
                continue
            moved = [(x, y - sum(1 for r in cleared if r < y)) for x, y in cells]
            if reachable(piece, live, moved) and go(placed | {i}):
                return True
        return False

    # Setup pieces first, as the method builds them before the solve.
    setup_ids = frozenset(range(len(setup.placements)))

    @lru_cache(None)
    def build(placed):
        if placed == setup_ids:
            return go(placed)
        live, _ = state(placed)
        return any(reachable(pieces[i][0], live, list(pieces[i][1])) and build(placed | {i})
                   for i in setup_ids - placed)

    return build(frozenset())


def transitions(base, movable, first=0, reach=reachable):
    """{placed mask: mask of `movable` pieces placeable next}, over the states
    reachable with `base` already on the board. Bit i is movable[i]; the
    first `first` pieces (setup pieces from the following bag) must all be
    placed before any other. `reach` decides a single placement."""
    pieces = base + movable
    height = max(y for _, cells in pieces for _, y in cells) + 1
    must = (1 << first) - 1
    table = {}
    stack = [0]
    while stack:
        mask = stack.pop()
        if mask in table:
            continue
        cells = {c for _, part in base for c in part}
        cells |= {c for i, (_, part) in enumerate(movable) if mask >> i & 1 for c in part}
        cleared = [y for y in range(height) if all((x, y) in cells for x in range(10))]
        live = {(x, y - sum(1 for r in cleared if r < y)) for x, y in cells if y not in cleared}
        allowed = 0
        for i, (piece, part) in enumerate(movable):
            if mask >> i & 1 or any(y in cleared for _, y in part):
                continue
            if i >= first and mask & must != must:
                continue
            moved = [(x, y - sum(1 for r in cleared if r < y)) for x, y in part]
            if reach(piece, live, moved):
                allowed |= 1 << i
                stack.append(mask | 1 << i)
        table[mask] = allowed
    return table


def playable_solve(cond, solve, queue_from_case, hold, bag):
    """Plays one `solvable` entry, as Rust `solvable_counts` does: from the
    case start when given, else the leftover case pieces then the bag (the
    setup's case pieces first when the table holds them)."""
    pieces = "".join(solve["pieces"])
    table = {m: a for m, a in solve["transitions"]}
    prefix = "".join(cond["prefix"])
    base = "".join(cond.get("setup", []))
    in_table = solve.get("setup_in_table", False)
    if queue_from_case is not None:
        if in_table:
            return playable(queue_from_case + bag, pieces, table, hold)
        order = {m: a for m, a in cond.get("setup_order", [])}
        return playable(queue_from_case + bag, pieces, table, hold, base, order)
    lead = (base + prefix) if in_table else prefix
    return playable(lead + bag, pieces, table)


def playable(queue, solve_pieces, table, start_hold=None, base="", base_order=None):
    """Mirrors Rust `Replay`: place Current, hold it into an empty HOLD
    (locked until the next placement), or swap by placing the held piece.
    `base` pieces (the setup's case pieces) go first, in an order their SRS
    table `base_order` allows (any order without one); solve pieces then
    follow the solve's SRS table. Bits 0.. are solve pieces, then base."""
    n = len(solve_pieces)
    pieces = list(solve_pieces) + list(base)
    goal = (1 << len(pieces)) - 1
    base_mask = ((1 << len(base)) - 1) << n

    def allowed(mask):
        if mask & base_mask != base_mask:
            if base_order:
                return base_order.get(mask >> n, 0) << n
            return ~mask & base_mask
        return table.get(mask & ((1 << n) - 1), 0)

    @lru_cache(None)
    def go(index, hold, mask, locked):
        if mask == goal:
            return True
        ok = allowed(mask)

        def place(piece, new_hold):
            return any(p == piece and ok >> i & 1 and go(index + 1, new_hold, mask | 1 << i, False)
                       for i, p in enumerate(pieces))

        if index >= len(queue):
            # The next bag's first piece is swapped for the held one, which
            # goes in last (a 7th PC places all ten of its pieces).
            return index == len(queue) and hold is not None and not locked and place(hold, None)
        current = queue[index]

        if place(current, hold):
            return True
        if locked:
            return False
        if hold is None:
            return go(index + 1, current, mask, True)
        return place(hold, current)

    return go(0, start_hold, 0, False)


def case_starts(case_id):
    """PC starts the fixture checks: HOLD empty, the first case piece held,
    and the last one held (queue order otherwise as in the case id)."""
    return [(None, case_id), (case_id[0], case_id[1:]), (case_id[-1], case_id[:-1])]


ALL_BAGS = ["".join(order) for order in itertools.permutations(PIECES)]


def split_setup(case, placements):
    """Returns (case part, extras from the following bag, leftover case
    pieces). Pieces beyond the case's multiset are extras."""
    left = list(case)
    base, extras = [], []
    for piece, cells in sorted(placements, key=lambda pc: min(y for _, y in pc[1])):
        if piece in left:
            left.remove(piece)
            base.append((piece, cells))
        else:
            extras.append((piece, cells))
    return base, extras, "".join(left)


def fits_case(case, placements, mirrored=False):
    """True if the case pieces and one bag supply `placements`' pieces."""
    used = [p.translate(MIRROR) if mirrored else p for p, _ in placements]
    supply = case + PIECES
    return all(used.count(p) <= supply.count(p) for p in set(used))


def matches(case, placements):
    return len(split_setup(case, placements)[0])


# --- output ---------------------------------------------------------------------

CATEGORY = {"main": "main", "alt": "alt", "T": "t_i", "T/I": "t_i", "extra": "extra"}


def placement_json(piece, cells):
    return {"piece": piece, "cells": [{"x": x, "y": y} for x, y in sorted(cells, key=lambda c: (c[1], c[0]))]}


def mirror_placements(placements):
    return [(piece.translate(MIRROR), frozenset((9 - x, y) for x, y in cells)) for piece, cells in placements]


class Shim:
    """Setup or solve geometry only, for the mirrored copies."""

    def __init__(self, placements, setup=None):
        self.placements = placements
        self.setup = setup


def mirrored_pair(setup, solve):
    """(setup, solve) geometry mirrored, with SRS rechecked: the SRS kick
    table is not left-right symmetric, so the mirror needs its own search."""
    mirror_setup = Shim(mirror_placements(setup.placements))
    mirror_solve = Shim(mirror_placements(solve.placements), mirror_setup)
    if getattr(solve, "interleaved", False):
        ok = reachable_order(Shim([]), Shim(mirror_setup.placements + mirror_solve.placements, Shim([])))
    else:
        ok = not solve.problems and reachable_order(mirror_setup, mirror_solve)
    return mirror_setup, mirror_solve, ok


def rule_sets(cases):
    """Yields (case id, RuleSet dict). In a section 'ABCD/ABCE' each setup is
    drawn for one of the two cases (the one its pieces match); the other case
    gets it mirrored. A case that is its own mirror gets both orientations
    (the source mirrors setups to cover fail queues)."""
    by_case = {}
    for order, entry in enumerate(cases, start=1):
        if not entry["case"] or not entry["setups"]:
            continue
        case, mirror = entry["case"], entry["mirror"] or entry["case"]
        for setup in entry["setups"]:
            if entry["cases"]:
                # A list of cases: every case and orientation the setup's
                # pieces match best, if one of its solves has the pieces
                # left (ST/ZT image28 holds L; it is no TSJ setup).
                options = [(c, m) for c in entry["cases"] for m in (False, True)]
                score = {(c, m): matches(c, mirror_placements(setup.placements) if m else setup.placements)
                         for c, m in options}
                best = max(score.values())
                for c, m in options:
                    if score[(c, m)] == best and (not setup.solves or any(
                            fits_case(c, setup.placements + so.placements, m) for so in setup.solves)):
                        by_case.setdefault(c, []).append((order, entry, setup, m))
                continue
            if mirror != case and matches(mirror, setup.placements) > matches(case, setup.placements):
                owner, other = mirror, case
            else:
                owner, other = case, mirror
            by_case.setdefault(owner, []).append((order, entry, setup, False))
            by_case.setdefault(other, []).append((order, entry, setup, True))
    for case_id, items in by_case.items():
        methods = {}
        images = []
        emitted = set()
        for order, entry, setup, mirrored in items:
            # A setup repeated in a later section (IJLO and IJLO (T)) is kept once.
            if (setup.image, mirrored) in emitted:
                continue
            emitted.add((setup.image, mirrored))
            category = CATEGORY.get(entry["variant"], "extra")
            method_id = f"{case_id.lower()}_{category}_{order}"
            method = methods.setdefault(method_id, {
                "id": method_id, "source_order": order, "category": category, "setups": []})
            setup_geo = mirror_placements(setup.placements) if mirrored else setup.placements
            base, extras, left = split_setup(case_id, setup_geo)
            solves_json, conds = [], []
            for index, solve in enumerate(setup.solves, start=1):
                if mirrored:
                    geo_setup, geo_solve, srs_ok = mirrored_pair(setup, solve)
                else:
                    geo_setup, geo_solve, srs_ok = setup, solve, solve.reachable
                solve_id = f"{setup.image}{'_m' if mirrored else ''}_solve_{index}"
                solves_json.append({
                    "id": solve_id, "source_order": index,
                    "review_status": "verified" if srs_ok else "review_required",
                    "condition": {"op": "always"},
                    "target": [placement_json(p, c) for p, c in geo_solve.placements],
                    "source": {"document": entry["document"],
                               "lines": [setup.line, setup.line],
                               "images": list(solve.images),
                               "note": ("Generated by tools/build_cases.py; "
                                        + (f"the source leaves the last cells empty; completed with "
                                           f"{solve.completed} by search. " if getattr(solve, "completed", "") else "")
                                        + ("setup and solve pieces interleave (a solve piece goes in before "
                                           "a setup piece). " if getattr(solve, "interleaved", False) else "")
                                        + ("an SRS 90-degree order exists." if srs_ok
                                           else "no SRS 90-degree order found or the cells do not form a PC: "
                                           + "; ".join(solve.problems[:2])))
                               + (" Mirrored." if mirrored else "")},
                })
                if srs_ok and getattr(solve, "interleaved", False):
                    # Setup pieces join the table; bits after the solve's own.
                    movable = list(geo_solve.placements) + extras + base
                    table = transitions([], movable)
                    conds.append({
                        "solve": solve_id,
                        "pieces": [p for p, _ in movable],
                        "transitions": [[m, a] for m, a in sorted(table.items())],
                        "setup_in_table": True,
                    })
                elif srs_ok:
                    movable = extras + list(geo_solve.placements)
                    table = transitions(base, movable, len(extras))
                    if table.get(0, 0) or not movable:
                        conds.append({
                            "solve": solve_id,
                            "pieces": [p for p, _ in movable],
                            "transitions": [[m, a] for m, a in sorted(table.items())],
                        })
            # The case pieces of the setup, in the orders TETR.IO's SRS+ can
            # place them on the empty board the PC starts with (L on top of
            # Z needs Z first; a J tucked under S may need a 180 spin).
            setup_order = transitions([], base, reach=reachable_plus) if base else {}
            verified_setup = bool(conds) and not setup.problems
            setup_id = f"{case_id.lower()}_{setup.image}{'_m' if mirrored else ''}"
            stages = getattr(setup, "image_stages", [setup.image])
            note = "Generated by tools/build_cases.py."
            if extras:
                note += (f" Queue-based: {''.join(p for p, _ in extras)} come from the following bag"
                         " and are placed before the solve.")
            if setup.notes:
                note += " Prose: " + " / ".join(setup.notes)[:600]
            if mirrored:
                note += " Mirrored from the source."
            method["setups"].append({
                "id": setup_id,
                "source_order": len(method["setups"]) + 1,
                "setup_piece_count": len(setup_geo),
                "review_status": "verified" if verified_setup else "review_required",
                "build_condition": {"op": "solvable", "prefix": list(left),
                                    "setup": [p for p, _ in base],
                                    "setup_order": [[m, a] for m, a in sorted(setup_order.items())],
                                    "solves": conds},
                "placements": [placement_json(p, c) for p, c in setup_geo],
                "solves": solves_json,
                "source": {"document": entry["document"],
                           "lines": [setup.line, setup.line],
                           "images": [stages[-1]],
                           "note": note + (f" Drawn in stages: {', '.join(stages)}." if len(stages) > 1 else "")},
            })
            images.append(setup.image)
        lines = [entry["line"] for _, entry, _, _ in items]
        yield case_id, {
            "schema_version": 1,
            "case": {"id": case_id, "family": "generated",
                     "source": {"document": items[0][1]["document"], "lines": [min(lines), max(lines)],
                                "images": sorted(set(images), key=lambda n: int(n[5:])),
                                "note": "Generated by tools/build_cases.py. A setup is verified when one of its "
                                        "solves has an SRS 90-degree placement order; its build condition is "
                                        "`solvable` over the bag that follows the case pieces."}},
            "methods": sorted(methods.values(), key=lambda m: m["source_order"]),
        }


def write():
    out = os.path.join(ROOT, "data", "cases")
    os.makedirs(out, exist_ok=True)
    cases = analyse_all()
    written = []
    for case_id, rules in rule_sets(cases):
        path = os.path.join(out, f"{case_id}.json")
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            json.dump(rules, f, ensure_ascii=False, indent=2)
            f.write("\n")
        setups = [st for m in rules["methods"] for st in m["setups"]]
        verified = sum(1 for st in setups if st["review_status"] == "verified")
        written.append(f"{case_id}: {len(setups)} setups, {verified} verified")
    print("\n".join(written))


def setup_pieces(case_id, prefix):
    left = list(case_id)
    for piece in prefix:
        left.remove(piece)
    return "".join(left)


def coverage_report():
    """Reads the written data/cases/*.json (not the source again), prints the
    share of following bags each verified setup solves (HOLD empty at the bag),
    and writes tests/fixtures/cases_coverage.json for tests/generated_cases.rs:
    per setup, [hold, case queue, count] for the bag alone ("" / "") and for
    each of `case_starts`."""
    folder = os.path.join(ROOT, "data", "cases")
    fixture = {}
    for name in sorted(os.listdir(folder)):
        rules = json.load(open(os.path.join(folder, name), encoding="utf-8"))
        case_id = rules["case"]["id"]
        for method in rules["methods"]:
            for st in method["setups"]:
                if st["review_status"] != "verified":
                    continue
                cond = st["build_condition"]
                prefix = "".join(cond["prefix"])
                base = "".join(cond.get("setup", []))
                rows = []
                ok = sum(1 for bag in ALL_BAGS
                         if any(playable_solve(cond, so, None, None, bag) for so in cond["solves"]))
                rows.append(["", "", ok])
                for hold, queue in case_starts(case_id):
                    count = sum(1 for bag in ALL_BAGS
                                if any(playable_solve(cond, so, queue, hold, bag) for so in cond["solves"]))
                    rows.append([hold or "", queue, count])
                fixture.setdefault(case_id, {})[st["id"]] = rows
                print(f"{case_id:5} {st['id']:<24} " + " ".join(
                    f"{h or '-'}|{q}:{100 * c / len(ALL_BAGS):5.1f}%" for h, q, c in rows))
    path = os.path.join(ROOT, "tests", "fixtures", "cases_coverage.json")
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(fixture, f, indent=1, sort_keys=True)
        f.write("\n")


# --- driver ---------------------------------------------------------------------


def complete(setup, solve, case, mirror):
    """Solves for a staged diagram whose last pieces are not drawn: tile the
    empty cells of the PC (at most two pieces) with pieces still available
    (the following bag plus case pieces the setup leaves over, minus what the
    solve already uses) and keep each tiling that some SRS order builds."""
    pieces = setup.placements + solve.placements
    if any(c1 & c2 for i, (_, c1) in enumerate(pieces) for _, c2 in pieces[i + 1:]):
        return []
    filled = {c for _, part in pieces for c in part}
    height = max(y for _, y in filled) + 1
    missing = frozenset((x, y) for y in range(height) for x in range(10) if (x, y) not in filled)
    if not missing or len(missing) > 8 or len(missing) % 4:
        return []
    owner = case
    if mirror and matches(mirror, setup.placements) > matches(case, setup.placements):
        owner = mirror
    available = {p: 1 for p in PIECES}
    for piece in split_setup(owner, setup.placements)[2]:
        available[piece] += 1
    for piece, _ in solve.placements:
        available[piece] -= 1
    found = []

    def cover(left, chosen):
        if not left:
            found.append(list(chosen))
            return
        anchor = min(left, key=lambda c: (c[1], c[0]))
        for piece in PIECES:
            if available[piece] <= 0:
                continue
            for part in candidates(left, piece):
                if anchor in part:
                    available[piece] -= 1
                    chosen.append((piece, part))
                    cover(left - part, chosen)
                    chosen.pop()
                    available[piece] += 1

    cover(missing, [])
    completed = []
    for extra in found:
        other = Solve(setup)
        other.images = list(solve.images)
        other.placements = solve.placements + extra
        other.alternatives = [other.placements]
        other.completed = "".join(p for p, _ in extra)
        if not frame_check(setup, other) and reachable_order(setup, other):
            other.reachable = True
            completed.append(other)
    return completed


def title_cases(title, style):
    """(case, mirror, variant, case list). A 7th title naming one case or a
    mirror pair reads like a 2nd-PC title; a longer list (with the mirror of
    every listed case added, as the source mirrors its setups) is assigned
    setup by setup in `rule_sets`."""
    if style != "7th":
        return case_of(title) + (None,)
    found = case_list(title) or []
    if len(found) == 1:
        return found[0], None, "main", None
    if len(found) == 2 and same_case(mirror_case(found[0]), found[1]):
        return found[0], found[1], "main", None
    # Cases are piece sets: a mirror spelt differently ('LZS' for the listed
    # 'LSZ') is the same case.
    listed = []
    for case in found + [mirror_case(c) for c in found]:
        if not any(same_case(case, known) for known in listed):
            listed.append(case)
    return listed[0], None, "main", listed


def same_case(a, b):
    return sorted(a) == sorted(b)


def list_owner(cases, placements):
    """The listed case the setup's pieces match best, unmirrored."""
    return max(cases, key=lambda case: matches(case, placements))


def analyse(path=SOURCE, style="2nd"):
    images = load_images(path)
    cases = []
    for title, line, body in sections(path, style):
        case, mirror, variant, listed = title_cases(title, style)
        pieces = "".join(sorted(set("".join(listed)))) if listed else case or ""
        setups, problems = group(title, body, images, pieces, style)
        for setup in setups:
            problems.extend(f"setup {setup.image}: {p}" for p in setup.problems)
            used = sorted(p for p, _ in setup.placements)
            fits = lambda name: name and all(used.count(p) <= name.count(p) for p in set(used))  # noqa: E731
            if listed and not any(fits(c) for c in listed):
                setup.notes.append(f"setup {setup.image} uses {''.join(used)}, not a subset of "
                                   + " or ".join(listed))
            elif not listed and case and not (fits(case) or fits(mirror)):
                setup.notes.append(f"setup {setup.image} uses {''.join(used)}, not a subset of {case}"
                                + (f" or {mirror}" if mirror else ""))
            owner = list_owner(listed, setup.placements) if listed else case
            completed = []
            for solve in setup.solves:
                solve.problems.extend(frame_check(setup, solve))
                solve.reachable = False
                if solve.problems and all("empty" in p or "do not make" in p for p in solve.problems):
                    extra = complete(setup, solve, owner, None if listed else mirror)
                    if extra:
                        solve.replaced_by = extra
                        completed.extend(extra)
                if not solve.problems:
                    # Several decompositions can draw the same cells; keep the
                    # first one that some SRS order can build.
                    for placements in solve.alternatives:
                        solve.placements = placements
                        if reachable_order(setup, solve):
                            solve.reachable = True
                            break
                    else:
                        solve.placements = solve.alternatives[0]
                        # Queue-based setups may need a solve piece before a
                        # setup piece (TTOZ image823's L tucks under the Z).
                        if reachable_order(Shim([]), Shim(setup.placements + solve.placements, Shim([]))):
                            solve.reachable = True
                            solve.interleaved = True
            # A staged diagram is replaced by its completions; a diagram with
            # more empty cells than two pieces shows only the first steps (or
            # a setup step) and is not a solve.
            kept = []
            for so in setup.solves:
                if getattr(so, "replaced_by", None):
                    continue
                if so.problems and all("empty" in p or "do not make" in p for p in so.problems):
                    setup.notes.append(f"{'+'.join(so.images)} is a step diagram, not a solve "
                                       f"({so.problems[0].split(' of ')[0]}).")
                    continue
                kept.append(so)
            setup.solves = kept + completed
        cases.append({"title": title, "line": line, "case": case, "mirror": mirror, "variant": variant,
                      "cases": listed, "document": os.path.basename(path),
                      "setups": setups, "problems": problems})
    return cases


def analyse_all():
    """Every source document's sections, in `SOURCES` order."""
    return [entry for name, style in SOURCES for entry in analyse(os.path.join(ROOT, name), style)]


def report(selected=None):
    cases = analyse_all()
    totals = [0, 0, 0, 0]
    for entry in cases:
        if selected and selected not in (entry["cases"] or [entry["case"]]):
            continue
        setups = entry["setups"]
        solves = [s for st in setups for s in st.solves]
        ok = sum(1 for s in solves if s.reachable)
        broken = sum(1 for s in solves if s.problems)
        totals[0] += len(setups)
        totals[1] += len(solves)
        totals[2] += ok
        totals[3] += broken
        print(f"{entry['title']:<24} case={entry['case']} variant={entry['variant']:<6} "
              f"setups={len(setups):>2} solves={len(solves):>3} srs_ok={ok:>3} "
              f"broken={broken:>2} section_issues={len(entry['problems'])}")
        if selected:
            for problem in entry["problems"]:
                print("    !", problem)
            for setup in setups:
                print(f"  setup {setup.image} line {setup.line}: "
                      + " ".join(sorted(p for p, _ in setup.placements)))
                for solve in setup.solves:
                    status = "ok" if solve.reachable else ("broken" if solve.problems else "no SRS order")
                    pieces = "".join(sorted(p for p, _ in solve.placements))
                    print(f"    solve {'+'.join(solve.images)} [{pieces}] {status}")
                    for problem in solve.problems:
                        print("       !", problem)
    print(f"TOTAL setups={totals[0]} solves={totals[1]} srs_ok={totals[2]} broken={totals[3]}")


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) > 1 else "report"
    if command == "report":
        report(sys.argv[2] if len(sys.argv) > 2 else None)
    elif command == "write":
        write()
    elif command == "coverage":
        coverage_report()
    else:
        sys.exit(__doc__)
