//! Setup build search with TETR.IO's SRS+ (90 and 180-degree kicks) and one
//! HOLD slot, after knewjade's solution-finder.
//!
//! As in solution-finder, the field is a bitboard and the search has two
//! layers: a move generator that finds every lockable placement of a piece
//! on a field (a BFS over position and rotation, cached per field and
//! piece), and a depth-first search over the order of pieces with HOLD that
//! memoises failed states. A setup is the target: its cells in the source
//! drawing, which does not draw line clears. Rows of the drawing that fill up
//! clear in the game, so the live field is the drawing with those rows
//! removed, and placements are mapped between the two.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::OnceLock,
};

use crate::model::Piece;

/// Rows tracked by the bitboard. Setups are at most 4 rows high; the rows
/// above stay empty, so a piece can move freely there.
pub const FIELD_ROWS: usize = 6;
const WIDTH: i32 = 10;
/// Pieces start above the field and may move up to this row (exclusive).
const TOP: i32 = FIELD_ROWS as i32 + 4;

/// Every piece, in the order used for per-piece tables.
pub const ALL_PIECES: [Piece; 7] = [
    Piece::I,
    Piece::J,
    Piece::L,
    Piece::O,
    Piece::S,
    Piece::T,
    Piece::Z,
];

/// A bit per cell, `y * 10 + x`, for the bottom `FIELD_ROWS` rows.
pub type Mask = u64;

pub const fn bit(x: u8, y: u8) -> Mask {
    1 << (y as u32 * 10 + x as u32)
}

const ROW: Mask = (1 << 10) - 1;

fn row_mask(y: usize) -> Mask {
    ROW << (y * 10)
}

/// Shapes as rows from the top of the rotation box (guideline SRS boxes:
/// 3x3 for JLSTZ, 4x4 for I). States are spawn, R, 2, L.
fn shape(piece: Piece) -> [&'static str; 4] {
    match piece {
        Piece::T => ["010111000", "010011010", "000111010", "010110010"],
        Piece::J => ["100111000", "011010010", "000111001", "010010110"],
        Piece::L => ["001111000", "010010011", "000111100", "110010010"],
        Piece::S => ["011110000", "010011001", "000011110", "100110010"],
        Piece::Z => ["110011000", "001011010", "000110011", "010110100"],
        Piece::I => [
            "0000111100000000",
            "0010001000100010",
            "0000000011110000",
            "0100010001000100",
        ],
        // O does not change under rotation; one state is enough.
        Piece::O => ["1111", "1111", "1111", "1111"],
    }
}

/// Kicks as TETR.IO lists them for SRS+: `(dx, dy)` with y pointing down,
/// tried after the unkicked rotation. Keys are `from * 10 + to`.
fn kicks(piece: Piece, from: usize, to: usize) -> &'static [(i32, i32)] {
    const JLSTZ: [(usize, &[(i32, i32)]); 12] = [
        (1, &[(-1, 0), (-1, -1), (0, 2), (-1, 2)]),
        (10, &[(1, 0), (1, 1), (0, -2), (1, -2)]),
        (12, &[(1, 0), (1, 1), (0, -2), (1, -2)]),
        (21, &[(-1, 0), (-1, -1), (0, 2), (-1, 2)]),
        (23, &[(1, 0), (1, -1), (0, 2), (1, 2)]),
        (32, &[(-1, 0), (-1, 1), (0, -2), (-1, -2)]),
        (30, &[(-1, 0), (-1, 1), (0, -2), (-1, -2)]),
        (3, &[(1, 0), (1, -1), (0, 2), (1, 2)]),
        (2, &[(0, -1), (1, -1), (-1, -1), (1, 0), (-1, 0)]),
        (13, &[(1, 0), (1, -2), (1, -1), (0, -2), (0, -1)]),
        (20, &[(0, 1), (-1, 1), (1, 1), (-1, 0), (1, 0)]),
        (31, &[(-1, 0), (-1, -2), (-1, -1), (0, -2), (0, -1)]),
    ];
    const I: [(usize, &[(i32, i32)]); 12] = [
        (1, &[(1, 0), (-2, 0), (-2, 1), (1, -2)]),
        (10, &[(-1, 0), (2, 0), (-1, 2), (2, -1)]),
        (12, &[(-1, 0), (2, 0), (-1, -2), (2, 1)]),
        (21, &[(-2, 0), (1, 0), (-2, -1), (1, 2)]),
        (23, &[(2, 0), (-1, 0), (2, -1), (-1, 2)]),
        (32, &[(1, 0), (-2, 0), (1, -2), (-2, 1)]),
        (30, &[(1, 0), (-2, 0), (1, 2), (-2, -1)]),
        (3, &[(-1, 0), (2, 0), (2, 1), (-1, -2)]),
        (2, &[(0, -1)]),
        (13, &[(1, 0)]),
        (20, &[(0, 1)]),
        (31, &[(-1, 0)]),
    ];
    let table = if piece == Piece::I { &I } else { &JLSTZ };
    table
        .iter()
        .find(|(key, _)| *key == from * 10 + to)
        .map_or(&[], |(_, kicks)| kicks)
}

/// A piece's cells per rotation, as `(x, y)` offsets from the bottom-left of
/// its box, y up.
struct Shapes {
    cells: [[[(i32, i32); 4]; 4]; 7],
    rotations: [usize; 7],
}

