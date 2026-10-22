# Perfect Clear 2nd PC learning overlay

This directory now contains the first implementation slice for the specification in
`tetrio_2nd_pc_learning_overlay_spec_v0.1.md`.

## Implemented core

- Typed 7-piece, 10x20 board, placement, and 1st-to-7th PC ordinal models.
- Three-valued condition evaluation (`satisfied`, `failed`, `pending`) so an
  undisclosed NEXT never becomes a false positive.
- Conditions for `seen`, `not_seen`, order, bounded prefixes, last-piece checks,
  accessibility, HOLD/Current, Boolean composition, and structured queue patterns.
- Explicit `[TJ]!,S`-style permutation-prefix representation. Free-form pattern
  strings are not interpreted at runtime.
- Allowed J/L and S/Z mirroring for rules and setup cells. Solves are not generated
  by mirroring.
- Recommendation ordering, local priority, fail-condition removal, and diagnostic
  candidate reports.
- Stable-observation session tracking, incremental NEXT history, 1st-to-7th cycle,
  confidence gating, manual resynchronization, snapshot rollback, and solve cycling
  when Undo returns to a setup start.
- JSON rule validation with source provenance and two explicit policies:
  verified-only selection for strict use, and a visibly labelled reference
  preview for still-unreviewed learning data.
- A separate image-derived IJLO transcription (`data/ijlo/diagram_cells.json`)
  with a bottom-left 10-column coordinate frame, cell-range validation, and
  tetromino-shape validation. Rows that were cleared earlier but are still
  drawn by the source image are listed in `cleared_rows`; shapes are checked
  after collapsing them, and a split piece without that explanation is
  reported as a warning.
- Runtime attachment of reviewed-later setup geometry to rules, plus setup
  progress calculation that separates correct, missing, mismatched, and
  unexpected fixed cells for overlay rendering.
- A user-controlled 10x20 board calibration and a stable-frame gate. TETR.IO's
  display magnification changes the board cell size, so the calibration stores
  the board centre, top edge, and width as client-area ratios. HOLD, Current,
  and NEXT5 are then searched relative to the calibrated board rather than
  using fixed screen rectangles. Recognition must provide the same complete
  observation for three consecutive frames at 70% confidence or higher before
  the session tracker can consume it. The three-frame agreement is the primary
  animation/noise guard; the previous 92% threshold rejected normal antialiased
  TETR.IO previews.
- Windows-only visible-window discovery that filters for `TETR.IO` titles,
  records the client rectangle and process identity, and chooses the largest
  qualifying candidate. It does not inspect TETR.IO internals or monitor input.
- A Windows GDI client-area frame source returning validated top-to-bottom RGBA
  frames. `probe-capture --bmp <path>` is an opt-in calibration diagnostic; the
  normal frame path does not write screenshots.
- A conservative pixel recognizer for the 10x20 board, HOLD, Current, and
  NEXT5. It uses an explicit palette plus hue fallback for the bright/dimmed
  TETR.IO colours, rejects empty or ambiguous previews, derives Current from
  the connected four-cell component in the board's spawn region, removes that
  exact active component and any detached substantially dimmer ghost component
  from the fixed board, and leaves low-confidence state at the recognition
  boundary. Frames captured while Current is outside the spawn region pause
  state updates instead of treating the falling piece as fixed blocks.
- Live-frame fixes from the first TETR.IO run (2026-10-18). The calibrated
  board snaps to the visible near-white border lines, because a drag
  selection that includes the border (194 px instead of the 192 px interior)
  drifts by a quarter cell at the bottom row. Current is also searched in
  three rows above the board, where TETR.IO spawns pieces. A dimmed HOLD is
  treated as the empty placeholder only when one row has four full cells and
  the other has none. The ghost outline's chromatic-aberration fringes (up to
  ~26 coloured pixels per cell) stay below the ambiguous threshold of 40. A
  coloured ghost must be Current shifted straight down, because TETR.IO
  draws locked blocks dimmer than Current. `tests/recognition_frames.rs`
  covers these cases with crops of recorded frames.
- A runnable `run-live` diagnostic that exercises window discovery, capture,
  recognition, three-frame stability, session tracking, and rule selection.
  It prints a compact status line and uses the same labelled preview policy as
  the overlay unless `--reviewed-only` is supplied.
