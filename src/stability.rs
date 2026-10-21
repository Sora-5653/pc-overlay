//! Stable-frame gate between recognition and the state tracker.

use serde::{Deserialize, Serialize};

use crate::model::Observation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StabilityConfig {
    pub required_consecutive_frames: u8,
    pub confidence_threshold_percent: u8,
}

impl Default for StabilityConfig {
    fn default() -> Self {
        Self {
            required_consecutive_frames: 3,
            // Region recognition scores include antialiasing and glow pixels.
            // Requiring 92% rejected correctly identified TETR.IO previews in
            // normal captures. Three identical frames remain the main guard
            // against transient animation frames.
            confidence_threshold_percent: 70,
        }
    }
}

impl StabilityConfig {
    fn confidence_is_acceptable(self, confidence: f32) -> bool {
        confidence.is_finite() && confidence >= f32::from(self.confidence_threshold_percent) / 100.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StabilityOutcome {
    IgnoredLowConfidence {
        confidence: f32,
    },
    Pending {
        consecutive_frames: u8,
    },
    Stable {
        observation: Box<Observation>,
        confidence: f32,
    },
}

#[derive(Debug, Clone)]
pub struct StabilityFilter {
    config: StabilityConfig,
    pending: Option<Observation>,
    consecutive_frames: u8,
    last_stable: Option<Observation>,
}

impl StabilityFilter {
    pub fn new(config: StabilityConfig) -> Self {
        Self {
            config,
            pending: None,
            consecutive_frames: 0,
            last_stable: None,
        }
    }

    pub fn last_stable(&self) -> Option<&Observation> {
        self.last_stable.as_ref()
    }

    pub fn reset(&mut self) {
        self.pending = None;
        self.consecutive_frames = 0;
        self.last_stable = None;
    }

    /// Discards only the in-flight candidate.  The last stable observation is
    /// deliberately retained so an unknown frame stops updates without
    /// erasing the guidance that was already displayed.
    pub fn clear_pending(&mut self) {
        self.pending = None;
        self.consecutive_frames = 0;
    }

    pub fn ingest(&mut self, observation: Observation, confidence: f32) -> StabilityOutcome {
        if !self.config.confidence_is_acceptable(confidence) {
            self.pending = None;
            self.consecutive_frames = 0;
            return StabilityOutcome::IgnoredLowConfidence { confidence };
        }

        if self.pending.as_ref() == Some(&observation) {
            self.consecutive_frames = self.consecutive_frames.saturating_add(1);
        } else {
            self.pending = Some(observation.clone());
            self.consecutive_frames = 1;
        }

        if self.consecutive_frames >= self.config.required_consecutive_frames.max(1) {
            self.last_stable = Some(observation.clone());
            StabilityOutcome::Stable {
                observation: Box::new(observation),
                confidence,
            }
        } else {
            StabilityOutcome::Pending {
                consecutive_frames: self.consecutive_frames,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Piece, empty_board};

    fn observation(piece: Piece) -> Observation {
        Observation {
            board: empty_board(),
            hold: Some(Piece::I),
            current: piece,
            next5: [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
        }
    }

    #[test]
    fn one_transient_frame_does_not_become_stable() {
        let mut filter = StabilityFilter::new(StabilityConfig {
            required_consecutive_frames: 3,
            confidence_threshold_percent: 90,
        });
        assert!(matches!(
            filter.ingest(observation(Piece::I), 1.0),
            StabilityOutcome::Pending {
                consecutive_frames: 1
            }
        ));
        assert!(matches!(
            filter.ingest(observation(Piece::J), 1.0),
            StabilityOutcome::Pending {
                consecutive_frames: 1
            }
        ));
        assert!(filter.last_stable().is_none());
    }

    #[test]
    fn three_equal_frames_are_stable_and_low_confidence_resets_pending() {
        let mut filter = StabilityFilter::new(StabilityConfig::default());
        let obs = observation(Piece::I);
        assert!(matches!(
            filter.ingest(obs.clone(), 1.0),
            StabilityOutcome::Pending { .. }
        ));
        assert!(matches!(
            filter.ingest(obs.clone(), 1.0),
            StabilityOutcome::Pending {
                consecutive_frames: 2
            }
        ));
        assert!(matches!(
            filter.ingest(obs.clone(), 1.0),
            StabilityOutcome::Stable { .. }
        ));
        assert_eq!(filter.last_stable(), Some(&obs));
        assert!(matches!(
            filter.ingest(observation(Piece::J), 0.2),
            StabilityOutcome::IgnoredLowConfidence { .. }
        ));
        assert_eq!(filter.last_stable(), Some(&obs));
    }

    #[test]
    fn default_threshold_accepts_realistic_antialiased_preview_confidence() {
        let mut filter = StabilityFilter::new(StabilityConfig::default());
        let obs = observation(Piece::I);
        assert!(matches!(
            filter.ingest(obs.clone(), 0.75),
            StabilityOutcome::Pending { .. }
        ));
        assert!(matches!(
            filter.ingest(obs.clone(), 0.75),
            StabilityOutcome::Pending { .. }
        ));
        assert!(matches!(
            filter.ingest(obs, 0.75),
            StabilityOutcome::Stable { .. }
        ));
    }
}
