use std::path::PathBuf;

use pc_overlay_core::diagram::DiagramSet;

#[test]
fn bundled_ijlo_diagrams_have_valid_tetromino_cells() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/ijlo/diagram_cells.json");
    let diagrams = DiagramSet::load(path).expect("diagram transcription must remain valid");
    let report = diagrams.validate().expect("geometry has no hard errors");
    assert_eq!(report.diagram_count, 15);
    assert_eq!(report.placement_count, 56);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[test]
fn images_9_and_12_place_through_the_row_cleared_by_the_previous_stage() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/ijlo/diagram_cells.json");
    let diagrams = DiagramSet::load(path).expect("diagram transcription must remain valid");
    for image in ["image9", "image12"] {
        let diagram = diagrams.diagram(image).expect("diagram exists");
        assert_eq!(diagram.cleared_rows, vec![1], "{image}");
    }
}