- A Windows transparent overlay runner (`run-overlay`) that follows the
  visible TETR.IO client rectangle, stays topmost and click-through, and draws
  recognition status plus IJLO setup cells only while the tracker is on PC 2
  and HOLD + Current + the earliest NEXT pieces form IJLO. Once the setup is
  complete it draws the remaining cells of a verified solve once the player
  presses `S` or `Ctrl+Alt+S` (OFF at the start of every PC). Unknown states
  never reuse a stale
  recommendation.

## IJLO transcription status

`data/ijlo.json` maps the five setup groups and source images from lines 20-32 of
`Algebruh's 2nd Methods.md`. Setup conditions are evaluated over the bag that
follows the four IJLO case pieces (`EvalContext::for_following_bag`); the case
pieces and the bag after that never count toward `[TJ]!,S`-style conditions.

- `ijlo_s_queue` (image 4) and `ijlo_z_queue` (image 1) are `verified`. They save
  T and fail when T and J both precede S (or T and L both precede Z), even with
  other pieces in between. When both can be built, the one whose key piece
  appears first is chosen.
- The save-O pair (images 7+8+9 left, 10+11+12 right) is `verified` since
  2026-10-18. The source gives no queue condition, so it is derived: without
  HOLD the left solve needs J before I, T before Z, and T and J before L (the
  L passes through row y=1, which only the image 8 T and J complete); the
  right is the J/L, S/Z swap. `ordered_with_hold` evaluates those
  prerequisites with one HOLD slot over every undisclosed completion of the
  bag (the saved O fills HOLD when it arrives). Each side covers 1344 of the
  5040 orders. Save O also waits until both S/Z queue setups have failed, as
  the source uses it only then; left wins a tie by source order.
- The sandbox extension (image 13) remains `review_required`: its trigger is
  not explicit in the prose, and skims and 180-degree spins are not analysed.
- The S/Z queue and save-O solves are `verified` and carry their cells in
  `data/ijlo.json`. `tools/ijlo_solves.py verify` plays every order of the
  following bag with one HOLD slot, using SRS 90-degree movement (standard
  kicks, no 180-degree rotation): each solve is reachable for exactly the
  orders its setup's condition accepts, whether HOLD was empty or occupied at
  the PC start. SRS reachability is computed once per set of placed pieces
  and cached, so the whole check takes about two seconds.
  `tools/ijlo_solves.py fixtures` also writes
  `tests/fixtures/ijlo_reachable_orders.json`, and
  `tests/ijlo_conditions.rs` checks that the Rust conditions accept exactly
  those orders. The sandbox solves remain `review_required`.
- `last_among` decides early within the one-bag window: once the other pieces
  are disclosed and the key piece is not, the key piece will be last.
- `evaluate_solve_progress` places a setup plus solve on the live board. The
  source draws the whole PC without clearing rows, so it infers which frame
  rows have already cleared and reports the remaining cells, or `Ambiguous` /
  `Inconsistent` when it cannot. Rule selection keeps a setup while a verified
  solve explains the board, so guidance no longer drops when solve pieces are
  placed early or rows clear. `run-overlay` draws the remaining solve cells
  only for an `InProgress` result whose setup pieces are all placed, and draws
  nothing for `Ambiguous` / `Inconsistent`.

The default overlay draws unreviewed setups as
`IJLO reference guidance ... (unreviewed)`; `--reviewed-only` suppresses them,
so strict mode stops when both S/Z queue setups fail.

## Generated cases (all sections of the source)

`tools/build_cases.py` turns every `###` section of the source document into
`data/cases/<CASE>.json` (49 cases, about 500 setups) without hand
transcription:

- Each embedded image is read cell by cell (`tools/diagrams.py`). An image
  without gray context is a setup; a later image that adds pieces to it is
  the same setup drawn in two steps (a queue-based setup). Solve images are
  matched to the setup, to an earlier stage, or to the mirrored setup by
  their gray context. A section that draws its setup only in gray (TJOO's
  PCO+O) gets it tiled with the case pieces, and gray cells beyond the setup
  are the leftover case piece of a 3-piece setup.
- The source draws a PC without line clears, so a piece placed after a row
  cleared appears split around that row. Coloured cells are split into
  tetrominoes allowing such skipped rows; among several splits the first one
  with an SRS order is kept.
