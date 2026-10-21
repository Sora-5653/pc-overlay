use pc_overlay_core::{Piece, StabilityConfig, StabilityFilter, StabilityOutcome, empty_board};

fn observation(piece: Piece) -> pc_overlay_core::Observation {
    pc_overlay_core::Observation {
        board: empty_board(),
        hold: Some(Piece::I),
        current: piece,
        next5: [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
    }
}

#[test]
fn recognition_requires_stability_before_tracker_boundary() {
    let mut filter = StabilityFilter::new(StabilityConfig {
        required_consecutive_frames: 2,
        confidence_threshold_percent: 90,
    });
    assert!(matches!(
        filter.ingest(observation(Piece::I), 1.0),
        StabilityOutcome::Pending {
            consecutive_frames: 1
        }
    ));
    assert!(matches!(
        filter.ingest(observation(Piece::I), 1.0),
        StabilityOutcome::Stable { .. }
    ));
}
