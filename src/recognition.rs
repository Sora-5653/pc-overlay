//! Conservative pixel recognition for the standard TETR.IO desktop layout.
//!
//! This module intentionally does not try to infer game state from hidden
//! APIs.  It samples the configured client-area ROIs, classifies the familiar
//! TETR.IO piece colours, and returns an observation only when every required
//! preview is readable.  Ambiguous board cells are treated as unknown and
//! reduce confidence; the stability gate must still approve the observation
//! before it can change the session tracker.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    capture::CapturedFrame,
    model::{BOARD_HEIGHT, BOARD_WIDTH, Observation, Piece, empty_board},
    roi::{FrameSize, PixelRect, RoiLayout},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    fn max(self) -> u8 {
        self.r.max(self.g).max(self.b)
    }

    fn min(self) -> u8 {
        self.r.min(self.g).min(self.b)
    }

    fn chroma(self) -> u8 {
        self.max().saturating_sub(self.min())
    }

    fn distance_squared(self, other: Self) -> u32 {
        let dr = i32::from(self.r) - i32::from(other.r);
        let dg = i32::from(self.g) - i32::from(other.g);
        let db = i32::from(self.b) - i32::from(other.b);
        (dr * dr + dg * dg + db * db) as u32
    }
}

/// Approximate solid colours observed in the standard TETR.IO desktop theme.
/// The classifier uses a distance threshold, so antialiasing and the dimmed
/// edges of a tetromino do not need to match these values exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PiecePalette {
    pub i: Rgb,
    pub j: Rgb,
    pub l: Rgb,
    pub o: Rgb,
    pub s: Rgb,
    pub t: Rgb,
    pub z: Rgb,
}

impl Default for PiecePalette {
    fn default() -> Self {
        Self {
            i: Rgb::new(62, 188, 140),
            j: Rgb::new(91, 76, 175),
            l: Rgb::new(194, 115, 65),
            o: Rgb::new(177, 151, 48),
            s: Rgb::new(142, 191, 64),
            // TETR.IO renders T as magenta/purple; keep it distinct from the
            // blue-violet J piece. Older screenshots can be brighter or less
            // saturated, so `classify_pixel` also has a hue fallback.
            t: Rgb::new(170, 75, 160),
            z: Rgb::new(192, 65, 70),
        }
    }
}

