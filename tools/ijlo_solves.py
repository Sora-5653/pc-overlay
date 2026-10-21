"""Checks verified IJLO solves and generates replay fixtures from them.

    python -I tools/ijlo_solves.py verify
    python -I tools/ijlo_solves.py fixtures

Both commands read data/ijlo.json, so they always use the reviewed cells.
`verify` plays every order of the bag after the IJLO case pieces against each
verified setup + solve, with one HOLD slot, and checks that the solve is
reachable exactly when the setup's condition allows it: the S/Z queue
setups' `last_among` fail condition, or an `ordered_with_hold` build
condition (placement prerequisites plus one HOLD slot, used by save O).
`fixtures` writes synthetic stable-observation sequences to tests/fixtures/,
plus `ijlo_reachable_orders.json`: for each verified setup, the following-bag
orders whose solve SRS reaches (HOLD empty at the bag start). The Rust tests
check that rule conditions accept exactly these orders. The sequences are
generated, not recorded from TETR.IO.
"""

import itertools
import json
import os
import sys
from functools import lru_cache

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from srs import reachable  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BAG = "TIJLOSZ"


def load_verified(rules_path):
    rules = json.load(open(rules_path, encoding="utf-8"))
    found = []
    for method in rules["methods"]:
        for setup in method["setups"]:
            if setup["review_status"] != "verified":
                continue
            for solve in setup["solves"]:
                if solve["review_status"] != "verified":
                    continue
                found.append({
                    "setup_id": setup["id"],
                    "save": setup["save"],
                    "allows": condition_of(setup),
                    "pieces": [as_piece(p) for p in setup["placements"]]
                    + [as_piece(p) for p in solve["target"]],
                    "setup_count": len(setup["placements"]),
                })
    return found


def condition_of(setup):
    """Returns order -> bool: True when the setup's rule says it can be solved."""
    fail = setup.get("fail_condition")
    if fail is not None:
        assert fail["op"] == "last_among", fail
        key, blockers = fail["piece"], tuple(fail["pieces"])
        return lambda order: not all(order.index(key) > order.index(b) for b in blockers)
    build = setup["build_condition"]
    # save O also waits for both queue setups to fail; that part is priority,
    # not reachability, so only the ordered_with_hold term is checked here.
    if build["op"] == "all":
        build = next(c for c in build["conditions"] if c["op"] == "ordered_with_hold")
    assert build["op"] == "ordered_with_hold", build
    rules = [(rule["first"], rule["then"]) for rule in build["prerequisites"]]
    return lambda order: ordered_with_hold(order, rules, build["saved"])


def ordered_with_hold(order, prerequisites, saved):
    """Mirrors Condition::OrderedWithHold: prerequisites plus one HOLD slot."""
    need = {}
    for first, then in prerequisites:
        need.setdefault(then, set()).add(first)
    goal = frozenset(piece for piece in BAG if piece != saved)

    def placeable(piece, placed):
        return piece != saved and piece not in placed and need.get(piece, set()) <= placed

    @lru_cache(None)
    def go(i, hold, placed, locked):
        if placed == goal:
            return True
        if i >= len(order):
            return False
        current = order[i]
        if placeable(current, placed) and go(i + 1, hold, placed | {current}, False):
            return True
        if locked:
            return False
        if hold is None:
            return go(i + 1, current, placed, True)
        return placeable(hold, placed) and go(i + 1, current, placed | {hold}, False)

    return go(0, None, frozenset(), False)


def as_piece(placement):
    return placement["piece"], frozenset((c["x"], c["y"]) for c in placement["cells"])


class Frame:
    """A whole PC drawn without line clears, as in the source images."""

    def __init__(self, pieces):
        self.pieces = pieces
        self.height = max(y for _, cells in pieces for _, y in cells) + 1

    @lru_cache(None)
    def board(self, placed):
        """Live fixed cells after completed rows have cleared: {(x, y): piece}."""
        cells = {c: self.pieces[i][0] for i in placed for c in self.pieces[i][1]}
        cleared = [y for y in range(self.height) if all((x, y) in cells for x in range(10))]
        drop = lambda y: y - sum(1 for row in cleared if row < y)  # noqa: E731
        live = {(x, drop(y)): p for (x, y), p in cells.items() if y not in cleared}
        return live, tuple(cleared)

    @lru_cache(None)
    def can_place(self, placed, index):
        live, cleared = self.board(placed)
        piece, cells = self.pieces[index]
        if any(y in cleared for _, y in cells):
            return False
        moved = [(x, y - sum(1 for row in cleared if row < y)) for x, y in cells]
        return reachable(piece, set(live), moved)


