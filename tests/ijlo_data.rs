use std::path::PathBuf;

use pc_overlay_core::{
    DiagramSet, EvalContext, EvalResult, Piece, RuleSet, empty_board,
    rules::{CandidateStatus, ReviewStatus},
};

use Piece::{I, J, L, O, S, T, Z};

fn bundled_rules() -> RuleSet {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut rules = RuleSet::load(root.join("data/ijlo.json")).expect("bundled IJLO data");
    let diagrams = DiagramSet::load(root.join("data/ijlo/diagram_cells.json")).unwrap();
    rules.attach_setup_diagrams(&diagrams).unwrap();
    rules
}

/// HOLD O at the PC start, so I/J/L are the case pieces in the queue.
fn context_for_bag(bag: &[Piece]) -> EvalContext {
    let history: Vec<Piece> = [I, J, L].iter().chain(bag).copied().collect();
    EvalContext::for_following_bag(&history, 3, Some(O), I)
}

fn strict_selection(bag: &[Piece]) -> Option<String> {
    bundled_rules()
        .select_for_board(&context_for_bag(bag), &empty_board())
        .selected
        .map(|selected| selected.setup_id)
}

fn candidate_status(bag: &[Piece], setup_id: &str) -> CandidateStatus {
    bundled_rules()
        .select_for_board(&context_for_bag(bag), &empty_board())
        .candidates
        .into_iter()
        .find(|candidate| candidate.setup_id == setup_id)
        .expect("candidate")
        .status
}

#[test]
fn queue_and_save_o_setups_are_verified_and_sandbox_is_not() {
    let rules = bundled_rules();
    let verified: Vec<_> = rules.methods[0]
        .setups
        .iter()
        .filter(|setup| setup.review_status == ReviewStatus::Verified)
        .map(|setup| setup.id.as_str())
        .collect();
    assert_eq!(
        verified,
        [
            "ijlo_s_queue",
            "ijlo_z_queue",
            "ijlo_save_o_left",
            "ijlo_save_o_right"
        ]
    );
    for setup in &rules.methods[0].setups {
        let expected = if verified.contains(&setup.id.as_str()) {
            ReviewStatus::Verified
        } else {
            ReviewStatus::ReviewRequired
        };
        assert!(
            setup
                .solves
                .iter()
                .all(|solve| solve.review_status == expected),
            "{}: solves are reviewed together with their setup",
            setup.id
        );
    }
}

#[test]
fn verified_solve_cells_match_the_reviewed_diagrams() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rules = RuleSet::load(root.join("data/ijlo.json")).unwrap();
    let diagrams = DiagramSet::load(root.join("data/ijlo/diagram_cells.json")).unwrap();
    for (setup_id, images) in [
        ("ijlo_s_queue", ["image5", "image6"]),
        ("ijlo_z_queue", ["image2", "image3"]),
    ] {
        let setup = rules.setup("ijlo_source_method", setup_id).unwrap();
        let expected: Vec<_> = images
            .iter()
            .flat_map(|image| &diagrams.diagram(image).unwrap().placements)
            .map(|placement| placement.as_placement())
            .collect();
        assert_eq!(setup.solves[0].target, expected, "{setup_id}");
    }
}

#[test]
fn verified_setup_cells_match_the_reviewed_diagrams() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rules = RuleSet::load(root.join("data/ijlo.json")).unwrap();
    let diagrams = DiagramSet::load(root.join("data/ijlo/diagram_cells.json")).unwrap();
    for (setup_id, image) in [("ijlo_s_queue", "image4"), ("ijlo_z_queue", "image1")] {
        let setup = rules.setup("ijlo_source_method", setup_id).unwrap();
        let expected: Vec<_> = diagrams
            .diagram(image)
            .unwrap()
            .placements
            .iter()
            .map(|placement| placement.as_placement())
            .collect();
        assert_eq!(setup.placements, expected, "{setup_id}");
    }
}

#[test]
fn case_pieces_and_the_next_bag_are_excluded_from_conditions() {
    // The IJLO case pieces and the T opening the bag after next must not count.
    let history = [I, J, L, S, T, Z, J, L, I, O, T, J];
    let context = EvalContext::for_following_bag(&history, 3, Some(O), I);
    assert_eq!(context.queue_history, [S, T, Z, J, L, I, O]);
    assert!(context.queue_closed);
}

