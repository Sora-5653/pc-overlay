//! Setup Konbini lookup: every PC's setups from the public "Setup Konbini"
//! spreadsheet, shown as reference overlays.
//!
//! Each `Nth: Data` sheet is fetched as CSV and cached under `data/konbini/`.
//! A sheet holds one or more blocks of setups (`ID`, `SOL`, `FUMEN` columns);
//! the setup cells come from the first page of the fumen. Candidates are the
//! setups that match the board and can still be built from the live HOLD and
//! queue with SRS+ (`crate::finder`), ranked by the sheet's solve rate.
//! Once a setup is built, `find_perfect_clears` searches the solves.

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use thiserror::Error;

use crate::{
    finder::{self, Buildable, Finder, Mask, Step, Supply, Target},
    fumen::{FumenCell, decode_first_page},
    model::{Board, Cell, Observation, Piece, Placement},
};

pub const SPREADSHEET_ID: &str = "1tI9fPeeEEEhWS2h0Gzib5UIjLKN33wd18WQSKRjxYMw";

/// The data sheet of the 1st to 7th PC.
pub const STAGE_SHEETS: [&str; 7] = [
    "1st: Data",
    "2nd: Data",
    "3rd: Data",
    "4th: Data",
    "5th: Data",
    "6th: Data",
    "7th: Data",
];

const PIECES: [Piece; 7] = [
    Piece::I,
    Piece::J,
    Piece::L,
    Piece::O,
    Piece::S,
    Piece::T,
    Piece::Z,
];

#[derive(Debug, Error)]
pub enum KonbiniError {
    #[error("no Setup Konbini data in {0}; fetch it first")]
    NotFetched(PathBuf),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("fetching {sheet}: {message}")]
    Fetch { sheet: String, message: String },
}

/// One setup row of a data sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct KonbiniSetup {
    /// 1 to 7.
    pub stage: u8,
    /// The block's header label (e.g. `KNEWJADE`), or empty.
    pub source: String,
    pub id: String,
    /// The sheet's `SOL` column, in percent.
    pub sol_percent: Option<f32>,
    /// The column after `SOL` (`CASES`, e.g. `4956/5040`): the orders that
    /// leave a perfect clear and the orders counted.
    pub sol_cases: Option<(u32, u32)>,
    pub fumen: String,
    pub cells: Vec<FumenCell>,
    pub target: Target,
}

impl KonbiniSetup {
    /// Pieces the setup needs: coloured cells per piece, rounded up to whole
    /// tetrominoes, and gray cells as any piece.
    fn required_pieces(&self) -> ([usize; 7], usize) {
        let mut coloured = [0usize; 7];
        let mut gray = 0usize;
        for cell in &self.cells {
            match cell.piece {
                Some(piece) => coloured[piece_index(piece)] += 1,
                None => gray += 1,
            }
        }
        (coloured.map(|cells| cells.div_ceil(4)), gray.div_ceil(4))
    }

    /// Whether the PC's pieces can supply this setup. A setup of `k` pieces
    /// is built from the first `k + 1` pieces from the PC start (HOLD first)
    /// with one HOLD slot; undisclosed pieces count as any piece.
    pub fn fits_pieces(&self, pc_pieces: &[Piece]) -> bool {
        let (coloured, gray) = self.required_pieces();
        let total = coloured.iter().sum::<usize>() + gray;
        let window = &pc_pieces[..pc_pieces.len().min(total + 1)];
        let unknown = total + 1 - window.len();
        let mut available = [0usize; 7];
        for piece in window {
            available[piece_index(*piece)] += 1;
        }
        let shortfall: usize = coloured
            .iter()
            .zip(available)
            .map(|(need, have)| need.saturating_sub(have))
            .sum();
        shortfall <= unknown
    }

    /// Reads the board as this setup in progress. Rows the setup fills
    /// clear in the game, so cleared rows are inferred. `None` when the board
    /// has a block the setup does not explain (a cell outside it, or the
    /// wrong colour on a coloured cell); gray cells accept any piece.
    pub fn progress(&self, board: &Board) -> Option<KonbiniProgress> {
        drawing_progress(&self.target, &self.cells, board)
    }
}

/// Reads `board` as the drawing `cells` (with their `target`) in progress,
/// inferring cleared rows; `None` when the board has a block the drawing
/// does not explain.
fn drawing_progress(
    target: &Target,
    cells: &[FumenCell],
    board: &Board,
) -> Option<KonbiniProgress> {
    let field = board_field(board)?;
    let placed = target.read_field(field, |x, y| {
        board[usize::from(y)][usize::from(x)].expect("read_field asks for filled cells")
    })?;
    let remaining = target
        .remaining_on_field(placed)
        .into_iter()
        .map(|(x, y, drawing_y)| FumenCell {
            cell: Cell { x, y },
            piece: cells
                .iter()
                .find(|cell| cell.cell == Cell { x, y: drawing_y })
                .and_then(|cell| cell.piece),
        })
        .collect();
    Some(KonbiniProgress { placed, remaining })
}

/// The board's blocks as a bitboard; `None` when one is above the rows the
/// finder tracks.
fn board_field(board: &Board) -> Option<Mask> {
    let mut field: Mask = 0;
    for (y, row) in board.iter().enumerate() {
        for (x, cell) in row.iter().enumerate() {
            if cell.is_some() {
                if y >= finder::FIELD_ROWS {
                    return None;
                }
                field |= finder::bit(x as u8, y as u8);
            }
        }
    }
    Some(field)
}

/// A perfect clear found by the solve search, kept as a drawing like a
/// setup: the board it was found on plus its steps, without line clears.
/// Later boards are read against it, so placed pieces drop out as rows
/// clear.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundSolve {
    /// The solve's pieces and drawing cells, sorted.
    pub steps: Vec<Step>,
    /// Lines the perfect clear clears.
    pub height: usize,
    cells: Vec<FumenCell>,
    target: Target,
}

