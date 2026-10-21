//! Checks the fetched Setup Konbini cache (`cargo run --bin fetch-konbini`).
//! Skipped when the cache is absent, as the data is not part of the repo.

use std::path::Path;

use pc_overlay_core::{KonbiniLibrary, Piece};

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
