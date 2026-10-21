//! Decoder for the first page of a fumen (`v115@...`) string.
//!
//! Setup Konbini stores every setup as a fumen. Only the field of the first
//! page is read: later pages, piece operations, and comments are ignored.
//! The format follows knewjade's tetris-fumen (v115): the field is 24 rows of
//! 10 cells, the 23 playfield rows from the top followed by one garbage row,
//! run-length encoded as (value, count) pairs in a 64-character alphabet.

use thiserror::Error;

use crate::model::{BOARD_HEIGHT, Cell, Piece};

const FIELD_WIDTH: usize = 10;
/// Playfield rows above the garbage row.
const FIELD_TOP: usize = 23;
const FIELD_BLOCKS: usize = (FIELD_TOP + 1) * FIELD_WIDTH;
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// A filled cell of a fumen field. `piece` is `None` for a gray block, which
/// Setup Konbini uses for "any piece" cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FumenCell {
    pub cell: Cell,
    pub piece: Option<Piece>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FumenError {
    #[error("not a v115 fumen")]
    UnsupportedVersion,
    #[error("invalid fumen character {0:?}")]
    InvalidCharacter(char),
    #[error("fumen data ends inside the field")]
    Truncated,
    #[error("fumen field run overflows the field")]
    Overflow,
    #[error("invalid fumen block value {0}")]
    InvalidBlock(usize),
    #[error("filled cell above the 20-row board at x={x}, y={y}")]
    AboveBoard { x: usize, y: usize },
}

/// Decodes the first page's playfield, bottom-left origin, top to bottom.
/// The garbage row is dropped. Cells above the 20-row board are an error, as
/// they cannot be drawn on the overlay.
pub fn decode_first_page(fumen: &str) -> Result<Vec<FumenCell>, FumenError> {
    let data = fumen
        .trim()
        .strip_prefix("v115@")
        .ok_or(FumenError::UnsupportedVersion)?;
    // Long fumens are wrapped with '?' every 47 characters.
    let mut values = data.chars().filter(|ch| *ch != '?').map(|ch| {
        u8::try_from(ch)
            .ok()
            .and_then(|byte| ALPHABET.iter().position(|known| *known == byte))
            .ok_or(FumenError::InvalidCharacter(ch))
    });
    let mut next = || values.next().unwrap_or(Err(FumenError::Truncated));

    let mut field = [0usize; FIELD_BLOCKS];
    let mut index = 0;
    while index < FIELD_BLOCKS {
        let value = next()? + next()? * 64;
        // The first page has an empty previous field, so diff - 8 is the block.
        let diff = value / FIELD_BLOCKS;
        let count = value % FIELD_BLOCKS + 1;
        if index + count > FIELD_BLOCKS {
            return Err(FumenError::Overflow);
        }
        let block = diff.checked_sub(8).ok_or(FumenError::InvalidBlock(diff))?;
        if block > 8 {
            return Err(FumenError::InvalidBlock(block));
        }
        field[index..index + count].fill(block);
        index += count;
    }

    let mut cells = Vec::new();
    for (index, block) in field.iter().enumerate().take(FIELD_TOP * FIELD_WIDTH) {
        if *block == 0 {
            continue;
        }
        let x = index % FIELD_WIDTH;
        let y = FIELD_TOP - 1 - index / FIELD_WIDTH;
        if y >= BOARD_HEIGHT {
            return Err(FumenError::AboveBoard { x, y });
        }
        cells.push(FumenCell {
            cell: Cell {
                x: x as u8,
                y: y as u8,
            },
            piece: block_piece(*block),
        });
    }
    Ok(cells)
}

fn block_piece(block: usize) -> Option<Piece> {
    match block {
        1 => Some(Piece::I),
        2 => Some(Piece::L),
        3 => Some(Piece::O),
        4 => Some(Piece::Z),
        5 => Some(Piece::T),
        6 => Some(Piece::J),
        7 => Some(Piece::S),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(cells: &[FumenCell], x: u8, y: u8) -> Option<Option<Piece>> {
        cells
            .iter()
            .find(|cell| cell.cell == Cell { x, y })
            .map(|cell| cell.piece)
    }

    #[test]
    fn decodes_a_coloured_setup() {
        // Setup Konbini 1st PC, LOLLI "ILSZ".
        let cells = decode_first_page("v115@9gwhilFewhglAtGewhBtR4EewhAtR4PeAgH").unwrap();
        assert_eq!(cells.len(), 16);
        for y in 0..4 {
            assert_eq!(at(&cells, 0, y), Some(Some(Piece::I)));
        }
        for x in 1..4 {
            assert_eq!(at(&cells, x, 3), Some(Some(Piece::L)));
        }
        assert_eq!(at(&cells, 1, 2), Some(Some(Piece::L)));
        assert_eq!(at(&cells, 2, 2), Some(Some(Piece::Z)));
        for piece in [Piece::I, Piece::L, Piece::S, Piece::Z] {
            assert_eq!(
                cells
                    .iter()
                    .filter(|cell| cell.piece == Some(piece))
                    .count(),
                4,
                "{piece:?}"
            );
        }
    }

    #[test]
    fn gray_cells_have_no_piece() {
        // Setup Konbini 1st PC, NILGIRI "3-01" (drawn in gray only).
        let cells = decode_first_page("v115@9gC8GeD8FeB8HeC8GeJeAgH").unwrap();
        assert_eq!(cells.len(), 12);
        assert!(cells.iter().all(|cell| cell.piece.is_none()));
    }

    #[test]
    fn wrapped_fumens_ignore_question_marks() {
        let plain = decode_first_page("v115@9gwhilFewhglAtGewhBtR4EewhAtR4PeAgH").unwrap();
        let wrapped = decode_first_page("v115@9gwhilFewhglAtGe?whBtR4EewhAtR4PeAgH").unwrap();
        assert_eq!(plain, wrapped);
    }

    #[test]
    fn rejects_other_versions_and_bad_data() {
        assert_eq!(
            decode_first_page("v110@abc"),
            Err(FumenError::UnsupportedVersion)
        );
        assert_eq!(decode_first_page("v115@9g"), Err(FumenError::Truncated));
        assert_eq!(
            decode_first_page("v115@9g*h"),
            Err(FumenError::InvalidCharacter('*'))
        );
    }
}
