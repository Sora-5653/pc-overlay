use std::path::PathBuf;

use pc_overlay_core::{
    DiagramSet, ExpectedCellStatus, OverlayPlan, RuleSet, evaluate_setup_progress,
    model::empty_board,
};

#[test]
fn source_setup_geometry_can_be_attached_without_promoting_review_status() {
    let rules_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/ijlo.json");
    let diagrams_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/ijlo/diagram_cells.json");
    let mut rules = RuleSet::load(rules_path).unwrap();
    let diagrams = DiagramSet::load(diagrams_path).unwrap();
    rules.attach_setup_diagrams(&diagrams).unwrap();

    let setup = rules
        .setup("ijlo_source_method", "ijlo_sandbox_extended")
        .expect("unreviewed setup exists");
    assert_eq!(setup.placements.len(), 4);
    assert_eq!(format!("{:?}", setup.review_status), "ReviewRequired");

    let progress = evaluate_setup_progress(&setup.placements, &empty_board());
    assert_eq!(progress.cells.len(), 16);
    assert_eq!(
        progress
            .cells
            .iter()
            .filter(|cell| cell.status == ExpectedCellStatus::Missing)
            .count(),
        16
    );
    let plan = OverlayPlan::from_progress(&progress);
    assert_eq!(plan.dim_cells.len(), 16);
    assert!(plan.warning_cells.is_empty());
}
