//! Checks the fetched Setup Konbini cache (`cargo run --bin fetch-konbini`).
//! Skipped when the cache is absent, as the data is not part of the repo.

use std::path::Path;

use pc_overlay_core::{
    KonbiniLibrary, Piece,
    finder::{ALL_PIECES, Buildable, Finder},
};

#[test]
fn fetched_setups_decode_to_whole_tetrominoes() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/konbini");
    let Ok(library) = KonbiniLibrary::load_dir(&dir) else {
        eprintln!("skipped: no Setup Konbini cache in {}", dir.display());
        return;
    };
    assert!(library.issues.is_empty(), "{:?}", library.issues);
    let mut odd = Vec::new();
    for setup in &library.setups {
        for piece in [
            None,
            Some(Piece::I),
            Some(Piece::J),
            Some(Piece::L),
            Some(Piece::O),
            Some(Piece::S),
            Some(Piece::T),
            Some(Piece::Z),
        ] {
            let count = setup
                .cells
                .iter()
                .filter(|cell| cell.piece == piece)
                .count();
            if count % 4 != 0 {
                odd.push(format!("{} {} {piece:?}={count}", setup.stage, setup.id));
            }
        }
    }
    for stage in 1..=7 {
        assert!(library.stage_count(stage) > 0, "stage {stage} is empty");
    }
    assert!(
        odd.is_empty(),
        "{} setups with partial pieces: {odd:?}",
        odd.len()
    );
}

fn orders(pieces: &[Piece]) -> Vec<Vec<Piece>> {
    if pieces.len() <= 1 {
        return vec![pieces.to_vec()];
    }
    let mut all = Vec::new();
    for index in 0..pieces.len() {
        let mut rest = pieces.to_vec();
        let first = rest.remove(index);
        for mut tail in orders(&rest) {
            tail.insert(0, first);
            all.push(tail);
        }
    }
    all.sort();
    all.dedup();
    all
}

/// Every coloured setup of the sheets is buildable in some order of its own
/// pieces (plus one spare) with SRS+ and HOLD. A setup that is never
/// buildable would point at a wrong kick table or search.
#[test]
fn every_coloured_setup_has_a_buildable_order() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/konbini");
    let Ok(library) = KonbiniLibrary::load_dir(&dir) else {
        eprintln!("skipped: no Setup Konbini cache in {}", dir.display());
        return;
    };
    let mut finder = Finder::new();
    let mut unbuildable = Vec::new();
    for setup in library
        .setups
        .iter()
        .filter(|setup| setup.cells.iter().all(|cell| cell.piece.is_some()))
    {
        let pieces: Vec<Piece> = ALL_PIECES
            .iter()
            .flat_map(|piece| {
                let cells = setup
                    .cells
                    .iter()
                    .filter(|cell| cell.piece == Some(*piece))
                    .count();
                std::iter::repeat_n(*piece, cells / 4)
            })
            .collect();
        let buildable = orders(&pieces).into_iter().any(|mut order| {
            order.push(Piece::I);
            finder.buildable(&setup.target, 0, None, &order, &[]) == Buildable::Always
        });
        if !buildable {
            unbuildable.push(format!("{} {} {}", setup.stage, setup.id, setup.fumen));
        }
    }
    assert!(unbuildable.is_empty(), "{unbuildable:?}");
}

/// The state's PC rate agrees with the sheet when the 2nd PC starts with the
/// case its ID names (HOLD and the bag's last three), and drops when one
/// case piece is swapped for another.
#[test]
fn state_pc_rate_matches_the_sheet_for_the_named_case() {
    use pc_overlay_core::{Observation, empty_board, konbini, piece_code};
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/konbini");
    let Ok(library) = KonbiniLibrary::load_dir(&dir) else {
        eprintln!("skipped: no Setup Konbini cache in {}", dir.display());
        return;
    };
    let piece = |code: char| {
        *ALL_PIECES
            .iter()
            .find(|piece| piece_code(**piece) == code)
            .unwrap()
    };
    let mut finder = Finder::new();
    let (mut checked, mut below, mut swapped_below) = (0, Vec::new(), 0);
    let setups = library
        .setups
        .iter()
        .filter(|setup| setup.stage == 2 && setup.sol_percent == Some(100.0))
        .filter(|setup| setup.id.len() == 4 && setup.id.chars().all(|c| "IJLOSTZ".contains(c)))
        .take(40);
    for setup in setups {
        let case: Vec<Piece> = setup.id.chars().map(piece).collect();
        // The next bag in one fixed order: Current is case[1] at index 11.
        let bag = [
            Piece::S,
            Piece::Z,
            Piece::I,
            Piece::O,
            Piece::T,
            Piece::L,
            Piece::J,
        ];
        let rate = |finder: &mut Finder, case: &[Piece]| {
            let observation = Observation {
                board: empty_board(),
                hold: Some(case[0]),
                current: case[1],
                next5: [case[2], case[3], bag[0], bag[1], bag[2]],
            };
            let progress = setup.progress(&observation.board)?;
            konbini::state_pc_rate(finder, setup, &progress, &observation, 11)
        };
        let Some(named) = rate(&mut finder, &case) else {
            continue;
        };
        checked += 1;
        if named < 100.0 {
            below.push(format!("{} {named:.1}%", setup.id));
        }
        // The same setup with the HOLD piece swapped for one it does not use.
        let other = ALL_PIECES
            .iter()
            .copied()
            .find(|piece| !case.contains(piece))
            .unwrap();
        let mut swapped = case.clone();
        swapped[0] = other;
        if rate(&mut finder, &swapped).is_some_and(|rate| rate < 100.0) {
            swapped_below += 1;
        }
    }
    eprintln!("checked {checked}, below 100%: {below:?}, swapped below 100%: {swapped_below}");
    assert!(checked >= 20, "only {checked} setups checked");
    assert!(
        below.len() * 10 <= checked,
        "named cases below 100%: {below:?}"
    );
    assert!(
        swapped_below * 2 >= checked,
        "swapped below 100%: {swapped_below}/{checked}"
    );
}
