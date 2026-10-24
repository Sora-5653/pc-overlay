//! Reads ZEN's PIECES counter (placed pieces), drawn left of the board when
//! the statistics option is on. A perfect-clear loop places 10 pieces per
//! PC, so the counter gives the PC number independently of the tracker.
//!
//! Which statistics ZEN shows, and in what order, depends on its settings,
//! so nothing is read from a fixed position. The text left of the board is
//! split into lines; the counter is the large number before the comma on the
//! line right below the "PIECES" label ("16, 0.04/S"). Without that label,
//! or with any glyph that does not match its template, the result is `None`
//! and the caller keeps its own count.

use crate::{capture::CapturedFrame, roi::PixelRect};

/// Template grid size in cells (columns x rows) over a glyph's bounding box.
const GRID_COLUMNS: usize = 6;
const GRID_ROWS: usize = 9;
/// Intensity ramp used to write the templates as text.
const RAMP: &str = " .:-=+*#%@";

/// Normalized intensity of each digit except 1 (told apart by its narrow
/// width). TETR.IO scales this text with the board, so each template is the
/// mean over recorded ZEN frames resampled to 0.45-2.3x (5 from a counter at
/// 150, resampled to 0.55-2.3x). 8 never appeared in the counter, so its
/// template comes from the score below the board at a single scale; the
/// score's 5 differed from the counter's (a dim top bar, a lit lower left)
/// and left every count with a 5 unread.
#[rustfmt::skip]
const DIGIT_TEMPLATES: [(u32, [&str; GRID_ROWS]); 9] = [
    (0, [
        ".+%%*.",
        "*@##@*",
        "%%..%%",
        "@# .#%",
        "@# .#%",
        "@# .#%",
        "%#..%%",
        "*@#*@*",
        ".*%%+.",
    ]),
    (2, [
        ".*%%*.",
        "*@##@#",
        "*+ .#@",
        "   -%%",
        "  :%%-",
        " :%%- ",
        ":#@-  ",
        "#@%**+",
        "%@@@@%",
    ]),
    (3, [
        ".*@%*.",
        "*@##@*",
        "--.:%%",
        "  :+%#",
        " .%@@*",
        "  :=%%",
        "*= .#%",
        "#@**%#",
        ":#@%*:",
    ]),
    (4, [
        "  :%- ",
        "  *%. ",
        " :@+  ",
        " *%:  ",
        ":@*-+.",
        "*@-*@:",
        "@@#%@*",
        "*##%@#",
        "   +%.",
    ]),
    (5, [
        "#@@@%-",
        "@@*++:",
        "@%:.  ",
        "@@@%*.",
        "%%**@*",
        ".. .%%",
        "*+..%%",
        "*@#%@+",
        ".+##+ ",
    ]),
    (6, [
        ".+%%*.",
        "*@**@+",
        "%%:.::",
        "%@@%*.",
        "%@**@#",
        "%%..%@",
        "%%..%@",
        "*@##@*",
        ".*%%*.",
    ]),
    (7, [
        "#@@@@#",
        "@@*#@#",
        "*= =%=",
        "  :%*.",
        "  +%: ",
        " :@+  ",
        " *%.  ",
        "=@=   ",
        "**.  .",
    ]),
    (8, [
        ".#%%* ",
        "*@%%@+",
        "%%::%#",
        "#%=+%#",
        "#@##@*",
        "%@--%#",
        "@%..%%",
        "*@##@*",
        ".#@@%.",
    ]),
    (9, [
        ".*%%+.",
        "*@#*%*",
        "%# .%%",
        "%# .%%",
        "*@#*@%",
        ".*%%@%",
        ":-.:%%",
        "+@#*%+",
        ".+%%+.",
    ]),
];

// Distances are sums of squared differences over a grid laid on the glyph's
// own bounding box, with intensity scaled to its brightest cell, so they do
// not grow with the display scale. Resampling still blurs small text: from
// 0.7x (a board ~135 px wide) recorded digits stay within 3 of their own
// template and at least 1.6x farther from any other; below that, digits
// drift past 10 and are rejected rather than guessed.

/// A digit match is accepted only when it is this close to its template ...
const MAX_DISTANCE: f32 = 4.0;
/// ... and the runner-up is this many times farther.
const MIN_RATIO: f32 = 1.4;
/// Digits are about 0.58 as wide as tall; 1 is about 0.26.
const ONE_MAX_ASPECT: f32 = 0.4;

/// The "PIECES" label over its bounding box, in the same ramp.
const LABEL_COLUMNS: usize = 24;
#[rustfmt::skip]
const PIECES_LABEL: [&str; 5] = [
    "%%+  +: +%*.:##::%#-.+%+",
    "%+%. #- *+  ++. :#: .#+-",
    "@%=  #- *%: ++  :@+  -#*",
    "#.   #- *+  ++..:#: .:-%",
    "+    *: +%*.-##::%#=.*%*",
];
/// From 0.7x, recorded "PIECES" labels score up to ~5 and "ATTACK", the
/// other six-letter label, at least ~12.
const LABEL_MAX_DISTANCE: f32 = 8.0;

