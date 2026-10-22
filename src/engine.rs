//! Solo TETR.IO v19 game engine.
//!
//! This is a port of the solo half of `ReplayMachine` in czsmall_nnue's
//! `trainer/ttrm_exact_replay.py`, whose reconstruction matches the end
//! snapshot of every stream in the local TETRA LEAGUE corpus. It covers the
//! MINSTD piece stream, SRS+ (including 180-degree kicks), DAS/ARR/DCD/SDF
//! handling, gravity with its ramp, lock delay with movement and rotation
//! resets, safelock, Hold, clutch spawning, and spin classification.
//!
//! Multiplayer garbage (packets, cancellation, `rngex` holes) is not ported.
//! Garbage rows can still be scheduled per placement, which lets a replay of a
//! multiplayer stream receive exactly the rows the Python reconstruction
//! tanked and keeps the rest of the timeline comparable.
//!
//! The float arithmetic deliberately follows the Python expression order,
//! including `round(..., 6)` and Python's float modulo, because TETR.IO's fall
//! legality probe is sensitive to the last digits of the piece's Y coordinate.

use std::collections::{BTreeMap, VecDeque};

use thiserror::Error;

use crate::model::Piece;

pub const WIDTH: usize = 10;
/// Internal board height: 20 visible rows plus a 20-row buffer.
pub const HEIGHT: usize = 40;

/// TETR.IO shuffles this order with MINSTD for every 7-bag.
pub const PIECE_ORDER: [Piece; 7] = [
    Piece::Z,
    Piece::L,
    Piece::O,
    Piece::S,
    Piece::I,
    Piece::J,
    Piece::T,
];

/// Bottom-up continuous centre Y of a freshly spawned piece. TETR.IO spawns at
/// client y = 17.96 on the 40-row board, i.e. 39 - 17.96 here.
const SPAWN_Y: f64 = 21.04;
const SPAWN_X: i32 = 4;
/// SDF value TETR.IO treats as an instant soft drop.
const SDF_INSTANT: f64 = 41.0;

#[derive(Debug, Error, PartialEq)]
pub enum EngineError {
    #[error("piece locks outside the board at ({x}, {y})")]
    LockOutside { x: i32, y: i32 },
    #[error("piece overlaps an occupied cell at ({x}, {y})")]
    LockOverlap { x: i32, y: i32 },
    #[error("garbage pushes occupied cells past the internal ceiling")]
    GarbageOverflow,
    #[error("invalid garbage hole column {0}")]
    GarbageHole(u8),
    #[error("garbage was scheduled for placement {0}, which cleared lines")]
    GarbageOnClear(usize),
    #[error("time moved backwards: target {target}, current {current}")]
    TimeBackwards { target: i64, current: i64 },
}

/// TETR.IO's MINSTD (Park-Miller) generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinStd {
    state: u64,
}

impl MinStd {
    pub const MODULUS: u64 = 2_147_483_647;
    pub const MULTIPLIER: u64 = 16_807;

    pub fn new(seed: i64) -> Self {
        let modulus = Self::MODULUS as i64;
        let mut state = seed.rem_euclid(modulus);
        if state <= 0 {
            state += modulus - 1;
        }
        Self {
            state: state as u64,
        }
    }

    pub fn state(&self) -> u64 {
        self.state
    }

    pub fn next_int(&mut self) -> u64 {
        self.state = (Self::MULTIPLIER * self.state) % Self::MODULUS;
        self.state
    }

    pub fn next_float(&mut self) -> f64 {
        (self.next_int() - 1) as f64 / (Self::MODULUS - 1) as f64
    }

    /// Fisher-Yates from the end, as the client shuffles each bag.
    pub fn shuffle_bag(&mut self) -> [Piece; 7] {
        let mut out = PIECE_ORDER;
        for i in (1..out.len()).rev() {
            let j = (self.next_float() * (i + 1) as f64) as usize;
            out.swap(i, j);
        }
        out
    }
}

/// The 7-bag piece stream derived from a replay seed.
#[derive(Debug, Clone)]
pub struct PieceStream {
    rng: MinStd,
    queue: VecDeque<Piece>,
}

impl PieceStream {
    pub fn new(seed: i64) -> Self {
        Self {
            rng: MinStd::new(seed),
            queue: VecDeque::new(),
        }
    }

    fn refill(&mut self) {
        self.queue.extend(self.rng.shuffle_bag());
    }

    pub fn pop(&mut self) -> Piece {
        if self.queue.is_empty() {
            self.refill();
        }
        self.queue.pop_front().expect("refilled bag is not empty")
    }

    pub fn preview(&mut self, count: usize) -> Vec<Piece> {
        while self.queue.len() < count {
            self.refill();
        }
        self.queue.iter().take(count).copied().collect()
    }
}

/// The first `count` pieces a seed produces, in spawn order.
pub fn piece_sequence(seed: i64, count: usize) -> Vec<Piece> {
    let mut stream = PieceStream::new(seed);
    (0..count).map(|_| stream.pop()).collect()
}