impl FoundSolve {
    fn new(board: &Board, height: usize, steps: Vec<Step>) -> Option<Self> {
        let mut cells = Vec::new();
        for (y, row) in board.iter().enumerate().take(finder::FIELD_ROWS) {
            for (x, piece) in row.iter().enumerate() {
                if let Some(piece) = piece {
                    cells.push(FumenCell {
                        cell: Cell {
                            x: x as u8,
                            y: y as u8,
                        },
                        piece: Some(*piece),
                    });
                }
            }
        }
        for (piece, mask) in &steps {
            for index in 0..finder::FIELD_ROWS as u32 * 10 {
                if mask & (1 << index) != 0 {
                    cells.push(FumenCell {
                        cell: Cell {
                            x: (index % 10) as u8,
                            y: (index / 10) as u8,
                        },
                        piece: Some(*piece),
                    });
                }
            }
        }
        let target = Target::new(
            cells
                .iter()
                .map(|cell| (cell.cell.x, cell.cell.y, cell.piece)),
        )?;
        Some(Self {
            steps,
            height,
            cells,
            target,
        })
    }

    /// The board read as this solve in progress.
    pub fn progress(&self, board: &Board) -> Option<KonbiniProgress> {
        drawing_progress(&self.target, &self.cells, board)
    }

    /// Whether the board still follows this solve and its pieces left can be
    /// played with the HOLD, Current, and NEXT shown now.
    pub fn playable(&self, finder: &mut Finder, observation: &Observation) -> bool {
        let Some(progress) = self.progress(&observation.board) else {
            return false;
        };
        let queue = visible_queue(observation);
        let supply = Supply::Queue {
            hold: observation.hold,
            queue: &queue,
        };
        !finder
            .solutions(&self.target, progress.placed, supply, 1)
            .is_empty()
    }
}

/// Whether the board follows the drawing `placements` (a source setup and
/// solve, rows not cleared) and its pieces left can be played with the HOLD,
/// Current, and NEXT shown now (SRS+, one HOLD slot), as
/// `FoundSolve::playable`. Pieces after NEXT, which a drawing may need, are
/// taken from `later` in any order (the rest of the bag), and are any piece
/// once `later` runs out.
pub fn placements_playable(
    finder: &mut Finder,
    placements: &[Placement],
    observation: &Observation,
    later: &[Piece],
) -> bool {
    let cells: Vec<FumenCell> = placements
        .iter()
        .flat_map(|placement| {
            placement.cells.iter().map(|cell| FumenCell {
                cell: *cell,
                piece: Some(placement.piece),
            })
        })
        .collect();
    let Some(target) = Target::new(
        cells
            .iter()
            .map(|cell| (cell.cell.x, cell.cell.y, cell.piece)),
    ) else {
        return false;
    };
    let Some(progress) = drawing_progress(&target, &cells, &observation.board) else {
        return false;
    };
    // Every placement takes a queue piece (Current, or one swapped into
    // HOLD), and a first HOLD into an empty slot takes one more.
    let left = (target.cells & !progress.placed).count_ones() as usize / 4;
    let useful = left + usize::from(observation.hold.is_none());
    let shown = visible_queue(observation);
    let mut queues = vec![(shown, later.to_vec())];
    for _ in 0..useful.saturating_sub(queues[0].0.len()) {
        queues = queues
            .into_iter()
            .flat_map(|(queue, rest)| {
                let options: Vec<(Piece, Vec<Piece>)> = if rest.is_empty() {
                    PIECES.iter().map(|piece| (*piece, Vec::new())).collect()
                } else {
                    (0..rest.len())
                        .filter(|at| !rest[..*at].contains(&rest[*at]))
                        .map(|at| {
                            let mut rest = rest.clone();
                            (rest.remove(at), rest)
                        })
                        .collect()
                };
                options.into_iter().map(move |(piece, rest)| {
                    let mut queue = queue.clone();
                    queue.push(piece);
                    (queue, rest)
                })
            })
            .collect();
    }
    queues.iter().any(|(queue, _)| {
        let supply = Supply::Queue {
            hold: observation.hold,
            queue,
        };
        !finder
            .solutions(&target, progress.placed, supply, 1)
            .is_empty()
    })
}

/// Current followed by NEXT.
fn visible_queue(observation: &Observation) -> Vec<Piece> {
    std::iter::once(observation.current)
        .chain(observation.next5)
        .collect()
}

/// Perfect clears from the live board with its HOLD, Current, and NEXT
/// (SRS+, one HOLD slot), at the lowest height that has one; at most
/// `limit`. Empty when the board is too high or the shown pieces cannot
/// finish it.
///
/// Only the pieces on screen are used, so every solve shown can be played
/// as it stands. The sheet's `SOL` counts every order of the next bag with
/// all 7 pieces, one of which may stay in HOLD; with Current and NEXT alone
/// (6 of them) a 100% setup measured 41-57% (`examples/konbini_sol_rate.rs`).
/// The 7th piece shows in NEXT once a piece is placed, and the search runs
/// again then.
pub fn find_perfect_clears(
    finder: &mut Finder,
    observation: &Observation,
    limit: usize,
) -> Vec<FoundSolve> {
    let Some(field) = board_field(&observation.board) else {
        return Vec::new();
    };
    let queue = visible_queue(observation);
    let supply = Supply::Queue {
        hold: observation.hold,
        queue: &queue,
    };
    let Some((height, solutions)) = finder.perfect_clears(field, supply, limit) else {
        return Vec::new();
    };
    solutions
        .into_iter()
        .filter_map(|steps| FoundSolve::new(&observation.board, height, steps))
        .collect()
}