fn shapes() -> &'static Shapes {
    static SHAPES: OnceLock<Shapes> = OnceLock::new();
    SHAPES.get_or_init(|| {
        let mut cells = [[[(0, 0); 4]; 4]; 7];
        let mut rotations = [4; 7];
        for piece in ALL_PIECES {
            let index = piece_index(piece);
            for (state, rows) in shape(piece).iter().enumerate() {
                let size = (rows.len() as f64).sqrt() as i32;
                let mut found = 0;
                for (offset, ch) in rows.chars().enumerate() {
                    if ch == '1' {
                        let column = offset as i32 % size;
                        let row = offset as i32 / size;
                        cells[index][state][found] = (column, size - 1 - row);
                        found += 1;
                    }
                }
                assert_eq!(found, 4, "{piece:?} state {state}");
            }
            if piece == Piece::O {
                rotations[index] = 1;
            }
        }
        Shapes { cells, rotations }
    })
}

fn piece_index(piece: Piece) -> usize {
    ALL_PIECES
        .iter()
        .position(|known| *known == piece)
        .expect("every piece is listed")
}

/// Cells of a placement inside the bitboard, or `None` when it hits the
/// walls, the floor, the field, or leaves the search area.
fn placement(field: Mask, cells: &[(i32, i32); 4], x: i32, y: i32) -> Option<Mask> {
    let mut mask = 0;
    for (dx, dy) in cells {
        let (cx, cy) = (x + dx, y + dy);
        if !(0..WIDTH).contains(&cx) || !(0..TOP).contains(&cy) {
            return None;
        }
        if cy < FIELD_ROWS as i32 {
            mask |= bit(cx as u8, cy as u8);
        }
    }
    (mask & field == 0).then_some(mask)
}

/// Every placement of `piece` that can lock on `field` with SRS+ movement
/// (shift, soft drop, 90 and 180-degree rotation), as the cells it covers.
/// The search starts in the free rows above the field, where the piece can
/// move and rotate without obstruction: one state per rotation and column,
/// resting just above the tracked rows, from which every higher state is
/// equivalent.
pub fn lockable_placements(field: Mask, piece: Piece) -> Vec<Mask> {
    let shapes = shapes();
    let index = piece_index(piece);
    let rotations = shapes.rotations[index];
    let cells = &shapes.cells[index];
    // States are (rotation, x + 3, y + 3) in a flat visited array.
    const SPAN: i32 = 16;
    let id = |rotation: usize, x: i32, y: i32| {
        (rotation as i32 * SPAN * SPAN + (x + 3) * SPAN + (y + 3)) as usize
    };
    let mut visited = vec![false; 4 * (SPAN * SPAN) as usize];
    let mut stack = Vec::new();
    for (rotation, shape) in cells.iter().enumerate().take(rotations) {
        let low = shape.iter().map(|(_, dy)| *dy).min().unwrap();
        let y = FIELD_ROWS as i32 - low;
        for x in -3..WIDTH {
            if placement(field, shape, x, y).is_some() {
                visited[id(rotation, x, y)] = true;
                stack.push((rotation, x, y));
            }
        }
    }

    let mut locked = HashSet::new();
    while let Some((rotation, x, y)) = stack.pop() {
        let shape = &cells[rotation];
        let here = placement(field, shape, x, y).expect("visited states are valid");
        if placement(field, shape, x, y - 1).is_none() && here.count_ones() == 4 {
            locked.insert(here);
        }
        let mut push = |rotation: usize, x: i32, y: i32| {
            let key = id(rotation, x, y);
            if !visited[key] {
                visited[key] = true;
                stack.push((rotation, x, y));
            }
        };
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1)] {
            if placement(field, shape, x + dx, y + dy).is_some() {
                push(rotation, x + dx, y + dy);
            }
        }
        if rotations == 1 {
            continue;
        }
        for turn in 1..4 {
            let to = (rotation + turn) % 4;
            let target = &cells[to];
            let tests = std::iter::once((0, 0))
                .chain(kicks(piece, rotation, to).iter().map(|(dx, dy)| (*dx, -dy)));
            for (dx, dy) in tests {
                if placement(field, target, x + dx, y + dy).is_some() {
                    push(to, x + dx, y + dy);
                    break;
                }
            }
        }
    }
    let mut locked: Vec<Mask> = locked.into_iter().collect();
    locked.sort_unstable();
    locked
}

/// A setup as drawn: its cells, and which piece each cell needs (`any` for
/// gray cells).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub cells: Mask,
    /// Per piece (in `ALL_PIECES` order), the cells that piece must cover.
    pub coloured: [Mask; 7],
    pub any: Mask,
}

impl Target {
    pub fn new(cells: impl IntoIterator<Item = (u8, u8, Option<Piece>)>) -> Option<Self> {
        let mut target = Self {
            cells: 0,
            coloured: [0; 7],
            any: 0,
        };
        for (x, y, piece) in cells {
            if usize::from(y) >= FIELD_ROWS || x >= 10 {
                return None;
            }
            let cell = bit(x, y);
            target.cells |= cell;
            match piece {
                Some(piece) => target.coloured[piece_index(piece)] |= cell,
                None => target.any |= cell,
            }
        }
        Some(target)
    }

    /// Whether HOLD and the rest of the queue still hold a piece for every
    /// unplaced coloured tetromino, and enough pieces in all.
    fn has_pieces_for(&self, placed: Mask, hold: Option<Piece>, queue: &[Piece]) -> bool {
        let mut available = [0u32; 7];
        for piece in hold.iter().chain(queue) {
            available[piece_index(*piece)] += 1;
        }
        let left = self.cells & !placed;
        let total = left.count_ones() / 4;
        let pieces = hold.iter().count() + queue.len();
        total as usize <= pieces
            && self
                .coloured
                .iter()
                .zip(available)
                .all(|(cells, have)| (cells & left).count_ones().div_ceil(4) <= have)
    }