/// One board cell: empty, garbage, or a locked piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoardCell {
    Empty,
    Garbage,
    Piece(Piece),
}

/// 40x10 bottom-up board (row 0 is the floor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineBoard {
    rows: Vec<[BoardCell; WIDTH]>,
}

impl Default for EngineBoard {
    fn default() -> Self {
        Self {
            rows: vec![[BoardCell::Empty; WIDTH]; HEIGHT],
        }
    }
}

impl EngineBoard {
    /// Builds a board from bottom-up rows; missing rows are empty.
    pub fn from_rows(rows: &[[BoardCell; WIDTH]]) -> Self {
        let mut board = Self::default();
        for (y, row) in rows.iter().take(HEIGHT).enumerate() {
            board.rows[y] = *row;
        }
        board
    }

    pub fn rows(&self) -> &[[BoardCell; WIDTH]] {
        &self.rows
    }

    pub fn cell(&self, x: usize, y: usize) -> BoardCell {
        self.rows[y][x]
    }

    pub fn occupied(&self, x: i32, y: i32) -> bool {
        if x < 0 || x >= WIDTH as i32 || y < 0 {
            return true;
        }
        if y >= HEIGHT as i32 {
            return false;
        }
        self.rows[y as usize][x as usize] != BoardCell::Empty
    }

    fn lock(&mut self, piece: &ActivePiece) -> Result<(), EngineError> {
        for (x, y) in piece.cells() {
            if !(0..WIDTH as i32).contains(&x) || y < 0 || y >= HEIGHT as i32 {
                return Err(EngineError::LockOutside { x, y });
            }
            let cell = &mut self.rows[y as usize][x as usize];
            if *cell != BoardCell::Empty {
                return Err(EngineError::LockOverlap { x, y });
            }
            *cell = BoardCell::Piece(piece.kind);
        }
        Ok(())
    }

    /// Removes full rows and returns (cleared, garbage rows cleared).
    fn clear_full_rows(&mut self) -> (u32, u32) {
        let mut cleared = 0;
        let mut garbage_cleared = 0;
        let mut kept = Vec::with_capacity(HEIGHT);
        for row in &self.rows {
            if row.iter().all(|cell| *cell != BoardCell::Empty) {
                cleared += 1;
                if row.contains(&BoardCell::Garbage) {
                    garbage_cleared += 1;
                }
            } else {
                kept.push(*row);
            }
        }
        kept.resize(HEIGHT, [BoardCell::Empty; WIDTH]);
        self.rows = kept;
        (cleared, garbage_cleared)
    }

    /// Inserts garbage rows at the floor, one hole column per row, in order.
    pub fn add_garbage(&mut self, holes: &[u8]) -> Result<(), EngineError> {
        for &hole in holes {
            if hole as usize >= WIDTH {
                return Err(EngineError::GarbageHole(hole));
            }
            if self.rows[HEIGHT - 1]
                .iter()
                .any(|cell| *cell != BoardCell::Empty)
            {
                return Err(EngineError::GarbageOverflow);
            }
            self.rows.pop();
            let mut row = [BoardCell::Garbage; WIDTH];
            row[hole as usize] = BoardCell::Empty;
            self.rows.insert(0, row);
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.rows
            .iter()
            .all(|row| row.iter().all(|cell| *cell == BoardCell::Empty))
    }

    /// Per-row occupancy bitmasks, bottom-up, bit x for column x.
    pub fn occupancy_masks(&self) -> Vec<u16> {
        self.masks(|cell| cell != BoardCell::Empty)
    }

    pub fn garbage_masks(&self) -> Vec<u16> {
        self.masks(|cell| cell == BoardCell::Garbage)
    }

    fn masks(&self, filter: impl Fn(BoardCell) -> bool) -> Vec<u16> {
        self.rows
            .iter()
            .map(|row| {
                row.iter()
                    .enumerate()
                    .filter(|(_, cell)| filter(**cell))
                    .fold(0u16, |mask, (x, _)| mask | (1 << x))
            })
            .collect()
    }
}

/// Spin classification under TETR.IO's `all-mini+` rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Spin {
    None,
    Mini,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActivePiece {
    pub kind: Piece,
    pub x: i32,
    /// Bottom-up continuous centre coordinate.
    pub y: f64,
    /// 0 = spawn, 1 = CW, 2 = 180, 3 = CCW.
    pub rotation: u8,
    /// Index into the kick list (0 is the identity test).
    pub last_kick: usize,
    pub rotation_active: bool,
    pub spin: Spin,
}

impl ActivePiece {
    fn spawn(kind: Piece) -> Self {
        Self {
            kind,
            x: SPAWN_X,
            y: SPAWN_Y,
            rotation: 0,
            last_kick: 0,
            rotation_active: false,
            spin: Spin::None,
        }
    }