impl PiecePalette {
    fn entries(self) -> [(Piece, Rgb); 7] {
        [
            (Piece::I, self.i),
            (Piece::J, self.j),
            (Piece::L, self.l),
            (Piece::O, self.o),
            (Piece::S, self.s),
            (Piece::T, self.t),
            (Piece::Z, self.z),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecognizerConfig {
    pub roi: RoiLayout,
    /// Explicit board calibration. TETR.IO changes the board scale with the
    /// game viewport, so the board rectangle is not derived from the client
    /// height. HOLD, Current, and NEXT are searched relative to this board.
    #[serde(default)]
    pub board_calibration: BoardCalibration,
    pub palette: PiecePalette,
    /// Minimum brightness for a pixel to be considered a solid tetromino
    /// colour.  This rejects the dark grid and the translucent ghost piece.
    pub minimum_value: u8,
    pub minimum_chroma: u8,
    /// Fraction of the cropped preview ROI that must contain a colour before
    /// it is considered a real tetromino.  This rejects the coloured border
    /// and text that remain visible when HOLD is empty.
    pub minimum_region_coverage: f32,
    /// Number of classified pixels in a cell required to call it fixed.
    pub board_cell_min_hits: u16,
    /// Cells between this threshold and `board_cell_min_hits` lower the frame
    /// confidence because they are usually a ghost or a grid edge.
    pub board_cell_ambiguous_hits: u16,
    /// Rows at the top of the board searched for the connected four-cell
    /// active piece. Its exact cells are masked from the fixed-board result.
    pub mask_active_rows: u8,
}

/// User-controlled board calibration expressed as ratios of the captured
/// client area. `board_width_ratio` is the complete ten-column board width;
/// the height is always two board widths because the playfield is 10x20.
///
/// Keeping the values normalized allows the same calibration to survive a
/// small window move or resize. If TETR.IO's display magnification changes,
/// rerun `configure-overlay` to write a new calibration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoardCalibration {
    pub center_x_ratio: f32,
    pub top_ratio: f32,
    pub board_width_ratio: f32,
}

impl Default for BoardCalibration {
    fn default() -> Self {
        Self {
            center_x_ratio: 0.5005,
            top_ratio: 0.309,
            board_width_ratio: 0.103,
        }
    }
}

impl BoardCalibration {
    pub fn validate(&self) -> Result<(), String> {
        let values = [self.center_x_ratio, self.top_ratio, self.board_width_ratio];
        if values.iter().any(|value| !value.is_finite()) {
            return Err("board calibration contains a non-finite value".to_string());
        }
        if !(0.0..=1.0).contains(&self.center_x_ratio)
            || !(0.0..=1.0).contains(&self.top_ratio)
            || self.board_width_ratio <= 0.0
            || self.board_width_ratio > 1.0
        {
            return Err("board calibration ratios are outside their valid ranges".to_string());
        }
        let left = self.center_x_ratio - self.board_width_ratio / 2.0;
        let right = self.center_x_ratio + self.board_width_ratio / 2.0;
        if left < 0.0 || right > 1.0 {
            return Err(
                "calibrated board does not fit horizontally in the client area".to_string(),
            );
        }
        if self.top_ratio + self.board_width_ratio * 2.0 > 1.0 {
            return Err(
                "calibrated 10x20 board does not fit vertically in the client area".to_string(),
            );
        }
        Ok(())
    }

    pub fn resolve(self, frame: FrameSize) -> PixelRect {
        let requested_width = (self.board_width_ratio * frame.width as f32)
            .round()
            .max(10.0) as u32;
        let center_x = (self.center_x_ratio * frame.width as f32).round() as i64;
        let left = (center_x - i64::from(requested_width / 2)).max(0) as u32;
        let top = (self.top_ratio * frame.height as f32).round() as u32;
        let max_width = frame.width.saturating_sub(left);
        let max_height_width = frame.height.saturating_sub(top) / 2;
        let width = requested_width.min(max_width).min(max_height_width);
        let height = width.saturating_mul(2);
        PixelRect {
            left,
            top,
            width,
            height,
        }
    }

    pub fn from_pixels(
        frame: FrameSize,
        center_x_px: u32,
        top_px: u32,
        board_width_px: u32,
    ) -> Result<Self, String> {
        if frame.width == 0 || frame.height == 0 {
            return Err("client frame dimensions must be non-zero".to_string());
        }
        if board_width_px == 0 || board_width_px > frame.width {
            return Err("board width must be positive and fit in the client width".to_string());
        }
        let calibration = Self {
            center_x_ratio: center_x_px as f32 / frame.width as f32,
            top_ratio: top_px as f32 / frame.height as f32,
            board_width_ratio: board_width_px as f32 / frame.width as f32,
        };
        calibration.validate()?;
        Ok(calibration)
    }
}

impl Default for RecognizerConfig {
    fn default() -> Self {
        Self::tetrio_desktop()
    }
}

impl RecognizerConfig {
    /// Default calibration for the 1920x1009 client-area layout used by
    /// TETR.IO ZEN. A user calibration file can replace `board_calibration`
    /// without changing the recognition logic.
    pub fn tetrio_desktop() -> Self {
        Self {
            roi: RoiLayout {
                board: crate::roi::NormalizedRect::new(0.449, 0.309, 0.103, 0.206),
                hold: crate::roi::NormalizedRect::new(0.397, 0.309, 0.052, 0.081),
                current: crate::roi::NormalizedRect::new(0.455, 0.278, 0.041, 0.031),
                next5: [
                    crate::roi::NormalizedRect::new(0.553, 0.309, 0.051, 0.060),
                    crate::roi::NormalizedRect::new(0.553, 0.369, 0.051, 0.060),
                    crate::roi::NormalizedRect::new(0.553, 0.429, 0.051, 0.060),
                    crate::roi::NormalizedRect::new(0.553, 0.489, 0.051, 0.060),
                    crate::roi::NormalizedRect::new(0.553, 0.549, 0.051, 0.060),
                ],
            },
            board_calibration: BoardCalibration::default(),
            palette: PiecePalette::default(),
            minimum_value: 100,
            minimum_chroma: 35,
            minimum_region_coverage: 0.08,
            board_cell_min_hits: 60,
            // TETR.IO's chromatic-aberration effect gives the white ghost
            // outline red/cyan fringes worth up to ~26 classified pixels per
            // cell, while a real cell (even one with a face) has 100+.
            board_cell_ambiguous_hits: 40,
            mask_active_rows: 8,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RecognitionError {
    #[error("captured frame has invalid dimensions or byte length")]
    MalformedFrame,
    #[error("recognizer ROI layout is invalid: {0}")]
    InvalidRoi(String),
    #[error("board calibration is invalid: {0}")]
    InvalidBoardCalibration(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecognitionResult {
    /// `None` means a required preview was missing or the board contained an
    /// ambiguous cell.  The caller must retain its last stable observation.
    pub observation: Option<Observation>,
    pub confidence: f32,
    pub board_cells: usize,
    pub ambiguous_board_cells: u16,
    pub weak_board_cells: u16,
    pub hold: Option<Piece>,
    pub current: Option<Piece>,
    pub next5: [Option<Piece>; 5],
    pub hold_confidence: f32,
    pub current_confidence: f32,
    pub next_confidences: [f32; 5],
    pub board_rect: PixelRect,
    /// The yellow ALL CLEAR text shown after a perfect clear covered the
    /// board; its cells were left out of `board`.
    pub all_clear_banner: bool,
    /// ZEN's PIECES counter, when its statistics display is on and readable.
    pub pieces_placed: Option<u32>,
    /// Diagnostic per-cell classification in image order (row 0 is the top):
    /// dominant piece, classified pixel count, and sampled pixel count.
    pub cell_samples: [[Option<(Piece, u16, u16)>; BOARD_WIDTH]; BOARD_HEIGHT],
}

#[derive(Debug, Clone)]
pub struct TetrioRecognizer {
    config: RecognizerConfig,
}

impl TetrioRecognizer {
    pub fn new(config: RecognizerConfig) -> Result<Self, RecognitionError> {
        config
            .roi
            .validate()
            .map_err(|error| RecognitionError::InvalidRoi(error.to_string()))?;
        config
            .board_calibration
            .validate()
            .map_err(RecognitionError::InvalidBoardCalibration)?;
        Ok(Self { config })
    }

    pub fn config(&self) -> &RecognizerConfig {
        &self.config
    }

    pub fn recognize(&self, frame: &CapturedFrame) -> Result<RecognitionResult, RecognitionError> {
        if !frame.is_well_formed() {
            return Err(RecognitionError::MalformedFrame);
        }

        let frame_size = FrameSize {
            width: frame.width,
            height: frame.height,
        };
        // A drag selection is only accurate to a pixel or two, and a 1% width
        // error accumulates to a quarter cell by the bottom row. Snap the
        // calibrated rectangle to the board's own border lines when they are
        // clearly visible nearby.
        let board_rect =
            refine_board_rect(frame, self.config.board_calibration.resolve(frame_size));
        // The fixed ROI rectangles remain useful for calibration and tests,
        // but the live panels contain borders, labels, and variable-height
        // previews.  Prefer a board-relative blob search so a small vertical
        // layout shift does not make Current/HOLD/NEXT disappear.
        let (hold, hold_confidence) = recognize_hold_panel(frame, board_rect, &self.config);
        let (dynamic_next, dynamic_next_confidences) =
            recognize_next_panel(frame, board_rect, &self.config);
        let next5 = dynamic_next;
        let next_confidences = dynamic_next_confidences;
        // Do not fall back to stale normalized NEXT slots here. A display
        // magnification change must either be covered by the configured board
        // calibration or remain unknown; guessing a wrong queue is unsafe.

        let cell_width = board_rect.width as f32 / 10.0;
        let cell_height = board_rect.height as f32 / 20.0;
        // TETR.IO spawns pieces above the visible 20 rows, so the sampled
        // grid starts SPAWN_ROWS cells above the board. Those rows only feed
        // Current detection; they never become fixed board cells.
        let mut extended = vec![[None; BOARD_WIDTH]; SPAWN_ROWS + BOARD_HEIGHT];
        for (extended_row, sample_row) in extended.iter_mut().enumerate() {
            let top =
                board_rect.top as f32 + (extended_row as f32 - SPAWN_ROWS as f32) * cell_height;
            if top < 0.0 {
                continue;
            }
            for (column, sample) in sample_row.iter_mut().enumerate() {
                let cell_rect = spawn_cell_rect(board_rect, cell_width, cell_height, column, top);
                *sample = classify_cell(frame, cell_rect, &self.config);
            }
        }
        let mut samples = [[None; BOARD_WIDTH]; BOARD_HEIGHT];
        samples.copy_from_slice(&extended[SPAWN_ROWS..]);

        // TETR.IO draws Current inside the playfield, not in a separate panel.
        // Locate the strong four-cell component near the spawn rows first, then
        // remove that exact component from the fixed-board model. While a piece
        // is falling outside the spawn band recognition deliberately pauses;
        // the next spawned piece produces the stable post-lock observation.
        let active = find_active_component(
            &extended,
            self.config.board_cell_min_hits,
            self.config
                .mask_active_rows
                .saturating_add(SPAWN_ROWS as u8),
        );
        let (current, current_confidence) = active
            .as_ref()
            .map(|active| (Some(active.piece), active.confidence))
            .unwrap_or((None, 0.0));
        let ghost_cells = active
            .as_ref()
            .map(|active| find_ghost_cells(&extended, active, self.config.board_cell_min_hits))
            .unwrap_or_default();
        // Convert the extended-grid cells back to board image rows.
        let to_board = |cells: &[(usize, usize)]| -> Vec<(usize, usize)> {
            cells
                .iter()
                .filter_map(|&(row, column)| row.checked_sub(SPAWN_ROWS).map(|row| (row, column)))
                .collect()
        };
        let active_cells = active
            .as_ref()
            .map(|active| to_board(&active.cells))
            .unwrap_or_default();
        let ghost_cells = to_board(&ghost_cells);
        let banner_cells = find_all_clear_banner(
            &samples,
            &[active_cells.as_slice(), ghost_cells.as_slice()].concat(),
            self.config.board_cell_ambiguous_hits,
        );

        let mut board = empty_board();
        let mut board_cells = 0usize;
        let mut ambiguous_board_cells = 0u16;
        let mut weak_board_cells = 0u16;
        for (image_row, sample_row) in samples.iter().enumerate() {
            let model_row = 19 - image_row;
            for (column, (sample, board_cell)) in sample_row
                .iter()
                .copied()
                .zip(board[model_row].iter_mut())
                .enumerate()
            {
                if active_cells.contains(&(image_row, column))
                    || ghost_cells.contains(&(image_row, column))
                    || banner_cells.contains(&(image_row, column))
                {
                    continue;
                }
                let Some(sample) = sample else {
                    continue;
                };
                if sample.hits >= self.config.board_cell_min_hits {
                    *board_cell = Some(sample.piece);
                    board_cells += 1;
                } else if sample.hits >= self.config.board_cell_ambiguous_hits {
                    ambiguous_board_cells = ambiguous_board_cells.saturating_add(1);
                } else {
                    weak_board_cells = weak_board_cells.saturating_add(1);
                }
            }
        }

        let required_confidence = [
            current_confidence,
            next_confidences[0],
            next_confidences[1],
            next_confidences[2],
            next_confidences[3],
            next_confidences[4],
        ];
        let preview_confidence = required_confidence.iter().copied().fold(1.0_f32, f32::min);
        let board_penalty = (f32::from(ambiguous_board_cells) * 0.08
            + f32::from(weak_board_cells) * 0.015)
            .min(0.40);
        let confidence = (preview_confidence - board_penalty).clamp(0.0, 1.0);
        let complete_previews = current.is_some() && next5.iter().all(Option::is_some);
        let observation = (complete_previews && ambiguous_board_cells == 0).then(|| Observation {
            board,
            hold,
            current: current.expect("complete_previews checked current"),
            next5: next5.map(|piece| piece.expect("complete_previews checked next5")),
        });

        Ok(RecognitionResult {
            observation,
            confidence,
            board_cells,
            ambiguous_board_cells,
            weak_board_cells,
            hold,
            current,
            next5,
            hold_confidence,
            current_confidence,
            next_confidences,
            board_rect,
            all_clear_banner: !banner_cells.is_empty(),
            pieces_placed: crate::pieces_ocr::read_pieces_placed(frame, board_rect),
            cell_samples: samples.map(|row| {
                row.map(|sample| {
                    sample.map(|sample| (sample.piece, sample.hits, sample.sampled_pixels))
                })
            }),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct CellSample {
    piece: Piece,
    hits: u16,
    sampled_pixels: u16,
    mean_value: f32,
}

#[derive(Debug, Clone)]
struct ActiveComponent {
    piece: Piece,
    cells: Vec<(usize, usize)>,
    mean_hits: f32,
    mean_value: f32,
    confidence: f32,
}

#[derive(Debug, Clone, Copy)]
#[cfg(test)]
struct RegionCrop {
    margin_x: f32,
    margin_y: f32,
}

#[cfg(test)]
impl RegionCrop {
    const PREVIEW: Self = Self {
        margin_x: 0.14,
        margin_y: 0.12,
    };
}

#[derive(Debug, Clone, Copy)]
struct ColorPoint {
    y: u32,
    index: usize,
    quality: f32,
}

fn recognize_hold_panel(
    frame: &CapturedFrame,
    board: PixelRect,
    config: &RecognizerConfig,
) -> (Option<Piece>, f32) {
    let cell_width = board.width as f32 / 10.0;
    let cell_height = board.height as f32 / 20.0;
    let rect = PixelRect {
        // Skip the HOLD header and the coloured panel border. The preview
        // itself is centred roughly one cell below the header.
        left: board.left.saturating_sub((cell_width * 5.2).round() as u32),
        top: board.top.saturating_add((cell_height * 0.9).round() as u32),
        width: (cell_width * 5.0).round() as u32,
        height: (cell_height * 3.2).round() as u32,
    };
    let points = collect_points(frame, rect, config);
    // Empty HOLD still contains a coloured border and a label.  Requiring a
    // substantial filled blob and a clear dominant palette colour avoids
    // turning that chrome into a held piece.
    let minimum_hits = preview_minimum_hits(cell_width, cell_height, 0.75);
    let coloured = dominant_points(&points, rect_area(rect), minimum_hits, 0.06, 0.50);
    if coloured.0.is_some() {
        coloured
    } else {
        recognize_dimmed_hold(frame, rect, cell_width, cell_height)
    }
}

fn recognize_dimmed_hold(
    frame: &CapturedFrame,
    rect: PixelRect,
    cell_width: f32,
    cell_height: f32,
) -> (Option<Piece>, f32) {
    // TETR.IO dims a held piece heavily. In that state its fill can be almost
    // grayscale, so palette classification intentionally cannot see it. Try
    // the seven fixed preview silhouettes against the low-luminance pixels.
    const SHAPES: [(Piece, [(u8, u8); 4]); 7] = [
        (Piece::I, [(0, 0), (1, 0), (2, 0), (3, 0)]),
        (Piece::J, [(0, 0), (0, 1), (1, 1), (2, 1)]),
        (Piece::L, [(2, 0), (0, 1), (1, 1), (2, 1)]),
        (Piece::O, [(0, 0), (1, 0), (0, 1), (1, 1)]),
        (Piece::S, [(1, 0), (2, 0), (0, 1), (1, 1)]),
        (Piece::T, [(1, 0), (0, 1), (1, 1), (2, 1)]),
        (Piece::Z, [(0, 0), (1, 0), (1, 1), (2, 1)]),
    ];
    let step = cell_width.round().max(1.0) as u32 / 3 + 1;
    let max_x = rect
        .left
        .saturating_add(rect.width)
        .saturating_sub((cell_width * 4.0).ceil() as u32);
    let max_y = rect
        .top
        .saturating_add(rect.height)
        .saturating_sub((cell_height * 2.0).ceil() as u32);
    // An empty HOLD is rendered as one flat gray bar. Detect that placeholder
    // before silhouette matching; otherwise it is indistinguishable from a
    // dimmed horizontal fragment and would create a false queue event.
    for origin_y in (rect.top..=max_y).step_by(step as usize) {
        for origin_x in (rect.left..=max_x).step_by(step as usize) {
            // Judge every cell, not the row mean: the three-cell base of a
            // dimmed J/L/T above an empty row has a high mean too.
            let row_scores = |row: u8| -> [f32; 4] {
                [0u8, 1, 2, 3].map(|column| {
                    dimmed_cell_score(
                        frame,
                        preview_cell(origin_x, origin_y, column, row, cell_width, cell_height),
                    )
                })
            };
            let first_row = row_scores(0);
            let second_row = row_scores(1);
            let full = |scores: &[f32; 4]| scores.iter().all(|score| *score >= 0.35);
            let blank = |scores: &[f32; 4]| scores.iter().all(|score| *score <= 0.16);
            if (full(&first_row) && blank(&second_row)) || (full(&second_row) && blank(&first_row))
            {
                return (None, 0.0);
            }
        }
    }
    let mut best = None;
    for origin_y in (rect.top..=max_y).step_by(step as usize) {
        for origin_x in (rect.left..=max_x).step_by(step as usize) {
            for (piece, shape) in SHAPES {
                // The empty HOLD placeholder is a flat four-cell bar. It is
                // intentionally not accepted as a dimmed I; a real held I
                // remains detectable through the coloured path above, while
                // a false HOLD would corrupt queue tracking.
                if piece == Piece::I {
                    continue;
                }
                let mut expected = 0.0_f32;
                let mut unexpected = 0.0_f32;
                let mut minimum_expected = 1.0_f32;
                for row in 0..2u8 {
                    for column in 0..4u8 {
                        let cell =
                            preview_cell(origin_x, origin_y, column, row, cell_width, cell_height);
                        let score = dimmed_cell_score(frame, cell);
                        if shape.contains(&(column, row)) {
                            expected += score;
                            minimum_expected = minimum_expected.min(score);
                        } else {
                            unexpected += score;
                        }
                    }
                }
                let mean_expected = expected / 4.0;
                let score = mean_expected - unexpected * 0.08;
                if minimum_expected >= 0.14 && mean_expected >= 0.22 {
                    let candidate = (piece, score, mean_expected, minimum_expected);
                    if best
                        .as_ref()
                        .is_none_or(|current: &(Piece, f32, f32, f32)| candidate.1 > current.1)
                    {
                        best = Some(candidate);
                    }
                }
            }
        }
    }
    let Some((piece, score, mean_expected, minimum_expected)) = best else {
        return (None, 0.0);
    };
    let confidence =
        (0.45 + 0.40 * mean_expected + 0.15 * minimum_expected + score * 0.10).clamp(0.0, 1.0);
    (Some(piece), confidence)
}

fn preview_cell(
    origin_x: u32,
    origin_y: u32,
    column: u8,
    row: u8,
    cell_width: f32,
    cell_height: f32,
) -> PixelRect {
    PixelRect {
        left: (origin_x as f32 + f32::from(column) * cell_width).round() as u32,
        top: (origin_y as f32 + f32::from(row) * cell_height).round() as u32,
        width: cell_width.round().max(1.0) as u32,
        height: cell_height.round().max(1.0) as u32,
    }
}

fn dimmed_cell_score(frame: &CapturedFrame, rect: PixelRect) -> f32 {
    let margin_x = (rect.width / 5).max(1);
    let margin_y = (rect.height / 5).max(1);
    let left = rect.left.saturating_add(margin_x);
    let top = rect.top.saturating_add(margin_y);
    let right = rect
        .left
        .saturating_add(rect.width.saturating_sub(margin_x));
    let bottom = rect
        .top
        .saturating_add(rect.height.saturating_sub(margin_y));
    let mut hits = 0usize;
    let mut total = 0usize;
    for y in top..bottom.min(frame.height) {
        for x in left..right.min(frame.width) {
            let Some(rgb) = frame_pixel(frame, x, y) else {
                continue;
            };
            total += 1;
            if rgb.max() >= 18 && rgb.chroma() <= 22 {
                hits += 1;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        hits as f32 / total as f32
    }
}

fn find_active_component(
    samples: &[[Option<CellSample>; BOARD_WIDTH]],
    minimum_hits: u16,
    search_rows: u8,
) -> Option<ActiveComponent> {
    let mut visited = vec![[false; BOARD_WIDTH]; samples.len()];
    let mut best = None;
    for row in 0..usize::from(search_rows.max(1)).min(samples.len()) {
        for column in 0..BOARD_WIDTH {
            if visited[row][column]
                || samples[row][column].is_none_or(|sample| sample.hits < minimum_hits)
            {
                continue;
            }
            let cells =
                connected_component(samples, (row, column), minimum_hits, &[], &mut visited);
            if cells.len() != 4 {
                continue;
            }
            let active = component_summary(samples, cells);
            if best.as_ref().is_none_or(|current: &ActiveComponent| {
                (active.confidence, active.mean_hits) > (current.confidence, current.mean_hits)
            }) {
                best = Some(active);
            }
        }
    }
    best
}

/// Cells of the yellow ALL CLEAR text TETR.IO draws across the middle of the
/// board after a perfect clear, in board image coordinates.
///
/// Placed pieces rest on the floor or on other cells and line clears remove
/// whole rows, so the occupied rows of a real board always run unbroken up
/// from the bottom. Coloured cells above the first empty row cannot be placed
/// pieces; when every such cell is yellow (the text reads as O) they are the
/// banner, and every yellow sample up there is dropped, faint edges included.
/// The banner stays up for a while, so pieces placed meanwhile still form the
/// board below it. `skip` holds the active piece and its ghost.
fn find_all_clear_banner(
    samples: &[[Option<CellSample>; BOARD_WIDTH]; BOARD_HEIGHT],
    skip: &[(usize, usize)],
    ambiguous_hits: u16,
) -> Vec<(usize, usize)> {
    let coloured = |row: usize, column: usize| {
        samples[row][column]
            .filter(|sample| sample.hits >= ambiguous_hits && !skip.contains(&(row, column)))
    };
    let Some(first_empty) = (0..BOARD_HEIGHT)
        .rev()
        .find(|&row| (0..BOARD_WIDTH).all(|column| coloured(row, column).is_none()))
    else {
        return Vec::new();
    };
    let mut floating = (0..first_empty)
        .flat_map(|row| (0..BOARD_WIDTH).filter_map(move |column| coloured(row, column)))
        .peekable();
    if floating.peek().is_none() || !floating.all(|sample| sample.piece == Piece::O) {
        return Vec::new();
    }
    (0..first_empty)
        .flat_map(|row| (0..BOARD_WIDTH).map(move |column| (row, column)))
        .filter(|&(row, column)| {
            !skip.contains(&(row, column))
                && samples[row][column].is_some_and(|sample| sample.piece == Piece::O)
        })
        .collect()
}

fn find_ghost_cells(
    samples: &[[Option<CellSample>; BOARD_WIDTH]],
    active: &ActiveComponent,
    minimum_hits: u16,
) -> Vec<(usize, usize)> {
    let mut visited = vec![[false; BOARD_WIDTH]; samples.len()];
    for &(row, column) in &active.cells {
        visited[row][column] = true;
    }
    let mut ghosts = Vec::new();
    for row in 0..samples.len() {
        for column in 0..BOARD_WIDTH {
            if visited[row][column]
                || samples[row][column]
                    .is_none_or(|sample| sample.hits < minimum_hits || sample.piece != active.piece)
            {
                continue;
            }
            let cells = connected_component(
                samples,
                (row, column),
                minimum_hits,
                &active.cells,
                &mut visited,
            );
            if cells.len() != 4 {
                continue;
            }
            // A ghost is Current dropped straight down: the same cells in the
            // same columns, shifted by whole rows. TETR.IO draws locked
            // blocks dimmer than Current, so without this a locked piece of
            // the same type would be mistaken for the ghost.
            let Some(shift) = cells
                .iter()
                .map(|(row, _)| *row)
                .min()
                .zip(active.cells.iter().map(|(row, _)| *row).min())
                .and_then(|(ghost_top, active_top)| ghost_top.checked_sub(active_top))
            else {
                continue;
            };
            let dropped_active = shift > 0
                && active
                    .cells
                    .iter()
                    .all(|&(row, column)| cells.contains(&(row + shift, column)));
            if !dropped_active {
                continue;
            }
            let component = component_summary(samples, cells);
            // A coloured ghost uses the same tetromino but is substantially
            // dimmer or much more sparsely filled than the active component.
            // Requiring an exact detached four-cell component avoids removing
            // ordinary fixed cells merely because they share Current's colour.
            if component.mean_value < active.mean_value * 0.76
                || component.mean_hits < active.mean_hits * 0.55
            {
                ghosts.extend(component.cells);
            }
        }
    }
    ghosts
}

fn connected_component(
    samples: &[[Option<CellSample>; BOARD_WIDTH]],
    start: (usize, usize),
    minimum_hits: u16,
    excluded: &[(usize, usize)],
    visited: &mut [[bool; BOARD_WIDTH]],
) -> Vec<(usize, usize)> {
    let Some(start_sample) = samples[start.0][start.1] else {
        return Vec::new();
    };
    let mut pending = vec![start];
    let mut cells = Vec::new();
    visited[start.0][start.1] = true;
    while let Some((row, column)) = pending.pop() {
        if excluded.contains(&(row, column)) {
            continue;
        }
        let Some(sample) = samples[row][column] else {
            continue;
        };
        if sample.hits < minimum_hits || sample.piece != start_sample.piece {
            continue;
        }
        cells.push((row, column));
        for (row_delta, column_delta) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            let next_row = row as isize + row_delta;
            let next_column = column as isize + column_delta;
            if next_row < 0
                || next_column < 0
                || next_row >= samples.len() as isize
                || next_column >= BOARD_WIDTH as isize
            {
                continue;
            }
            let next = (next_row as usize, next_column as usize);
            if !visited[next.0][next.1]
                && !excluded.contains(&next)
                && samples[next.0][next.1].is_some_and(|next_sample| {
                    next_sample.hits >= minimum_hits && next_sample.piece == start_sample.piece
                })
            {
                visited[next.0][next.1] = true;
                pending.push(next);
            }
        }
    }
    cells
}

fn component_summary(
    samples: &[[Option<CellSample>; BOARD_WIDTH]],
    cells: Vec<(usize, usize)>,
) -> ActiveComponent {
    let piece = samples[cells[0].0][cells[0].1]
        .expect("component cells are classified")
        .piece;
    let mut hit_sum = 0.0_f32;
    let mut value_sum = 0.0_f32;
    let mut coverage_sum = 0.0_f32;
    for &(row, column) in &cells {
        let sample = samples[row][column].expect("component cells are classified");
        hit_sum += f32::from(sample.hits);
        value_sum += sample.mean_value;
        coverage_sum +=
            f32::from(sample.hits) / f32::from(sample.sampled_pixels.max(sample.hits).max(1));
    }
    let count = cells.len() as f32;
    let mean_coverage = coverage_sum / count;
    ActiveComponent {
        piece,
        cells,
        mean_hits: hit_sum / count,
        mean_value: value_sum / count,
        confidence: (0.65 + 0.35 * mean_coverage).clamp(0.0, 1.0),
    }
}

fn recognize_next_panel(
    frame: &CapturedFrame,
    board: PixelRect,
    config: &RecognizerConfig,
) -> ([Option<Piece>; 5], [f32; 5]) {
    let cell_width = board.width as f32 / 10.0;
    let cell_height = board.height as f32 / 20.0;
    let rect = PixelRect {
        left: board.left.saturating_add(board.width),
        top: board.top.saturating_add((cell_height * 1.0).round() as u32),
        width: (cell_width * 6.2).round() as u32,
        height: (cell_height * 15.5).round() as u32,
    };
    let points = collect_points(frame, rect, config);
    let runs = vertical_runs(
        &points,
        rect.top,
        rect.top.saturating_add(rect.height),
        cell_width,
        cell_height,
    );
    let mut next = [None; 5];
    let mut confidences = [0.0; 5];
    let mut slot = 0usize;
    for (start, end) in runs {
        if slot >= 5 {
            break;
        }
        let band: Vec<_> = points
            .iter()
            .copied()
            .filter(|point| point.y >= start && point.y <= end)
            .collect();
        let (piece, confidence) = dominant_points(
            &band,
            rect_area(PixelRect {
                left: rect.left,
                top: start,
                width: rect.width,
                height: end.saturating_sub(start).saturating_add(1),
            }),
            preview_minimum_hits(cell_width, cell_height, 0.65),
            0.025,
            0.45,
        );
        if piece.is_some() {
            next[slot] = piece;
            confidences[slot] = confidence;
            slot += 1;
        }
    }
    (next, confidences)
}

fn collect_points(
    frame: &CapturedFrame,
    rect: PixelRect,
    config: &RecognizerConfig,
) -> Vec<ColorPoint> {
    let right = rect.left.saturating_add(rect.width).min(frame.width);
    let bottom = rect.top.saturating_add(rect.height).min(frame.height);
    let mut points = Vec::new();
    for y in rect.top.min(frame.height)..bottom {
        for x in rect.left.min(frame.width)..right {
            let Some(rgb) = frame_pixel(frame, x, y) else {
                continue;
            };
            let Some((index, quality)) = classify_pixel(rgb, config) else {
                continue;
            };
            points.push(ColorPoint { y, index, quality });
        }
    }
    points
}

fn dominant_points(
    points: &[ColorPoint],
    area: u32,
    minimum_hits: usize,
    minimum_coverage: f32,
    minimum_dominance: f32,
) -> (Option<Piece>, f32) {
    if points.len() < minimum_hits || area == 0 {
        return (None, 0.0);
    }
    let coverage = points.len() as f32 / area as f32;
    if coverage < minimum_coverage {
        return (None, 0.0);
    }
    let mut counts = [0usize; 7];
    let mut quality_sum = 0.0_f32;
    for point in points {
        counts[point.index] = counts[point.index].saturating_add(1);
        quality_sum += point.quality;
    }
    let (best_index, best_count) = counts
        .iter()
        .copied()
        .enumerate()
        .max_by_key(|(_, count)| *count)
        .expect("palette has entries");
    let second_count = counts
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, _)| *index != best_index)
        .map(|(_, count)| count)
        .max()
        .unwrap_or_default();
    let dominance = best_count as f32 / points.len() as f32;
    if dominance < minimum_dominance {
        return (None, 0.0);
    }
    let separation = (best_count.saturating_sub(second_count)) as f32 / points.len() as f32;
    let quality = (quality_sum / points.len() as f32).clamp(0.0, 1.0);
    let confidence = (0.65 * dominance + 0.25 * quality + 0.10 * separation).clamp(0.0, 1.0);
    (Some(palette_piece(best_index)), confidence)
}

fn vertical_runs(
    points: &[ColorPoint],
    top: u32,
    bottom: u32,
    cell_width: f32,
    cell_height: f32,
) -> Vec<(u32, u32)> {
    let mut counts = vec![0u16; bottom.saturating_sub(top).saturating_add(1) as usize];
    for point in points {
        if point.y >= top && point.y <= bottom {
            let index = point.y.saturating_sub(top) as usize;
            if let Some(count) = counts.get_mut(index) {
                *count = count.saturating_add(1);
            }
        }
    }
    let mut runs = Vec::new();
    let mut start = None;
    let mut last = 0u32;
    for (index, count) in counts.into_iter().enumerate() {
        let y = top.saturating_add(index as u32);
        if count >= (cell_width * 0.55).round().max(3.0) as u16 {
            if start.is_none() {
                start = Some(y);
            }
            last = y;
        } else if let Some(run_start) = start.take()
            && last.saturating_sub(run_start) + 1 >= (cell_height * 0.55).round().max(4.0) as u32
        {
            runs.push((run_start, last));
        }
    }
    if let Some(run_start) = start
        && last.saturating_sub(run_start) + 1 >= (cell_height * 0.55).round().max(4.0) as u32
    {
        runs.push((run_start, last));
    }
    runs
}

fn preview_minimum_hits(cell_width: f32, cell_height: f32, cell_area_factor: f32) -> usize {
    (cell_width * cell_height * cell_area_factor)
        .round()
        .max(30.0) as usize
}

fn rect_area(rect: PixelRect) -> u32 {
    rect.width.saturating_mul(rect.height)
}

#[cfg(test)]
fn recognize_region(
    frame: &CapturedFrame,
    rect: PixelRect,
    config: &RecognizerConfig,
    crop: RegionCrop,
) -> (Option<Piece>, f32) {
    let left = rect
        .left
        .saturating_add((rect.width as f32 * crop.margin_x) as u32);
    let top = rect
        .top
        .saturating_add((rect.height as f32 * crop.margin_y) as u32);
    let right = rect.left.saturating_add(
        rect.width
            .saturating_sub((rect.width as f32 * crop.margin_x) as u32),
    );
    let bottom = rect.top.saturating_add(
        rect.height
            .saturating_sub((rect.height as f32 * crop.margin_y) as u32),
    );
    let mut counts = [0u32; 7];
    let mut classified = 0u32;
    let mut quality_sum = 0.0_f32;
    for y in top..bottom.min(frame.height) {
        for x in left..right.min(frame.width) {
            let Some(rgb) = frame_pixel(frame, x, y) else {
                continue;
            };
            let Some((index, quality)) = classify_pixel(rgb, config) else {
                continue;
            };
            counts[index] = counts[index].saturating_add(1);
            classified = classified.saturating_add(1);
            quality_sum += quality;
        }
    }
    if classified == 0 {
        return (None, 0.0);
    }
    let sample_width = right.saturating_sub(left);
    let sample_height = bottom.saturating_sub(top);
    let sample_count = sample_width.saturating_mul(sample_height);
    let coverage = if sample_count == 0 {
        0.0
    } else {
        classified as f32 / sample_count as f32
    };
    if coverage < config.minimum_region_coverage {
        return (None, 0.0);
    }
    let (best_index, best_count) = counts
        .iter()
        .copied()
        .enumerate()
        .max_by_key(|(_, count)| *count)
        .expect("palette has entries");
    let second_count = counts
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, _)| *index != best_index)
        .map(|(_, count)| count)
        .max()
        .unwrap_or_default();
    let dominance = best_count as f32 / classified as f32;
    let quality = (quality_sum / classified as f32).clamp(0.0, 1.0);
    let separation =
        ((best_count.saturating_sub(second_count)) as f32 / classified as f32).clamp(0.0, 1.0);
    let confidence = (0.65 * dominance + 0.25 * quality + 0.10 * separation).clamp(0.0, 1.0);
    (Some(palette_piece(best_index)), confidence)
}

fn classify_cell(
    frame: &CapturedFrame,
    rect: PixelRect,
    config: &RecognizerConfig,
) -> Option<CellSample> {
    let margin_x = (rect.width / 5).max(1);
    let margin_y = (rect.height / 5).max(1);
    let left = rect.left.saturating_add(margin_x);
    let top = rect.top.saturating_add(margin_y);
    let right = rect
        .left
        .saturating_add(rect.width.saturating_sub(margin_x));
    let bottom = rect
        .top
        .saturating_add(rect.height.saturating_sub(margin_y));
    let mut counts = [0u16; 7];
    let mut classified = 0u16;
    let mut sampled_pixels = 0u16;
    let mut value_sums = [0u32; 7];
    for y in top..bottom.min(frame.height) {
        for x in left..right.min(frame.width) {
            let Some(rgb) = frame_pixel(frame, x, y) else {
                continue;
            };
            sampled_pixels = sampled_pixels.saturating_add(1);
            let Some((index, _)) = classify_pixel(rgb, config) else {
                continue;
            };
            counts[index] = counts[index].saturating_add(1);
            value_sums[index] = value_sums[index].saturating_add(u32::from(rgb.max()));
            classified = classified.saturating_add(1);
        }
    }
    (classified > 0).then(|| {
        let (index, hits) = counts
            .iter()
            .copied()
            .enumerate()
            .max_by_key(|(_, count)| *count)
            .expect("palette has entries");
        CellSample {
            piece: palette_piece(index),
            hits,
            sampled_pixels,
            mean_value: value_sums[index] as f32 / f32::from(hits.max(1)),
        }
    })
}

fn classify_pixel(rgb: Rgb, config: &RecognizerConfig) -> Option<(usize, f32)> {
    if rgb.max() < config.minimum_value || rgb.chroma() < config.minimum_chroma {
        return None;
    }
    let (index, distance) = config
        .palette
        .entries()
        .into_iter()
        .enumerate()
        .map(|(index, (_, colour))| (index, rgb.distance_squared(colour)))
        .min_by_key(|(_, distance)| *distance)?;
    let max_distance = 145_u32 * 145;
    if distance <= max_distance {
        return Some((index, 1.0 - (distance as f32 / max_distance as f32)));
    }
    hue_piece(rgb)
}

fn hue_piece(rgb: Rgb) -> Option<(usize, f32)> {
    let max = f32::from(rgb.max());
    let min = f32::from(rgb.min());
    let delta = max - min;
    if delta < 1.0 {
        return None;
    }
    let red = f32::from(rgb.r);
    let green = f32::from(rgb.g);
    let blue = f32::from(rgb.b);
    let hue = if (max - red).abs() < f32::EPSILON {
        60.0 * ((green - blue) / delta).rem_euclid(6.0)
    } else if (max - green).abs() < f32::EPSILON {
        60.0 * (((blue - red) / delta) + 2.0)
    } else {
        60.0 * (((red - green) / delta) + 4.0)
    };
    let (index, center, half_width) = if !(10.0..340.0).contains(&hue) {
        (6, 0.0, 30.0) // Z: red
    } else if hue < 35.0 {
        (2, 22.0, 20.0) // L: orange
    } else if hue < 65.0 {
        (3, 50.0, 18.0) // O: yellow
    } else if hue < 145.0 {
        (4, 95.0, 50.0) // S: green
    } else if hue < 205.0 {
        (0, 175.0, 35.0) // I: cyan
    } else if hue < 280.0 {
        (1, 245.0, 45.0) // J: blue-violet
    } else {
        (5, 310.0, 35.0) // T: magenta
    };
    let distance = (hue - center).abs().min(360.0 - (hue - center).abs());
    let confidence = (0.60 + 0.35 * (1.0 - distance / half_width).clamp(0.0, 1.0)).clamp(0.0, 1.0);
    Some((index, confidence))
}

fn palette_piece(index: usize) -> Piece {
    match index {
        0 => Piece::I,
        1 => Piece::J,
        2 => Piece::L,
        3 => Piece::O,
        4 => Piece::S,
        5 => Piece::T,
        _ => Piece::Z,
    }
}

#[cfg(test)]
fn cell_rect(
    board: PixelRect,
    cell_width: f32,
    cell_height: f32,
    column: usize,
    row: usize,
) -> PixelRect {
    let left = board.left as f32 + column as f32 * cell_width;
    let top = board.top as f32 + row as f32 * cell_height;
    let right = board.left as f32 + (column + 1) as f32 * cell_width;
    let bottom = board.top as f32 + (row + 1) as f32 * cell_height;
    PixelRect {
        left: left.round() as u32,
        top: top.round() as u32,
        width: (right.round() as u32).saturating_sub(left.round() as u32),
        height: (bottom.round() as u32).saturating_sub(top.round() as u32),
    }
}

/// Cells sampled above the visible board for Current detection.
const SPAWN_ROWS: usize = 3;

fn spawn_cell_rect(
    board: PixelRect,
    cell_width: f32,
    cell_height: f32,
    column: usize,
    top: f32,
) -> PixelRect {
    let left = board.left as f32 + column as f32 * cell_width;
    let right = board.left as f32 + (column + 1) as f32 * cell_width;
    let bottom = top + cell_height;
    PixelRect {
        left: left.round() as u32,
        top: top.round() as u32,
        width: (right.round() as u32).saturating_sub(left.round() as u32),
        height: (bottom.round() as u32).saturating_sub(top.round() as u32),
    }
}

/// TETR.IO outlines the playfield's left, right, and bottom edges with a thin
/// near-white line (the top is open).
fn is_border_pixel(rgb: Rgb) -> bool {
    rgb.min() >= 140
}

/// Snap a calibrated board rectangle to the visible border lines. Searches
/// within ~0.6 cell of each calibrated edge for the line that is near-white
/// along most of its length, then keeps cells square by anchoring the
/// 20-row height on the bottom border. Falls back to the calibrated
/// rectangle when any edge is missing or the width disagrees by over 6%.
fn refine_board_rect(frame: &CapturedFrame, calibrated: PixelRect) -> PixelRect {
    let cell = calibrated.width as f32 / 10.0;
    let reach = (cell * 0.6).round().max(2.0) as i64;
    let width = i64::from(frame.width);
    let height = i64::from(frame.height);
    let column_hits = |x: i64, top: i64, bottom: i64| -> usize {
        if x < 0 || x >= width {
            return 0;
        }
        (top.max(0)..bottom.min(height))
            .filter(|&y| frame_pixel(frame, x as u32, y as u32).is_some_and(is_border_pixel))
            .count()
    };
    let row_hits = |y: i64, left: i64, right: i64| -> usize {
        if y < 0 || y >= height {
            return 0;
        }
        (left.max(0)..right.min(width))
            .filter(|&x| frame_pixel(frame, x as u32, y as u32).is_some_and(is_border_pixel))
            .count()
    };
    // Choose the strongest line, then step inward across any adjacent line
    // pixels; the result is the first interior column/row.
    let snap = |centre: i64, inward: i64, hits: &dyn Fn(i64) -> usize, required: usize| {
        let best = (centre - reach..=centre + reach).max_by_key(|&position| hits(position))?;
        if hits(best) < required {
            return None;
        }
        let mut edge = best;
        while hits(edge + inward) >= required {
            edge += inward;
        }
        Some(edge + inward)
    };

    let left = i64::from(calibrated.left);
    let right = left + i64::from(calibrated.width);
    let bottom = i64::from(calibrated.top) + i64::from(calibrated.height);
    // Use the lower half for the side borders: the HOLD/NEXT panels join the
    // board near the top.
    let side_top = i64::from(calibrated.top) + i64::from(calibrated.height) / 2;
    let side_bottom = bottom - cell.round() as i64;
    let side_required = ((side_bottom - side_top).max(1) as f32 * 0.6) as usize;
    let side_hits = |x: i64| column_hits(x, side_top, side_bottom);
    let Some(inner_left) = snap(left, 1, &side_hits, side_required) else {
        return calibrated;
    };
    // For the right and bottom edges `snap` returns the last interior
    // column/row; add one for an exclusive bound.
    let Some(inner_right) = snap(right, -1, &side_hits, side_required).map(|x| x + 1) else {
        return calibrated;
    };
    let inner_width = inner_right - inner_left;
    if inner_width <= 0
        || (inner_width - i64::from(calibrated.width)).abs() as f32 > calibrated.width as f32 * 0.06
    {
        return calibrated;
    }
    let bottom_required = (inner_width as f32 * 0.6) as usize;
    let bottom_hits = |y: i64| row_hits(y, inner_left, inner_right);
    let Some(inner_bottom) = snap(bottom, -1, &bottom_hits, bottom_required).map(|y| y + 1) else {
        return calibrated;
    };
    let inner_height = (inner_width as f32 * 2.0).round() as i64;
    let top = inner_bottom - inner_height;
    if top < 0 {
        return calibrated;
    }
    PixelRect {
        left: inner_left as u32,
        top: top as u32,
        width: inner_width as u32,
        height: inner_height as u32,
    }
}

fn frame_pixel(frame: &CapturedFrame, x: u32, y: u32) -> Option<Rgb> {
    if x >= frame.width || y >= frame.height {
        return None;
    }
    let index = ((y as usize) * (frame.width as usize) + x as usize) * 4;
    let pixel = frame.rgba.get(index..index + 4)?;
    Some(Rgb::new(pixel[0], pixel[1], pixel[2]))
}

pub fn piece_code(piece: Piece) -> char {
    match piece {
        Piece::I => 'I',
        Piece::J => 'J',
        Piece::L => 'L',
        Piece::O => 'O',
        Piece::S => 'S',
        Piece::T => 'T',
        Piece::Z => 'Z',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, colour: Rgb) -> CapturedFrame {
        let mut rgba = vec![0; (width * height * 4) as usize];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[colour.r, colour.g, colour.b, 255]);
        }
        CapturedFrame {
            width,
            height,
            rgba,
            captured_at_ms: 0,
        }
    }

    #[test]
    fn palette_classifies_reference_colours() {
        let config = RecognizerConfig::default();
        for (piece, colour) in config.palette.entries() {
            let (index, _) = classify_pixel(colour, &config).unwrap();
            assert_eq!(palette_piece(index), piece);
        }
    }

    #[test]
    fn malformed_frame_is_rejected_before_sampling() {
        let recognizer = TetrioRecognizer::new(RecognizerConfig::default()).unwrap();
        let mut bad = frame(4, 4, Rgb::new(0, 0, 0));
        bad.rgba.pop();
        assert_eq!(
            recognizer.recognize(&bad),
            Err(RecognitionError::MalformedFrame)
        );
    }

    #[test]
    fn sparse_coloured_border_does_not_become_a_hold_piece() {
        let config = RecognizerConfig::default();
        let mut frame = frame(20, 20, Rgb::new(0, 0, 0));
        frame.rgba[0..4].copy_from_slice(&[
            config.palette.j.r,
            config.palette.j.g,
            config.palette.j.b,
            255,
        ]);
        let (piece, confidence) = recognize_region(
            &frame,
            PixelRect {
                left: 0,
                top: 0,
                width: 20,
                height: 20,
            },
            &config,
            RegionCrop::PREVIEW,
        );
        assert_eq!(piece, None);
        assert_eq!(confidence, 0.0);
    }

    #[test]
    fn board_relative_panel_search_finds_all_five_next_slots() {
        let config = RecognizerConfig::default();
        let frame_size = FrameSize {
            width: 1920,
            height: 1009,
        };
        let board = config.board_calibration.resolve(frame_size);
        let mut captured = frame(frame_size.width, frame_size.height, Rgb::new(0, 0, 0));
        let cell_width = board.width as f32 / 10.0;
        let cell_height = board.height as f32 / 20.0;

        let hold_left = board.left.saturating_sub((cell_width * 5.8) as u32);
        fill_colour(
            &mut captured,
            hold_left.saturating_add(16),
            board.top.saturating_add(14),
            58,
            34,
            config.palette.o,
        );
        for (row, column) in [(2, 4), (3, 3), (3, 4), (3, 5)] {
            fill_board_cell(
                &mut captured,
                board,
                cell_width,
                cell_height,
                row,
                column,
                config.palette.t,
            );
        }

        let pieces = [
            (Piece::I, config.palette.i),
            (Piece::J, config.palette.j),
            (Piece::L, config.palette.l),
            (Piece::S, config.palette.s),
            (Piece::Z, config.palette.z),
        ];
        for (index, (_, colour)) in pieces.into_iter().enumerate() {
            fill_colour(
                &mut captured,
                board.left.saturating_add(board.width).saturating_add(12),
                board
                    .top
                    .saturating_add(cell_height as u32)
                    .saturating_add((index as u32) * (cell_height as u32 * 3)),
                46,
                24,
                colour,
            );
        }

        let result = TetrioRecognizer::new(config)
            .unwrap()
            .recognize(&captured)
            .unwrap();
        assert_eq!(result.hold, Some(Piece::O));
        assert_eq!(result.current, Some(Piece::T));
        assert_eq!(result.board_cells, 0);
        assert_eq!(
            result.next5,
            [
                Some(Piece::I),
                Some(Piece::J),
                Some(Piece::L),
                Some(Piece::S),
                Some(Piece::Z),
            ]
        );
    }

    #[test]
    fn board_calibration_uses_square_cells_at_a_different_display_scale() {
        let frame_size = FrameSize {
            width: 948,
            height: 957,
        };
        let calibration = BoardCalibration::from_pixels(frame_size, 474, 407, 94).unwrap();
        assert_eq!(
            calibration.resolve(frame_size),
            PixelRect {
                left: 427,
                top: 407,
                width: 94,
                height: 188,
            }
        );
    }

    fn fill_colour(
        frame: &mut CapturedFrame,
        left: u32,
        top: u32,
        width: u32,
        height: u32,
        colour: Rgb,
    ) {
        for y in top..top.saturating_add(height).min(frame.height) {
            for x in left..left.saturating_add(width).min(frame.width) {
                let index = ((y as usize) * frame.width as usize + x as usize) * 4;
                frame.rgba[index..index + 4].copy_from_slice(&[colour.r, colour.g, colour.b, 255]);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn fill_board_cell(
        frame: &mut CapturedFrame,
        board: PixelRect,
        cell_width: f32,
        cell_height: f32,
        row: usize,
        column: usize,
        colour: Rgb,
    ) {
        let rect = cell_rect(board, cell_width, cell_height, column, row);
        fill_colour(frame, rect.left, rect.top, rect.width, rect.height, colour);
    }

    fn cell_sample(piece: Piece, hits: u16, mean_value: f32) -> CellSample {
        CellSample {
            piece,
            hits,
            sampled_pixels: 100,
            mean_value,
        }
    }

    #[test]
    fn active_and_dim_ghost_components_are_not_fixed_board_cells() {
        let mut samples = [[None; BOARD_WIDTH]; BOARD_HEIGHT];
        for row in samples.iter_mut().take(6).skip(2) {
            row[0] = Some(cell_sample(Piece::I, 95, 190.0));
        }
        for row in samples.iter_mut().take(19).skip(15) {
            row[0] = Some(cell_sample(Piece::I, 65, 105.0));
        }
        let active = find_active_component(&samples, 60, 8).unwrap();
        assert_eq!(active.piece, Piece::I);
        assert_eq!(active.cells.len(), 4);
        let ghosts = find_ghost_cells(&samples, &active, 60);
        assert_eq!(ghosts.len(), 4);
        assert!(ghosts.iter().all(|(row, _)| *row >= 15));
    }

    #[test]
    fn equally_bright_fixed_component_is_not_removed_as_a_ghost() {
        let mut samples = [[None; BOARD_WIDTH]; BOARD_HEIGHT];
        for row in samples.iter_mut().take(6).skip(2) {
            row[0] = Some(cell_sample(Piece::I, 95, 190.0));
        }
        for row in samples.iter_mut().take(19).skip(15) {
            row[0] = Some(cell_sample(Piece::I, 90, 185.0));
        }
        let active = find_active_component(&samples, 60, 8).unwrap();
        assert!(find_ghost_cells(&samples, &active, 60).is_empty());
    }

    #[test]
    fn dimmer_locked_piece_in_other_columns_is_not_a_ghost() {
        let mut samples = [[None; BOARD_WIDTH]; BOARD_HEIGHT];
        for row in samples.iter_mut().take(6).skip(2) {
            row[4] = Some(cell_sample(Piece::I, 95, 190.0));
        }
        // Same piece type and dimmer, as TETR.IO draws locked blocks, but
        // not directly below Current.
        for row in samples.iter_mut().take(20).skip(16) {
            row[0] = Some(cell_sample(Piece::I, 95, 130.0));
        }
        let active = find_active_component(&samples, 60, 8).unwrap();
        assert!(find_ghost_cells(&samples, &active, 60).is_empty());
    }

    /// Yellow text across image rows 7-10, plus a faint edge at row 6.
    fn banner_samples() -> [[Option<CellSample>; BOARD_WIDTH]; BOARD_HEIGHT] {
        let mut samples = [[None; BOARD_WIDTH]; BOARD_HEIGHT];
        for row in samples.iter_mut().take(11).skip(7) {
            for cell in row.iter_mut().take(9).skip(1) {
                *cell = Some(cell_sample(Piece::O, 85, 200.0));
            }
        }
        samples[6][3] = Some(cell_sample(Piece::O, 14, 150.0));
        samples
    }

    #[test]
    fn all_clear_text_above_an_empty_row_is_the_banner() {
        let samples = banner_samples();
        let banner = find_all_clear_banner(&samples, &[], 40);
        assert_eq!(banner.len(), 4 * 8 + 1);

        // Pieces placed while the text is still up form the board below it.
        let mut samples = banner_samples();
        for cell in samples[19].iter_mut().take(6) {
            *cell = Some(cell_sample(Piece::L, 120, 150.0));
        }
        samples[18][0] = Some(cell_sample(Piece::O, 120, 150.0));
        let banner = find_all_clear_banner(&samples, &[], 40);
        assert_eq!(banner.len(), 4 * 8 + 1);
        assert!(banner.iter().all(|&(row, _)| row < 18));
    }

    #[test]
    fn a_board_resting_on_the_floor_is_not_the_banner() {
        // Yellow cells stacked up from the floor are placed O pieces.
        let mut samples = [[None; BOARD_WIDTH]; BOARD_HEIGHT];
        for row in samples.iter_mut().skip(16) {
            row[0] = Some(cell_sample(Piece::O, 120, 150.0));
        }
        assert!(find_all_clear_banner(&samples, &[], 40).is_empty());

        // Other colours above an empty row are not the yellow text.
        let mut samples = banner_samples();
        samples[8][4] = Some(cell_sample(Piece::T, 85, 200.0));
        assert!(find_all_clear_banner(&samples, &[], 40).is_empty());

        // The active piece in the spawn rows is left to its own mask.
        let mut samples = [[None; BOARD_WIDTH]; BOARD_HEIGHT];
        samples[0][4] = Some(cell_sample(Piece::O, 120, 200.0));
        samples[0][5] = Some(cell_sample(Piece::O, 120, 200.0));
        assert!(find_all_clear_banner(&samples, &[(0, 4), (0, 5)], 40).is_empty());
    }

    #[test]
    fn cell_coordinates_are_converted_to_bottom_left_board_space() {
        let rect = PixelRect {
            left: 0,
            top: 0,
            width: 100,
            height: 200,
        };
        assert_eq!(cell_rect(rect, 10.0, 10.0, 2, 3).top, 30);
    }
}