/// Placed pieces shown by the PIECES counter, or `None` when it cannot be
/// read with confidence (option off, label missing, or an unfamiliar glyph).
pub fn read_pieces_placed(frame: &CapturedFrame, board: PixelRect) -> Option<u32> {
    let cell = board.width as f32 / 10.0;
    if cell < 4.0 {
        return None;
    }
    // The statistics are right-aligned just left of the board; a five-digit
    // counter line stays within 7 cells.
    let band = Band {
        left: (board.left as f32 - 7.5 * cell).max(0.0) as u32,
        right: (board.left as f32 - 0.3 * cell).max(0.0) as u32,
        top: board.top,
        bottom: board.top + board.height,
    };
    if band.right <= band.left {
        return None;
    }
    let lines = runs(band.top..band.bottom, |y| {
        (band.left..band.right).any(|x| lit(frame, x, y))
    });
    let label = lines
        .iter()
        .position(|&(top, bottom)| is_pieces_label(frame, &band, top, bottom, cell))?;
    let label_bottom = lines[label].1;
    let &(value_top, value_bottom) = lines.get(label + 1)?;
    let value_height = (value_bottom - value_top) as f32;
    if (value_top - label_bottom) as f32 > cell
        || !(0.75 * cell..=1.25 * cell).contains(&value_height)
    {
        return None;
    }

    let glyphs = glyph_boxes(frame, &band, value_top, value_bottom);
    let digit_count = glyphs
        .iter()
        .take_while(|glyph| glyph.height() as f32 >= 0.7 * cell)
        .count();
    // The digits must be followed by the small comma.
    let comma = glyphs.get(digit_count)?;
    if digit_count == 0 || digit_count > 5 || comma.height() as f32 > 0.4 * cell {
        return None;
    }
    glyphs[..digit_count].iter().try_fold(0u32, |value, glyph| {
        Some(value * 10 + classify_digit(frame, glyph)?)
    })
}

/// Whether the line's rightmost word is the six-letter "PIECES" label.
fn is_pieces_label(frame: &CapturedFrame, band: &Band, top: u32, bottom: u32, cell: f32) -> bool {
    let height = (bottom - top) as f32;
    if !(0.3 * cell..=0.65 * cell).contains(&height) {
        return false;
    }
    let glyphs = glyph_boxes(frame, band, top, bottom);
    // Letters are about 0.2 cell apart, words more than half a cell.
    let word_start = glyphs
        .windows(2)
        .rposition(|pair| (pair[1].left - pair[0].right) as f32 > 0.5 * cell)
        .map_or(0, |index| index + 1);
    let word = &glyphs[word_start..];
    if word.len() != 6 {
        return false;
    }
    let bounds = GlyphBox {
        left: word[0].left,
        right: word[5].right,
        top,
        bottom,
    };
    sample_grid(frame, &bounds, LABEL_COLUMNS, PIECES_LABEL.len())
        .is_some_and(|grid| template_distance(&grid, &PIECES_LABEL) <= LABEL_MAX_DISTANCE)
}

struct Band {
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
}

#[derive(Debug, Clone, Copy)]
struct GlyphBox {
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
}

impl GlyphBox {
    fn width(&self) -> u32 {
        self.right - self.left
    }

    fn height(&self) -> u32 {
        self.bottom - self.top
    }
}

/// White text, including its red/cyan chromatic-aberration fringes.
fn lit(frame: &CapturedFrame, x: u32, y: u32) -> bool {
    luminance(frame, x, y) >= 110.0
}

fn luminance(frame: &CapturedFrame, x: u32, y: u32) -> f32 {
    if x >= frame.width || y >= frame.height {
        return 0.0;
    }
    let index = ((y as usize) * (frame.width as usize) + x as usize) * 4;
    frame.rgba.get(index..index + 3).map_or(0.0, |pixel| {
        pixel.iter().map(|&value| f32::from(value)).sum::<f32>() / 3.0
    })
}

/// Maximal runs of consecutive positions where `hit` holds, as half-open
/// ranges in order.
fn runs(range: std::ops::Range<u32>, hit: impl Fn(u32) -> bool) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    let mut start = None;
    for position in range.start..=range.end {
        let on = position < range.end && hit(position);
        match (on, start) {
            (true, None) => start = Some(position),
            (false, Some(from)) => {
                found.push((from, position));
                start = None;
            }
            _ => {}
        }
    }
    found
}