    /// Board cells covered by the piece. The client's `IsOccupied()` applies
    /// `ceil()` to its top-down Y, which is `floor()` of the bottom-up Y.
    pub fn cells(&self) -> [(i32, i32); 4] {
        let base_y = floor_cell(self.y);
        piece_offsets(self.kind, self.rotation).map(|(dx, dy)| (self.x + dx, base_y + dy))
    }
}

fn floor_cell(y: f64) -> i32 {
    (y + 1e-9).floor() as i32
}

// Centre-relative SRS cells, +x right, +y up.
const T_CELLS: [(i32, i32); 4] = [(-1, 0), (0, 0), (1, 0), (0, 1)];
const L_CELLS: [(i32, i32); 4] = [(1, 1), (-1, 0), (0, 0), (1, 0)];
const J_CELLS: [(i32, i32); 4] = [(-1, 1), (-1, 0), (0, 0), (1, 0)];
const O_CELLS: [(i32, i32); 4] = [(0, 0), (0, 1), (1, 0), (1, 1)];
const S_CELLS: [(i32, i32); 4] = [(0, 0), (-1, 0), (0, 1), (1, 1)];
const Z_CELLS: [(i32, i32); 4] = [(0, 0), (1, 0), (0, 1), (-1, 1)];
const I_CELLS: [[(i32, i32); 4]; 4] = [
    [(2, 0), (-1, 0), (0, 0), (1, 0)],
    [(1, 1), (1, 0), (1, -1), (1, -2)],
    [(2, -1), (-1, -1), (0, -1), (1, -1)],
    [(0, 1), (0, 0), (0, -1), (0, -2)],
];

fn rotate_offset((x, y): (i32, i32), rotation: u8) -> (i32, i32) {
    match rotation % 4 {
        0 => (x, y),
        1 => (y, -x),
        2 => (-x, -y),
        _ => (-y, x),
    }
}

pub fn piece_offsets(kind: Piece, rotation: u8) -> [(i32, i32); 4] {
    let base = match kind {
        Piece::I => return I_CELLS[(rotation % 4) as usize],
        Piece::O => return O_CELLS,
        Piece::T => T_CELLS,
        Piece::L => L_CELLS,
        Piece::J => J_CELLS,
        Piece::S => S_CELLS,
        Piece::Z => Z_CELLS,
    };
    base.map(|cell| rotate_offset(cell, rotation))
}

type Kicks = &'static [(i32, i32)];

/// SRS+ kick lists, identity first, +y up.
fn kick_list(kind: Piece, from: u8, to: u8) -> Kicks {
    const IDENTITY: Kicks = &[(0, 0)];
    let (from, to) = (from % 4, to % 4);
    if kind == Piece::O {
        return IDENTITY;
    }
    if (to + 4 - from) % 4 == 2 {
        return match (from, to) {
            (0, 2) => &[(0, 0), (0, 1), (1, 1), (-1, 1), (1, 0), (-1, 0)],
            (2, 0) => &[(0, 0), (0, -1), (-1, -1), (1, -1), (-1, 0), (1, 0)],
            (3, 1) => &[(0, 0), (-1, 0), (-1, 2), (-1, 1), (0, 2), (0, 1)],
            (1, 3) => &[(0, 0), (1, 0), (1, 2), (1, 1), (0, 2), (0, 1)],
            _ => IDENTITY,
        };
    }
    if kind == Piece::I {
        return match (from, to) {
            (0, 1) => &[(0, 0), (1, 0), (-2, 0), (-2, -1), (1, 2)],
            (0, 3) => &[(0, 0), (-1, 0), (2, 0), (2, -1), (-1, 2)],
            (1, 0) => &[(0, 0), (-1, 0), (2, 0), (-1, -2), (2, 1)],
            (1, 2) => &[(0, 0), (-1, 0), (2, 0), (-1, 2), (2, -1)],
            (2, 1) => &[(0, 0), (-2, 0), (1, 0), (-2, 1), (1, -2)],
            (2, 3) => &[(0, 0), (2, 0), (-1, 0), (2, 1), (-1, -2)],
            (3, 0) => &[(0, 0), (1, 0), (-2, 0), (1, -2), (-2, 1)],
            (3, 2) => &[(0, 0), (1, 0), (-2, 0), (1, 2), (-2, -1)],
            _ => IDENTITY,
        };
    }
    match (from, to) {
        (0, 1) | (2, 1) => &[(0, 0), (-1, 0), (-1, 1), (0, -2), (-1, -2)],
        (1, 0) | (1, 2) => &[(0, 0), (1, 0), (1, -1), (0, 2), (1, 2)],
        (2, 3) | (0, 3) => &[(0, 0), (1, 0), (1, 1), (0, -2), (1, -2)],
        (3, 2) | (3, 0) => &[(0, 0), (-1, 0), (-1, -1), (0, 2), (-1, 2)],
        _ => IDENTITY,
    }
}

/// Python's `round(value, 6)`: correctly rounded, ties to even.
fn round6(value: f64) -> f64 {
    format!("{value:.6}")
        .parse()
        .expect("formatted float parses")
}

/// Python's float `%` with a positive divisor.
fn py_mod(value: f64, divisor: f64) -> f64 {
    let rem = value % divisor;
    if rem != 0.0 && (rem < 0.0) != (divisor < 0.0) {
        rem + divisor
    } else {
        rem
    }
}

fn near_integer(value: f64) -> bool {
    py_mod(value, 1.0).abs() <= 1e-12
}

fn collides(board: &EngineBoard, piece: &ActivePiece) -> bool {
    piece.cells().iter().any(|&(x, y)| board.occupied(x, y))
}

/// TETR.IO v19 `_InternalFall`, in bottom-up coordinates.
fn internal_fall(
    board: &EngineBoard,
    piece: &mut ActivePiece,
    distance: f64,
    clear_rotation_on_row_cross: bool,
) -> bool {
    if distance <= 0.0 {
        return true;
    }
    let client_y = 39.0 - piece.y;
    let mut target_client = round6(client_y + distance);
    if near_integer(target_client) {
        target_client += 1e-6;
    }
    let mut one_below_client = client_y + 1.0;
    if near_integer(one_below_client) {
        one_below_client -= 2e-6;
    }
    let mut target = *piece;
    target.y = 39.0 - target_client;
    let mut one_below = *piece;
    one_below.y = 39.0 - one_below_client;
    if collides(board, &target) || collides(board, &one_below) {
        return false;
    }
    let old_cell_y = floor_cell(piece.y);
    piece.y = target.y;
    if clear_rotation_on_row_cross && floor_cell(piece.y) != old_cell_y {
        piece.rotation_active = false;
        piece.spin = Spin::None;
        piece.last_kick = 0;
    }
    true
}

fn grounded(board: &EngineBoard, piece: &ActivePiece) -> bool {
    let mut trial = *piece;
    !internal_fall(board, &mut trial, 1.0, false)
}

fn hard_drop(board: &EngineBoard, piece: &mut ActivePiece) {
    while internal_fall(board, piece, 1.0, false) {}
}

fn try_rotate(board: &EngineBoard, piece: &mut ActivePiece, delta: u8) -> bool {
    let target = (piece.rotation + delta) % 4;

    // ComputeKick first tests the rotated piece at the unchanged continuous
    // position; only if that fails does it enter the kick table.
    let mut direct = *piece;
    direct.rotation = target;
    if !collides(board, &direct) {
        piece.rotation = target;
        piece.last_kick = 0;
        piece.rotation_active = true;
        piece.spin = spin_type(board, piece);
        return true;
    }

    // For an actual kick, v19 snaps client Y to floor(y) + 0.1 first.
    let client_y = 39.0 - piece.y;
    let snapped_bottom = 39.0 - (client_y.floor() + 0.1);
    for (index, &(dx, dy)) in kick_list(piece.kind, piece.rotation, target)
        .iter()
        .enumerate()
    {
        if dx == 0 && dy == 0 {
            continue;
        }
        let mut trial = *piece;
        trial.rotation = target;
        trial.x += dx;
        trial.y = snapped_bottom + dy as f64;
        if !collides(board, &trial) {
            piece.rotation = target;
            piece.x = trial.x;
            piece.y = trial.y;
            piece.last_kick = index;
            piece.rotation_active = true;
            piece.spin = spin_type(board, piece);
            return true;
        }
    }
    false
}

fn immobile(board: &EngineBoard, piece: &ActivePiece) -> bool {
    [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().all(|&(dx, dy)| {
        let mut trial = *piece;
        trial.x += dx;
        trial.y += dy as f64;
        collides(board, &trial)
    })
}

/// `all-mini+` spin detection (the only spin rule ported).
fn spin_type(board: &EngineBoard, piece: &ActivePiece) -> Spin {
    if !piece.rotation_active {
        return Spin::None;
    }
    if piece.kind != Piece::T {
        return if immobile(board, piece) {
            Spin::Mini
        } else {
            Spin::None
        };
    }
    let base_y = floor_cell(piece.y);
    let (x, y) = (piece.x, base_y);
    let bl = board.occupied(x - 1, y - 1);
    let br = board.occupied(x + 1, y - 1);
    let tl = board.occupied(x - 1, y + 1);
    let tr = board.occupied(x + 1, y + 1);
    let fronts = match piece.rotation % 4 {
        0 => [tl, tr],
        1 => [tr, br],
        2 => [bl, br],
        _ => [tl, bl],
    };
    let occupied = [bl, br, tl, tr].iter().filter(|c| **c).count();
    let front_count = fronts.iter().filter(|c| **c).count();
    if occupied >= 3 {
        // Index 4 in a list with the identity at 0 is the client's kick 3.
        if front_count == 2 || piece.last_kick == 4 {
            return Spin::Full;
        }
        return Spin::Mini;
    }
    if immobile(board, piece) {
        Spin::Mini
    } else {
        Spin::None
    }
}

/// Player handling settings, in frames (SDF is a factor; 41 is instant).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Handling {
    pub arr: f64,
    pub das: f64,
    pub dcd: f64,
    pub sdf: f64,
    pub cancel: bool,
    pub safelock: bool,
    pub may20g: bool,
}

impl Default for Handling {
    fn default() -> Self {
        Self {
            arr: 0.0,
            das: 0.0,
            dcd: 0.0,
            sdf: SDF_INSTANT,
            cancel: false,
            safelock: false,
            may20g: true,
        }
    }
}

/// Game options the solo engine honours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GameOptions {
    /// Base gravity in cells per frame.
    pub gravity: f64,
    /// Gravity increase per second (at 60 FPS) after `gravity_margin`.
    pub gravity_increase: f64,
    /// Frames before the gravity ramp starts.
    pub gravity_margin: f64,
    pub gravity_may20g: bool,
    pub lock_delay: f64,
    pub lock_reset_limit: u32,
    pub clutch: bool,
}

