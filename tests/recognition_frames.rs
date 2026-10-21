//! Regression tests on frames recorded from TETR.IO ZEN (1920x1009 client
//! area). Each fixture is a 440x500 crop around the playfield, taken at
//! (740, 240); the test pastes it back into a black frame of the original
//! size. The calibration is the user's original drag selection, which
//! included the border lines (194 px wide instead of the 192 px interior), so
//! these tests also cover snapping to the visible border.

use std::path::Path;

use pc_overlay_core::{BoardCalibration, CapturedFrame, Piece, RecognizerConfig, TetrioRecognizer};

const FRAME_WIDTH: u32 = 1920;
const FRAME_HEIGHT: u32 = 1009;
const CROP_LEFT: u32 = 740;
const CROP_TOP: u32 = 240;

fn recognizer() -> TetrioRecognizer {
    TetrioRecognizer::new(RecognizerConfig {
        board_calibration: BoardCalibration {
            center_x_ratio: 0.5,
            top_ratio: 0.311_199_22,
            board_width_ratio: 0.101_041_67,
        },
        ..Default::default()
    })
    .unwrap()
}

/// Reads an uncompressed bottom-up 24-bit BMP crop into a full-size frame.
fn load_fixture(name: &str) -> CapturedFrame {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/frames")
        .join(name);
    let bytes = std::fs::read(&path).unwrap();
    let field = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let pixel_offset = field(10) as usize;
    let width = field(18);
    let height = field(22);
    assert_eq!(u16::from_le_bytes([bytes[28], bytes[29]]), 24);
    let stride = (width as usize * 3).div_ceil(4) * 4;

    let mut rgba = vec![0u8; (FRAME_WIDTH * FRAME_HEIGHT * 4) as usize];
    for chunk in rgba.chunks_exact_mut(4) {
        chunk[3] = 255;
    }
    for y in 0..height {
        let source_row = pixel_offset + (height - 1 - y) as usize * stride;
        for x in 0..width {
            let source = source_row + x as usize * 3;
            let target = (((CROP_TOP + y) * FRAME_WIDTH + CROP_LEFT + x) * 4) as usize;
            rgba[target..target + 3].copy_from_slice(&[
                bytes[source + 2],
                bytes[source + 1],
                bytes[source],
            ]);
        }
    }
    CapturedFrame {
        width: FRAME_WIDTH,
        height: FRAME_HEIGHT,
        rgba,
        captured_at_ms: 0,
    }
}

fn queue(text: &str) -> [Piece; 5] {
    let pieces: Vec<_> = text
        .chars()
        .map(|code| match code {
            'I' => Piece::I,
            'J' => Piece::J,
            'L' => Piece::L,
            'O' => Piece::O,
            'S' => Piece::S,
            'T' => Piece::T,
            'Z' => Piece::Z,
            other => panic!("unknown piece {other}"),
        })
        .collect();
    pieces.try_into().unwrap()
}

#[test]
fn board_rectangle_snaps_to_the_border_lines() {
    let result = recognizer()
        .recognize(&load_fixture("zen_000005.bmp"))
        .unwrap();
    let rect = result.board_rect;
    assert_eq!(
        (rect.left, rect.top, rect.width, rect.height),
        (864, 313, 192, 384)
    );
}

#[test]
fn falling_piece_frame_reads_the_stack_and_ignores_the_ghost_outline() {
    let result = recognizer()
        .recognize(&load_fixture("zen_000005.bmp"))
        .unwrap();
    let observation = result.observation.expect("complete observation");
    assert_eq!(observation.current, Piece::S);
    assert_eq!(observation.hold, Some(Piece::L));
    assert_eq!(observation.next5, queue("ZLJIO"));
    assert_eq!(result.ambiguous_board_cells, 0);
    assert!(
        result.confidence >= 0.70,
        "confidence {}",
        result.confidence
    );
    use Piece::*;
    assert_eq!(
        observation.board[0],
        [
            Some(T),
            Some(S),
            Some(S),
            None,
            None,
            None,
            Some(I),
            Some(J),
            Some(J),
            Some(J)
        ]
    );
    assert_eq!(result.board_cells, 28);
}

#[test]
fn piece_spawned_above_the_board_is_current() {
    let result = recognizer()
        .recognize(&load_fixture("zen_000007.bmp"))
        .unwrap();
    let observation = result.observation.expect("complete observation");
    assert_eq!(observation.current, Piece::T);
    assert_eq!(observation.hold, Some(Piece::J));
    assert_eq!(observation.next5, queue("ZOLTL"));
    // The T straddles the top edge; its in-board cell is not a fixed block.
    assert!(observation.board[19].iter().all(Option::is_none));
}

#[test]
fn dimmed_hold_piece_is_read_by_shape() {
    let result = recognizer()
        .recognize(&load_fixture("zen_000010.bmp"))
        .unwrap();
    let observation = result.observation.expect("complete observation");
    assert_eq!(observation.hold, Some(Piece::L));
    assert_eq!(observation.current, Piece::S);
    assert_eq!(observation.next5, queue("JIOJI"));
}

#[test]
fn all_clear_banner_pauses_recognition() {
    let result = recognizer()
        .recognize(&load_fixture("zen_000008.bmp"))
        .unwrap();
    assert!(result.observation.is_none());
    assert_eq!(result.current, Some(Piece::T));
}
