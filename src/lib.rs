//! Pure domain core for the 2nd PC learning overlay.
//!
//! Screen capture, recognition, hotkeys, and overlay rendering deliberately live
//! outside this crate. This boundary lets recorded observations exercise the
//! recommendation and Undo behavior without launching TETR.IO.

pub mod calibration;
pub mod capture;
pub mod condition;
pub mod diagram;
pub mod finder;
pub mod fumen;
pub mod guidance;
pub mod konbini;
pub mod model;
pub mod overlay;
pub mod recognition;
pub mod roi;
pub mod rules;
pub mod stability;
pub mod tracker;
pub mod window;

pub use capture::{CaptureError, CapturedFrame};
pub use condition::{
    Condition, EvalContext, EvalResult, Prerequisite, QueuePattern, SolvableSolve,
};
pub use diagram::{Diagram, DiagramPlacement, DiagramSet, DiagramValidationReport};
pub use fumen::{FumenCell, FumenError, decode_first_page};
pub use guidance::{
    ExpectedCell, ExpectedCellStatus, OverlayPlan, SetupProgress, SolveProgress,
    evaluate_setup_progress, evaluate_solve_progress,
};
pub use konbini::{KonbiniCandidate, KonbiniError, KonbiniLibrary, KonbiniSetup};
pub use model::{
    Board, Cell, Observation, PcOrdinal, Piece, Placement, board_is_empty, empty_board,
};
pub use overlay::{
    OverlayBoard, OverlayCell, OverlayCellKind, OverlayError, OverlayState, OverlayWindow,
};
pub use recognition::{
    BoardCalibration, PiecePalette, RecognitionError, RecognitionResult, RecognizerConfig, Rgb,
    TetrioRecognizer, piece_code,
};
pub use roi::{
    FrameSize, NormalizedRect, PixelRect, ResolvedRoiLayout, RoiLayout, RoiValidationError,
};
pub use rules::{RuleSet, RuleValidationError, Selection, SelectionReport, SourceSections};
pub use stability::{StabilityConfig, StabilityFilter, StabilityOutcome};
pub use tracker::{CommitOutcome, SessionTracker, TrackerConfig};
pub use window::{WindowInfo, WindowLocator, WindowLocatorError, WindowRect, is_tetrio_title};
