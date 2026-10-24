use serde::{Deserialize, Serialize};

pub const BOARD_WIDTH: usize = 10;
pub const BOARD_HEIGHT: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Piece {
    I,
    J,
    L,
    O,
    S,
    T,
    Z,
}

impl Piece {
    /// Mirrors a piece under the normalization allowed by the specification.
    pub const fn mirrored(self) -> Self {
        match self {
            Self::J => Self::L,
            Self::L => Self::J,
            Self::S => Self::Z,
            Self::Z => Self::S,
            piece => piece,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Cell {
    pub x: u8,
    pub y: u8,
}

impl Cell {
    pub const fn mirrored(self) -> Self {
        Self {
            x: (BOARD_WIDTH as u8 - 1) - self.x,
            y: self.y,
        }
    }

    pub const fn in_bounds(self) -> bool {
        self.x < BOARD_WIDTH as u8 && self.y < BOARD_HEIGHT as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub piece: Piece,
    pub cells: Vec<Cell>,
}

impl Placement {
    pub fn mirrored(&self) -> Self {
        let mut cells: Vec<_> = self.cells.iter().copied().map(Cell::mirrored).collect();
        cells.sort_by_key(|cell| (cell.y, cell.x));
        Self {
            piece: self.piece.mirrored(),
            cells,
        }
    }
}

/// Fixed cells only. Ghost and active-piece cells must be removed by recognition.
pub type Board = [[Option<Piece>; BOARD_WIDTH]; BOARD_HEIGHT];

pub fn empty_board() -> Board {
    [[None; BOARD_WIDTH]; BOARD_HEIGHT]
}

pub fn board_is_empty(board: &Board) -> bool {
    board.iter().flatten().all(Option::is_none)
}

/// A fully recognized stable observation. Partial/low-confidence frames never
/// enter this type and are rejected by the recognition boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub board: Board,
    pub hold: Option<Piece>,
    pub current: Piece,
    pub next5: [Piece; 5],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PcOrdinal(u8);

impl PcOrdinal {
    pub fn new(value: u8) -> Option<Self> {
        (1..=7).contains(&value).then_some(Self(value))
    }

    /// The PC being built after `pieces` placements, assuming every PC of the
    /// loop took 10 pieces: 0-9 is the 1st, 10-19 the 2nd, 70-79 the 1st again.
    pub const fn from_pieces_placed(pieces: u32) -> Self {
        Self(((pieces / 10) % 7) as u8 + 1)
    }

    /// The PC that starts after `pieces` placements, when every PC took a
    /// multiple of 5 pieces (10 for 4 lines, 5 for a 2-line PC). The 7-bag
    /// position (`pieces mod 7`) decides it: the k-th PC of a 10-piece loop
    /// starts at 3(k - 1) mod 7, so k - 1 = 5 * position mod 7 (5 is the
    /// inverse of 3). `None` when `pieces` cannot be a PC start.
    pub const fn at_pc_start(pieces: u32) -> Option<Self> {
        if !pieces.is_multiple_of(5) {
            return None;
        }
        Some(Self(((pieces % 7) * 5 % 7) as u8 + 1))
    }

    /// The PC after this one, which took `placed` pieces. A 2-line PC
    /// (5 pieces) moves the 7-bag as far as four 4-line PCs, so it advances
    /// the number by 4. Counts that are not a multiple of 5 advance by one.
    pub const fn after_pieces(self, placed: usize) -> Self {
        if placed == 0 || !placed.is_multiple_of(5) {
            return self.advance();
        }
        let position = (3 * (self.0 as usize - 1) + placed) % 7;
        Self((position * 5 % 7) as u8 + 1)
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub const fn advance(self) -> Self {
        if self.0 == 7 {
            Self(1)
        } else {
            Self(self.0 + 1)
        }
    }
}

impl Default for PcOrdinal {
    fn default() -> Self {
        Self(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinal_cycles_after_seventh() {
        let mut ordinal = PcOrdinal::default();
        for expected in 2..=7 {
            ordinal = ordinal.advance();
            assert_eq!(ordinal.get(), expected);
        }
        assert_eq!(ordinal.advance().get(), 1);
    }

    #[test]
    fn ordinal_from_pieces_placed_counts_ten_per_pc() {
        let ordinal = |pieces| PcOrdinal::from_pieces_placed(pieces).get();
        assert_eq!(ordinal(0), 1);
        assert_eq!(ordinal(4), 1);
        assert_eq!(ordinal(10), 2);
        assert_eq!(ordinal(69), 7);
        assert_eq!(ordinal(70), 1);
        assert_eq!(ordinal(125), 6);
    }

    #[test]
    fn ordinal_at_pc_start_follows_the_bag_position() {
        let at = |pieces| PcOrdinal::at_pc_start(pieces).map(PcOrdinal::get);
        // Every 10-piece PC start agrees with the 10-per-PC count.
        for pieces in (0..210).step_by(10) {
            assert_eq!(
                at(pieces),
                Some(PcOrdinal::from_pieces_placed(pieces).get())
            );
        }
        // A 2-line PC first: 5 pieces is where the 5th PC starts.
        assert_eq!(at(5), Some(5));
        assert_eq!(at(15), Some(6));
        assert_eq!(at(7), None);
    }

    #[test]
    fn a_two_line_pc_advances_the_ordinal_by_four() {
        let after = |ordinal, placed| PcOrdinal::new(ordinal).unwrap().after_pieces(placed).get();
        assert_eq!(after(1, 5), 5);
        assert_eq!(after(2, 5), 6);
        assert_eq!(after(5, 5), 2);
        assert_eq!(after(7, 5), 4);
        for ordinal in 1..=7 {
            assert_eq!(
                after(ordinal, 10),
                PcOrdinal::new(ordinal).unwrap().advance().get()
            );
            // An unknown or odd count keeps the one-step count.
            assert_eq!(
                after(ordinal, 0),
                PcOrdinal::new(ordinal).unwrap().advance().get()
            );
            assert_eq!(
                after(ordinal, 9),
                PcOrdinal::new(ordinal).unwrap().advance().get()
            );
        }
    }

    #[test]
    fn mirror_is_an_involution() {
        let placement = Placement {
            piece: Piece::J,
            cells: vec![Cell { x: 0, y: 1 }, Cell { x: 1, y: 1 }],
        };
        assert_eq!(placement.mirrored().mirrored(), placement);
    }
}