def play(frame, queue, hold, placed):
    """Finds actions that place every frame piece.

    `queue` holds a frame index per upcoming piece, or None for a piece that is
    not part of the frame (the saved T, later bags). Returns a list of
    ("place" | "hold" | "swap", ...) actions, or None when impossible.
    """
    goal = frozenset(range(len(frame.pieces)))

    @lru_cache(None)
    def go(i, hold, placed, locked):
        if placed == goal:
            return ()
        if i >= len(queue):
            return None
        current = queue[i]
        if current is not None and current not in placed and frame.can_place(placed, current):
            rest = go(i + 1, hold, placed | {current}, False)
            if rest is not None:
                return (("place",),) + rest
        if not locked:
            if hold is None:
                rest = go(i + 1, current, placed, True)
                if rest is not None:
                    return (("hold",),) + rest
            elif hold not in placed and hold is not None and frame.can_place(placed, hold):
                rest = go(i + 1, current, placed | {hold}, False)
                if rest is not None:
                    return (("swap",),) + rest
        return None

    return go(0, hold, frozenset(placed), False)


def solve_queue(entry, order):
    """Frame indices for the bag after the case pieces; the saved piece is None."""
    index = {piece: i for i, (piece, _) in enumerate(entry["pieces"]) if i >= entry["setup_count"]}
    return [index.get(piece) for piece in order]


def verify(rules_path):
    ok = True
    for entry in load_verified(rules_path):
        frame = Frame(entry["pieces"])
        setup = frozenset(range(entry["setup_count"]))
        counts = {}
        for order in itertools.permutations(BAG):
            fails = not entry["allows"](order)
            queue = solve_queue(entry, order)
            for hold_start in (False, True):
                if hold_start:
                    # A HOLD piece at the PC start forces the first bag piece into HOLD.
                    solvable = play(frame, queue[1:], queue[0], setup) is not None
                else:
                    solvable = play(frame, queue, None, setup) is not None
                counts[(fails, solvable)] = counts.get((fails, solvable), 0) + 1
                if solvable == fails:
                    ok = False
        print(entry["setup_id"], {f"fail={f} solvable={s}": n for (f, s), n in sorted(counts.items())})
    print("OK: reachable exactly when the fail condition does not match" if ok else "MISMATCH")
    return ok


def render(live):
    rows = []
    for y in range(max((y for _, y in live), default=-1) + 1):
        rows.append("".join(live.get((x, y), ".") for x in range(10)))
    return rows