    /// Whether some row of the drawing clears while the setup is built.
    pub fn clears_rows(&self) -> bool {
        self.full_rows().next().is_some()
    }

    pub fn piece_count(&self) -> usize {
        self.cells.count_ones() as usize / 4
    }

    /// Cells `piece` may cover.
    fn allowed(&self, piece: Piece) -> Mask {
        self.coloured[piece_index(piece)] | self.any
    }

    /// Drawing rows that clear once filled: every cell of the row is in the
    /// setup.
    fn full_rows(&self) -> impl Iterator<Item = usize> + '_ {
        (0..FIELD_ROWS).filter(|y| self.cells & row_mask(*y) == row_mask(*y))
    }

    /// Rows of `placed` that have cleared.
    fn cleared(&self, placed: Mask) -> Vec<usize> {
        self.full_rows()
            .filter(|y| placed & row_mask(*y) == row_mask(*y))
            .collect()
    }

    /// The drawing row of each field row, skipping cleared rows.
    fn row_map(cleared: &[usize]) -> Vec<usize> {
        (0..FIELD_ROWS + cleared.len())
            .filter(|y| !cleared.contains(y))
            .take(FIELD_ROWS)
            .collect()
    }

    /// The live field for `placed` drawing cells: cleared rows removed.
    pub fn field(&self, placed: Mask) -> Mask {
        let cleared = self.cleared(placed);
        let map = Self::row_map(&cleared);
        let mut field = 0;
        for (field_row, drawing_row) in map.iter().enumerate() {
            if *drawing_row < FIELD_ROWS {
                field |= ((placed >> (drawing_row * 10)) & ROW) << (field_row * 10);
            }
        }
        field
    }

    /// Maps field cells to drawing cells for `placed`; `None` when a cell
    /// falls above the tracked rows.
    pub fn to_drawing(&self, placed: Mask, field_cells: Mask) -> Option<Mask> {
        let map = Self::row_map(&self.cleared(placed));
        let mut drawing = 0;
        for (field_row, drawing_row) in map.iter().enumerate() {
            let row = (field_cells >> (field_row * 10)) & ROW;
            if row == 0 {
                continue;
            }
            if *drawing_row >= FIELD_ROWS {
                return None;
            }
            drawing |= row << (drawing_row * 10);
        }
        Some(drawing)
    }

    /// The drawing cells placed on a live `field`, inferring cleared rows:
    /// the fewest cleared full rows that explain the field with the setup's
    /// colours (`colour_of` gives a field cell's piece). `None` when no
    /// reading fits.
    pub fn read_field(&self, field: Mask, colour_of: impl Fn(u8, u8) -> Piece) -> Option<Mask> {
        let full: Vec<usize> = self.full_rows().collect();
        let mut subsets: Vec<Vec<usize>> = (0..1usize << full.len())
            .map(|bits| {
                full.iter()
                    .enumerate()
                    .filter(|(index, _)| bits & (1 << index) != 0)
                    .map(|(_, row)| *row)
                    .collect()
            })
            .collect();
        subsets.sort_by_key(Vec::len);
        'subsets: for cleared in subsets {
            let mut placed: Mask = cleared
                .iter()
                .map(|row| row_mask(*row))
                .fold(0, |a, b| a | b);
            let map = Self::row_map(&cleared);
            for y in 0..FIELD_ROWS {
                for x in 0..10u8 {
                    if field & bit(x, y as u8) == 0 {
                        continue;
                    }
                    let Some(drawing_row) = map.get(y).copied().filter(|row| *row < FIELD_ROWS)
                    else {
                        continue 'subsets;
                    };
                    let cell = bit(x, drawing_row as u8);
                    if self.allowed(colour_of(x, y as u8)) & cell == 0 {
                        continue 'subsets;
                    }
                    placed |= cell;
                }
            }
            // A full row that is not cleared cannot be complete.
            let consistent = full
                .iter()
                .filter(|row| !cleared.contains(row))
                .all(|row| placed & row_mask(*row) != row_mask(*row));
            if consistent && placed.count_ones().is_multiple_of(4) && self.field(placed) == field {
                return Some(placed);
            }
        }
        None
    }

    /// Unplaced drawing cells as `(x, field row, drawing row)`, for display.
    pub fn remaining_on_field(&self, placed: Mask) -> Vec<(u8, u8, u8)> {
        let cleared = self.cleared(placed);
        let mut cells = Vec::new();
        for y in 0..FIELD_ROWS {
            if cleared.contains(&y) {
                continue;
            }
            let field_row = y - cleared.iter().filter(|row| **row < y).count();
            for x in 0..10u8 {
                let cell = bit(x, y as u8);
                if self.cells & cell != 0 && placed & cell == 0 {
                    cells.push((x, field_row as u8, y as u8));
                }
            }
        }
        cells
    }
}

/// Whether a setup can still be built from a state, and for which of the
/// undisclosed queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Buildable {
    /// Buildable whatever the undisclosed pieces are.
    Always,
    /// Buildable for some undisclosed pieces only.
    Sometimes,
    Never,
}

/// The build search with its move-generation cache, shared across setups
/// and frames.
#[derive(Debug, Default)]
pub struct Finder {
    moves: HashMap<(Mask, Piece), Rc<[Mask]>>,
    /// Drawn placements per number of drawing rows.
    drawn: HashMap<usize, Rc<DrawnTable>>,
}

/// Failed states of one target: placed cells, HOLD, and the rest of the
/// queue, so completions of undisclosed pieces share them.
type Failed = HashSet<(Mask, Option<Piece>, u32)>;

