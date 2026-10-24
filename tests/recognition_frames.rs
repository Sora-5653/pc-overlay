//! Regression tests on frames recorded from TETR.IO ZEN (1920x1009 client
//! area). Each fixture is a 440x500 crop around the playfield, taken at
//! (740, 240); the test pastes it back into a black frame of the original
//! size. The calibration is the user's original drag selection, which
//! included the border lines (194 px wide instead of the 192 px interior), so
//! these tests also cover snapping to the visible border. `zen_pieces_150.bmp`
//! is the same crop of a screenshot whose window had a 23 px title bar, with
//! the overlay's guidance drawn over an empty board. `zen_pieces_18/25/38.bmp`
//! are the same crop of 1920x1080 ZEN recordings, taken at (740, 271).

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

#[test]
fn reads_the_pieces_counter_left_of_the_board() {
    let recognizer = recognizer();
    for (name, pieces) in [
        ("zen_000005.bmp", 7),
        ("zen_000007.bmp", 16),
        ("zen_000008.bmp", 20),
        ("zen_000010.bmp", 9),
        ("zen_pieces_150.bmp", 150),
        ("zen_pieces_18.bmp", 18),
        ("zen_pieces_25.bmp", 25),
        ("zen_pieces_38.bmp", 38),
    ] {
        let result = recognizer.recognize(&load_fixture(name)).unwrap();
        assert_eq!(result.pieces_placed, Some(pieces), "{name}");
    }
}

#[test]
fn pieces_counter_needs_its_label() {
    // Blank the "PIECES" label: the ATTACK line below must not stand in.
    let mut frame = load_fixture("zen_000007.bmp");
    for y in CROP_TOP + 374..CROP_TOP + 389 {
        for x in CROP_LEFT + 78..CROP_LEFT + 120 {
            let index = ((y * FRAME_WIDTH + x) * 4) as usize;
            frame.rgba[index..index + 3].fill(0);
        }
    }
    let result = recognizer().recognize(&frame).unwrap();
    assert_eq!(result.pieces_placed, None);
}

#[test]
fn pieces_counter_reads_a_five_it_was_not_fitted_on() {
    // The 5 template was fitted on the counter's 150. Paste the ATTACK value
    // ("105, 16.76/M", right-aligned at the same x) over the counter line, so
    // a 5 drawn elsewhere on the screen is read too.
    let mut frame = load_fixture("zen_pieces_150.bmp");
    let row = |y: u32| (((CROP_TOP + y) * FRAME_WIDTH + CROP_LEFT) * 4) as usize;
    for y in 0..22 {
        let (source, target) = (row(436 + y), row(391 + y));
        frame.rgba.copy_within(source..source + 120 * 4, target);
    }
    let result = recognizer().recognize(&frame).unwrap();
    assert_eq!(result.pieces_placed, Some(105));
}

#[test]
fn empty_board_under_the_guidance_is_read_as_empty() {
    // The guidance cells drawn by the overlay at PIECES 150 are not pieces.
    let result = recognizer()
        .recognize(&load_fixture("zen_pieces_150.bmp"))
        .unwrap();
    let observation = result.observation.expect("complete observation");
    assert!(pc_overlay_core::board_is_empty(&observation.board));
    assert_eq!(observation.current, Piece::I);
    assert_eq!(observation.hold, Some(Piece::S));
    assert_eq!(observation.next5, queue("LZLZS"));
}

/// Bilinear resize, standing in for TETR.IO drawing the board and its
/// statistics at another display scale.
fn rescale(frame: &CapturedFrame, scale: f32) -> CapturedFrame {
    let width = (frame.width as f32 * scale).round() as u32;
    let height = (frame.height as f32 * scale).round() as u32;
    let source = |x: u32, y: u32, channel: usize| {
        f32::from(frame.rgba[((y * frame.width + x) * 4) as usize + channel])
    };
    let mut rgba = vec![255u8; (width * height * 4) as usize];
    for y in 0..height {
        let sy = ((y as f32 + 0.5) / scale - 0.5).clamp(0.0, (frame.height - 1) as f32);
        let (y0, fy) = (sy.floor() as u32, sy.fract());
        let y1 = (y0 + 1).min(frame.height - 1);
        for x in 0..width {
            let sx = ((x as f32 + 0.5) / scale - 0.5).clamp(0.0, (frame.width - 1) as f32);
            let (x0, fx) = (sx.floor() as u32, sx.fract());
            let x1 = (x0 + 1).min(frame.width - 1);
            for channel in 0..3 {
                let top = source(x0, y0, channel) * (1.0 - fx) + source(x1, y0, channel) * fx;
                let bottom = source(x0, y1, channel) * (1.0 - fx) + source(x1, y1, channel) * fx;
                rgba[((y * width + x) * 4) as usize + channel] =
                    (top * (1.0 - fy) + bottom * fy).round() as u8;
            }
        }
    }
    CapturedFrame {
        width,
        height,
        rgba,
        captured_at_ms: 0,
    }
}

#[test]
fn pieces_counter_follows_the_display_scale() {
    let recognizer = recognizer();
    for (name, pieces) in [
        ("zen_000007.bmp", 16),
        ("zen_000008.bmp", 20),
        ("zen_pieces_150.bmp", 150),
        ("zen_pieces_18.bmp", 18),
        ("zen_pieces_25.bmp", 25),
        ("zen_pieces_38.bmp", 38),
    ] {
        let frame = load_fixture(name);
        for scale in [0.75, 0.9, 1.25, 1.5, 2.0] {
            let result = recognizer.recognize(&rescale(&frame, scale)).unwrap();
            assert_eq!(result.pieces_placed, Some(pieces), "{name} at {scale}x");
        }
    }
}
