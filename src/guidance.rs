//! Board-independent guidance calculations for the future transparent overlay.
//!
//! The recognizer supplies fixed cells only. Active and ghost cells must be
//! excluded before these functions are called.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{Board, Cell, Piece, Placement};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedCellStatus {
    Correct,
    Missing,
    OccupiedByOtherPiece,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedCell {
    pub cell: Cell,
    pub expected: Piece,
    pub observed: Option<Piece>,
    pub status: ExpectedCellStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupProgress {
    pub cells: Vec<ExpectedCell>,
    pub unexpected_fixed_cells: Vec<Cell>,
    pub complete: bool,
}

impl SetupProgress {
    pub fn correct_cells(&self) -> impl Iterator<Item = Cell> + '_ {
        self.cells
            .iter()
            .filter(|cell| cell.status == ExpectedCellStatus::Correct)
            .map(|cell| cell.cell)
    }

    pub fn missing_cells(&self) -> impl Iterator<Item = Cell> + '_ {
        self.cells
            .iter()
            .filter(|cell| cell.status == ExpectedCellStatus::Missing)
            .map(|cell| cell.cell)
    }

    pub fn mismatched_cells(&self) -> impl Iterator<Item = Cell> + '_ {
        self.cells
            .iter()
            .filter(|cell| cell.status == ExpectedCellStatus::OccupiedByOtherPiece)
            .map(|cell| cell.cell)
    }
}

/// Compares a registered arrangement with fixed board cells. Empty expected
/// arrangements are treated as incomplete rather than complete, so a malformed
/// or unreviewed rule cannot silently suppress the overlay.
pub fn evaluate_setup_progress(expected: &[Placement], board: &Board) -> SetupProgress {
    let mut expected_cells = BTreeMap::<Cell, Piece>::new();
    for placement in expected {
        for cell in &placement.cells {
            expected_cells.insert(*cell, placement.piece);
        }
    }

    let mut cells = Vec::with_capacity(expected_cells.len());
    for (cell, expected_piece) in expected_cells {
        let observed = cell
            .in_bounds()
            .then(|| board[cell.y as usize][cell.x as usize])
            .flatten();
        let status = match observed {
            Some(piece) if piece == expected_piece => ExpectedCellStatus::Correct,
            Some(_) => ExpectedCellStatus::OccupiedByOtherPiece,
            None => ExpectedCellStatus::Missing,
        };
        cells.push(ExpectedCell {
            cell,
            expected: expected_piece,
            observed,
            status,
        });
    }

    let expected_positions: std::collections::BTreeSet<_> =
        cells.iter().map(|expected| expected.cell).collect();
    let mut unexpected_fixed_cells = Vec::new();
    for (y, row) in board.iter().enumerate() {
        for (x, piece) in row.iter().enumerate() {
            if piece.is_some() {
                let cell = Cell {
                    x: x as u8,
                    y: y as u8,
                };
                if !expected_positions.contains(&cell) {
                    unexpected_fixed_cells.push(cell);
                }
            }
        }
    }

    let complete = !cells.is_empty()
        && cells
            .iter()
            .all(|cell| cell.status == ExpectedCellStatus::Correct)
        && unexpected_fixed_cells.is_empty();

    SetupProgress {
        cells,
        unexpected_fixed_cells,
        complete,
    }
}

/// Solve frames taller than this are not enumerated; a 2nd PC spans four rows.
const MAX_SOLVE_ROWS: u8 = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SolveProgress {
    /// Every line-clear history that explains the board leads to the same
    /// remaining cells. `progress` is in board coordinates, with cleared rows
    /// already collapsed. Which frame rows cleared may differ between those
    /// histories, so only their number is reported. `setup_complete` is true
    /// once every setup piece is on the board, which is when the solve may
    /// be shown.
    InProgress {
        cleared_row_count: u8,
        setup_complete: bool,
        progress: SetupProgress,
    },
    Complete,
    /// Line-clear histories that explain the board disagree about what is
    /// left (for example an empty board before the setup or after the PC);
    /// nothing should be drawn.
    Ambiguous,
    /// The board is not a partial state of this setup plus solve.
    Inconsistent,
}