/// The queue from `next` on, 3 bits per piece.
fn queue_code(queue: &[Piece], next: usize) -> u32 {
    queue
        .get(next..)
        .unwrap_or(&[])
        .iter()
        .fold(1, |code, piece| code << 3 | piece_index(*piece) as u32)
}

/// Whether the cells left can still be split into tetrominoes: every
/// 4-connected group of them must have a multiple of 4 cells. As in
/// solution-finder, this prunes most dead ends before move generation. Only
/// valid while no row clears, as a piece placed after a clear is drawn split.
fn separable(remaining: Mask) -> bool {
    const LEFT: Mask = {
        let mut mask = 0;
        let mut y = 0;
        while y < FIELD_ROWS {
            mask |= 1 << (y * 10);
            y += 1;
        }
        mask
    };
    const RIGHT: Mask = LEFT << 9;
    let mut left = remaining;
    while left != 0 {
        let mut group = left & left.wrapping_neg();
        loop {
            let grown = (group
                | ((group & !RIGHT) << 1)
                | ((group & !LEFT) >> 1)
                | (group << 10)
                | (group >> 10))
                & remaining;
            if grown == group {
                break;
            }
            group = grown;
        }
        if !group.count_ones().is_multiple_of(4) {
            return false;
        }
        left &= !group;
    }
    true
}

impl Finder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fields and pieces whose placements are cached.
    pub fn cached_moves(&self) -> usize {
        self.moves.len()
    }

    pub fn clear_cache(&mut self) {
        self.moves.clear();
    }

    fn moves(&mut self, field: Mask, piece: Piece) -> Rc<[Mask]> {
        self.moves
            .entry((field, piece))
            .or_insert_with(|| lockable_placements(field, piece).into())
            .clone()
    }

    /// Whether `target` can be finished from `placed` drawing cells, with
    /// `hold` and the queue starting at the current piece. `unknown[i]`
    /// lists the pieces the `i`-th undisclosed piece after `queue` may be.
    pub fn buildable(
        &mut self,
        target: &Target,
        placed: Mask,
        hold: Option<Piece>,
        queue: &[Piece],
        unknown: &[Vec<Piece>],
    ) -> Buildable {
        let remaining = (target.cells & !placed).count_ones() as usize / 4;
        let needed = (remaining + 1)
            .saturating_sub(queue.len())
            .min(unknown.len());
        let mut completions: Vec<Vec<Piece>> =
            vec![queue[..queue.len().min(remaining + 1)].to_vec()];
        for options in &unknown[..needed] {
            completions = completions
                .into_iter()
                .flat_map(|prefix| {
                    options.iter().map(move |piece| {
                        let mut queue = prefix.clone();
                        queue.push(*piece);
                        queue
                    })
                })
                .collect();
        }
        let total = completions.len();
        let mut failed = Failed::new();
        let built = completions
            .iter()
            .filter(|queue| self.search(target, placed, hold, queue, 0, &mut failed))
            .count();
        match built {
            0 => Buildable::Never,
            n if n == total => Buildable::Always,
            _ => Buildable::Sometimes,
        }
    }

    /// Every state `target` can be finished in from `placed` with `hold` and
    /// `queue` (Current first): HOLD then, and the queue index of the next
    /// Current. Which piece stays in HOLD depends on the order played.
    pub fn build_end_states(
        &mut self,
        target: &Target,
        placed: Mask,
        hold: Option<Piece>,
        queue: &[Piece],
    ) -> Vec<(Option<Piece>, usize)> {
        let mut ends = Vec::new();
        let mut seen = HashSet::new();
        self.collect_ends(target, placed, hold, queue, 0, &mut seen, &mut ends);
        ends
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_ends(
        &mut self,
        target: &Target,
        placed: Mask,
        hold: Option<Piece>,
        queue: &[Piece],
        next: usize,
        seen: &mut HashSet<(Mask, Option<Piece>, usize)>,
        ends: &mut Vec<(Option<Piece>, usize)>,
    ) {
        if !seen.insert((placed, hold, next)) {
            return;
        }
        if placed == target.cells {
            ends.push((hold, next));
            return;
        }
        if !target.has_pieces_for(placed, hold, &queue[next.min(queue.len())..])
            || (!target.clears_rows() && !separable(target.cells & !placed))
        {
            return;
        }
        let Some(&current) = queue.get(next) else {
            return;
        };
        let mut options = vec![(current, hold, next + 1)];
        match hold {
            Some(held) if held != current => options.push((held, Some(current), next + 1)),
            Some(_) => {}
            None => {
                if let Some(&after) = queue.get(next + 1) {
                    options.push((after, Some(current), next + 2));
                }
            }
        }
        let field = target.field(placed);
        for (piece, hold, next) in options {
            let allowed = target.allowed(piece) & !placed;
            for &cells in self.moves(field, piece).iter() {
                let Some(drawing) = target.to_drawing(placed, cells) else {
                    continue;
                };
                if drawing & !allowed != 0 {
                    continue;
                }
                self.collect_ends(target, placed | drawing, hold, queue, next, seen, ends);
            }
        }
    }

    /// Depth-first search over the piece order with one HOLD slot.
    fn search(
        &mut self,
        target: &Target,
        placed: Mask,
        hold: Option<Piece>,
        queue: &[Piece],
        next: usize,
        failed: &mut Failed,
    ) -> bool {
        if placed == target.cells {
            return true;
        }
        let key = (placed, hold, queue_code(queue, next));
        if failed.contains(&key) {
            return false;
        }
        if !target.has_pieces_for(placed, hold, &queue[next.min(queue.len())..])
            || (!target.clears_rows() && !separable(target.cells & !placed))
        {
            failed.insert(key);
            return false;
        }
        let Some(&current) = queue.get(next) else {
            return false;
        };
        // (piece to place, HOLD afterwards, next queue index)
        let mut options = vec![(current, hold, next + 1)];
        match hold {
            Some(held) if held != current => options.push((held, Some(current), next + 1)),
            Some(_) => {}
            None => {
                if let Some(&after) = queue.get(next + 1) {
                    options.push((after, Some(current), next + 2));
                }
            }
        }
        let field = target.field(placed);
        for (piece, hold, next) in options {
            let allowed = target.allowed(piece) & !placed;
            for &cells in self.moves(field, piece).iter() {
                let Some(drawing) = target.to_drawing(placed, cells) else {
                    continue;
                };
                if drawing & !allowed != 0 {
                    continue;
                }
                if self.search(target, placed | drawing, hold, queue, next, failed) {
                    return true;
                }
            }
        }
        failed.insert(key);
        false
    }
}

