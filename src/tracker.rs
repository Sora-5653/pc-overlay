use serde::{Deserialize, Serialize};

use crate::{
    condition::EvalContext,
    model::{Observation, PcOrdinal, Piece, board_is_empty},
};

/// HOLD plus the first playable pieces at a 2nd-PC start form a four-piece case.
pub const SECOND_PC_CASE_SIZE: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackerConfig {
    pub recognition_confidence_threshold: f32,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            recognition_confidence_threshold: 0.70,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GuidanceState {
    pub case_id: Option<String>,
    pub method_id: Option<String>,
    pub setup_id: Option<String>,
    pub setup_progress: u16,
    pub solve_candidates: Vec<String>,
    pub solve_index: usize,
    pub show_solve: bool,
}

impl GuidanceState {
    fn advance_solve_after_undo(&mut self) {
        if self.show_solve && self.setup_progress == 0 && self.solve_candidates.len() > 1 {
            self.solve_index = (self.solve_index + 1) % self.solve_candidates.len();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub timestamp_ms: u64,
    pub pc_ordinal: PcOrdinal,
    pub observation: Observation,
    pub observed_queue_history: Vec<Piece>,
    pub guidance: GuidanceState,
    pub recognition_confidence: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommitOutcome {
    Accepted {
        snapshot: Snapshot,
    },
    Unchanged,
    IgnoredLowConfidence {
        confidence: f32,
    },
    RejectedHistoryDiscontinuity,
    RolledBack {
        steps: usize,
        snapshot: Snapshot,
    },
    /// The game was restarted; the tracker is back at PC 1 with no history.
    NewGame {
        snapshot: Snapshot,
    },
}

#[derive(Debug, Clone)]
pub struct SessionTracker {
    config: TrackerConfig,
    pc_ordinal: PcOrdinal,
    observed_queue_history: Vec<Piece>,
    guidance: GuidanceState,
    history: Vec<Snapshot>,
}

impl SessionTracker {
    pub fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            pc_ordinal: PcOrdinal::default(),
            observed_queue_history: Vec::new(),
            guidance: GuidanceState::default(),
            history: Vec::new(),
        }
    }

    pub fn pc_ordinal(&self) -> PcOrdinal {
        self.pc_ordinal
    }

    pub fn observed_queue_history(&self) -> &[Piece] {
        &self.observed_queue_history
    }

    pub fn guidance(&self) -> &GuidanceState {
        &self.guidance
    }

    pub fn snapshots(&self) -> &[Snapshot] {
        &self.history
    }

    /// The earliest retained snapshot of the current PC interval. Ordinals
    /// repeat after the 7th PC, so this walks back only through the latest
    /// run of the current ordinal. After a manual resync it is the first
    /// observation since the resync, not necessarily the true PC start.
    pub fn current_pc_start(&self) -> Option<&Snapshot> {
        let start = self
            .history
            .iter()
            .rposition(|snapshot| snapshot.pc_ordinal != self.pc_ordinal)
            .map_or(0, |index| index + 1);
        self.history.get(start)
    }

    /// Rule conditions for the current PC, evaluated over the bag that follows
    /// the four case pieces. Live HOLD/Current come from the latest snapshot.
    pub fn following_bag_context(&self) -> Option<EvalContext> {
        let start = self.current_pc_start()?;
        let latest = self.history.last()?;
        let case_pieces_in_queue =
            SECOND_PC_CASE_SIZE - usize::from(start.observation.hold.is_some());
        let case_queue: Vec<Piece> = self
            .observed_queue_history
            .iter()
            .take(case_pieces_in_queue)
            .copied()
            .collect();
        Some(
            EvalContext::for_following_bag(
                &self.observed_queue_history,
                case_pieces_in_queue,
                latest.observation.hold,
                latest.observation.current,
            )
            // Only once every case piece has been seen.
            .with_case(
                start.observation.hold,
                if case_queue.len() == case_pieces_in_queue {
                    &case_queue
                } else {
                    &[]
                },
            ),
        )
    }

    pub fn set_guidance(&mut self, guidance: GuidanceState) {
        self.guidance = guidance.clone();
        if let Some(last) = self.history.last_mut() {
            last.guidance = guidance;
        }
    }

    /// Flips solve display and stores it in the latest snapshot, so Undo to an
    /// earlier state restores the setting that state was shown with. The
    /// setting belongs to the current PC: a PC boundary or a resync turns it
    /// OFF again, so each solve is revealed only when the player asks.
    pub fn toggle_show_solve(&mut self) -> bool {
        let mut guidance = self.guidance.clone();
        guidance.show_solve = !guidance.show_solve;
        self.set_guidance(guidance);
        self.guidance.show_solve
    }

    /// Manual resynchronization is explicit and clears transition history. This
    /// prevents observations from before the resync from changing the new ordinal.
    pub fn resync(&mut self, ordinal: PcOrdinal) {
        self.pc_ordinal = ordinal;
        self.observed_queue_history.clear();
        self.guidance = GuidanceState::default();
        self.history.clear();
    }

    pub fn commit(
        &mut self,
        observation: Observation,
        recognition_confidence: f32,
        timestamp_ms: u64,
    ) -> CommitOutcome {
        if !recognition_confidence.is_finite()
            || recognition_confidence < self.config.recognition_confidence_threshold
        {
            return CommitOutcome::IgnoredLowConfidence {
                confidence: recognition_confidence,
            };
        }

        if self
            .history
            .last()
            .is_some_and(|last| last.observation == observation)
        {
            return CommitOutcome::Unchanged;
        }

        if let Some(index) = self
            .history
            .iter()
            .rposition(|snapshot| snapshot.observation == observation)
        {
            let steps = self.history.len() - 1 - index;
            self.history.truncate(index + 1);
            let target = self.history.last_mut().expect("rollback target exists");
            target.guidance.advance_solve_after_undo();
            self.pc_ordinal = target.pc_ordinal;
            self.observed_queue_history = target.observed_queue_history.clone();
            self.guidance = target.guidance.clone();
            return CommitOutcome::RolledBack {
                steps,
                snapshot: target.clone(),
            };
        }

        if self
            .history
            .last()
            .is_some_and(|last| starts_new_game(&last.observation, &observation))
        {
            // Undo never crosses a restart, so the old game's states are gone.
            self.pc_ordinal = PcOrdinal::default();
            self.observed_queue_history = seed_queue(&observation);
            self.guidance = GuidanceState::default();
            self.history.clear();
            let snapshot = self.push_snapshot(observation, recognition_confidence, timestamp_ms);
            return CommitOutcome::NewGame { snapshot };
        }

        let crossed_pc_boundary = self.history.last().is_some_and(|last| {
            !board_is_empty(&last.observation.board) && board_is_empty(&observation.board)
        });

        if crossed_pc_boundary {
            self.pc_ordinal = self.pc_ordinal.advance();
            self.observed_queue_history = seed_queue(&observation);
            self.guidance = GuidanceState::default();
        } else if let Some(last) = self.history.last() {
            let Some(updated) = append_revealed(
                &self.observed_queue_history,
                &last.observation.next5,
                &observation.next5,
            ) else {
                return CommitOutcome::RejectedHistoryDiscontinuity;
            };
            self.observed_queue_history = updated;
        } else {
            self.observed_queue_history = seed_queue(&observation);
        }

        let snapshot = self.push_snapshot(observation, recognition_confidence, timestamp_ms);
        CommitOutcome::Accepted { snapshot }
    }

    fn push_snapshot(
        &mut self,
        observation: Observation,
        recognition_confidence: f32,
        timestamp_ms: u64,
    ) -> Snapshot {
        let snapshot = Snapshot {
            timestamp_ms,
            pc_ordinal: self.pc_ordinal,
            observation,
            observed_queue_history: self.observed_queue_history.clone(),
            guidance: self.guidance.clone(),
            recognition_confidence,
        };
        self.history.push(snapshot.clone());
        snapshot
    }
}

/// A restarted game (`R` in ZEN) is told from a PC by HOLD, not by the board:
/// once filled, HOLD never empties within a game, while a restart empties it.
/// Before the first hold, a NEXT queue that does not continue the previous
/// one is a restart too; such a state is in PC 1, so a false positive from
/// skipped frames there costs nothing. Undo to an earlier state of the same
/// game is matched by the rollback before this check.
fn starts_new_game(previous: &Observation, observation: &Observation) -> bool {
    observation.hold.is_none()
        && (previous.hold.is_some()
            || append_revealed(&[], &previous.next5, &observation.next5).is_none())
}

fn seed_queue(observation: &Observation) -> Vec<Piece> {
    std::iter::once(observation.current)
        .chain(observation.next5)
        .collect()
}

fn append_revealed(
    history: &[Piece],
    previous: &[Piece; 5],
    current: &[Piece; 5],
) -> Option<Vec<Piece>> {
    let overlap = (1..=5)
        .rev()
        .find(|size| previous[5 - size..] == current[..*size]);
    let overlap = overlap?;
    let mut updated = history.to_vec();
    updated.extend_from_slice(&current[overlap..]);
    Some(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Piece, empty_board};

    fn observation(filled: bool, current: Piece, next5: [Piece; 5]) -> Observation {
        let mut board = empty_board();
        if filled {
            board[0][0] = Some(Piece::T);
        }
        Observation {
            board,
            hold: Some(Piece::I),
            current,
            next5,
        }
    }

    fn tracker() -> SessionTracker {
        SessionTracker::new(TrackerConfig {
            recognition_confidence_threshold: 0.9,
        })
    }

    #[test]
    fn following_bag_context_skips_case_pieces_from_the_pc_start() {
        let mut tracker = tracker();
        tracker.commit(
            observation(
                true,
                Piece::T,
                [Piece::Z, Piece::S, Piece::I, Piece::J, Piece::L],
            ),
            1.0,
            1,
        );
        // PC 2 starts with HOLD I, so J/L/O are the three case pieces in queue.
        tracker.commit(
            observation(
                false,
                Piece::J,
                [Piece::L, Piece::O, Piece::T, Piece::J, Piece::S],
            ),
            1.0,
            2,
        );
        tracker.commit(
            observation(
                true,
                Piece::L,
                [Piece::O, Piece::T, Piece::J, Piece::S, Piece::Z],
            ),
            1.0,
            3,
        );

        let start = tracker.current_pc_start().expect("PC start observed");
        assert_eq!(start.pc_ordinal.get(), 2);
        assert_eq!(start.observation.current, Piece::J);
        let context = tracker.following_bag_context().expect("context");
        assert_eq!(
            context.queue_history,
            [Piece::T, Piece::J, Piece::S, Piece::Z]
        );
        assert!(!context.queue_closed);
        assert_eq!(context.current, Piece::L);
    }

    #[test]
    fn low_confidence_does_not_mutate_state() {
        let mut tracker = tracker();
        let result = tracker.commit(
            observation(
                false,
                Piece::J,
                [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
            ),
            0.5,
            1,
        );
        assert!(matches!(result, CommitOutcome::IgnoredLowConfidence { .. }));
        assert!(tracker.snapshots().is_empty());
    }

    #[test]
    fn next_overlap_appends_only_newly_disclosed_piece() {
        let mut tracker = tracker();
        tracker.commit(
            observation(
                true,
                Piece::I,
                [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
            ),
            1.0,
            1,
        );
        tracker.commit(
            observation(
                true,
                Piece::J,
                [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
            ),
            1.0,
            2,
        );
        assert_eq!(
            tracker.observed_queue_history(),
            &[
                Piece::I,
                Piece::J,
                Piece::L,
                Piece::O,
                Piece::S,
                Piece::T,
                Piece::Z
            ]
        );
    }

    #[test]
    fn nonempty_to_empty_advances_ordinal_and_undo_restores_it() {
        let mut tracker = tracker();
        let filled = observation(
            true,
            Piece::I,
            [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
        );
        tracker.commit(filled.clone(), 1.0, 1);
        let cleared = observation(
            false,
            Piece::J,
            [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
        );
        tracker.commit(cleared, 1.0, 2);
        assert_eq!(tracker.pc_ordinal().get(), 2);

        let outcome = tracker.commit(filled, 1.0, 3);
        assert!(matches!(outcome, CommitOutcome::RolledBack { .. }));
        assert_eq!(tracker.pc_ordinal().get(), 1);
    }

    #[test]
    fn returning_to_setup_start_cycles_solve_when_enabled() {
        let mut tracker = tracker();
        let start = observation(
            true,
            Piece::I,
            [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
        );
        tracker.commit(start.clone(), 1.0, 1);
        tracker.set_guidance(GuidanceState {
            setup_id: Some("setup".into()),
            show_solve: true,
            solve_candidates: vec!["a".into(), "b".into(), "c".into()],
            ..GuidanceState::default()
        });
        tracker.commit(
            observation(
                true,
                Piece::J,
                [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
            ),
            1.0,
            2,
        );
        tracker.commit(start, 1.0, 3);
        assert_eq!(tracker.guidance().solve_index, 1);
    }

    #[test]
    fn show_solve_resets_at_pc_boundaries_and_resync_but_follows_undo() {
        let mut tracker = tracker();
        let filled = observation(
            true,
            Piece::I,
            [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
        );
        tracker.commit(filled.clone(), 1.0, 1);
        assert!(!tracker.guidance().show_solve, "solve display starts OFF");
        assert!(tracker.toggle_show_solve());
        assert!(tracker.snapshots().last().unwrap().guidance.show_solve);

        let cleared = observation(
            false,
            Piece::J,
            [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
        );
        tracker.commit(cleared.clone(), 1.0, 2);
        assert_eq!(tracker.pc_ordinal().get(), 2);
        assert!(
            !tracker.guidance().show_solve,
            "turned OFF at the PC boundary"
        );

        tracker.commit(filled, 1.0, 3);
        assert!(
            tracker.guidance().show_solve,
            "Undo restores the earlier setting"
        );
        tracker.commit(cleared, 1.0, 4);
        assert!(!tracker.guidance().show_solve);

        assert!(tracker.toggle_show_solve());
        tracker.resync(PcOrdinal::new(2).unwrap());
        assert!(
            !tracker.guidance().show_solve,
            "turned OFF by a manual resync"
        );
    }

    #[test]
    fn emptied_hold_restarts_at_pc_one_instead_of_counting_a_pc() {
        let mut tracker = tracker();
        tracker.resync(PcOrdinal::new(3).unwrap());
        tracker.commit(
            observation(
                true,
                Piece::I,
                [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
            ),
            1.0,
            1,
        );
        // R in ZEN: an empty board, an empty HOLD, and a fresh queue that
        // happens to continue the old one.
        let mut restarted = observation(
            false,
            Piece::J,
            [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
        );
        restarted.hold = None;
        let outcome = tracker.commit(restarted, 1.0, 2);
        assert!(matches!(outcome, CommitOutcome::NewGame { .. }));
        assert_eq!(tracker.pc_ordinal().get(), 1);
        assert_eq!(tracker.snapshots().len(), 1);
        assert_eq!(tracker.observed_queue_history().len(), 6);
    }

    #[test]
    fn discontinuous_queue_before_the_first_hold_restarts() {
        let mut tracker = tracker();
        let mut early = observation(
            false,
            Piece::I,
            [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
        );
        early.hold = None;
        tracker.commit(early.clone(), 1.0, 1);
        let mut placed = observation(
            true,
            Piece::J,
            [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
        );
        placed.hold = None;
        assert!(matches!(
            tracker.commit(placed, 1.0, 2),
            CommitOutcome::Accepted { .. }
        ));

        // Previously rejected as a discontinuity, which froze the tracker.
        let mut restarted = observation(
            false,
            Piece::T,
            [Piece::S, Piece::Z, Piece::I, Piece::O, Piece::L],
        );
        restarted.hold = None;
        assert!(matches!(
            tracker.commit(restarted, 1.0, 3),
            CommitOutcome::NewGame { .. }
        ));
        assert_eq!(tracker.pc_ordinal().get(), 1);
    }

    #[test]
    fn undo_to_the_empty_hold_start_is_a_rollback_not_a_restart() {
        let mut tracker = tracker();
        let mut start = observation(
            false,
            Piece::I,
            [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
        );
        start.hold = None;
        tracker.commit(start.clone(), 1.0, 1);
        tracker.commit(
            observation(
                false,
                Piece::J,
                [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
            ),
            1.0,
            2,
        );
        assert!(matches!(
            tracker.commit(start, 1.0, 3),
            CommitOutcome::RolledBack { steps: 1, .. }
        ));
    }

    #[test]
    fn default_threshold_tracks_a_stable_pc_clear_at_realistic_confidence() {
        let mut tracker = SessionTracker::new(TrackerConfig::default());
        tracker.commit(
            observation(
                true,
                Piece::I,
                [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
            ),
            0.75,
            1,
        );
        let outcome = tracker.commit(
            observation(
                false,
                Piece::J,
                [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
            ),
            0.75,
            2,
        );
        assert!(matches!(outcome, CommitOutcome::Accepted { .. }));
        assert_eq!(tracker.pc_ordinal().get(), 2);
    }
}
