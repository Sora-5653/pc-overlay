//! Image-derived IJLO diagram transcriptions.
//!
//! These are deliberately separate from live rule data. A diagram can be
//! geometrically transcribed while its semantic meaning (line-clear phase,
//! repeated piece, or solve branch) is still under human review.

use std::{collections::BTreeSet, fs, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    model::{Cell, Piece, Placement},
    rules::{ReviewStatus, SourceReference},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagramCoordinateSystem {
    pub width: u8,
    pub origin: String,
    pub y_increases: String,
    pub source_cell_grid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagramPlacement {
    pub piece: Piece,
    /// Coordinates are `[x, y]` in the image-local bottom-left coordinate frame.
    pub cells: [[u8; 2]; 4],
}

impl DiagramPlacement {
    pub fn as_placement(&self) -> Placement {
        Placement {
            piece: self.piece,
            cells: self
                .cells
                .iter()
                .map(|[x, y]| Cell { x: *x, y: *y })
                .collect(),
        }
    }

    /// Returns the cells after removing rows that were already cleared when
    /// this piece was placed. The source diagrams keep drawing cleared rows,
    /// so a piece placed through one appears split in the raster.
    pub fn collapsed_cells(&self, cleared_rows: &[u8]) -> [[u8; 2]; 4] {
        self.cells.map(|[x, y]| {
            let below = cleared_rows.iter().filter(|row| **row < y).count() as u8;
            [x, y - below]
        })
    }

    fn has_piece_shape(&self, cleared_rows: &[u8]) -> bool {
        let target = normalized(
            self.collapsed_cells(cleared_rows)
                .map(|[x, y]| [x as i8, y as i8]),
        );
        let mut rotation = tetromino(self.piece);
        (0..4).any(|_| {
            let matches = normalized(rotation) == target;
            rotation = rotation.map(|[x, y]| [y, -x]);
            matches
        })
    }

    fn is_connected(&self) -> bool {
        let cells: BTreeSet<_> = self.cells.iter().copied().collect();
        if cells.len() != 4 {
            return false;
        }
        let mut visited = BTreeSet::new();
        let mut pending = vec![self.cells[0]];
        while let Some(cell) = pending.pop() {
            if !visited.insert(cell) {
                continue;
            }
            let [x, y] = cell;
            for neighbor in [
                [x.saturating_sub(1), y],
                [x.saturating_add(1), y],
                [x, y.saturating_sub(1)],
                [x, y.saturating_add(1)],
            ] {
                if cells.contains(&neighbor) && !visited.contains(&neighbor) {
                    pending.push(neighbor);
                }
            }
        }
        visited.len() == cells.len()
    }
}

fn tetromino(piece: Piece) -> [[i8; 2]; 4] {
    match piece {
        Piece::I => [[0, 0], [1, 0], [2, 0], [3, 0]],
        Piece::O => [[0, 0], [1, 0], [0, 1], [1, 1]],
        Piece::T => [[0, 0], [1, 0], [2, 0], [1, 1]],
        Piece::S => [[0, 0], [1, 0], [1, 1], [2, 1]],
        Piece::Z => [[1, 0], [2, 0], [0, 1], [1, 1]],
        Piece::J => [[0, 0], [1, 0], [2, 0], [0, 1]],
        Piece::L => [[0, 0], [1, 0], [2, 0], [2, 1]],
    }
}

fn normalized(cells: [[i8; 2]; 4]) -> BTreeSet<[i8; 2]> {
    let min_x = cells.iter().map(|[x, _]| *x).min().unwrap_or(0);
    let min_y = cells.iter().map(|[_, y]| *y).min().unwrap_or(0);
    cells.iter().map(|[x, y]| [x - min_x, y - min_y]).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagram {
    pub image: String,
    pub width: u8,
    pub height: u8,
    #[serde(default)]
    pub review_status: ReviewStatus,
    /// Image-local rows that were filled and cleared before this diagram's
    /// colored placements but are still drawn by the source image.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleared_rows: Vec<u8>,
    pub placements: Vec<DiagramPlacement>,
    pub source: SourceReference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagramSet {
    pub schema_version: u32,
    pub coordinate_system: DiagramCoordinateSystem,
    pub diagrams: Vec<Diagram>,
}

#[derive(Debug, Error)]
pub enum DiagramLoadError {
    #[error("could not read diagram data: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid diagram JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Validation(#[from] DiagramValidationError),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("diagram validation failed:\n{}", .issues.join("\n"))]
pub struct DiagramValidationError {
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagramValidationReport {
    pub diagram_count: usize,
    pub placement_count: usize,
    pub warnings: Vec<String>,
}

impl DiagramSet {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, DiagramLoadError> {
        let contents = fs::read_to_string(path)?;
        let diagrams: Self = serde_json::from_str(&contents)?;
        diagrams.validate()?;
        Ok(diagrams)
    }

    pub fn validate(&self) -> Result<DiagramValidationReport, DiagramValidationError> {
        let mut issues = Vec::new();
        let mut warnings = Vec::new();
        if self.schema_version != 1 {
            issues.push(format!(
                "schema_version must be 1, got {}",
                self.schema_version
            ));
        }
        if self.coordinate_system.width != 10 {
            issues.push(format!(
                "coordinate_system.width must be 10, got {}",
                self.coordinate_system.width
            ));
        }
        if self.coordinate_system.origin != "bottom_left" {
            issues.push("coordinate_system.origin must be bottom_left".into());
        }
        if self.coordinate_system.y_increases != "up" {
            issues.push("coordinate_system.y_increases must be up".into());
        }

        let mut images = BTreeSet::new();
        let mut placement_count = 0;
        for diagram in &self.diagrams {
            let path = format!("diagrams[{}]", diagram.image);
            if !images.insert(&diagram.image) {
                issues.push(format!("duplicate diagram image id: {}", diagram.image));
            }
            if diagram.width != 10 {
                issues.push(format!("{path}.width must be 10"));
            }
            if diagram.height == 0 || diagram.height > 20 {
                issues.push(format!("{path}.height must be between 1 and 20"));
            }
            if diagram.placements.is_empty() {
                issues.push(format!("{path}.placements must not be empty"));
            }
            for row in &diagram.cleared_rows {
                if *row >= diagram.height {
                    issues.push(format!(
                        "{path}.cleared_rows contains row {row} outside the diagram"
                    ));
                }
            }
            let mut occupied = BTreeSet::new();
            for (index, placement) in diagram.placements.iter().enumerate() {
                placement_count += 1;
                let placement_path = format!("{path}.placements[{index}]");
                let mut local = BTreeSet::new();
                for [x, y] in placement.cells {
                    if x >= diagram.width || y >= diagram.height {
                        issues.push(format!(
                            "{placement_path} cell ({x},{y}) is outside {}x{} diagram",
                            diagram.width, diagram.height
                        ));
                    }
                    if !local.insert([x, y]) {
                        issues.push(format!("{placement_path} contains duplicate cells"));
                    }
                    if !occupied.insert([x, y]) {
                        issues.push(format!(
                            "{placement_path} overlaps another colored placement at ({x},{y})"
                        ));
                    }
                    if diagram.cleared_rows.contains(&y) {
                        issues.push(format!(
                            "{placement_path} cell ({x},{y}) lies on a row listed as already cleared"
                        ));
                    }
                }
                if placement.has_piece_shape(&diagram.cleared_rows) {
                    continue;
                }
                if placement.is_connected() {
                    issues.push(format!(
                        "{placement_path} cells do not form a {:?} tetromino",
                        placement.piece
                    ));
                } else {
                    warnings.push(format!(
                        "{placement_path} piece {:?} is disconnected in the raster transcription; inspect line-clear or repeated-piece semantics",
                        placement.piece
                    ));
                }
            }
        }

        if issues.is_empty() {
            Ok(DiagramValidationReport {
                diagram_count: self.diagrams.len(),
                placement_count,
                warnings,
            })
        } else {
            Err(DiagramValidationError { issues })
        }
    }

    pub fn diagram(&self, image: &str) -> Option<&Diagram> {
        self.diagrams.iter().find(|diagram| diagram.image == image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connectedness_detects_raster_ambiguity_without_rejecting_it() {
        let placement = DiagramPlacement {
            piece: Piece::L,
            cells: [[0, 0], [1, 0], [1, 1], [1, 3]],
        };
        assert!(!placement.is_connected());
    }

    #[test]
    fn normal_tetromino_is_connected() {
        let placement = DiagramPlacement {
            piece: Piece::T,
            cells: [[1, 0], [2, 0], [3, 0], [2, 1]],
        };
        assert!(placement.is_connected());
    }

    #[test]
    fn collapsing_a_cleared_row_restores_a_split_piece() {
        // Image 9: an L placed through row 1 after that row was cleared.
        let placement = DiagramPlacement {
            piece: Piece::L,
            cells: [[2, 3], [3, 3], [3, 2], [3, 0]],
        };
        assert!(!placement.has_piece_shape(&[]));
        assert!(placement.has_piece_shape(&[1]));
        assert_eq!(
            placement.collapsed_cells(&[1]),
            [[2, 2], [3, 2], [3, 1], [3, 0]]
        );
    }

    #[test]
    fn mirrored_shape_is_rejected() {
        let placement = DiagramPlacement {
            piece: Piece::J,
            cells: [[0, 0], [1, 0], [2, 0], [2, 1]],
        };
        assert!(placement.is_connected());
        assert!(!placement.has_piece_shape(&[]));
    }

    fn single_diagram(cleared_rows: Vec<u8>, placement: DiagramPlacement) -> DiagramSet {
        DiagramSet {
            schema_version: 1,
            coordinate_system: DiagramCoordinateSystem {
                width: 10,
                origin: "bottom_left".into(),
                y_increases: "up".into(),
                source_cell_grid: "test".into(),
            },
            diagrams: vec![Diagram {
                image: "test".into(),
                width: 10,
                height: 4,
                review_status: ReviewStatus::ReviewRequired,
                cleared_rows,
                placements: vec![placement],
                source: SourceReference {
                    document: "test".into(),
                    lines: [1, 1],
                    images: vec![],
                    note: None,
                },
            }],
        }
    }

    #[test]
    fn validation_rejects_wrong_shapes_and_cells_on_cleared_rows() {
        let wrong_shape = single_diagram(
            vec![],
            DiagramPlacement {
                piece: Piece::S,
                cells: [[0, 0], [1, 0], [2, 0], [3, 0]],
            },
        );
        assert!(wrong_shape.validate().is_err());

        let on_cleared_row = single_diagram(
            vec![0],
            DiagramPlacement {
                piece: Piece::I,
                cells: [[0, 0], [1, 0], [2, 0], [3, 0]],
            },
        );
        assert!(on_cleared_row.validate().is_err());
    }
}