/// One piece of a solution: the piece and the drawing cells it covers.
pub type Step = (Piece, Mask);

/// The order pieces can be played in.
#[derive(Debug, Clone, Copy)]
pub enum Supply<'a> {
    /// The live queue (Current first) with one HOLD slot.
    Queue {
        hold: Option<Piece>,
        queue: &'a [Piece],
    },
    /// These pieces (counts in `ALL_PIECES` order) in any order: whether a
    /// set of placements can be stacked at all, as the source solves were
    /// checked.
    AnyOrder([u8; 7]),
}

/// Where a solution search stands in its supply: HOLD and the next queue
/// index, or the pieces left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SupplyState {
    Queue { hold: Option<Piece>, next: usize },
    AnyOrder([u8; 7]),
}

/// A placement in drawing coordinates: the piece, its cells, and the rows
/// (bit per row) it is split around, which must clear before it is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DrawnPiece {
    piece: Piece,
    cells: Mask,
    gaps: u8,
}

/// Every drawn placement within the drawing rows `0..rows`, indexed by its
/// lowest cell (`Mask::trailing_zeros`). A piece placed after rows cleared
/// is drawn split around them: a shape `k` rows tall takes any `k` of the
/// rows, as in solution-finder's line-clear-aware placements.
#[derive(Debug)]
struct DrawnTable {
    by_lowest: Vec<Vec<DrawnPiece>>,
}

impl DrawnTable {
    fn new(rows: usize) -> Self {
        let shapes = shapes();
        let mut by_lowest = vec![Vec::new(); FIELD_ROWS * 10];
        let mut seen = HashSet::new();
        for piece in ALL_PIECES {
            let index = piece_index(piece);
            for shape in shapes.cells[index].iter().take(shapes.rotations[index]) {
                let low = shape.iter().map(|(_, dy)| *dy).min().unwrap();
                let left = shape.iter().map(|(dx, _)| *dx).min().unwrap();
                let height = (shape.iter().map(|(_, dy)| *dy).max().unwrap() - low + 1) as usize;
                let width = shape.iter().map(|(dx, _)| *dx).max().unwrap() - left + 1;
                // Each increasing choice of `height` rows out of `0..rows`.
                for choice in 0u32..1 << rows {
                    if choice.count_ones() as usize != height {
                        continue;
                    }
                    let chosen: Vec<u8> =
                        (0..rows as u8).filter(|y| choice & (1 << y) != 0).collect();
                    let first = chosen[0];
                    let last = chosen[height - 1];
                    let gaps = (first..=last)
                        .filter(|y| choice & (1 << y) == 0)
                        .fold(0u8, |gaps, y| gaps | 1 << y);
                    for x in 0..=(WIDTH - width) {
                        let cells = shape.iter().fold(0, |cells, (dx, dy)| {
                            cells | bit((x + dx - left) as u8, chosen[(dy - low) as usize])
                        });
                        if seen.insert((piece, cells)) {
                            by_lowest[cells.trailing_zeros() as usize].push(DrawnPiece {
                                piece,
                                cells,
                                gaps,
                            });
                        }
                    }
                }
            }
        }
        Self { by_lowest }
    }
}

impl Target {
    /// The rows `0..height` of the drawing filled completely, any piece in
    /// any cell: a perfect clear of that height.
    pub fn perfect_clear(height: usize) -> Option<Self> {
        (height <= FIELD_ROWS).then(|| {
            let cells = (0..height).map(row_mask).fold(0, |a, b| a | b);
            Self {
                cells,
                coloured: [0; 7],
                any: cells,
            }
        })
    }

    /// Rows of `placed` that have cleared, a bit per row.
    fn cleared_bits(&self, placed: Mask) -> u8 {
        self.cleared(placed)
            .iter()
            .fold(0, |bits, row| bits | 1 << row)
    }

    /// Maps drawing cells in rows that have not cleared to the live field.
    fn to_field(&self, placed: Mask, drawing: Mask) -> Mask {
        let cleared = self.cleared_bits(placed);
        let mut field = 0;
        let mut field_row = 0;
        for y in 0..FIELD_ROWS {
            if cleared & (1 << y) != 0 {
                continue;
            }
            field |= ((drawing >> (y * 10)) & ROW) << (field_row * 10);
            field_row += 1;
        }
        field
    }
}

impl Finder {
    fn drawn_table(&mut self, rows: usize) -> Rc<DrawnTable> {
        self.drawn
            .entry(rows)
            .or_insert_with(|| Rc::new(DrawnTable::new(rows)))
            .clone()
    }