- A solve is `verified` when setup plus solve fill whole rows and some order
  is reachable with SRS 90-degree movement (standard kicks), clearing rows as
  they complete. Every generated solve passes. Setup pieces go first, except
  for a solve that only works with a solve piece tucked under a setup piece
  before the setup is finished (TTOZ image823's L under the Z); its table
  then holds the setup pieces too (`setup_in_table`).
- Staged diagrams: when the last stage leaves at most two pieces' worth of
  cells empty (image55, 180-182, 791), the empty cells are tiled with pieces
  still available and every tiling with an SRS order becomes a solve, noted
  "completed ... by search". Diagrams with more empty cells show only the
  first steps or a setup step (image96, 148, 154, 435, 680, 681); they are
  recorded in the setup's note and are not solves.
- A setup drawn without solves borrows them from a setup with the same cells
  and pieces (TJLS image361/362) or from its mirror (JLSZ image159/160), and
  they are re-checked with SRS. JLOS image94 (the JIO queue-based step) has
  no solve in the source and stays `review_required`.
- A setup's `build_condition` is `solvable`: from the 2nd-PC start (HOLD
  then, the case pieces in queue order, then the following bag) played with
  one HOLD slot. The setup's case pieces go first in any order, then the
  solve follows its SRS placement table, so a HOLD still occupied when the
  bag starts is modelled. When the case start is unknown, the leftover case
  pieces and the bag are played from an empty HOLD. No queue rule is taken from the prose; the prose is kept
  in the setup's note. Pieces a queue-based setup takes from the following
  bag are placed before the solve.
- A section `ABCD/ABCE` writes both cases: each setup goes to the case its
  pieces match and is mirrored into the other. A case that is its own mirror
  (JLSZ, TISZ, ...) gets both orientations. Mirrors are re-searched with SRS,
  because the kick table is not symmetric.
- Selection: a setup is chosen as usual once `solvable` is satisfied for
  every completion of the bag. Until some setup is decided, the verified
  setup that solves the largest share of the remaining bag orders is shown
  (reason `likely: ...`), as long as it matches the board.
- `run-overlay` loads `data/ijlo.json` (hand-checked IJLO) and every file in
  `--cases` (default `data/cases`), skipping generated cases whose pieces the
  hand data already covers. The case is the sorted HOLD + first pieces at the
  2nd-PC start, so duplicate cases such as TJJO are recognised too.

```powershell
python -I tools/build_cases.py report           # parse summary per section
python -I tools/build_cases.py report TJOO      # one case in detail
python -I tools/build_cases.py write            # data/cases/*.json (~6 min)
python -I tools/build_cases.py coverage         # share of bags per setup +
                                                # tests/fixtures/cases_coverage.json
cargo test --test generated_cases               # Rust solvable == Python on all 5040 bags
```

`tests/fixtures/cases_coverage.json` checks each verified setup for the bag
alone and for three case starts (HOLD empty, holding the first case piece,
holding the last). Not modelled yet: 180-degree spins (the source marks some
solves "needs 180"), and setup choices the prose makes for comfort rather
than coverage.

## Verification

