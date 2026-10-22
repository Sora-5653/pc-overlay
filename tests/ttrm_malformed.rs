//! Malformed replay files are rejected with an error instead of panicking or
//! running forever.

use pc_overlay_core::ttrm::{MAX_FRAME, ReplayError, StreamRun, parse_board, run_stream, streams};
use serde_json::{Value, json};

/// A solo `.ttr` with no gravity, so an idle engine never tops out, ending at
/// `end` with one `keydown` at `input` (frame, subframe).
fn idle_replay(end: Value, input: Option<(Value, Value)>) -> Value {
    let mut events = Vec::new();
    if let Some((frame, subframe)) = input {
        events.push(json!({
            "type": "keydown",
            "frame": frame,
            "data": {"key": "hardDrop", "subframe": subframe},
        }));
    }
    events.push(json!({"type": "end", "frame": end, "data": {}}));
    json!({"replay": {
        "options": {"seed": 12345, "g": 0, "gincrease": 0},
        "events": events,
    }})
}

fn run(document: &Value) -> Result<(), ReplayError> {
    let stream = streams(document).unwrap().remove(0);
    run_stream(&stream, &StreamRun::default()).map(|_| ())
}

#[test]
fn non_ascii_board_cell_is_an_unknown_cell() {
    let rows = vec![json!(["é"])];
    assert!(matches!(
        parse_board(&rows),
        Err(ReplayError::UnknownCell(cell)) if cell == "é"
    ));
}

#[test]
fn frames_beyond_the_limit_are_rejected() {
    for frame in [
        json!(MAX_FRAME + 1),
        json!(1e12),
        json!(i64::MAX),
        json!(-1),
    ] {
        let result = run(&idle_replay(frame.clone(), None));
        assert!(
            matches!(result, Err(ReplayError::FrameOutOfRange { .. })),
            "end frame {frame}: {result:?}"
        );
    }
}

#[test]
fn huge_subframes_are_rejected() {
    for subframe in [json!(1e300), json!(-1e300)] {
        let result = run(&idle_replay(json!(10), Some((json!(5), subframe.clone()))));
        assert!(
            matches!(result, Err(ReplayError::FrameOutOfRange { .. })),
            "subframe {subframe}: {result:?}"
        );
    }
}

#[test]
fn the_frame_limit_itself_still_replays() {
    let start = std::time::Instant::now();
    run(&idle_replay(json!(MAX_FRAME), None)).unwrap();
    // Not a benchmark, just a guard that the limit stays affordable.
    assert!(start.elapsed().as_secs() < 30, "{:?}", start.elapsed());
}