/// Locates a setup-plus-solve arrangement on the live board. Source diagrams
/// draw the whole PC without removing completed rows, while the game clears
/// each row as soon as it fills, so the frame rows that have already cleared
/// are inferred: every piece must be either fully placed or fully absent, a
/// cleared row must be entirely placed, a remaining row must not be, and the
/// board must contain nothing else.
pub fn evaluate_solve_progress(
    setup: &[Placement],
    solve: &[Placement],
    board: &Board,
) -> SolveProgress {
    let pieces: Vec<&Placement> = setup.iter().chain(solve).collect();
    let Some(height) = pieces
        .iter()
        .flat_map(|placement| &placement.cells)
        .map(|cell| cell.y + 1)
        .max()
    else {
        return SolveProgress::Inconsistent;
    };
    if height > MAX_SOLVE_ROWS
        || pieces.iter().any(|placement| {
            placement.cells.is_empty() || !placement.cells.iter().all(|cell| cell.in_bounds())
        })
    {
        return SolveProgress::Inconsistent;
    }

    let mut owner = BTreeMap::<Cell, usize>::new();
    for (index, placement) in pieces.iter().enumerate() {
        for cell in &placement.cells {
            if owner.insert(*cell, index).is_some() {
                return SolveProgress::Inconsistent;
            }
        }
    }
    let occupied_count = board.iter().flatten().filter(|cell| cell.is_some()).count();

    let mut matches = Vec::new();
    for mask in 0u32..(1 << height) {
        if let Some(found) = match_cleared_rows(
            &pieces,
            setup.len(),
            &owner,
            height,
            mask,
            board,
            occupied_count,
        ) && !matches.contains(&found)
        {
            matches.push(found);
        }
    }
    match matches.len() {
        0 => SolveProgress::Inconsistent,
        1 => matches.pop().expect("one match"),
        _ => SolveProgress::Ambiguous,
    }
}