    /// Every way to finish `target` from `placed` drawing cells with the
    /// pieces `supply` gives, as sets of steps (the order is not part of a
    /// solution); at most `limit`. As in solution-finder, the empty cells
    /// are first tiled with the pieces available, ignoring order (each step
    /// covers the lowest empty cell), and each tiling is kept when some
    /// order of it can be played: every piece comes from the supply, lands
    /// with SRS+ movement on the field of the moment, and is placed only
    /// after the rows it is drawn split around have cleared.
    pub fn solutions(
        &mut self,
        target: &Target,
        placed: Mask,
        supply: Supply<'_>,
        limit: usize,
    ) -> Vec<Vec<Step>> {
        let (queue, state, counts) = match supply {
            Supply::Queue { hold, queue } => {
                let mut counts = [0u8; 7];
                for piece in hold.iter().chain(queue) {
                    counts[piece_index(*piece)] += 1;
                }
                (queue, SupplyState::Queue { hold, next: 0 }, counts)
            }
            Supply::AnyOrder(counts) => (&[][..], SupplyState::AnyOrder(counts), counts),
        };
        let rows = (0..FIELD_ROWS)
            .rev()
            .find(|y| target.cells & row_mask(*y) != 0)
            .map_or(0, |y| y + 1);
        let table = self.drawn_table(rows);
        let mut search = Solutions {
            target,
            placed,
            queue,
            start: state,
            table: &table,
            tiling: Vec::new(),
            found: Vec::new(),
            limit,
        };
        search.tile(self, target.cells & !placed, counts);
        search.found
    }

    /// Perfect clears from a live field (no rows cleared yet): the lowest
    /// height from the top block up to 4 rows whose empty cells the supply
    /// can fill, with its solutions. `None` when no height works.
    pub fn perfect_clears(
        &mut self,
        field: Mask,
        supply: Supply<'_>,
        limit: usize,
    ) -> Option<(usize, Vec<Vec<Step>>)> {
        let top = (0..FIELD_ROWS)
            .rev()
            .find(|y| field & row_mask(*y) != 0)
            .map_or(0, |y| y + 1);
        let pieces = match supply {
            Supply::Queue { hold, queue } => queue.len() + usize::from(hold.is_some()),
            Supply::AnyOrder(counts) => counts.iter().map(|count| usize::from(*count)).sum(),
        };
        for height in top.max(1)..=4 {
            let empty = height * 10 - field.count_ones() as usize;
            if !empty.is_multiple_of(4) || empty / 4 > pieces {
                continue;
            }
            let target = Target::perfect_clear(height).expect("4 rows fit the field");
            let found = self.solutions(&target, field, supply, limit);
            if !found.is_empty() {
                return Some((height, found));
            }
        }
        None
    }
}

/// One run of `Finder::solutions`.
struct Solutions<'a> {
    target: &'a Target,
    placed: Mask,
    queue: &'a [Piece],
    start: SupplyState,
    table: &'a DrawnTable,
    /// The tiling being built.
    tiling: Vec<DrawnPiece>,
    found: Vec<Vec<Step>>,
    limit: usize,
}

impl Solutions<'_> {
    /// Tiles `empty` with the pieces in `counts`, each step covering the
    /// lowest empty cell, and keeps the tilings that can be played.
    fn tile(&mut self, finder: &mut Finder, empty: Mask, counts: [u8; 7]) {
        if self.found.len() >= self.limit {
            return;
        }
        if empty == 0 {
            let mut failed = HashSet::new();
            if self.playable(finder, self.placed, self.start, 0, &mut failed) {
                let mut steps: Vec<Step> = self
                    .tiling
                    .iter()
                    .map(|drawn| (drawn.piece, drawn.cells))
                    .collect();
                steps.sort_unstable();
                self.found.push(steps);
            }
            return;
        }
        let table = self.table;
        for drawn in &table.by_lowest[empty.trailing_zeros() as usize] {
            let index = piece_index(drawn.piece);
            if counts[index] == 0
                || drawn.cells & !empty != 0
                || drawn.cells & !self.target.allowed(drawn.piece) != 0
            {
                continue;
            }
            let mut left = counts;
            left[index] -= 1;
            self.tiling.push(*drawn);
            self.tile(finder, empty & !drawn.cells, left);
            self.tiling.pop();
        }
    }

    /// Whether the tiling's unplaced steps (`used` has a bit per placed
    /// step) can be played from `placed` in some order the supply allows.
    fn playable(
        &self,
        finder: &mut Finder,
        placed: Mask,
        state: SupplyState,
        used: u16,
        failed: &mut HashSet<(u16, SupplyState)>,
    ) -> bool {
        if usize::from(used) == (1 << self.tiling.len()) - 1 {
            return true;
        }
        if failed.contains(&(used, state)) {
            return false;
        }
        let cleared = self.target.cleared_bits(placed);
        let field = self.target.field(placed);
        for (piece, after) in supply_options(self.queue, state) {
            let moves = finder.moves(field, piece);
            for (step, drawn) in self.tiling.iter().enumerate() {
                if used & (1 << step) != 0 || drawn.piece != piece || drawn.gaps & !cleared != 0 {
                    continue;
                }
                let live = self.target.to_field(placed, drawn.cells);
                if moves.binary_search(&live).is_ok()
                    && self.playable(
                        finder,
                        placed | drawn.cells,
                        after,
                        used | 1 << step,
                        failed,
                    )
                {
                    return true;
                }
            }
        }
        failed.insert((used, state));
        false
    }
}