/// The share of the queues the 7-bag still allows after NEXT for which the
/// setup, built from this state, leaves a perfect clear: the sheet's `SOL`
/// for the state on screen. `SOL` counts the orders of the pieces the sheet
/// assumes (its ID names them on some sheets, such as the 2nd PC's case);
/// built from other pieces, another piece stays in HOLD and the rate can be
/// far lower. `first_index` is the absolute queue index of Current (pieces
/// dealt before it). `None` when the queue does not follow the 7-bag there.
pub fn state_pc_rate(
    finder: &mut Finder,
    setup: &KonbiniSetup,
    progress: &KonbiniProgress,
    observation: &Observation,
    first_index: usize,
) -> Option<f32> {
    let built = setup.target.field(setup.target.cells);
    let filled = built.count_ones() as usize;
    // The loop's PC is 4 lines, less the rows the setup clears.
    let cleared = (setup.target.cells.count_ones() as usize - filled) / 10;
    let pc_pieces = (4usize.checked_sub(cleared)? * 10).checked_sub(filled)? / 4;
    let remaining = setup.target.piece_count() - progress.placed_pieces();
    // One piece may stay in HOLD; without HOLD yet, the queue gives it.
    let length = remaining + pc_pieces + usize::from(observation.hold.is_none());
    let mut completions = Vec::new();
    let consistent = for_each_bag_completion(
        &mut visible_queue(observation),
        first_index,
        length,
        &mut |queue| completions.push(queue.to_vec()),
    );
    if !consistent || completions.is_empty() {
        return None;
    }
    // Evenly spaced orders when there are many (the 1st PC has hundreds).
    let step = completions.len().div_ceil(RATE_SAMPLES);
    let mut outcomes: HashMap<(Option<Piece>, Vec<Piece>), bool> = HashMap::new();
    let (mut total, mut cleared) = (0usize, 0usize);
    for queue in completions.iter().step_by(step) {
        total += 1;
        let ends = finder.build_end_states(&setup.target, progress.placed, observation.hold, queue);
        let found = ends.into_iter().any(|(hold, next)| {
            let rest = queue[next.min(queue.len())..].to_vec();
            *outcomes
                .entry((hold, rest))
                .or_insert_with_key(|(hold, rest)| {
                    let supply = Supply::Queue {
                        hold: *hold,
                        queue: rest,
                    };
                    finder.perfect_clears(built, supply, 1).is_some()
                })
        });
        cleared += usize::from(found);
    }
    Some(100.0 * cleared as f32 / total as f32)
}

/// A first placement for a board the shown pieces cannot perfect clear
/// yet: the piece (Current, or the one HOLD gives) and the board cells it
/// covers, and for how many of the orders the 7-bag allows after NEXT a
/// perfect clear is still there after it.
#[derive(Debug, Clone, PartialEq)]
pub struct StagedMove {
    pub piece: Piece,
    pub cells: Vec<Cell>,
    pub cleared: usize,
    pub total: usize,
}

/// The placements of the next piece, best first, after which a perfect
/// clear stays for the most orders of the pieces not shown yet (as for
/// `state_pc_rate`, at most `RATE_SAMPLES` of them). Played a piece at a
/// time, as in a perfect-clear loop: the next piece shows in NEXT after
/// each placement and the search runs again. Only placements that keep a
/// perfect clear for some order; empty when the board is above the rows the
/// finder tracks, the perfect clear needs more than `STAGED_UNSEEN` pieces
/// past NEXT, or the queue does not follow the 7-bag at `first_index` (the
/// absolute queue index of Current).
pub fn staged_moves(
    finder: &mut Finder,
    observation: &Observation,
    first_index: usize,
) -> Vec<StagedMove> {
    let Some(field) = board_field(&observation.board) else {
        return Vec::new();
    };
    let top = observation
        .board
        .iter()
        .rposition(|row| row.iter().any(Option::is_some))
        .map_or(0, |y| y + 1);
    let filled = field.count_ones() as usize;
    // The lowest height with a whole number of pieces, as for
    // `Finder::perfect_clears`.
    let Some(pieces) = (top.max(1)..=4)
        .map(|height| height * 10 - filled)
        .find(|empty| empty.is_multiple_of(4))
        .map(|empty| empty / 4)
    else {
        return Vec::new();
    };
    // One piece may stay in HOLD; without HOLD yet, the queue gives it.
    let length = pieces + usize::from(observation.hold.is_none());
    let queue = visible_queue(observation);
    // A built setup needs at most the piece past NEXT. Keep the search
    // bounded for boards still far from their perfect clear.
    if length > queue.len() + STAGED_UNSEEN {
        return Vec::new();
    }
    let mut completions = Vec::new();
    let consistent =
        for_each_bag_completion(&mut queue.clone(), first_index, length, &mut |queue| {
            completions.push(queue.to_vec())
        });
    if !consistent || completions.is_empty() {
        return Vec::new();
    }
    let step = completions.len().div_ceil(RATE_SAMPLES);
    let completions: Vec<Vec<Piece>> = completions.into_iter().step_by(step).collect();
    let current = observation.current;
    // The first piece and the supply after it: Current, or the piece HOLD
    // gives (the next one when HOLD is empty).
    let mut firsts = vec![(current, observation.hold, 1)];
    match observation.hold {
        Some(held) if held != current => firsts.push((held, Some(current), 1)),
        Some(_) => {}
        None => firsts.push((observation.next5[0], Some(current), 2)),
    }
    let mut outcomes: HashMap<(Mask, Option<Piece>, usize), usize> = HashMap::new();
    let mut staged = Vec::new();
    for (piece, hold, next) in firsts {
        for &cells in finder.moves(field, piece).iter() {
            let after = clear_full_rows(field | cells);
            let cleared = *outcomes.entry((after, hold, next)).or_insert_with(|| {
                if after == 0 {
                    return completions.len();
                }
                let queues: Vec<&[Piece]> = completions
                    .iter()
                    .map(|queue| &queue[next.min(queue.len())..])
                    .collect();
                finder
                    .perfect_clear_queues(after, hold, &queues)
                    .into_iter()
                    .filter(|found| *found)
                    .count()
            });
            if cleared > 0 {
                staged.push(StagedMove {
                    piece,
                    cells: mask_cells(cells),
                    cleared,
                    total: completions.len(),
                });
            }
        }
    }
    // Stable: ties keep Current first.
    staged.sort_by_key(|staged| std::cmp::Reverse(staged.cleared));
    staged
}