/// Glyphs of the line from left to right, split at blank columns and
/// trimmed to their lit rows.
fn glyph_boxes(frame: &CapturedFrame, band: &Band, top: u32, bottom: u32) -> Vec<GlyphBox> {
    runs(band.left..band.right, |x| {
        (top..bottom).any(|y| lit(frame, x, y))
    })
    .into_iter()
    .map(|(left, right)| {
        let rows = runs(top..bottom, |y| (left..right).any(|x| lit(frame, x, y)));
        GlyphBox {
            left,
            right,
            top: rows.first().map_or(top, |row| row.0),
            bottom: rows.last().map_or(bottom, |row| row.1),
        }
    })
    .collect()
}

fn classify_digit(frame: &CapturedFrame, glyph: &GlyphBox) -> Option<u32> {
    if (glyph.width() as f32) < ONE_MAX_ASPECT * glyph.height() as f32 {
        return Some(1);
    }
    best_digit(&sample_grid(frame, glyph, GRID_COLUMNS, GRID_ROWS)?)
}

/// The digit whose template is clearly the closest, if any.
fn best_digit(grid: &[f32]) -> Option<u32> {
    let mut distances: Vec<(f32, u32)> = DIGIT_TEMPLATES
        .iter()
        .map(|(digit, template)| (template_distance(grid, template), *digit))
        .collect();
    distances.sort_by(|left, right| left.0.total_cmp(&right.0));
    let (best, digit) = distances[0];
    let second = distances[1].0;
    (best <= MAX_DISTANCE && second >= MIN_RATIO * best).then_some(digit)
}

/// Mean intensity of each cell of a `columns` x `rows` grid over the box,
/// row by row, 4x4 supersampled and scaled so the brightest cell is 1.
fn sample_grid(
    frame: &CapturedFrame,
    glyph: &GlyphBox,
    columns: usize,
    rows: usize,
) -> Option<Vec<f32>> {
    const SUBSAMPLES: usize = 4;
    let cell_width = glyph.width() as f32 / columns as f32;
    let cell_height = glyph.height() as f32 / rows as f32;
    let mut grid = Vec::with_capacity(columns * rows);
    for row in 0..rows {
        for column in 0..columns {
            let mut total = 0.0;
            for i in 0..SUBSAMPLES {
                for j in 0..SUBSAMPLES {
                    let x = glyph.left as f32
                        + cell_width * (column as f32 + (i as f32 + 0.5) / SUBSAMPLES as f32);
                    let y = glyph.top as f32
                        + cell_height * (row as f32 + (j as f32 + 0.5) / SUBSAMPLES as f32);
                    total +=
                        ((luminance(frame, x as u32, y as u32) - 60.0) / 100.0).clamp(0.0, 1.0);
                }
            }
            grid.push(total / (SUBSAMPLES * SUBSAMPLES) as f32);
        }
    }
    let peak = grid.iter().copied().fold(0.0, f32::max);
    if peak <= 0.0 {
        return None;
    }
    grid.iter_mut().for_each(|value| *value /= peak);
    Some(grid)
}

/// Sum of squared differences between a sampled grid and a text template.
fn template_distance(grid: &[f32], template: &[&str]) -> f32 {
    let steps = (RAMP.len() - 1) as f32;
    grid.iter()
        .zip(template.iter().flat_map(|row| row.chars()))
        .map(|(value, symbol)| {
            let expected = RAMP.find(symbol).unwrap_or(0) as f32 / steps;
            (value - expected).powi(2)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template_grid(template: &[&str]) -> Vec<f32> {
        let steps = (RAMP.len() - 1) as f32;
        template
            .iter()
            .flat_map(|row| row.chars())
            .map(|symbol| RAMP.find(symbol).unwrap() as f32 / steps)
            .collect()
    }

    #[test]
    fn templates_are_well_formed() {
        for (digit, template) in DIGIT_TEMPLATES {
            for row in template {
                assert_eq!(row.chars().count(), GRID_COLUMNS, "digit {digit}");
                assert!(
                    row.chars().all(|symbol| RAMP.contains(symbol)),
                    "digit {digit}"
                );
            }
        }
        for row in PIECES_LABEL {
            assert_eq!(row.chars().count(), LABEL_COLUMNS);
            assert!(row.chars().all(|symbol| RAMP.contains(symbol)));
        }
    }

    #[test]
    fn every_template_is_accepted_as_its_own_digit() {
        for (digit, template) in DIGIT_TEMPLATES {
            assert_eq!(best_digit(&template_grid(&template)), Some(digit));
        }
    }

    #[test]
    fn blank_frame_has_no_counter() {
        let frame = CapturedFrame {
            width: 400,
            height: 400,
            rgba: vec![0; 400 * 400 * 4],
            captured_at_ms: 0,
        };
        let board = PixelRect {
            left: 200,
            top: 0,
            width: 190,
            height: 380,
        };
        assert_eq!(read_pieces_placed(&frame, board), None);
    }
}