/// The pieces the supply can play next, with its state afterwards.
fn supply_options(queue: &[Piece], state: SupplyState) -> Vec<(Piece, SupplyState)> {
    let mut options = Vec::new();
    match state {
        SupplyState::Queue { hold, next } => {
            if let Some(&current) = queue.get(next) {
                options.push((
                    current,
                    SupplyState::Queue {
                        hold,
                        next: next + 1,
                    },
                ));
                match hold {
                    Some(held) if held != current => options.push((
                        held,
                        SupplyState::Queue {
                            hold: Some(current),
                            next: next + 1,
                        },
                    )),
                    Some(_) => {}
                    None => {
                        if let Some(&after) = queue.get(next + 1) {
                            options.push((
                                after,
                                SupplyState::Queue {
                                    hold: Some(current),
                                    next: next + 2,
                                },
                            ));
                        }
                    }
                }
            } else if let Some(held) = hold {
                // The queue is used up; HOLD can still be played.
                options.push((held, SupplyState::Queue { hold: None, next }));
            }
        }
        SupplyState::AnyOrder(counts) => {
            for (index, piece) in ALL_PIECES.iter().enumerate() {
                if counts[index] > 0 {
                    let mut left = counts;
                    left[index] -= 1;
                    options.push((*piece, SupplyState::AnyOrder(left)));
                }
            }
        }
    }
    options
}

