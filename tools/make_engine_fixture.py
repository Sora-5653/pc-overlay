#!/usr/bin/env python3
"""Generate `tests/fixtures/engine_differential.json`.

Builds synthetic solo replay streams with random inputs (DAS holds, ARR,
soft drop, 180 spins, Hold, waits long enough for automatic locks) under
several handling and gravity settings, runs czsmall_nnue's exact TETR.IO
reconstruction on them, and stores the events with the Python placements.
`tests/engine_differential.rs` replays the same events on the Rust engine.
The streams contain no player data, unlike real `.ttrm` files.

    python tools/make_engine_fixture.py --reconstructor <czsmall_nnue>/trainer
"""

from __future__ import annotations

import argparse
import json
import os
import random
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "tests" / "fixtures" / "engine_differential.json"

CONFIGS = [
    # League-like: instant ARR/SDF, safelock, gravity ramp starting early.
    {"handling": {"arr": 0, "das": 5.7, "dcd": 0, "sdf": 41, "safelock": True, "cancel": False, "may20g": True},
     "options": {"g": 0.02, "gincrease": 0.0035, "gmargin": 600, "locktime": 30, "lockresets": 15}},
    # Slow ARR with DCD, finite SDF and DAS cancel.
    {"handling": {"arr": 2, "das": 10, "dcd": 1, "sdf": 6, "safelock": False, "cancel": True, "may20g": True},
     "options": {"g": 0.02, "gincrease": 0, "gmargin": 0, "locktime": 30, "lockresets": 15}},
    # Fractional ARR/DAS, fast gravity.
    {"handling": {"arr": 0.5, "das": 8.3, "dcd": 0.5, "sdf": 20, "safelock": False, "cancel": False, "may20g": True},
     "options": {"g": 1.0, "gincrease": 0, "gmargin": 0, "locktime": 30, "lockresets": 15}},
    # 20G.
    {"handling": {"arr": 0, "das": 6, "dcd": 0, "sdf": 41, "safelock": True, "cancel": False, "may20g": True},
     "options": {"g": 20, "gincrease": 0, "gmargin": 0, "locktime": 30, "lockresets": 15}},
    # No gravity, long lock delay, few resets.
    {"handling": {"arr": 1, "das": 7, "dcd": 0, "sdf": 10, "safelock": False, "cancel": False, "may20g": False},
     "options": {"g": 0, "gincrease": 0, "gmargin": 0, "locktime": 60, "lockresets": 4}},
]

KEYS_ROTATE = ["rotateCW", "rotateCCW", "rotate180"]


def piece_chunk(rng: random.Random, t: int) -> tuple[list[dict], int]:
    """Inputs for one piece starting at time `t` (tenths of a frame)."""
    events: list[dict] = []

    def at(time: int, kind: str, key: str, **extra) -> None:
        data = {"key": key, "subframe": (time % 10) / 10, **extra}
        events.append({"frame": time // 10, "type": kind, "data": data})

    def tap(key: str, hold_for: int, **extra) -> None:
        nonlocal t
        at(t, "keydown", key, **extra)
        t += hold_for
        at(t, "keyup", key)
        t += rng.randint(0, 30)

    if rng.random() < 0.2:
        tap("hold", rng.randint(1, 20))
    for _ in range(rng.choice([0, 1, 1, 2, 3])):
        tap(rng.choice(KEYS_ROTATE), rng.randint(1, 30))
    for _ in range(rng.choice([0, 0, 1, 2])):
        direction = rng.choice(["moveLeft", "moveRight"])
        # Short taps and DAS holds that run into the wall.
        tap(direction, rng.choice([5, 20, 60, 150]), hoisted=rng.random() < 0.1)
    if rng.random() < 0.3:
        tap("softDrop", rng.randint(5, 200))
        if rng.random() < 0.5:
            tap(rng.choice(KEYS_ROTATE), rng.randint(1, 10))
    if rng.random() < 0.15:
        # Let gravity and lock delay finish the piece.
        t += rng.randint(300, 1500)
    else:
        tap("hardDrop", rng.randint(1, 40))
    return events, t


def well_board(depth: int) -> list[list[str | None]]:
    """Top-down 40-row board whose bottom rows leave columns 3-6 open, so
    pieces dropped near the spawn column clear lines."""
    rows: list[list[str | None]] = [[None] * 10 for _ in range(40 - depth)]
    rows += [["gb"] * 3 + [None] * 4 + ["gb"] * 3 for _ in range(depth)]
    return rows


def build_stream(config: dict, seed: int, board: list, chunks: list[list[dict]], end_time: int) -> dict:
    options = {"version": 19, "seed": seed, "clutch": True, "spinbonuses": "all-mini+", **config["options"]}
    full = {
        "frame": 0,
        "type": "full",
        "data": {
            "options": options,
            "game": {
                "board": board,
                "bag": [],
                "hold": {"piece": None, "locked": False},
                "g": options["g"],
                "handling": config["handling"],
            },
        },
    }
    events = [{"frame": 0, "type": "start", "data": {}}, full]
    for chunk in chunks:
        events.extend(chunk)
    events.append({"frame": end_time // 10 + 1, "type": "end", "data": {}})
    return {"replay": {"events": events}}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--reconstructor", default=os.environ.get("TTRM_RECONSTRUCTOR"))
    ap.add_argument("--streams-per-config", type=int, default=4)
    ap.add_argument("--pieces", type=int, default=60)
    ap.add_argument("--rng-seed", type=int, default=20261021)
    args = ap.parse_args()
    if not args.reconstructor:
        ap.error("--reconstructor or TTRM_RECONSTRUCTOR is required")
    sys.path.insert(0, args.reconstructor)
    import ttrm_exact_replay as t  # type: ignore[import-not-found]
    sys.path.insert(0, str(ROOT / "tools"))
    from ttrm_python_dump import placement_row

    rng = random.Random(args.rng_seed)
    cases = []
    for config_index, config in enumerate(CONFIGS):
        for _ in range(args.streams_per_config):
            seed = rng.randint(1, 2**31 - 2)
            board = well_board(rng.choice([0, 6, 12]))
            chunks, ends = [], []
            time = rng.randint(0, 9)
            for _ in range(args.pieces):
                chunk, time = piece_chunk(rng, time)
                chunks.append(chunk)
                ends.append(time)
            # Random play tops out; keep the longest prefix that replays.
            for count in range(len(chunks), 0, -1):
                stream = build_stream(config, seed, board, chunks[:count], ends[count - 1])
                try:
                    run = t.reconstruct_stream(stream, garbage_column_mode="seed")
                except Exception:
                    continue
                if run.warnings or not run.placements:
                    continue
                cases.append(
                    {
                        "config": config_index,
                        "events": stream["replay"]["events"],
                        "placements": [placement_row(p, []) for p in run.placements],
                    }
                )
                break
    OUTPUT.write_text(json.dumps(cases, separators=(",", ":")) + "\n", encoding="utf-8")
    total = sum(len(case["placements"]) for case in cases)
    print(f"wrote {len(cases)} streams, {total} placements to {OUTPUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