#[test]
fn s_queue_fails_on_the_source_queue_tj_s() {
    for bag in [[T, J, S, Z, L, I, O], [J, T, S, Z, L, I, O]] {
        assert_eq!(
            candidate_status(&bag, "ijlo_s_queue"),
            CandidateStatus::FailConditionMatched
        );
        assert_eq!(strict_selection(&bag).as_deref(), Some("ijlo_z_queue"));
    }
}

#[test]
fn z_queue_fails_on_the_source_queue_tl_z() {
    for bag in [[T, L, Z, S, J, I, O], [L, T, Z, S, J, I, O]] {
        assert_eq!(
            candidate_status(&bag, "ijlo_z_queue"),
            CandidateStatus::FailConditionMatched
        );
        assert_eq!(strict_selection(&bag).as_deref(), Some("ijlo_s_queue"));
    }
}

#[test]
fn failure_also_applies_when_other_pieces_come_between() {
    // T is held, so J must be placed before S even with O in between.
    let bag = [T, O, J, S, Z, L, I];
    assert_eq!(
        candidate_status(&bag, "ijlo_s_queue"),
        CandidateStatus::FailConditionMatched
    );
    assert_eq!(strict_selection(&bag).as_deref(), Some("ijlo_z_queue"));
}

#[test]
fn one_blocker_before_the_key_piece_is_not_a_failure() {
    // Only T precedes S; J can wait until S has been placed.
    let bag = [T, S, J, Z, L, I, O];
    assert_eq!(
        candidate_status(&bag, "ijlo_s_queue"),
        CandidateStatus::Eligible
    );
    assert_eq!(strict_selection(&bag).as_deref(), Some("ijlo_s_queue"));
}

#[test]
fn s_z_order_breaks_the_tie_between_two_buildable_setups() {
    assert_eq!(
        strict_selection(&[S, Z, T, J, L, I, O]).as_deref(),
        Some("ijlo_s_queue")
    );
    assert_eq!(
        strict_selection(&[Z, S, T, J, L, I, O]).as_deref(),
        Some("ijlo_z_queue")
    );
}

#[test]
fn save_o_takes_over_when_both_queue_setups_fail() {
    let bag = [T, J, L, S, Z, I, O];
    assert_eq!(
        candidate_status(&bag, "ijlo_s_queue"),
        CandidateStatus::FailConditionMatched
    );
    assert_eq!(
        candidate_status(&bag, "ijlo_z_queue"),
        CandidateStatus::FailConditionMatched
    );
    assert_eq!(strict_selection(&bag).as_deref(), Some("ijlo_save_o_left"));
}

#[test]
fn strict_mode_stops_when_no_verified_setup_can_be_solved() {
    // O fills HOLD first, then I comes before both J and L.
    let bag = [O, I, L, T, J, S, Z];
    for id in ["ijlo_save_o_left", "ijlo_save_o_right"] {
        assert_eq!(candidate_status(&bag, id), CandidateStatus::BuildFailed);
    }
    assert_eq!(strict_selection(&bag), None);
}

#[test]
fn undisclosed_key_pieces_keep_the_recommendation_pending() {
    let rules = bundled_rules();
    let context = context_for_bag(&[T, I, O]);
    assert!(!context.queue_closed);
    let report = rules.select_for_board(&context, &empty_board());
    assert_eq!(report.selected, None);
    for candidate in report
        .candidates
        .iter()
        .filter(|candidate| candidate.setup_id.ends_with("_queue"))
    {
        assert_eq!(candidate.status, CandidateStatus::BuildPending);
        assert_eq!(candidate.build_result, EvalResult::Pending);
    }
}

#[test]
fn unreviewed_setups_only_appear_as_a_labelled_preview() {
    let rules = bundled_rules();
    let bag = [O, I, L, T, J, S, Z];
    let strict = rules.select_for_board(&context_for_bag(&bag), &empty_board());
    assert_eq!(strict.selected, None);

    let preview = rules.select_preview_for_board(&context_for_bag(&bag), &empty_board());
    let selected = preview
        .selected
        .expect("preview falls back to unreviewed data");
    let setup = rules
        .setup(&selected.method_id, &selected.setup_id)
        .unwrap();
    assert_eq!(setup.review_status, ReviewStatus::ReviewRequired);
    assert_eq!(selected.setup_id, "ijlo_sandbox_extended");
}
