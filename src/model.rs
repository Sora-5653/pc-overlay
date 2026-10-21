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
    fn mirror_is_an_involution() {
        let placement = Placement {
            piece: Piece::J,
            cells: vec![Cell { x: 0, y: 1 }, Cell { x: 1, y: 1 }],
        };
        assert_eq!(placement.mirrored().mirrored(), placement);
    }
}