/// Pieces past NEXT `staged_moves` looks at most.
/// With two, `staged_time` measured 13 ms mean and 32 ms worst for two
/// pieces placed, down from 115 ms mean and 242 ms worst.
pub const STAGED_UNSEEN: usize = 2;

/// `field` with its full rows removed and the rows above dropped.
fn clear_full_rows(field: Mask) -> Mask {
    let full: Mask = (1 << 10) - 1;
    let mut kept = 0;
    let mut row = 0;
    for y in 0..finder::FIELD_ROWS {
        let line = (field >> (y * 10)) & full;
        if line != full {
            kept |= line << (row * 10);
            row += 1;
        }
    }
    kept
}

/// The board cells of a live mask.
fn mask_cells(mask: Mask) -> Vec<Cell> {
    (0..finder::FIELD_ROWS as u32 * 10)
        .filter(|index| mask & (1 << index) != 0)
        .map(|index| Cell {
            x: (index % 10) as u8,
            y: (index / 10) as u8,
        })
        .collect()
}

/// Orders of the undisclosed pieces `state_pc_rate` checks at most.
pub const RATE_SAMPLES: usize = 120;

/// Calls `visit` with `queue` extended to `length` pieces in every order the
/// 7-bag allows, `queue[0]` being the piece at absolute index `first_index`.
/// False when the known pieces repeat within a bag (the position is off).
fn for_each_bag_completion(
    queue: &mut Vec<Piece>,
    first_index: usize,
    length: usize,
    visit: &mut dyn FnMut(&[Piece]),
) -> bool {
    if queue.len() >= length {
        visit(&queue[..length]);
        return true;
    }
    let index = first_index + queue.len();
    // The bag of a piece after NEXT starts within the queue: NEXT and
    // Current cover the six pieces before it.
    let bag_start = (index - index % 7).max(first_index) - first_index;
    let seen = &queue[bag_start..];
    if (0..seen.len()).any(|at| seen[..at].contains(&seen[at])) {
        return false;
    }
    let open: Vec<Piece> = finder::ALL_PIECES
        .iter()
        .copied()
        .filter(|piece| !seen.contains(piece))
        .collect();
    for piece in open {
        queue.push(piece);
        let consistent = for_each_bag_completion(queue, first_index, length, visit);
        queue.pop();
        if !consistent {
            return false;
        }
    }
    true
}

/// 2-line perfect clears (5 pieces) from the live board with its HOLD,
/// Current, and NEXT: empty when the board is above the bottom two rows or
/// the shown pieces cannot fill them. Only `IJLOO`, `IILLO`, `IIJJO`, and
/// `JJLLO` tile two rows, so no piece filter is needed. A 2-line PC moves the
/// PC number by four (`PcOrdinal::after_pieces`).
pub fn find_two_line_perfect_clears(
    finder: &mut Finder,
    observation: &Observation,
    limit: usize,
) -> Vec<FoundSolve> {
    let Some(field) = board_field(&observation.board) else {
        return Vec::new();
    };
    let target = Target::perfect_clear(2).expect("2 rows fit the field");
    if field & !target.cells != 0 {
        return Vec::new();
    }
    let queue = visible_queue(observation);
    let supply = Supply::Queue {
        hold: observation.hold,
        queue: &queue,
    };
    finder
        .solutions(&target, field, supply, limit)
        .into_iter()
        .filter_map(|steps| FoundSolve::new(&observation.board, 2, steps))
        .collect()
}

/// The drawing cells already placed, and the rest as cells of the live
/// board (after line clears).
#[derive(Debug, Clone, PartialEq)]
pub struct KonbiniProgress {
    pub placed: Mask,
    pub remaining: Vec<FumenCell>,
}

impl KonbiniProgress {
    pub fn complete(&self) -> bool {
        self.remaining.is_empty()
    }

    /// Pieces placed for the setup, counting cleared rows.
    pub fn placed_pieces(&self) -> usize {
        self.placed.count_ones() as usize / 4
    }
}

/// The live state a candidate is searched from.
#[derive(Debug, Clone)]
pub struct BuildQuery<'a> {
    /// HOLD at the PC start followed by the queue disclosed since.
    pub pc_pieces: &'a [Piece],
    pub observation: &'a Observation,
    /// Pieces dealt before this PC began (10 per PC in a perfect-clear
    /// loop), for the 7-bag of undisclosed pieces. `None` assumes nothing.
    pub pieces_before_pc: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct KonbiniCandidate<'a> {
    /// Index into `KonbiniLibrary::setups`.
    pub setup_index: usize,
    pub setup: &'a KonbiniSetup,
    pub progress: KonbiniProgress,
    /// `Always` or `Sometimes` (depends on pieces not yet in NEXT).
    pub buildable: Buildable,
}

#[derive(Debug, Clone, Default)]
pub struct KonbiniLibrary {
    pub setups: Vec<KonbiniSetup>,
    /// Rows that could not be read, as `stage row: reason`.
    pub issues: Vec<String>,
}

impl KonbiniLibrary {
    /// Loads every cached stage CSV in `dir`. Missing stages are skipped;
    /// a directory with none of them is `NotFetched`.
    pub fn load_dir(dir: &Path) -> Result<Self, KonbiniError> {
        let mut library = Self::default();
        let mut found = false;
        for stage in 1..=STAGE_SHEETS.len() as u8 {
            let path = dir.join(cache_file_name(stage));
            if !path.exists() {
                continue;
            }
            let text = fs::read_to_string(&path).map_err(|source| KonbiniError::Io {
                path: path.clone(),
                source,
            })?;
            library.add_stage_csv(stage, &text);
            found = true;
        }
        if found {
            Ok(library)
        } else {
            Err(KonbiniError::NotFetched(dir.to_path_buf()))
        }
    }