GitHub Actions (`.github/workflows/ci.yml`) runs `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, and `cargo test --release`
(the generated-case tests take minutes unoptimized) on a clean
Windows checkout for every pull request and every push to `main`. The
workflow pins Rust 1.97.1, the local toolchain, because newer releases add
Clippy lints (Rust 1.99 flags `chunks_exact(4)` and a manual
lowest-bit isolation); update the pin together with the local toolchain. It
covers only what the repository itself contains. The Python tools, captured
TETR.IO sessions, and anything that needs a running TETR.IO are checked
locally.

```powershell
cargo test
cargo run --bin validate-rules -- data/ijlo.json
cargo run --bin validate-diagrams -- data/ijlo/diagram_cells.json
python -I tools/ijlo_solves.py verify
cargo run --bin probe-window
cargo run --bin probe-capture
cargo run --bin configure-overlay
cargo run --bin probe-recognition -- --bmp <captured.bmp>
cargo run --bin run-live -- --once --verbose
cargo run --bin run-overlay
```

`run-live --record-dir DIR [--record-max N]` writes every frame's verbose
summary to `DIR/log.txt` and saves up to N (default 40) frames that did not
yield a confident observation as BMP, at most one per 0.7 s.
`probe-recognition --bmp PATH --cells` prints the per-cell piece and
classified-pixel count, top row first.

`run-live` without `--once` keeps sampling every 100 ms. `--once` is useful for
calibration and smoke checks; `--verbose` adds per-region confidence and the
resolved board rectangle. The default calibration targets the observed
1920x1009 TETR.IO Desktop client area, but a different display magnification
should be configured before trusting recognition.

The PC number starts at 1 and advances after a stable nonempty-to-empty board
transition. While `run-overlay` is running, press `Ctrl+Alt+1` through
`Ctrl+Alt+7` to resynchronize it after a mid-session launch or recognition
mistake. The overlay status says whether it is waiting for PC 2, whether the
four-piece case is not IJLO, or which IJLO setup is active.

Solve display (`show_solve` in the tracker's guidance state) starts OFF on
every PC, so finishing a setup never reveals the solve by itself. Once a
verified S/Z queue setup is complete, the status line reads
`IJLO setup complete (...); press S to show the solve`, and nothing is drawn,
so the player's own solve is not flagged as a deviation. Then:

- `S` turns the display ON. It never turns it OFF, because `S` may also be a
  game key (the overlay only reads key state, so TETR.IO still receives it).
- `Ctrl+Alt+S` toggles the display ON and OFF.
- Pressed before the setup is complete, either key leaves the display OFF and
  the status line shows `| solve hotkey ignored: setup not complete` for about
  1.5 seconds. Accepted presses show `| solve shown` or `| solve hidden`.

With it ON, the completed setup is replaced by the cells of its verified
solve, and placed solve pieces drop out as rows clear. A PC boundary or a
`Ctrl+Alt+1..7` resync turns it OFF again, and Undo to an earlier state
restores the setting that state had. The detail line ends with `solve ON` or
`solve OFF`.

## Unified guidance (source rules + Setup Konbini)

Since 2026-10-21 `run-overlay` shows one guidance without a mode switch
(the former `Ctrl+Alt+K` Setup Konbini mode is merged into it):

- 2nd PC: the source rules above (`data/ijlo.json`, `data/cases/*.json`)
  come first. When they have no data for the case, or select no setup for
  the board and queue, the 2nd-PC Setup Konbini setups the live queue can
  build are shown instead, and the detail line says why
  (`guide=Konbini (source: ...)`).
- Every other PC: the Setup Konbini setups for that PC number.
- The status line names the origin: `PC Overlay [source]: ...` or
  `PC Overlay [Konbini 2nd]: ...`; the detail line ends with
  `guide=source` / `guide=Konbini`.

Hotkeys (changes from the two-mode version in bold):

| Key | Action |
|---|---|
| `S` / `Ctrl+Alt+S` | show / toggle the solve once the setup is complete (as before) |
| `Ctrl+Alt+1..7` | resynchronize the PC number (as before) |
| `Ctrl+Alt+N` / `Ctrl+Alt+B` | next / previous Setup Konbini candidate; **now always active**, back to the best candidate when a new PC starts |
| **`Ctrl+Alt+K`** | **was: Setup Konbini mode ON/OFF. Now: the 2nd PC uses Setup Konbini even when source rules apply (press again to go back)** |
| `Ctrl+Alt+F` | download the sheets again (as before) |

The sheets are read from `data/konbini/` at startup, and **downloaded in the
background at startup when that folder is empty** (previously on the first
`Ctrl+Alt+K`); the status line shows `fetching Setup Konbini...` meanwhile.
A completed Konbini setup gets solves from the perfect-clear search (see
"Perfect-clear solve search").

Tests: `cargo test --bin run-overlay` covers source-first on the 2nd PC,
the `Ctrl+Alt+K` override, fallback with the reason in the note, other PCs,
completed source setups enabling `S`, and the missing-data message.
Not tried in TETR.IO yet.

### Setup Konbini data and build search

Setups for every PC (1st to 7th) come from the public
[Setup Konbini](https://docs.google.com/spreadsheets/d/1tI9fPeeEEEhWS2h0Gzib5UIjLKN33wd18WQSKRjxYMw/edit)
spreadsheet. Only setups the live HOLD and queue can build are shown.

`cargo run --bin fetch-konbini` downloads the same files from the console.
Each `Nth: Data` sheet is fetched as CSV with the system `curl`
(`gviz/tq?tqx=out:csv`, which works while the spreadsheet is public) and saved
as `data/konbini/1st.csv` ... `7th.csv`; the folder is not committed. In a
sheet, every `FUMEN` header starts a block whose `ID` and `SOL` columns are the
nearest ones to its left; the 5th-PC sheet has no `SOL` label and uses the
percentage column after its glue fumen. Setup cells come from the first page
of the fumen (`src/fumen.rs`); gray cells are drawn as "any piece".
On 2026-10-21 the sheets gave 184 / 359 / 363 / 266 / 802 / 476 / 75 setups
for the 1st to 7th PC, and every one decodes into whole tetrominoes
(`tests/konbini_data.rs`, skipped without the cache).

The sheet for the tracked PC number is used. A setup is a candidate when

- the PC can supply its pieces (a quick filter): a setup of `k` pieces must
  come from the first `k + 1` pieces from the PC start (HOLD first, one HOLD
  slot),
- the board is the setup in progress: every block is one of its cells with
  the same colour. Rows the setup fills clear in the game but stay in the
  sheet's drawing (179 setups do this), so cleared rows are inferred and
  the remaining cells are drawn where they are on the live board, and
- it can still be built from the live HOLD, Current, and NEXT
  (`src/finder.rs`).

The build search follows knewjade's solution-finder: a bitboard field, a
move generator that finds every lockable placement of a piece by a BFS over
position and rotation (shift, soft drop, 90 and 180-degree rotation with
TETR.IO's SRS+ kicks, cached per field and piece), and a depth-first search
over the piece order with one HOLD slot that memoises failed states. Two
prunings cut the search: HOLD plus the rest of the queue must still hold a
piece for every unplaced coloured tetromino, and, for setups without line
clears, the unplaced cells must split into groups of a multiple of 4 cells.
The SRS+ tables are TETR.IO's as reproduced by the TETR.IO engine
reimplementation triangle.js (`src/engine/utils/kicks/data.ts`, y down,
tried after the unkicked rotation); solution-finder ships no SRS+ table.

A setup of 7 pieces needs one or two pieces after NEXT. Their options come
from the 7-bag, assuming a perfect-clear loop (10 pieces per PC). A setup
buildable for every option is shown first; one buildable for some is shown
after them with `depends on next pieces`. As in solution-finder's
percentages, each option is judged with the whole queue known. Candidates
are then ordered by the sheet's `SOL` rate and drawn like setup guidance
(filled for the Current piece). Frames that are not stable draw nothing.
A state is searched once and reused until the board, HOLD, or queue changes.

Checks (`tests/konbini_data.rs`, with the cache): every one of the 2424
setups drawn in colour is buildable in some order of its own pieces; over
every order, 88.4% of orders build on average and 1489 setups build in every
order. From each PC's start with random 7-bag queues and a cold cache, the
search takes 0.1-8 ms on average and 26 ms at worst (5th PC), measured by
`cargo run --release --example konbini_timing`. Tried in TETR.IO on
2026-10-21 with no problems reported.
The solves after the setup and the sheets' cover data are not used.

## Perfect-clear solve search (SRS+)

The user's decision (2026-10-21) was to use knewjade's solution-finder with
SRS+. It is integrated as a Rust reimplementation of its perfect-clear
search inside `src/finder.rs`, not by calling the Java `sfinder.jar`:

| | Rust in `finder.rs` (chosen) | `sfinder.jar` as a process |
|---|---|---|
| Speed | 3.6 ms mean, 31 ms max per live search (below) | JVM start alone is hundreds of ms per call; a resident process would need a protocol |
| Distribution | nothing extra; the overlay stays one `.exe` | needs a Java runtime and the jar next to the overlay |
| SRS+ | the same TETR.IO table the setup search already uses (triangle.js), 180-degree turns included | sfinder ships no SRS+ table; a kicks file would have to duplicate this table and could drift from it |

The search follows solution-finder's two layers for a perfect clear:

1. Tile the empty cells of the 4-line (or lower) field with the pieces
   available, ignoring order: each step covers the lowest empty cell. A
   piece placed after a row clears is drawn split around that row, so
   each shape `k` rows tall may take any `k` of the drawing rows
   (solution-finder's line-clear-aware placements).
2. Keep a tiling when some order of it can be played: every piece comes
   from the queue with one HOLD slot (HOLD may be played after the queue
   ends), lands with SRS+ movement on the field of the moment (the move
   generator above, cached per field and piece), and is placed only after
   the rows it is drawn split around have cleared.

`Finder::perfect_clears` tries the lowest height from the top block up to
4 rows whose empty cells the pieces can fill; `konbini::find_perfect_clears`
runs it on the live board with HOLD, Current, and NEXT5 (pieces beyond
NEXT are not guessed, so a solve needing them is not found). A first
version that searched piece orders directly (DFS over every placement)
took about 1 s per 7-piece search, as ~100,000 fields needed move
generation; tiling first cut that to milliseconds.

In `run-overlay`, once the shown Setup Konbini setup is complete (or the
board matches no setup, e.g. the player went their own way), the solves
are searched once for that state (at most 100) and

- the status reads `N perfect clear(s) found (4 lines); press S to show the solve`;
- `S` / `Ctrl+Alt+S` show / toggle it as before; the solve's remaining cells
  are drawn like setup cells (Current's piece highlighted);
- `Ctrl+Alt+N` / `Ctrl+Alt+B` switch between the solves while any exist
  (between setups otherwise); the status reads
  `perfect clear i/N (search, SRS+, 4 lines)`;
- each later board is read against the solve as a drawing (cleared rows
  inferred, as for setups), so placed pieces drop out; a board that
  follows no found solve drops them and searches again from that board.

The 2nd PC with source rules keeps the source solves (`[source]`); the
searched solves are used for Setup Konbini guidance.

Checks:

- `tests/pc_solutions.rs`: every one of the 1008 verified source solves
  (`data/ijlo.json` and `data/cases/*.json`) is found by the search from
  its finished setup with its pieces in any order (each piece kept to the
  cells of its type, which prunes only the other solutions). The source
  solves were checked with SRS 90-degree movement; all of them are also
  reachable with SRS+.
- `cargo run --release --example pc_timing`: the same 1008 setups with the
  solve's pieces shuffled into a queue, HOLD empty, cold cache. The 988
  searches of at most 7 pieces (what the overlay sees) take 3.6 ms on
  average and 31 ms at worst; with up to 9 pieces (setups of one piece)
  7.7 ms on average and 475 ms at worst. 950 of the 1008 queues allow a
  perfect clear; the source solve itself is among them in 567 of 921
  comparable cases, since a shuffled order often rules it out.
- `cargo test --bin run-overlay`: a built TILJ setup is announced, `S`
  shows a 24-cell solve, `Ctrl+Alt+N` switches to a different one, and a
  placed solve piece drops out.

Not done: solves for a source setup whose own solves do not apply, the
undisclosed pieces after NEXT, and a check in TETR.IO.

## Settings window (`pc-overlay-gui`)

```powershell
cargo build --release
.\target\release\pc-overlay-gui.exe      # run from the repository folder
```

A small window with four buttons and a status text, refreshed every second:

- **Start overlay / Stop overlay**: runs `run-overlay.exe` from the same
  folder as the window (stop kills the process; the overlay has no other
  exit than Ctrl+C).
- **Calibrate board**: runs `configure-overlay.exe` (drag over the board,
  Enter saves, Esc cancels); restart the overlay afterwards.
- **Download Setup Konbini**: the same download as `fetch-konbini`, in the
  background.
- Status: whether the overlay and the calibration run, the saved
  calibration ratios (or that the default profile is used), the source
  rules found, how many of the 7 Konbini sheets are cached, and the last
  message. Below it, the hotkey list.

Choice: plain Win32 through `windows-sys`, which the overlay already uses
(`BUTTON` and `STATIC` controls, a 1-second `WM_TIMER`), so the GUI adds no
dependency. egui/eframe would add a GPU or GL backend and about 200 crates
for four buttons and a text block; worth it only if the window grows
editable settings or a live board preview.

Tests: `cargo test --bin pc-overlay-gui` (status text, missing data,
hotkey list). The window was opened and captured on 2026-10-21 (layout
checked); the buttons were not pressed, so starting the overlay and the
calibration from it, and the download, are not verified.

Design notes for later: the window only knows whether the overlay process
runs. Showing the overlay's own status (PC number, guidance origin, solve
count) needs a channel from `run-overlay`, e.g. a status line written to
stdout and read through a pipe, or a small JSON file. Editable settings
(paths, `--reviewed-only`, the Konbini preference) would become
command-line arguments of the started overlay.

## TETR.IO display-scale calibration

Run the following while the visible TETR.IO window is showing the normal ZEN
playfield:

```powershell
cargo run --release --bin configure-overlay
```

The command brings up a temporary dimmed selection surface over TETR.IO. Drag
from the board's upper-left corner to its lower-right corner, then press Enter
to save. Esc cancels without changing the existing calibration. The selected
rectangle is client-area relative; the title bar is not part of the captured
image. HOLD/NEXT are not selected separately because they are derived from the
saved board cell size.

For scripted or headless calibration, the mouse mode can be replaced with
explicit values. `--cell-px` is one board cell in the captured client image, so
the complete board width is ten times that value. If measuring the complete
board is easier, use `--board-width-px N` instead. `--top-px` is the board's top
edge, and `--center-x-px N` overrides the default horizontal centre:

```powershell
cargo run --release --bin configure-overlay -- `
  --board-width-px 94 --top-px 406 --center-x-px 474
```

Re-run the calibration whenever the TETR.IO display magnification or client
layout changes. The command writes `data/overlay-calibration.json`.

The overlay and `run-live` load that file automatically. `--calibration PATH`
selects another file. `probe-recognition --bmp PATH` is an offline diagnostic;
it prints the resolved board rectangle and each Current/HOLD/NEXT result
without changing any session state.

## Next implementation slice

1. Done (2026-10-18): the IJLO cell grid matches the source raster for all
   15 images, every stage's gray context equals the previous image, every
   placement is a supported tetromino, and the final images complete four
   lines. Images 9 and 12 place L/J through row y=1, which the image 8/11
   T+J and T+L placements completed and cleared (`cleared_rows: [1]`).
   Placement order inside a stage and spin reachability are not reviewed.
2. Done (2026-10-18) for the S/Z queue setups, with fixtures in
   `tests/ijlo_data.rs` for both stated fail queues, interleaved failures, and the
   S/Z ordering tie-break. The save-O pair and sandbox still need source
   conditions before promotion.
3. Done (2026-10-18) for the S/Z queue solves. `tests/ijlo_replay.rs` replays
   the stable-observation sequences in `tests/fixtures/` (an S-queue run with
   Undo, and a Z-queue run that waits for Z and tucks S in with a spin)
   through the tracker, selection, and solve progress. They are synthetic,
   generated by `python -I tools/ijlo_solves.py fixtures`; frames recorded from
   TETR.IO belong to step 4. `run-overlay` draws verified solves after the
   setup completes and the player presses `S` / `Ctrl+Alt+S` (unit-tested
   only; not yet tried in TETR.IO).
4. Validate the pixel recognizer against recorded stable frames at multiple
   window sizes and promote reviewed IJLO entries. The core crate should
   continue to receive only complete, stable observations.
5. Done (2026-10-18): every case of the source is generated by
   `tools/build_cases.py` (see "Generated cases"). Not yet tried in TETR.IO.
6. Done (2026-10-21): a Rust perfect-clear search after solution-finder
   with SRS+ finds the solves after a Setup Konbini setup, several at once
   (see "Perfect-clear solve search"). Not yet tried in TETR.IO.
7. Done (2026-10-22): resetting the board with `R` in TETR.IO restarts the
   PC number at 1 instead of counting a PC. The tracker reads the restart
   from the screen, not from the reset key (which can be rebound and whose
   short press a 100 ms poll can miss): once filled, HOLD never empties
   within a game, so HOLD going from a piece to empty is a new game; before
   the first hold, a NEXT queue that does not continue the previous one is
   a new game too (`CommitOutcome::NewGame`, unit-tested in
   `src/tracker.rs`). Undo is matched first, so Undo back to the empty-HOLD
   start is still a rollback. Not detected: a restart whose first stable
   frame already has a piece in HOLD; use `Ctrl+Alt+1` then. Not yet tried
   in TETR.IO.

`run-overlay` is the user-facing Windows surface. The S/Z queue and save-O setups are verified;
the sandbox setup is shown as reference guidance, not as a verified
recommendation. `run-live` remains useful for console diagnostics
and calibration.
