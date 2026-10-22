//! Re-measures the Setup Konbini solve rate (`SOL`) of 2nd-PC setups with
//! the overlay's perfect-clear search, over every order of the next bag
//! with HOLD empty, as the sheet counts it (`CASES 5040/5040`).
//!
//! Compares the queue the overlay searched before 2026-10-22 (Current and
//! NEXT only: the first 6 pieces of the bag) with the full bag of 7, the 7th
//! being the piece the 7-bag still owes. `cargo run --release --example
//! konbini_sol_rate [-- --setups N]`. Needs `cargo run --bin fetch-konbini`.

use std::path::Path;

use pc_overlay_core::{
    KonbiniLibrary,
    finder::{ALL_PIECES, Finder, Supply, bit},
};

fn permutations(
    pieces: &mut Vec<pc_overlay_core::Piece>,
    at: usize,
    out: &mut Vec<Vec<pc_overlay_core::Piece>>,
) {
    if at == pieces.len() {
        out.push(pieces.clone());
        return;
    }
    for index in at..pieces.len() {
        pieces.swap(at, index);
        permutations(pieces, at + 1, out);
        pieces.swap(at, index);
    }
}

fn main() {
    let limit: usize = std::env::args()
        .skip_while(|arg| arg != "--setups")
        .nth(1)
        .map_or(5, |value| value.parse().unwrap());
    let library = KonbiniLibrary::load_dir(Path::new("data/konbini")).expect("fetched cache");
    let mut orders = Vec::new();
    permutations(&mut ALL_PIECES.to_vec(), 0, &mut orders);

    let setups = library
        .setups
        .iter()
        .filter(|setup| setup.stage == 2 && setup.sol_percent == Some(100.0))
        .filter(|setup| setup.cells.len() == 16)
        .take(limit);
    for setup in setups {
        let drawn = setup
            .cells
            .iter()
            .fold(0, |mask, cell| mask | bit(cell.cell.x, cell.cell.y));
        let field = setup.target.field(drawn);
        let mut finder = Finder::new();
        let (mut six, mut seven) = (0, 0);
        for order in &orders {
            let found = |finder: &mut Finder, queue: &[pc_overlay_core::Piece]| {
                finder
                    .perfect_clears(field, Supply::Queue { hold: None, queue }, 1)
                    .is_some()
            };
            six += usize::from(found(&mut finder, &order[..6]));
            seven += usize::from(found(&mut finder, order));
        }
        let rate = |count: usize| 100.0 * count as f64 / orders.len() as f64;
        println!(
            "{} {} sheet {:.2}% | NEXT only (6): {:.2}% | full bag (7): {:.2}%",
            setup.id,
            setup.source,
            setup.sol_percent.unwrap_or(0.0),
            rate(six),
            rate(seven)
        );
    }
}