    /// Adds one stage's sheet. Within the stage, setups are ordered by solve
    /// rate (unknown last, sheet order otherwise), and a fumen drawing the
    /// same cells as an earlier one is dropped.
    pub fn add_stage_csv(&mut self, stage: u8, text: &str) {
        let rows = parse_csv(text);
        let mut setups = Vec::new();
        for block in find_blocks(&rows) {
            for (row_index, row) in rows.iter().enumerate().skip(block.header_row + 1) {
                let fumen = cell(row, block.fumen);
                if !fumen.starts_with("v115@") {
                    continue;
                }
                let issue =
                    |message: String| format!("stage {stage} row {}: {message}", row_index + 1);
                match decode_first_page(fumen) {
                    Ok(cells) if !cells.is_empty() => {
                        let Some(target) = Target::new(
                            cells
                                .iter()
                                .map(|cell| (cell.cell.x, cell.cell.y, cell.piece)),
                        ) else {
                            self.issues
                                .push(issue(format!("taller than {} rows", finder::FIELD_ROWS)));
                            continue;
                        };
                        setups.push(KonbiniSetup {
                            stage,
                            source: block.source.clone(),
                            id: cell(row, block.id).trim().to_string(),
                            sol_percent: block
                                .sol
                                .and_then(|column| parse_percent(cell(row, column))),
                            sol_cases: block
                                .sol
                                .and_then(|column| parse_cases(cell(row, column + 1))),
                            fumen: fumen.to_string(),
                            cells,
                            target,
                        });
                    }
                    Ok(_) => {}
                    Err(error) => self.issues.push(issue(error.to_string())),
                }
            }
        }
        setups.sort_by(|a, b| {
            b.sol_percent
                .unwrap_or(-1.0)
                .total_cmp(&a.sol_percent.unwrap_or(-1.0))
        });
        let mut seen = BTreeSet::new();
        setups.retain(|setup| seen.insert(setup.cells.clone()));
        self.setups.retain(|setup| setup.stage != stage);
        self.setups.extend(setups);
    }

    pub fn stage_count(&self, stage: u8) -> usize {
        self.setups
            .iter()
            .filter(|setup| setup.stage == stage)
            .count()
    }

    /// Setups of `stage` that the board matches and that can still be built
    /// from the live HOLD, Current, and NEXT with SRS+, those buildable for
    /// any undisclosed pieces first, then by solve rate.
    pub fn candidates(
        &self,
        stage: u8,
        query: &BuildQuery<'_>,
        finder: &mut Finder,
    ) -> Vec<KonbiniCandidate<'_>> {
        let observation = query.observation;
        let queue: Vec<Piece> = std::iter::once(observation.current)
            .chain(observation.next5)
            .collect();
        let mut candidates: Vec<KonbiniCandidate<'_>> = self
            .setups
            .iter()
            .enumerate()
            .filter(|(_, setup)| setup.stage == stage && setup.fits_pieces(query.pc_pieces))
            .filter_map(|(setup_index, setup)| {
                let progress = setup.progress(&observation.board)?;
                let remaining = setup.target.piece_count() - progress.placed_pieces();
                let slots = (remaining + 1).saturating_sub(queue.len());
                let unknown = match query.pieces_before_pc {
                    Some(before) => finder::bag_options(
                        before + progress.placed_pieces() + usize::from(observation.hold.is_some()),
                        &queue,
                        slots,
                    ),
                    None => vec![finder::ALL_PIECES.to_vec(); slots],
                };
                let buildable = finder.buildable(
                    &setup.target,
                    progress.placed,
                    observation.hold,
                    &queue,
                    &unknown,
                );
                (buildable != Buildable::Never).then_some(KonbiniCandidate {
                    setup_index,
                    setup,
                    progress,
                    buildable,
                })
            })
            .collect();
        // Stable: setups are already ordered by solve rate.
        candidates.sort_by_key(|candidate| candidate.buildable != Buildable::Always);
        candidates
    }
}

pub fn stage_label(stage: u8) -> &'static str {
    STAGE_SHEETS
        .get(usize::from(stage).wrapping_sub(1))
        .and_then(|sheet| sheet.split(':').next())
        .unwrap_or("?")
}

pub fn cache_file_name(stage: u8) -> String {
    format!("{}.csv", stage_label(stage))
}

pub fn sheet_csv_url(sheet: &str) -> String {
    let encoded: String = sheet
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect();
    format!(
        "https://docs.google.com/spreadsheets/d/{SPREADSHEET_ID}/gviz/tq?tqx=out:csv&sheet={encoded}"
    )
}

/// The largest sheet download accepted. The biggest sheet (5th PC) is about
/// 1.3 MB, so this only stops a runaway response.
const MAX_SHEET_BYTES: u64 = 32 * 1024 * 1024;

/// The system `curl`. On Windows it is named by its full path under
/// `System32`, so a `curl.exe` placed next to this program is not run instead.
fn curl_program() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        Path::new(&root).join("System32").join("curl.exe")
    } else {
        PathBuf::from("curl")
    }
}