/// The pieces each undisclosed position may hold under the 7-bag, given the
/// absolute index (pieces dealt since the game start) of `known[0]`. The
/// pieces before `known` in the same bag are not known, so the lists can be
/// larger than the truth but never miss a piece.
pub fn bag_options(first_index: usize, known: &[Piece], slots: usize) -> Vec<Vec<Piece>> {
    let mut options = Vec::with_capacity(slots);
    for slot in 0..slots {
        let index = first_index + known.len() + slot;
        let bag_start = index - index % 7;
        let seen: Vec<Piece> = known
            .iter()
            .enumerate()
            .filter(|(offset, _)| (bag_start..index).contains(&(first_index + offset)))
            .map(|(_, piece)| *piece)
            .collect();
        let duplicates = seen
            .iter()
            .enumerate()
            .any(|(at, piece)| seen[..at].contains(piece));
        let open: Vec<Piece> = if duplicates {
            // The queue does not follow the expected bag; assume nothing.
            ALL_PIECES.to_vec()
        } else {
            ALL_PIECES
                .iter()
                .copied()
                .filter(|piece| !seen.contains(piece))
                .collect()
        };
        options.push(open);
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(mask: Mask) -> Vec<(u8, u8)> {
        (0..60)
            .filter(|index| mask & (1 << index) != 0)
            .map(|index| ((index % 10) as u8, (index / 10) as u8))
            .collect()
    }

    fn mask(cells: &[(u8, u8)]) -> Mask {
        cells.iter().fold(0, |acc, (x, y)| acc | bit(*x, *y))
    }

    #[test]
    fn every_drop_position_is_lockable_on_an_empty_field() {
        // Flat I: 7 columns; vertical I: 10 columns.
        assert_eq!(lockable_placements(0, Piece::I).len(), 17);
        assert_eq!(lockable_placements(0, Piece::O).len(), 9);
        // T: 8 flat up, 8 flat down, 9 + 9 vertical.
        assert_eq!(lockable_placements(0, Piece::T).len(), 34);
    }

    #[test]
    fn soft_drop_reaches_under_an_overhang() {
        // A roof over columns 0-1 at row 1; O fits under it from the side.
        let field = mask(&[(0, 1), (1, 1), (2, 2)]);
        let tucked = mask(&[(2, 0), (3, 0), (2, 1), (3, 1)]);
        assert!(lockable_placements(field, Piece::O).contains(&tucked));
        let blocked = mask(&[(0, 0)]);
        assert!(
            lockable_placements(field, Piece::O)
                .iter()
                .all(|placement| placement & blocked == 0),
            "column 0 under the roof needs a piece that turns in"
        );
    }

    #[test]
    fn t_spin_double_slot_is_reached_by_kicks() {
        // Standard TSD: rows 0-1 full except a T slot at x=4, overhang at
        // (3,2). Row 0 hole at 4, row 1 holes at 3,4,5.
        let mut field = 0;
        for x in 0..10u8 {
            if x != 4 {
                field |= bit(x, 0);
            }
            if !(3..=5).contains(&x) {
                field |= bit(x, 1);
            }
        }
        field |= bit(3, 2) | bit(2, 2);
        let tsd = mask(&[(4, 0), (3, 1), (4, 1), (5, 1)]);
        assert!(lockable_placements(field, Piece::T).contains(&tsd));
    }

    #[test]
    fn build_order_uses_hold() {
        // Two O pieces side by side need O, O; with I first, HOLD keeps it.
        let target = Target::new(
            [
                (0, 0),
                (1, 0),
                (0, 1),
                (1, 1),
                (2, 0),
                (3, 0),
                (2, 1),
                (3, 1),
            ]
            .map(|(x, y)| (x, y, Some(Piece::O))),
        )
        .unwrap();
        let mut finder = Finder::new();
        assert_eq!(
            finder.buildable(&target, 0, None, &[Piece::I, Piece::O, Piece::O], &[]),
            Buildable::Always
        );
        assert_eq!(
            finder.buildable(&target, 0, None, &[Piece::I, Piece::T, Piece::O], &[]),
            Buildable::Never
        );
        // One O is still to come.
        let unknown = vec![vec![Piece::O, Piece::T]];
        assert_eq!(
            finder.buildable(&target, 0, None, &[Piece::O], &unknown),
            Buildable::Sometimes
        );
    }

    #[test]
    fn a_floating_piece_must_wait_for_its_support() {
        // L on top of an O: O must go first.
        let o = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(x, y)| (x, y, Some(Piece::O)));
        let l = [(0, 2), (1, 2), (2, 2), (2, 3)].map(|(x, y)| (x, y, Some(Piece::L)));
        let target = Target::new(o.into_iter().chain(l)).unwrap();
        let mut finder = Finder::new();
        assert_eq!(
            finder.buildable(&target, 0, None, &[Piece::O, Piece::L], &[]),
            Buildable::Always
        );
        // L first waits in HOLD and comes back with the next piece.
        let queue = [Piece::L, Piece::O, Piece::T];
        assert_eq!(
            finder.buildable(&target, 0, None, &queue, &[]),
            Buildable::Always
        );
        // With HOLD taken by T, L cannot wait.
        assert_eq!(
            finder.buildable(&target, 0, Some(Piece::T), &queue, &[]),
            Buildable::Never
        );
    }

    #[test]
    fn cleared_rows_are_skipped_on_the_field() {
        // Row 0 is filled by I, I and an O at the right; it clears, so the O
        // placed after it on the left is drawn one row higher.
        let mut drawn = Vec::new();
        for x in 0..4 {
            drawn.push((x, 0, Some(Piece::I)));
        }
        for x in 4..8 {
            drawn.push((x, 0, Some(Piece::I)));
        }
        drawn.extend([(8, 0), (9, 0), (8, 1), (9, 1)].map(|(x, y)| (x, y, Some(Piece::O))));
        // After row 0 clears, an O at field rows 0-1 is drawn at rows 1-2.
        drawn.extend([(0, 1), (1, 1), (0, 2), (1, 2)].map(|(x, y)| (x, y, Some(Piece::O))));
        let target = Target::new(drawn).unwrap();
        let placed = target.cells & !mask(&[(0, 1), (1, 1), (0, 2), (1, 2)]);
        assert_eq!(target.field(placed), mask(&[(8, 0), (9, 0)]));
        assert_eq!(
            target.to_drawing(placed, mask(&[(0, 0), (1, 0), (0, 1), (1, 1)])),
            Some(mask(&[(0, 1), (1, 1), (0, 2), (1, 2)]))
        );
        let mut finder = Finder::new();
        assert_eq!(
            finder.buildable(
                &target,
                0,
                None,
                &[Piece::I, Piece::I, Piece::O, Piece::O],
                &[]
            ),
            Buildable::Always
        );
        // Reading the live field back finds the cleared row.
        let read = target.read_field(mask(&[(8, 0), (9, 0)]), |_, _| Piece::O);
        assert_eq!(read, Some(placed));
        assert_eq!(
            cells(
                target
                    .remaining_on_field(placed)
                    .iter()
                    .fold(0, |acc, (x, y, _)| acc | bit(*x, *y))
            ),
            vec![(0, 0), (1, 0), (0, 1), (1, 1)]
        );
    }

    #[test]
    fn perfect_clears_follow_the_queue_and_hold() {
        // Two rows full except a 4x2 hole at the left: two O side by side
        // with the queue O, O.
        let mut field = 0;
        for y in 0..2u8 {
            for x in 4..10u8 {
                field |= bit(x, y);
            }
        }
        let mut finder = Finder::new();
        let queue = [Piece::O, Piece::O];
        let (height, found) = finder
            .perfect_clears(
                field,
                Supply::Queue {
                    hold: None,
                    queue: &queue,
                },
                10,
            )
            .unwrap();
        assert_eq!(height, 2);
        assert_eq!(found.len(), 1);
        // I, I fills it as two flat rows; HOLD lets T wait.
        let queue = [Piece::T, Piece::I, Piece::I];
        let (_, found) = finder
            .perfect_clears(
                field,
                Supply::Queue {
                    hold: None,
                    queue: &queue,
                },
                10,
            )
            .unwrap();
        assert_eq!(
            found,
            vec![vec![
                (Piece::I, mask(&[(0, 0), (1, 0), (2, 0), (3, 0)])),
                (Piece::I, mask(&[(0, 1), (1, 1), (2, 1), (3, 1)])),
            ]]
        );
        // Without the second I there is none.
        let queue = [Piece::T, Piece::I, Piece::O];
        assert!(
            finder
                .perfect_clears(
                    field,
                    Supply::Queue {
                        hold: None,
                        queue: &queue
                    },
                    10
                )
                .is_none()
        );
    }

    #[test]
    fn separable_needs_groups_of_four() {
        assert!(separable(mask(&[(0, 0), (1, 0), (2, 0), (3, 0)])));
        // Split by a gap: 3 + 1.
        assert!(!separable(mask(&[(0, 0), (1, 0), (2, 0), (4, 0)])));
        // Column 9 and column 0 of the next row are not neighbours.
        assert!(!separable(mask(&[(7, 0), (8, 0), (9, 0), (0, 1)])));
        assert!(separable(0));
    }

    #[test]
    fn bag_options_exclude_pieces_seen_in_the_same_bag() {
        // First known piece is index 3 of a bag; the 4 known fill it to 7.
        let known = [Piece::T, Piece::I, Piece::O, Piece::S];
        let options = bag_options(3, &known, 1);
        // Index 7 starts a new bag: anything.
        assert_eq!(options[0].len(), 7);
        let options = bag_options(0, &known, 1);
        assert_eq!(options[0], vec![Piece::J, Piece::L, Piece::Z]);
    }
}