#[allow(clippy::too_many_arguments)]
fn match_cleared_rows(
    pieces: &[&Placement],
    setup_len: usize,
    owner: &BTreeMap<Cell, usize>,
    height: u8,
    mask: u32,
    board: &Board,
    occupied_count: usize,
) -> Option<SolveProgress> {
    let cleared = |y: u8| mask & (1 << y) != 0;
    let collapse = |cell: Cell| Cell {
        x: cell.x,
        y: cell.y - (0..cell.y).filter(|row| cleared(*row)).count() as u8,
    };
    let observed = |cell: Cell| board[cell.y as usize][cell.x as usize];

    let mut placed = Vec::with_capacity(pieces.len());
    for placement in pieces {
        let remaining: Vec<Cell> = placement
            .cells
            .iter()
            .filter(|cell| !cleared(cell.y))
            .map(|cell| collapse(*cell))
            .collect();
        let present = remaining
            .iter()
            .filter(|cell| observed(**cell) == Some(placement.piece))
            .count();
        let is_placed = if present == remaining.len() {
            true
        } else if present == 0 && remaining.iter().all(|cell| observed(*cell).is_none()) {
            false
        } else {
            return None;
        };
        if !is_placed && remaining.len() != placement.cells.len() {
            return None;
        }
        placed.push(is_placed);
    }

    for y in 0..height {
        let row_full = (0..10).all(|x| {
            owner
                .get(&Cell { x, y })
                .is_some_and(|index| placed[*index])
        });
        if row_full != cleared(y) {
            return None;
        }
    }

    let placed_remaining: usize = pieces
        .iter()
        .zip(&placed)
        .filter(|(_, is_placed)| **is_placed)
        .map(|(placement, _)| {
            placement
                .cells
                .iter()
                .filter(|cell| !cleared(cell.y))
                .count()
        })
        .sum();
    if placed_remaining != occupied_count {
        return None;
    }

    if placed.iter().all(|is_placed| *is_placed) {
        return Some(SolveProgress::Complete);
    }
    let mut cells = Vec::new();
    for (placement, is_placed) in pieces.iter().zip(&placed) {
        for cell in placement.cells.iter().filter(|cell| !cleared(cell.y)) {
            let cell = collapse(*cell);
            cells.push(ExpectedCell {
                cell,
                expected: placement.piece,
                observed: observed(cell),
                status: if *is_placed {
                    ExpectedCellStatus::Correct
                } else {
                    ExpectedCellStatus::Missing
                },
            });
        }
    }
    cells.sort_by_key(|expected| expected.cell);
    Some(SolveProgress::InProgress {
        cleared_row_count: mask.count_ones() as u8,
        setup_complete: placed[..setup_len].iter().all(|is_placed| *is_placed),
        progress: SetupProgress {
            cells,
            unexpected_fixed_cells: Vec::new(),
            complete: false,
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayPlan {
    pub dim_cells: Vec<ExpectedCell>,
    pub warning_cells: Vec<ExpectedCell>,
    pub unexpected_fixed_cells: Vec<Cell>,
}

impl OverlayPlan {
    pub fn from_progress(progress: &SetupProgress) -> Self {
        Self {
            dim_cells: progress
                .cells
                .iter()
                .filter(|cell| cell.status == ExpectedCellStatus::Missing)
                .cloned()
                .collect(),
            warning_cells: progress
                .cells
                .iter()
                .filter(|cell| cell.status == ExpectedCellStatus::OccupiedByOtherPiece)
                .cloned()
                .collect(),
            unexpected_fixed_cells: progress.unexpected_fixed_cells.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Board, Piece, empty_board};

    fn placement(piece: Piece, cells: &[(u8, u8)]) -> Placement {
        Placement {
            piece,
            cells: cells.iter().map(|(x, y)| Cell { x: *x, y: *y }).collect(),
        }
    }

    #[test]
    fn progress_distinguishes_missing_mismatch_and_unexpected_cells() {
        let expected = vec![placement(Piece::I, &[(0, 0), (1, 0), (2, 0), (3, 0)])];
        let mut board = empty_board();
        board[0][0] = Some(Piece::I);
        board[0][1] = Some(Piece::T);
        board[1][8] = Some(Piece::O);

        let progress = evaluate_setup_progress(&expected, &board);
        assert_eq!(
            progress
                .cells
                .iter()
                .filter(|cell| cell.status == ExpectedCellStatus::Correct)
                .count(),
            1
        );
        assert_eq!(
            progress
                .cells
                .iter()
                .filter(|cell| cell.status == ExpectedCellStatus::Missing)
                .count(),
            2
        );
        assert_eq!(progress.mismatched_cells().count(), 1);
        assert_eq!(progress.unexpected_fixed_cells, vec![Cell { x: 8, y: 1 }]);
        assert!(!progress.complete);
    }

    #[test]
    fn complete_setup_has_no_overlay_cells() {
        let expected = vec![placement(Piece::O, &[(0, 0), (1, 0), (0, 1), (1, 1)])];
        let mut board = empty_board();
        for cell in &expected[0].cells {
            board[cell.y as usize][cell.x as usize] = Some(Piece::O);
        }
        let progress = evaluate_setup_progress(&expected, &board);
        let plan = OverlayPlan::from_progress(&progress);
        assert!(progress.complete);
        assert!(plan.dim_cells.is_empty());
        assert!(plan.warning_cells.is_empty());
    }

    /// Two-row PC: an O on the right, four horizontal I pieces on the left.
    fn two_row_frame() -> (Vec<Placement>, Vec<Placement>) {
        let setup = vec![placement(Piece::O, &[(8, 0), (9, 0), (8, 1), (9, 1)])];
        let solve = vec![
            placement(Piece::I, &[(0, 0), (1, 0), (2, 0), (3, 0)]),
            placement(Piece::I, &[(4, 0), (5, 0), (6, 0), (7, 0)]),
            placement(Piece::I, &[(0, 1), (1, 1), (2, 1), (3, 1)]),
            placement(Piece::I, &[(4, 1), (5, 1), (6, 1), (7, 1)]),
        ];
        (setup, solve)
    }

    fn fill(board: &mut Board, piece: Piece, cells: &[(usize, usize)]) {
        for (x, y) in cells {
            board[*y][*x] = Some(piece);
        }
    }

    fn missing(progress: &SolveProgress) -> Vec<Cell> {
        match progress {
            SolveProgress::InProgress { progress, .. } => progress.missing_cells().collect(),
            other => panic!("expected progress, got {other:?}"),
        }
    }

    #[test]
    fn solve_progress_before_any_line_clear() {
        let (setup, solve) = two_row_frame();
        let mut board = empty_board();
        fill(&mut board, Piece::O, &[(8, 0), (9, 0), (8, 1), (9, 1)]);
        let progress = evaluate_solve_progress(&setup, &solve, &board);
        assert!(matches!(
            &progress,
            SolveProgress::InProgress {
                cleared_row_count: 0,
                ..
            }
        ));
        assert_eq!(missing(&progress).len(), 16);
        assert!(matches!(
            &progress,
            SolveProgress::InProgress {
                setup_complete: true,
                ..
            }
        ));
    }

    #[test]
    fn solve_progress_reports_an_unfinished_setup() {
        // Two-piece setup where only the first piece is down: the board is a
        // valid partial state, but the solve must not be shown yet.
        let setup = vec![
            placement(Piece::O, &[(8, 0), (9, 0), (8, 1), (9, 1)]),
            placement(Piece::I, &[(0, 0), (1, 0), (2, 0), (3, 0)]),
        ];
        let solve = vec![
            placement(Piece::I, &[(4, 0), (5, 0), (6, 0), (7, 0)]),
            placement(Piece::I, &[(0, 1), (1, 1), (2, 1), (3, 1)]),
            placement(Piece::I, &[(4, 1), (5, 1), (6, 1), (7, 1)]),
        ];
        let mut board = empty_board();
        fill(&mut board, Piece::O, &[(8, 0), (9, 0), (8, 1), (9, 1)]);
        assert!(matches!(
            evaluate_solve_progress(&setup, &solve, &board),
            SolveProgress::InProgress {
                setup_complete: false,
                ..
            }
        ));
    }

    #[test]
    fn solve_progress_collapses_a_cleared_frame_row() {
        let (setup, solve) = two_row_frame();
        // One I row was completed and cleared; the O's other half remains.
        // Either I row could have been the one, but both leave the same cells.
        let mut board = empty_board();
        fill(&mut board, Piece::O, &[(8, 0), (9, 0)]);
        let progress = evaluate_solve_progress(&setup, &solve, &board);
        assert!(matches!(
            &progress,
            SolveProgress::InProgress {
                cleared_row_count: 1,
                ..
            }
        ));
        let cells = missing(&progress);
        assert_eq!(cells.len(), 8);
        assert!(cells.iter().all(|cell| cell.y == 0 && cell.x < 8));
    }

    #[test]
    fn solve_progress_rejects_partial_or_foreign_cells() {
        let (setup, solve) = two_row_frame();
        let mut board = empty_board();
        fill(&mut board, Piece::O, &[(8, 0), (9, 0), (8, 1), (9, 1)]);
        fill(&mut board, Piece::I, &[(0, 0), (1, 0)]);
        assert_eq!(
            evaluate_solve_progress(&setup, &solve, &board),
            SolveProgress::Inconsistent
        );

        let mut board = empty_board();
        fill(&mut board, Piece::O, &[(8, 0), (9, 0), (8, 1), (9, 1)]);
        fill(&mut board, Piece::T, &[(5, 3)]);
        assert_eq!(
            evaluate_solve_progress(&setup, &solve, &board),
            SolveProgress::Inconsistent
        );
    }

    #[test]
    fn empty_board_is_ambiguous_between_not_started_and_complete() {
        let (setup, solve) = two_row_frame();
        assert_eq!(
            evaluate_solve_progress(&setup, &solve, &empty_board()),
            SolveProgress::Ambiguous
        );
    }

    #[test]
    fn malformed_out_of_bounds_expected_cell_does_not_panic() {
        let expected = vec![placement(Piece::I, &[(10, 0), (10, 1), (10, 2), (10, 3)])];
        let progress = evaluate_setup_progress(&expected, &empty_board());
        assert!(!progress.complete);
        assert_eq!(progress.missing_cells().count(), 4);
    }
}