def generate(entry, case_queue, hold_start, bag, later, undo_after=None):
    """Plays the setup and solve from a PC 2 start and records each stable state."""
    frame = Frame(entry["pieces"])
    setup_index = {p: i for i, (p, _) in enumerate(entry["pieces"]) if i < entry["setup_count"]}
    pieces = list(case_queue) + list(bag) + list(later)
    queue = [setup_index[p] for p in case_queue] + solve_queue(entry, bag) + [None] * len(later)
    hold = setup_index[hold_start] if hold_start else None
    actions = play(frame, queue, hold, frozenset())
    assert actions is not None, "fixture queue cannot be solved"

    piece_of = lambda index, i: pieces[i] if index is None else frame.pieces[index][0]  # noqa: E731
    steps = []

    def record(i, hold_piece, placed, label):
        live, _ = frame.board(frozenset(placed))
        steps.append({
            "label": label,
            "pc": 2,
            "board": render(live),
            "hold": hold_piece,
            "current": pieces[i],
            "next": "".join(pieces[i + 1:i + 6]),
            "pieces_left": len(frame.pieces) - len(placed),
            "setup_left": sum(1 for k in range(entry["setup_count"]) if k not in placed),
        })

    first_pc1 = {
        "label": "end of PC 1",
        "pc": 1,
        "board": ["IIII......"],
        "hold": hold_start,
        "current": "Z",
        "next": "".join(pieces[:5]),
        "pieces_left": None,
        "setup_left": None,
    }
    i, held, held_piece, placed = 0, hold, hold_start, set()
    record(0, held_piece, placed, "PC 2 start")
    for action in actions:
        if action[0] == "place":
            placed.add(queue[i])
            label = f"place {pieces[i]}"
        elif action[0] == "hold":
            held, held_piece = queue[i], pieces[i]
            label = f"hold {pieces[i]}"
        else:
            # The swap itself is a stable state before the held piece is placed.
            steps.append(dict(steps[-1], label=f"swap {pieces[i]} for {held_piece}",
                              hold=pieces[i], current=held_piece))
            placed.add(held)
            label = f"place {held_piece} from HOLD"
            held, held_piece = queue[i], pieces[i]
        i += 1
        if len(placed) == len(frame.pieces):
            steps.append({"label": "perfect clear", "pc": 3, "board": [], "hold": held_piece,
                          "current": pieces[i], "next": "".join(pieces[i + 1:i + 6]),
                          "pieces_left": 0, "setup_left": 0})
        else:
            record(i, held_piece, placed, label)

    steps = [first_pc1] + steps
    if undo_after is not None:
        setup_done = next(k for k, s in enumerate(steps) if s["setup_left"] == 0)
        cut = setup_done + undo_after
        replay = [dict(s) for s in steps[setup_done + 1:]]
        undo = dict(steps[setup_done], label="Undo back to the completed setup", rollback=True)
        steps = steps[:cut + 1] + [undo] + replay
    return {
        "description": f"Synthetic stable observations for {entry['setup_id']}; generated by "
                       "tools/ijlo_solves.py, not recorded from TETR.IO.",
        "setup_id": entry["setup_id"],
        "steps": steps,
    }


def fixtures(rules_path):
    entries = {e["setup_id"]: e for e in load_verified(rules_path)}
    out = os.path.join(ROOT, "tests", "fixtures")
    os.makedirs(out, exist_ok=True)
    cases = {
        # HOLD O at the start; S opens the next bag, so the S setup is chosen at once.
        "ijlo_s_queue_with_undo.json": generate(
            entries["ijlo_s_queue"], "IJL", "O", "STZJLIO", "LOJISTZ" + "ZTJSOIL", undo_after=2),
        # HOLD empty; T J S makes the S setup fail and Z is revealed after two pieces.
        "ijlo_z_queue.json": generate(
            entries["ijlo_z_queue"], "OIJL", None, "TJSZLIO", "ISZTOJL" + "LJOTSZI"),
        # T J L before S and Z: both queue setups fail. Both save-O sides can
        # be solved, and source order picks the left one.
        "ijlo_save_o_left.json": generate(
            entries["ijlo_save_o_left"], "OIJL", None, "TJLISZO", "SZIOTJL" + "IOLJTZS"),
        # T L J before S and Z, and I before J: the left side cannot hold both
        # I and O, so only the right side remains.
        "ijlo_save_o_right.json": generate(
            entries["ijlo_save_o_right"], "OIJL", None, "TLIJZSO", "JZSOTIL" + "ZOLSITJ"),
    }
    reachable_orders = {}
    for entry in entries.values():
        frame = Frame(entry["pieces"])
        setup = frozenset(range(entry["setup_count"]))
        reachable_orders[entry["setup_id"]] = [
            "".join(order) for order in itertools.permutations(BAG)
            if play(frame, solve_queue(entry, order), None, setup) is not None
        ]
    cases["ijlo_reachable_orders.json"] = {
        "description": "Following-bag orders whose solve is reachable with SRS 90-degree "
                       "movement and one HOLD slot, empty at the bag start; generated by "
                       "tools/ijlo_solves.py.",
        "orders": reachable_orders,
    }
    for name, data in cases.items():
        path = os.path.join(out, name)
        with open(path, "w", encoding="utf-8", newline="\n") as handle:
            json.dump(data, handle, indent=2)
            handle.write("\n")
        size = f"{len(data['steps'])} steps" if "steps" in data else "orders"
        print(f"wrote {os.path.relpath(path, ROOT)} ({size})")


if __name__ == "__main__":
    rules = os.path.join(ROOT, "data", "ijlo.json")
    command = sys.argv[1] if len(sys.argv) > 1 else "verify"
    if command == "verify":
        sys.exit(0 if verify(rules) else 1)
    elif command == "fixtures":
        fixtures(rules)
    else:
        sys.exit(f"unknown command: {command}")