impl Default for GameOptions {
    fn default() -> Self {
        Self {
            gravity: 0.02,
            gravity_increase: 0.0,
            gravity_margin: 0.0,
            gravity_may20g: true,
            lock_delay: 30.0,
            lock_reset_limit: 15,
            clutch: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    MoveLeft,
    MoveRight,
    SoftDrop,
    HardDrop,
    RotateCw,
    RotateCcw,
    Rotate180,
    Hold,
}

impl Key {
    /// Parses TETR.IO's replay key names.
    pub fn from_replay_name(name: &str) -> Option<Self> {
        Some(match name {
            "moveLeft" => Self::MoveLeft,
            "moveRight" => Self::MoveRight,
            "softDrop" => Self::SoftDrop,
            "hardDrop" => Self::HardDrop,
            "rotateCW" => Self::RotateCw,
            "rotateCCW" => Self::RotateCcw,
            "rotate180" => Self::Rotate180,
            "hold" => Self::Hold,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ShiftState {
    held: bool,
    das: f64,
    arr: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

impl Side {
    fn direction(self) -> i32 {
        match self {
            Self::Left => -1,
            Self::Right => 1,
        }
    }
}

/// One locked piece and the state right after it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementRecord {
    /// Replay time of the lock in tenths of a frame.
    pub subframe: i64,
    pub piece: Piece,
    pub rotation: u8,
    pub x: i32,
    pub y: f64,
    pub used_hold: bool,
    pub hold: Option<Piece>,
    pub spin: Spin,
    pub kick_index: usize,
    pub lines_cleared: u32,
    pub garbage_received: usize,
    /// Occupancy after the lock, line clears, and any scheduled garbage.
    pub rows: Vec<u16>,
}

/// Initial state of a game.
#[derive(Debug, Clone)]
pub struct EngineSetup {
    pub seed: i64,
    pub handling: Handling,
    pub options: GameOptions,
    pub board: EngineBoard,
    pub hold: Option<Piece>,
    pub hold_locked: bool,
}

impl EngineSetup {
    pub fn new(seed: i64) -> Self {
        Self {
            seed,
            handling: Handling::default(),
            options: GameOptions::default(),
            board: EngineBoard::default(),
            hold: None,
            hold_locked: false,
        }
    }
}

/// The solo game state machine. Time is measured in tenths of a frame, the
/// resolution TETR.IO serializes subframes with.
#[derive(Debug, Clone)]
pub struct Engine {
    handling: Handling,
    options: GameOptions,
    board: EngineBoard,
    pieces: PieceStream,
    active: ActivePiece,
    hold: Option<Piece>,
    hold_locked: bool,
    used_hold_this_piece: bool,
    last_was_clear: bool,
    left: ShiftState,
    right: ShiftState,
    held_soft: bool,
    last_shift_dir: i32,
    hit_wall: bool,
    gravity: f64,
    lock_elapsed: f64,
    lock_resets: u32,
    rot_resets: u32,
    hy_client: f64,
    safelock_remaining: u32,
    last_time: i64,
    lines_cleared: u32,
    holds: u32,
    placements: Vec<PlacementRecord>,
    garbage_schedule: BTreeMap<usize, Vec<u8>>,
}

impl Engine {
    pub fn new(setup: EngineSetup) -> Self {
        let mut pieces = PieceStream::new(setup.seed);
        let first = pieces.pop();
        Self {
            handling: setup.handling,
            options: setup.options,
            board: setup.board,
            pieces,
            active: ActivePiece::spawn(first),
            hold: setup.hold,
            hold_locked: setup.hold_locked,
            used_hold_this_piece: false,
            last_was_clear: false,
            left: ShiftState::default(),
            right: ShiftState::default(),
            held_soft: false,
            last_shift_dir: -1,
            hit_wall: false,
            gravity: setup.options.gravity,
            lock_elapsed: 0.0,
            lock_resets: 0,
            rot_resets: 0,
            hy_client: 18.0,
            safelock_remaining: 0,
            last_time: 0,
            lines_cleared: 0,
            holds: 0,
            placements: Vec::new(),
            garbage_schedule: BTreeMap::new(),
        }
    }

    pub fn board(&self) -> &EngineBoard {
        &self.board
    }

    pub fn active(&self) -> &ActivePiece {
        &self.active
    }

    pub fn hold(&self) -> Option<Piece> {
        self.hold
    }

    /// Overrides HOLD; used for TETR.IO's terminal hold-topout snapshot.
    pub fn set_hold(&mut self, hold: Option<Piece>) {
        self.hold = hold;
    }

    pub fn hold_locked(&self) -> bool {
        self.hold_locked
    }

    pub fn next_pieces(&mut self, count: usize) -> Vec<Piece> {
        self.pieces.preview(count)
    }

    pub fn placements(&self) -> &[PlacementRecord] {
        &self.placements
    }

    pub fn lines_cleared(&self) -> u32 {
        self.lines_cleared
    }

    /// Successful Hold presses.
    pub fn holds(&self) -> u32 {
        self.holds
    }

    pub fn time(&self) -> i64 {
        self.last_time
    }

    pub fn gravity(&self) -> f64 {
        self.gravity
    }

    /// Landing position of the active piece (the ghost).
    pub fn ghost(&self) -> ActivePiece {
        let mut ghost = self.active;
        hard_drop(&self.board, &mut ghost);
        ghost
    }

    /// Adds garbage rows right after placement `index` (0-based), provided it
    /// clears no lines, before the next piece spawns. This is where TETR.IO's
    /// instant garbage entry tanks rows.
    pub fn schedule_garbage(&mut self, index: usize, holes: Vec<u8>) {
        self.garbage_schedule
            .entry(index)
            .or_default()
            .extend(holes);
    }

    fn reset_spawn(&mut self) {
        self.active.x = SPAWN_X;
        self.active.y = SPAWN_Y;
        self.active.rotation = 0;
        self.active.last_kick = 0;
        self.active.rotation_active = false;
        self.active.spin = Spin::None;
        self.lock_elapsed = 0.0;
        self.lock_resets = 0;
        self.rot_resets = 0;
        self.hy_client = 18.0;
        self.hold_locked = false;
        self.used_hold_this_piece = false;
        if collides(&self.board, &self.active) && self.last_was_clear && self.options.clutch {
            // ConsiderBlockout walks the spawned piece upward to the first legal
            // position, but only after the previous lock cleared a line.
            while collides(&self.board, &self.active) && self.active.y < (HEIGHT + 4) as f64 {
                self.active.y += 1.0;
            }
        }
        if !collides(&self.board, &self.active) && self.is_20g() {
            self.slam_to_floor();
        }
    }

    /// `_InternalDCD`: rewinds both DAS charges only after a wall hit.
    fn apply_dcd(&mut self) {
        if !self.hit_wall || self.handling.dcd <= 0.0 {
            return;
        }
        let target = self.handling.das - self.handling.dcd;
        for shift in [&mut self.left, &mut self.right] {
            shift.das = shift.das.min(target);
            shift.arr = self.handling.arr;
        }
    }

    fn spawn_next(&mut self) {
        self.apply_dcd();
        self.active = ActivePiece::spawn(self.pieces.pop());
        self.hit_wall = false;
        self.reset_spawn();
    }

    fn do_hold(&mut self) {
        if self.hold_locked {
            return;
        }
        self.apply_dcd();
        self.holds += 1;
        let old = self.active.kind;
        let next = match self.hold.replace(old) {
            None => self.pieces.pop(),
            Some(swap) => swap,
        };
        self.active = ActivePiece::spawn(next);
        self.hit_wall = false;
        self.reset_spawn();
        self.hold_locked = true;
        self.used_hold_this_piece = true;
    }

    fn record_manipulation(&mut self, rotation: bool) {
        // v19 counts movement resets even while airborne; falling below the
        // historical `hy` resets them again.
        if self.lock_resets < 31 {
            self.lock_resets += 1;
        }
        if rotation && self.rot_resets < 63 {
            self.rot_resets += 1;
        }
        if self.lock_resets < self.options.lock_reset_limit {
            self.lock_elapsed = 0.0;
        }
    }

    fn fall_active(&mut self, distance: f64, clear_rotation_on_row_cross: bool) -> bool {
        let old_hy = self.hy_client;
        if !internal_fall(
            &self.board,
            &mut self.active,
            distance,
            clear_rotation_on_row_cross,
        ) {
            return false;
        }
        let client_y = 39.0 - self.active.y;
        if client_y > old_hy {
            self.hy_client = client_y.ceil();
            self.lock_resets = 0;
            self.rot_resets = 0;
        }
        true
    }

    fn is_20g(&self) -> bool {
        let gravity_20g = self.gravity >= 20.0;
        if self.held_soft {
            let allowed = self.handling.may20g || (gravity_20g && self.options.gravity_may20g);
            return (self.handling.sdf == SDF_INSTANT || self.gravity * self.handling.sdf >= 20.0)
                && allowed;
        }
        gravity_20g && self.options.gravity_may20g
    }

    fn slam_to_floor(&mut self) {
        while self.fall_active(1.0, false) {}
    }

    fn internal_shift(&mut self, direction: i32) -> bool {
        let mut trial = self.active;
        trial.x += direction;
        if collides(&self.board, &trial) {
            self.hit_wall = true;
            return false;
        }
        self.active.x += direction;
        self.active.last_kick = 0;
        self.active.rotation_active = false;
        self.active.spin = Spin::None;
        self.hit_wall = false;
        self.record_manipulation(false);
        // At effective 20G every successful step slams again before the next
        // ARR=0 repeat, which matters for floor traversal under SDF=41.
        if self.is_20g() {
            self.slam_to_floor();
        }
        if self.lock_resets < self.options.lock_reset_limit {
            self.lock_elapsed = 0.0;
        }
        true
    }

    fn shift_mut(&mut self, side: Side) -> &mut ShiftState {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    fn process_shift(&mut self, side: Side, delta_frames: f64) {
        let (das, arr) = (self.handling.das, self.handling.arr);
        let last_shift_dir = self.last_shift_dir;
        let shift = self.shift_mut(side);
        if !shift.held || last_shift_dir != side.direction() {
            return;
        }
        // Only the part of this interval after DAS charges counts toward ARR.
        let arr_delta = (delta_frames - (das - shift.das).max(0.0)).max(0.0);
        shift.das = (shift.das + delta_frames).min(das);
        if shift.das < das {
            return;
        }
        shift.arr += arr_delta;
        if shift.arr < arr {
            return;
        }
        let repeats = if arr == 0.0 {
            WIDTH
        } else {
            (shift.arr / arr).floor() as usize
        };
        if arr != 0.0 {
            shift.arr -= arr * repeats as f64;
        }
        for _ in 0..repeats {
            self.internal_shift(side.direction());
        }
    }

    /// The v19 per-frame gravity ramp; constant within one frame.
    fn update_gravity_for_frame(&mut self, frame: i64) {
        let elapsed = (frame as f64 - self.options.gravity_margin).max(0.0);
        self.gravity = self.options.gravity + self.options.gravity_increase * elapsed / 60.0;
    }

    /// One client `_ProcessSubframe` (or frame-tail) interval. Movement is
    /// not integrated in fixed steps: Fall's legality probe is not additive.
    fn advance_physics(&mut self, delta_frames: f64, end_time: i64) -> Result<(), EngineError> {
        if delta_frames <= 0.0 {
            return Ok(());
        }
        // Fall() decrements safelock once per call, not per frame of delta.
        if self.safelock_remaining > 0 {
            self.safelock_remaining -= 1;
        }

        self.process_shift(Side::Left, delta_frames);
        self.process_shift(Side::Right, delta_frames);

        let mut fall_distance = self.gravity * delta_frames;
        if self.held_soft {
            if self.handling.sdf == SDF_INSTANT {
                fall_distance = 400.0 * delta_frames;
            } else {
                fall_distance *= self.handling.sdf;
                fall_distance = fall_distance.max(0.05 * self.handling.sdf);
            }
        }
        let rot_limit = self.options.lock_reset_limit + 15;
        if self.rot_resets > rot_limit {
            fall_distance += 0.5 * delta_frames * (self.rot_resets - rot_limit) as f64;
        }

        let mut remaining = fall_distance;
        let mut failed_fall = false;
        while remaining > 1e-12 {
            let step = remaining.min(1.0);
            if !self.fall_active(step, true) {
                failed_fall = true;
                break;
            }
            remaining -= step;
        }

        if failed_fall {
            self.lock_elapsed += delta_frames;
            let forced = self.lock_resets >= self.options.lock_reset_limit;
            if forced || self.lock_elapsed > self.options.lock_delay {
                if self.handling.safelock {
                    self.safelock_remaining = 7;
                }
                self.lock(end_time, true)?;
            }
        } else if !grounded(&self.board, &self.active) {
            self.lock_elapsed = 0.0;
        }
        Ok(())
    }

    /// Runs physics up to `target` (tenths of a frame), finishing each frame
    /// with one frame-tail interval exactly as the client's main loop does.
    pub fn advance_to(&mut self, target: i64) -> Result<(), EngineError> {
        if target < self.last_time {
            return Err(EngineError::TimeBackwards {
                target,
                current: self.last_time,
            });
        }
        self.update_gravity_for_frame(self.last_time.div_euclid(10));
        while self.last_time < target {
            let current_frame = self.last_time.div_euclid(10);
            self.update_gravity_for_frame(current_frame);
            let current_tick = self.last_time.rem_euclid(10);
            if current_frame == target.div_euclid(10) {
                let delta_ticks = target.rem_euclid(10) - current_tick;
                self.advance_physics(delta_ticks as f64 / 10.0, target)?;
                self.last_time = target;
                break;
            }
            let frame_end = (current_frame + 1) * 10;
            self.advance_physics((10 - current_tick) as f64 / 10.0, frame_end)?;
            self.last_time = frame_end;
        }
        Ok(())
    }

    fn lock(&mut self, event_time: i64, automatic: bool) -> Result<(), EngineError> {
        if automatic && !grounded(&self.board, &self.active) {
            return Ok(());
        }
        let mut piece = self.active;
        if !automatic {
            hard_drop(&self.board, &mut piece);
            self.active = piece;
        }
        let spin = if piece.rotation_active {
            piece.spin
        } else {
            Spin::None
        };
        self.board.lock(&piece)?;
        let (cleared, _garbage_cleared) = self.board.clear_full_rows();
        self.lines_cleared += cleared;
        self.last_was_clear = cleared > 0;

        // An automatic lock at an exact frame boundary belongs to the frame
        // whose tail ran it.
        let replay_time = if automatic && event_time % 10 == 0 {
            event_time - 10
        } else {
            event_time
        };
        let index = self.placements.len();
        let mut garbage_received = 0;
        if let Some(holes) = self.garbage_schedule.remove(&index) {
            if cleared > 0 {
                return Err(EngineError::GarbageOnClear(index));
            }
            self.board.add_garbage(&holes)?;
            garbage_received = holes.len();
        }
        self.placements.push(PlacementRecord {
            subframe: replay_time,
            piece: piece.kind,
            rotation: piece.rotation,
            x: piece.x,
            y: piece.y,
            used_hold: self.used_hold_this_piece,
            hold: self.hold,
            spin,
            kick_index: piece.last_kick,
            lines_cleared: cleared,
            garbage_received,
            rows: self.board.occupancy_masks(),
        });
        self.spawn_next();
        Ok(())
    }

    fn activate_shift(&mut self, side: Side, hoisted: bool) {
        let (das, dcd, arr) = (self.handling.das, self.handling.dcd, self.handling.arr);
        let shift = self.shift_mut(side);
        shift.held = true;
        shift.das = if hoisted { das - dcd } else { 0.0 };
        // Engine version >= 12 primes ARR on key activation.
        shift.arr = arr;
        self.last_shift_dir = side.direction();
    }

    fn rotate(&mut self, delta: u8) {
        if try_rotate(&self.board, &mut self.active, delta) {
            self.record_manipulation(true);
            self.apply_dcd();
            if self.lock_resets < self.options.lock_reset_limit {
                self.lock_elapsed = 0.0;
            }
        }
    }

    /// A key press at time `at` (tenths of a frame). Call `advance_to(at)`
    /// first. `hoisted` marks a key held through the previous piece.
    pub fn key_down(&mut self, key: Key, at: i64, hoisted: bool) -> Result<(), EngineError> {
        match key {
            Key::MoveLeft => {
                self.activate_shift(Side::Left, hoisted);
                self.internal_shift(-1);
            }
            Key::MoveRight => {
                self.activate_shift(Side::Right, hoisted);
                self.internal_shift(1);
            }
            // Only flips the input; Fall() moves the piece afterwards.
            Key::SoftDrop => self.held_soft = true,
            Key::RotateCw => self.rotate(1),
            Key::RotateCcw => self.rotate(3),
            Key::Rotate180 => self.rotate(2),
            Key::Hold => self.do_hold(),
            Key::HardDrop => {
                if self.safelock_remaining == 0 {
                    self.lock(at, false)?;
                }
            }
        }
        Ok(())
    }

    pub fn key_up(&mut self, key: Key) {
        let arr = self.handling.arr;
        let cancel = self.handling.cancel;
        match key {
            Key::MoveLeft => {
                self.left.held = false;
                self.left.das = 0.0;
                if self.right.held {
                    self.last_shift_dir = 1;
                }
                if cancel {
                    self.right.arr = arr;
                    self.right.das = 0.0;
                }
            }
            Key::MoveRight => {
                self.right.held = false;
                self.right.das = 0.0;
                if self.left.held {
                    self.last_shift_dir = -1;
                }
                if cancel {
                    self.left.arr = arr;
                    self.left.das = 0.0;
                }
            }
            Key::SoftDrop => self.held_soft = false,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minstd_matches_reference_sequence() {
        // Park-Miller: 16807^n mod (2^31 - 1) starting from seed 1.
        let mut rng = MinStd::new(1);
        assert_eq!(rng.next_int(), 16_807);
        assert_eq!(rng.next_int(), 282_475_249);
        assert_eq!(rng.next_int(), 1_622_650_073);
    }

    #[test]
    fn every_bag_is_a_permutation() {
        let pieces = piece_sequence(1_002_385_019, 70);
        for bag in pieces.chunks(7) {
            let mut sorted = bag.to_vec();
            sorted.sort();
            assert_eq!(sorted, {
                let mut all = PIECE_ORDER.to_vec();
                all.sort();
                all
            });
        }
    }

    #[test]
    fn hard_drop_on_empty_board_lands_on_the_floor() {
        let mut engine = Engine::new(EngineSetup::new(12_345));
        let first = engine.active().kind;
        engine.key_down(Key::HardDrop, 0, false).unwrap();
        let placement = &engine.placements()[0];
        assert_eq!(placement.piece, first);
        let min_y = engine
            .board()
            .occupancy_masks()
            .iter()
            .position(|mask| *mask != 0)
            .unwrap();
        assert_eq!(min_y, 0);
    }

    #[test]
    fn round6_and_py_mod_follow_python() {
        assert_eq!(round6(0.0078125), 0.007812);
        assert_eq!(round6(1.0000005), 1.000001);
        assert_eq!(py_mod(-0.25, 1.0), 0.75);
        assert_eq!(py_mod(2.5, 1.0), 0.5);
    }
}
