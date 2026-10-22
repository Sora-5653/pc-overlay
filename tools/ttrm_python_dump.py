#!/usr/bin/env python3
"""Dump czsmall_nnue's exact TETR.IO reconstruction for the Rust engine.

The Rust engine (`src/engine.rs`) is a port of the solo half of
`ReplayMachine` in czsmall_nnue's `trainer/ttrm_exact_replay.py`. This script
runs that Python reconstruction on `.ttrm` files and writes, per stream, every
placement plus the garbage holes it tanked. `verify-ttrm --reference` replays
the same streams in Rust with those holes and compares each placement.

    python tools/ttrm_python_dump.py --reconstructor <czsmall_nnue>/trainer \
        <file-or-dir>... -o dump.json

The reconstructor directory can also come from `TTRM_RECONSTRUCTOR`.
Only exactly reconstructed streams are written.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path


def load_reconstructor(directory: str):
    sys.path.insert(0, directory)
    import ttrm_exact_replay  # type: ignore[import-not-found]

    return ttrm_exact_replay


def replay_files(inputs: list[str]) -> list[Path]:
    out: list[Path] = []
    for item in inputs:
        path = Path(item)
        if path.is_dir():
            out.extend(sorted(p for p in path.rglob("*") if p.suffix in {".ttrm", ".ttr"}))
        else:
            out.append(path)
    return out


def placement_row(placement, holes: list[int]) -> dict:
    return {
        "subframe": placement.subframe,
        "piece": placement.piece,
        "rotation": placement.rotation,
        "x": placement.center_x,
        "y": placement.center_y,
        "hold": placement.hold,
        "used_hold": placement.used_hold,
        "lines": placement.lines_cleared,
        "rows": list(placement.post_rows),
        "holes": holes,
    }


def with_solo_snapshot(t, stream):
    """A solo `.ttr` has no `full` event; give the reconstructor one built
    from `replay.options` with an empty board, as the Rust replay does.

    Its `end` event carries no snapshot either, so the Python end check
    cannot pass; `verify-ttrm` checks `replay.results.stats` instead.
    Returns the stream and whether it was solo."""
    events = t._stream_events(stream)
    if any(str(e.get("type", "")).lower() == "full" for e in events):
        return stream, False
    options = dict(stream.get("replay", {}).get("options", {}))
    full = {
        "frame": 0,
        "type": "full",
        "data": {
            "options": options,
            "game": {
                "board": [[None] * 10 for _ in range(40)],
                "bag": [],
                "hold": {"piece": None, "locked": False},
                "handling": options.get("handling", {}),
            },
        },
    }
    return {"replay": {"events": [full, *events]}}, True


def dump_stream(t, stream, opponent, tanked: list[list[int]]) -> list[dict] | None:
    tanked.clear()
    stream, solo = with_solo_snapshot(t, stream)
    if opponent is None:
        run = t.reconstruct_stream(stream, garbage_column_mode="seed")
        if solo and not run.warnings:
            return [placement_row(p, []) for p in run.placements]
    else:
        run = t.reconstruct_stream(
            stream,
            garbage_column_mode="seed",
            recorded_outgoing=t.recorded_outgoing_by_source_frame(opponent),
            opponent_end_frame=t._integer(
                t.last_event(t._stream_events(opponent), "end").get("frame"), 0
            ),
            opponent_min_interaction_delay=t.minimum_interaction_delay(opponent),
        )
    if not run.verification.exact or run.warnings:
        return None
    calls = iter(list(tanked))
    rows = []
    for placement in run.placements:
        holes = next(calls) if placement.garbage_received > 0 else []
        if len(holes) != placement.garbage_received:
            raise AssertionError("tanked garbage does not line up with placements")
        rows.append(placement_row(placement, holes))
    return rows


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("inputs", nargs="+")
    ap.add_argument("--reconstructor", default=os.environ.get("TTRM_RECONSTRUCTOR"))
    ap.add_argument("-o", "--output", required=True, type=Path)
    args = ap.parse_args()
    if not args.reconstructor:
        ap.error("--reconstructor or TTRM_RECONSTRUCTOR is required")
    t = load_reconstructor(args.reconstructor)

    # Record the holes of every tanked batch without touching the reconstructor.
    tanked: list[list[int]] = []
    original_add_garbage = t.Board.add_garbage

    def recording_add_garbage(self, holes):
        tanked.append([int(h) for h in holes])
        return original_add_garbage(self, holes)

    t.Board.add_garbage = recording_add_garbage

    out = []
    skipped = 0
    for path in replay_files(args.inputs):
        root = json.loads(path.read_text(encoding="utf-8"))
        rounds = t.rounds_from_document(root)
        documents = (
            [(index, t.player_streams(round_data)) for index, round_data in enumerate(rounds)]
            if rounds
            else [(0, [root])]
        )
        for round_index, streams in documents:
            for player, stream in enumerate(streams[:2]):
                opponent = streams[1 - player] if len(streams) == 2 else None
                try:
                    rows = dump_stream(t, stream, opponent, tanked)
                except Exception as exc:  # reconstruction failures are skipped
                    print(f"{path} round {round_index} player {player}: {exc}", file=sys.stderr)
                    rows = None
                if rows is None:
                    skipped += 1
                    continue
                out.append(
                    {
                        "file": str(path.resolve()),
                        "round": round_index,
                        "player": player,
                        "placements": rows,
                    }
                )
    args.output.write_text(json.dumps(out), encoding="utf-8")
    print(f"wrote {len(out)} exact streams ({skipped} skipped) to {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