/// Downloads every stage sheet into `dir` with the system `curl`, replacing
/// a cached file only once its download looks like a CSV with fumens.
pub fn fetch_all(dir: &Path) -> Result<(), KonbiniError> {
    fs::create_dir_all(dir).map_err(|source| KonbiniError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for (index, sheet) in STAGE_SHEETS.iter().enumerate() {
        let stage = index as u8 + 1;
        let target = dir.join(cache_file_name(stage));
        let partial = target.with_extension("csv.part");
        let fetch_error = |message: String| KonbiniError::Fetch {
            sheet: sheet.to_string(),
            message,
        };
        // HTTPS only, redirects included, and a size limit.
        let output = Command::new(curl_program())
            .args(["-sSfL", "--max-time", "60"])
            .args(["--proto", "=https", "--proto-redir", "=https"])
            .arg("--max-filesize")
            .arg(MAX_SHEET_BYTES.to_string())
            .arg("-o")
            .arg(&partial)
            .arg(sheet_csv_url(sheet))
            .output()
            .map_err(|error| fetch_error(format!("could not run curl: {error}")))?;
        if !output.status.success() {
            let _ = fs::remove_file(&partial);
            return Err(fetch_error(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let text = fs::read_to_string(&partial).map_err(|source| KonbiniError::Io {
            path: partial.clone(),
            source,
        })?;
        if text.trim_start().starts_with('<') || !text.contains("v115@") {
            let _ = fs::remove_file(&partial);
            return Err(fetch_error(
                "the response is not the sheet's CSV (is the spreadsheet still public?)"
                    .to_string(),
            ));
        }
        fs::rename(&partial, &target).map_err(|source| KonbiniError::Io {
            path: target.clone(),
            source,
        })?;
    }
    Ok(())
}

/// A setup block of a sheet: the columns of one `FUMEN` header.
#[derive(Debug, PartialEq)]
struct Block {
    header_row: usize,
    source: String,
    id: usize,
    sol: Option<usize>,
    fumen: usize,
}

/// Finds the header row (the first with a `FUMEN` cell) and, for each
/// `FUMEN` column, the `ID` and `SOL` columns to its left that lie after the
/// previous block. A block without `SOL` (the 5th PC sheet) uses the
/// unlabelled column after `FUMEN` + glue fumen when it holds percentages.
fn find_blocks(rows: &[Vec<String>]) -> Vec<Block> {
    let Some(header_row) = rows
        .iter()
        .position(|row| row.iter().any(|value| value.trim() == "FUMEN"))
    else {
        return Vec::new();
    };
    let header = &rows[header_row];
    let label = |column: usize| header.get(column).map_or("", |value| value.trim());
    let mut blocks = Vec::new();
    let mut start = 0;
    for fumen in (0..header.len()).filter(|column| label(*column) == "FUMEN") {
        let find = |names: &[&str]| {
            (start..fumen)
                .rev()
                .find(|column| names.contains(&label(*column)))
        };
        let Some(id) = find(&["ID"]).or_else(|| find(&["ID2"])) else {
            start = fumen + 1;
            continue;
        };
        let sol = find(&["SOL"]).or_else(|| {
            let column = fumen + 2;
            (label(column).is_empty()
                && rows
                    .iter()
                    .skip(header_row + 1)
                    .any(|row| parse_percent(cell(row, column)).is_some()))
            .then_some(column)
        });
        // The block title: in a row above the header when there is one (the
        // 5th PC sheet), otherwise the header cell left of `ID`.
        let source = rows
            .iter()
            .take(header_row)
            .flat_map(|row| (start..=id).map(|column| cell(row, column).trim()))
            .find(|value| !value.is_empty())
            .or_else(|| (start..id).rev().map(label).find(|value| !value.is_empty()))
            .unwrap_or("")
            .to_string();
        blocks.push(Block {
            header_row,
            source,
            id,
            sol,
            fumen,
        });
        start = fumen + 1;
    }
    blocks
}

fn cell(row: &[String], column: usize) -> &str {
    row.get(column).map_or("", String::as_str)
}

fn parse_percent(value: &str) -> Option<f32> {
    value.trim().strip_suffix('%')?.trim().parse().ok()
}

/// `4956/5040` as (4956, 5040).
fn parse_cases(value: &str) -> Option<(u32, u32)> {
    let (cleared, total) = value.trim().split_once('/')?;
    let total: u32 = total.trim().parse().ok()?;
    let cleared: u32 = cleared.trim().parse().ok()?;
    (total > 0 && cleared <= total).then_some((cleared, total))
}

fn piece_index(piece: Piece) -> usize {
    PIECES
        .iter()
        .position(|known| *known == piece)
        .expect("every piece is listed")
}

/// RFC 4180 CSV: quoted fields may hold commas, doubled quotes, and newlines.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(ch),
            }
            continue;
        }
        match ch {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(ch),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::empty_board;

    /// Excerpt of the 2nd PC sheet: two setups and one with a lower SOL.
    const SECOND: &str = "\"KNEWJADE\",\"ID2\",\"SOL\",\"CASES\",\"FUMEN\",\"GLUEMEN\"\n\
        \"\",\"TILJ\",\"98.33%\",\"4956/5040\",\"v115@Ghg0DeglAewwBeg0Deglywh0DehlzhJeAgH\",\"v115@vhD+IJKrBVsBxxB\"\n\
        \"\",\"TILJ\",\"100.00%\",\"5040/5040\",\"v115@GhwhDeglCeg0whDeglywg0whDehlwwh0whJeAgH\",\"v115@vhD5IJKrBetBFsB\"\n";

    /// Excerpt of the 6th PC sheet: two blocks side by side.
    const SIXTH: &str = "\"STICKMANCOMIC\",\"ID\",\"ID2\",\"SOL\",\"FUMEN\",\"13PAKE\",\"ID\",\"SOL\",\"FUMEN\"\n\
        \"\",\"T-LO\",\"IJSZ\",\"93.17%\",\"v115@9gzhFeh0AtGeg0BtR4Eeg0AtR4FeJeAgH\",\"\",\"T-IL-TSZO\",\"99.96%\",\"v115@FhR4GeR4wwRpDeBtxwRpEeBtwwJeAgH\"\n";

    /// Excerpt of the 5th PC sheet: statistics above the header, no SOL label.
    const FIFTH: &str = "\"\",\"BRAINDEAD\",\"\"\n\
        \"\",\"TI\",\"10\"\n\
        \"\",\"BASE\",\"BASE#\",\"\",\"ID\",\"IMGUR\",\"IMAGE\",\"FUMEN\",\"GLUE\",\"\",\"COVER\"\n\
        \"\",\"TT\",\"11\",\"4\",\"TT-LO\",\"rmulkkY\",\"\",\"v115@FhRpGewwRpEezwglDeywilJeAgH\",\"v115@vhDTDJ1sB1wBSyB\",\"100.00%\",\"x\"\n";

    fn library(stage: u8, text: &str) -> KonbiniLibrary {
        let mut library = KonbiniLibrary::default();
        library.add_stage_csv(stage, text);
        assert!(library.issues.is_empty(), "{:?}", library.issues);
        library
    }

    #[test]
    fn only_four_piece_sets_tile_two_lines() {
        let mut finder = Finder::new();
        let target = Target::perfect_clear(2).unwrap();
        let mut sets = Vec::new();
        // Every 5 pieces with at most two of each, as a 7-bag can deal.
        for code in 0..3u32.pow(7) {
            let counts: [u8; 7] = std::array::from_fn(|i| (code / 3u32.pow(i as u32) % 3) as u8);
            if counts.iter().sum::<u8>() != 5 {
                continue;
            }
            if !finder
                .solutions(&target, 0, Supply::AnyOrder(counts), 1)
                .is_empty()
            {
                let name: String = finder::ALL_PIECES
                    .iter()
                    .zip(counts)
                    .flat_map(|(piece, count)| {
                        std::iter::repeat_n(format!("{piece:?}"), usize::from(count))
                    })
                    .collect();
                sets.push(name);
            }
        }
        sets.sort();
        assert_eq!(sets, ["IIJJO", "IILLO", "IJLOO", "JJLLO"]);
    }

    fn on_empty(hold: Option<Piece>, current: Piece, next5: [Piece; 5]) -> Observation {
        Observation {
            board: empty_board(),
            hold,
            current,
            next5,
        }
    }

    #[test]
    fn two_line_perfect_clears_use_the_shown_pieces() {
        use Piece::*;
        let mut finder = Finder::new();
        let ooilj = on_empty(Some(O), I, [L, T, J, O, S]);
        let found = find_two_line_perfect_clears(&mut finder, &ooilj, 100);
        assert!(!found.is_empty());
        for solve in &found {
            assert_eq!(solve.height, 2);
            let mut pieces: Vec<Piece> = solve.steps.iter().map(|(piece, _)| *piece).collect();
            pieces.sort_by_key(|piece| format!("{piece:?}"));
            assert_eq!(pieces, [I, J, L, O, O]);
        }
        // The second O comes after the five shown: not with HOLD + NEXT.
        let late = on_empty(None, O, [I, L, T, J, S]);
        assert!(find_two_line_perfect_clears(&mut finder, &late, 100).is_empty());
        // A block above the bottom two rows.
        let mut high = ooilj.clone();
        high.board[2][0] = Some(T);
        assert!(find_two_line_perfect_clears(&mut finder, &high, 100).is_empty());
        // A started 2-line PC is still found, and a contradicting board is not.
        let mut started = ooilj.clone();
        started.board[0][0] = Some(O);
        started.board[0][1] = Some(O);
        started.board[1][0] = Some(O);
        started.board[1][1] = Some(O);
        assert!(!find_two_line_perfect_clears(&mut finder, &started, 100).is_empty());
        let mut off = ooilj;
        off.board[0][0] = Some(T);
        assert!(find_two_line_perfect_clears(&mut finder, &off, 100).is_empty());
    }

    #[test]
    fn csv_handles_quotes_commas_and_newlines() {
        let rows = parse_csv("a,\"b,c\",\"d\"\"e\"\r\n\"f\ng\",h\n");
        assert_eq!(
            rows,
            vec![
                vec!["a".to_string(), "b,c".to_string(), "d\"e".to_string()],
                vec!["f\ng".to_string(), "h".to_string()],
            ]
        );
    }

    #[test]
    fn setups_are_ranked_by_solve_rate() {
        let library = library(2, SECOND);
        assert_eq!(library.stage_count(2), 2);
        assert_eq!(library.setups[0].sol_percent, Some(100.0));
        assert_eq!(library.setups[0].sol_cases, Some((5040, 5040)));
        assert_eq!(library.setups[1].sol_cases, Some((4956, 5040)));
        assert_eq!(library.setups[0].source, "KNEWJADE");
        assert_eq!(library.setups[0].id, "TILJ");
        assert_eq!(library.setups[0].cells.len(), 16);
    }

    #[test]
    fn side_by_side_blocks_are_both_read() {
        let library = library(6, SIXTH);
        let ids: Vec<_> = library.setups.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["T-IL-TSZO", "T-LO"]);
        assert_eq!(library.setups[0].source, "13PAKE");
        assert_eq!(library.setups[1].source, "STICKMANCOMIC");
    }

    #[test]
    fn a_block_without_sol_label_uses_the_percent_column() {
        let library = library(5, FIFTH);
        assert_eq!(library.setups.len(), 1);
        assert_eq!(library.setups[0].id, "TT-LO");
        assert_eq!(library.setups[0].sol_percent, Some(100.0));
        // No count beside the percentage on this sheet.
        assert_eq!(library.setups[0].sol_cases, None);
        assert_eq!(library.setups[0].source, "BRAINDEAD");
    }

    #[test]
    fn candidates_need_the_pieces_within_reach() {
        let library = library(2, SECOND);
        let best = &library.setups[0];
        let (coloured, gray) = best.required_pieces();
        assert_eq!(gray, 0);
        let total: usize = coloured.iter().sum();
        assert_eq!(total, 4);
        let needed: Vec<Piece> = PIECES
            .iter()
            .flat_map(|piece| std::iter::repeat_n(*piece, coloured[piece_index(*piece)]))
            .collect();
        // Exactly the pieces plus one spare: fits.
        let mut queue = needed.clone();
        queue.push(Piece::O);
        assert!(best.fits_pieces(&queue));
        // A needed piece beyond the first k + 1 positions does not.
        let mut late = needed[1..].to_vec();
        late.extend([Piece::O, Piece::O, needed[0]]);
        assert!(!best.fits_pieces(&late));
        // Undisclosed pieces may still bring it.
        assert!(best.fits_pieces(&needed[1..]));
    }

    fn observation(hold: Option<Piece>, queue: [Piece; 6]) -> Observation {
        Observation {
            board: empty_board(),
            hold,
            current: queue[0],
            next5: [queue[1], queue[2], queue[3], queue[4], queue[5]],
        }
    }

    fn shown(library: &KonbiniLibrary, observation: &Observation) -> Vec<(f32, Buildable)> {
        let pc_pieces: Vec<Piece> = observation
            .hold
            .into_iter()
            .chain(std::iter::once(observation.current))
            .chain(observation.next5)
            .collect();
        let query = BuildQuery {
            pc_pieces: &pc_pieces,
            observation,
            pieces_before_pc: Some(10),
        };
        library
            .candidates(2, &query, &mut Finder::new())
            .iter()
            .map(|candidate| (candidate.setup.sol_percent.unwrap(), candidate.buildable))
            .collect()
    }

    #[test]
    fn candidates_are_the_setups_the_queue_can_build() {
        let library = library(2, SECOND);
        // Both TILJ setups build from T, I, L, J in this order.
        let tilj = [Piece::T, Piece::I, Piece::L, Piece::J, Piece::O, Piece::S];
        assert_eq!(
            shown(&library, &observation(None, tilj)),
            vec![(100.0, Buildable::Always), (98.33, Buildable::Always)]
        );
        // Some order is not buildable for one of them: every order of the
        // four pieces, with HOLD empty, finds a difference.
        let mut differs = false;
        for order in permutations(&[Piece::T, Piece::I, Piece::L, Piece::J]) {
            let queue = [order[0], order[1], order[2], order[3], Piece::O, Piece::S];
            differs |= shown(&library, &observation(None, queue)).len() < 2;
        }
        assert!(differs, "the build order should matter for some queue");
        // A queue without the pieces shows nothing.
        let other = [Piece::O, Piece::S, Piece::Z, Piece::O, Piece::S, Piece::Z];
        assert!(shown(&library, &observation(None, other)).is_empty());
    }

    fn permutations(pieces: &[Piece]) -> Vec<Vec<Piece>> {
        if pieces.len() <= 1 {
            return vec![pieces.to_vec()];
        }
        let mut all = Vec::new();
        for index in 0..pieces.len() {
            let mut rest = pieces.to_vec();
            let first = rest.remove(index);
            for mut tail in permutations(&rest) {
                tail.insert(0, first);
                all.push(tail);
            }
        }
        all
    }

    #[test]
    fn the_board_must_match_the_setup() {
        let library = library(2, SECOND);
        let setup = &library.setups[0];
        // Place every cell of one piece colour that has exactly 4 cells.
        let piece = PIECES
            .into_iter()
            .find(|piece| {
                setup
                    .cells
                    .iter()
                    .filter(|cell| cell.piece == Some(*piece))
                    .count()
                    == 4
            })
            .unwrap();
        let mut board = empty_board();
        for cell in setup.cells.iter().filter(|cell| cell.piece == Some(piece)) {
            board[usize::from(cell.cell.y)][usize::from(cell.cell.x)] = Some(piece);
        }
        let progress = setup.progress(&board).unwrap();
        assert_eq!(progress.placed_pieces(), 1);
        assert_eq!(progress.remaining.len(), setup.cells.len() - 4);

        // A wrong colour on a setup cell rejects it.
        let first = setup
            .cells
            .iter()
            .find(|cell| cell.piece == Some(piece))
            .unwrap();
        let wrong = PIECES.into_iter().find(|other| *other != piece).unwrap();
        board[usize::from(first.cell.y)][usize::from(first.cell.x)] = Some(wrong);
        assert!(setup.progress(&board).is_none());

        // So does a block outside it.
        let mut board = empty_board();
        board[19][9] = Some(Piece::I);
        assert!(setup.progress(&board).is_none());
    }

    #[test]
    fn sheet_urls_are_percent_encoded() {
        assert!(sheet_csv_url("2nd: Data").ends_with("&sheet=2nd%3A%20Data"));
        assert_eq!(cache_file_name(3), "3rd.csv");
        assert_eq!(stage_label(7), "7th");
        assert_eq!(stage_label(8), "?");
    }

    #[test]
    fn a_staged_move_keeps_the_perfect_clear_for_every_order() {
        use Piece::*;
        // LZ-ISO built on a 4th PC (Current at index 34): with J the last
        // piece shown, HOLD and NEXT have no perfect clear, as it needs the
        // piece after J. Some first placement keeps one for every order.
        let mut board = empty_board();
        for (x, y, piece) in [
            (0, 0, S),
            (1, 0, S),
            (1, 1, S),
            (2, 1, S),
            (7, 0, O),
            (8, 0, O),
            (7, 1, O),
            (8, 1, O),
            (9, 0, I),
            (9, 1, I),
            (9, 2, I),
            (9, 3, I),
        ] {
            board[y][x] = Some(piece);
        }
        let built = Observation {
            board,
            hold: Some(T),
            current: Z,
            next5: [I, T, S, Z, J],
        };
        let mut finder = Finder::new();
        assert!(find_perfect_clears(&mut finder, &built, 1).is_empty());
        let staged = staged_moves(&mut finder, &built, 34);
        let best = &staged[0];
        assert_eq!((best.cleared, best.total), (2, 2), "{staged:?}");
        assert!([Z, T].contains(&best.piece) && best.cells.len() == 4);
        // Rates fall off from the best, and the queue must follow the bag.
        assert!(
            staged
                .windows(2)
                .all(|pair| pair[0].cleared >= pair[1].cleared)
        );
        assert!(staged_moves(&mut finder, &built, 35).is_empty());
    }
}
