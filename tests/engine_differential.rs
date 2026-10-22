//! The Rust engine against czsmall_nnue's exact Python reconstruction.
//!
//! `tests/fixtures/engine_differential.json` holds synthetic solo streams and
//! the placements Python produced for them (`tools/make_engine_fixture.py`).
//! Real `.ttrm` corpora are checked with `verify-ttrm --reference`.

use pc_overlay_core::ttrm::{
    ReferencePlacement, StreamRef, StreamRun, first_reference_difference, run_stream,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Case {
    config: usize,
    events: Vec<Value>,
    placements: Vec<ReferencePlacement>,
}

#[test]
fn engine_matches_python_placement_by_placement() {
    let text = std::fs::read_to_string("tests/fixtures/engine_differential.json").unwrap();
    let cases: Vec<Case> = serde_json::from_str(&text).unwrap();
    assert!(cases.len() >= 15);
    let mut placements = 0;
    for (index, case) in cases.iter().enumerate() {
        let stream = StreamRef::from_events(&case.events);
        let result = run_stream(&stream, &StreamRun::default())
            .unwrap_or_else(|e| panic!("case {index} (config {}): {e}", case.config));
        if let Some(diff) = first_reference_difference(&result.engine, &case.placements) {
            panic!("case {index} (config {}): {diff}", case.config);
        }
        placements += case.placements.len();
    }
    assert!(placements > 200);
}
