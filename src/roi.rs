//! Window-relative regions of interest. Capture code can resolve these against
//! a TETR.IO client rectangle without baking a particular window size into the
//! recognizer.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormalizedRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl NormalizedRect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn validate(&self, path: &str) -> Result<(), RoiValidationError> {
        let values = [self.x, self.y, self.width, self.height];
        if values.iter().any(|value| !value.is_finite()) {
            return Err(RoiValidationError::Invalid(format!(
                "{path} contains a non-finite value"
            )));
        }
        if self.x < 0.0
            || self.y < 0.0
            || self.width <= 0.0
            || self.height <= 0.0
            || self.x + self.width > 1.0
            || self.y + self.height > 1.0
        {
            return Err(RoiValidationError::Invalid(format!(
                "{path} must be a positive rectangle inside [0,1]"
            )));
        }
        Ok(())
    }

    pub fn resolve(self, frame: FrameSize) -> PixelRect {
        let left = (self.x * frame.width as f32).round() as u32;
        let top = (self.y * frame.height as f32).round() as u32;
        let right = ((self.x + self.width) * frame.width as f32).round() as u32;
        let bottom = ((self.y + self.height) * frame.height as f32).round() as u32;
        PixelRect {
            left,
            top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRect {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoiLayout {
    pub board: NormalizedRect,
    pub hold: NormalizedRect,
    pub current: NormalizedRect,
    pub next5: [NormalizedRect; 5],
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum RoiValidationError {
    #[error("invalid ROI: {0}")]
    Invalid(String),
    #[error("ROI regions overlap: {0} and {1}")]
    Overlap(String, String),
}

impl RoiLayout {
    pub fn validate(&self) -> Result<(), RoiValidationError> {
        let regions = self.regions();
        for (name, rect) in &regions {
            rect.validate(name)?;
        }
        for (index, (left_name, left)) in regions.iter().enumerate() {
            for (right_name, right) in regions.iter().skip(index + 1) {
                if intersects(*left, *right) {
                    return Err(RoiValidationError::Overlap(
                        left_name.to_string(),
                        right_name.to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn resolve(&self, frame: FrameSize) -> ResolvedRoiLayout {
        ResolvedRoiLayout {
            board: self.board.resolve(frame),
            hold: self.hold.resolve(frame),
            current: self.current.resolve(frame),
            next5: self.next5.map(|rect| rect.resolve(frame)),
        }
    }

    fn regions(&self) -> Vec<(&'static str, NormalizedRect)> {
        let mut regions = vec![
            ("board", self.board),
            ("hold", self.hold),
            ("current", self.current),
        ];
        regions.extend(
            self.next5
                .iter()
                .enumerate()
                .map(|(index, rect)| (next_name(index), *rect)),
        );
        regions
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRoiLayout {
    pub board: PixelRect,
    pub hold: PixelRect,
    pub current: PixelRect,
    pub next5: [PixelRect; 5],
}

fn next_name(index: usize) -> &'static str {
    match index {
        0 => "next5[0]",
        1 => "next5[1]",
        2 => "next5[2]",
        3 => "next5[3]",
        _ => "next5[4]",
    }
}

fn intersects(left: NormalizedRect, right: NormalizedRect) -> bool {
    left.x < right.x + right.width
        && right.x < left.x + left.width
        && left.y < right.y + right.height
        && right.y < left.y + left.height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> RoiLayout {
        RoiLayout {
            board: NormalizedRect::new(0.3, 0.1, 0.4, 0.8),
            hold: NormalizedRect::new(0.05, 0.2, 0.15, 0.2),
            current: NormalizedRect::new(0.75, 0.05, 0.1, 0.15),
            next5: [
                NormalizedRect::new(0.75, 0.25, 0.1, 0.1),
                NormalizedRect::new(0.75, 0.37, 0.1, 0.1),
                NormalizedRect::new(0.75, 0.49, 0.1, 0.1),
                NormalizedRect::new(0.75, 0.61, 0.1, 0.1),
                NormalizedRect::new(0.75, 0.73, 0.1, 0.1),
            ],
        }
    }

    #[test]
    fn normalized_layout_validates_and_resolves() {
        let layout = layout();
        layout.validate().unwrap();
        let resolved = layout.resolve(FrameSize {
            width: 1920,
            height: 1080,
        });
        assert_eq!(
            resolved.board,
            PixelRect {
                left: 576,
                top: 108,
                width: 768,
                height: 864
            }
        );
    }

    #[test]
    fn overlap_is_rejected() {
        let mut layout = layout();
        layout.current = NormalizedRect::new(0.5, 0.5, 0.2, 0.2);
        assert!(matches!(
            layout.validate(),
            Err(RoiValidationError::Overlap(_, _))
        ));
    }
}
