use serde::{Deserialize, Serialize};

use crate::{
    condition::EvalContext,
    model::{Observation, PcOrdinal, Piece, board_is_empty},
};

/// How long a state whose NEXT does not continue the tracked history must
/// stay on screen before the tracker starts over from it. A misread that
/// passed the stability filter rarely lasts this long; a real state does,
/// and rejecting it forever froze the guidance on an outdated state.
pub const DISCONTINUITY_CONFIRM_MS: u64 = 500;

/// How long after a game key is pressed the next change on screen is read as
/// its result. The key is a hint: what the state became is still read from
/// the screen, since a press does nothing when there is nothing to undo.
pub const KEY_HINT_MS: u64 = 1500;

/// A TETR.IO key the overlay sees pressed, passed to the tracker as a hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameKey {
    /// `R` by default: restart the game.
    Retry,
    /// Ctrl+Z in ZEN: undo the last placement.
    Undo,
    /// Ctrl+Y in ZEN: redo an undone placement.
    Redo,
}

/// HOLD plus the first playable pieces at a 2nd-PC start form a four-piece case.
pub const SECOND_PC_CASE_SIZE: usize = 4;
/// The 7th PC starts with three pieces left in the bag (60 placed = 8 bags + 4).
pub const SEVENTH_PC_CASE_SIZE: usize = 3;

/// The size of the case a PC starts with, for the PCs the source documents
/// cover: the 2nd (four pieces) and the 7th (three pieces).
pub fn case_size(ordinal: PcOrdinal) -> Option<usize> {
    match ordinal.get() {
        2 => Some(SECOND_PC_CASE_SIZE),
        7 => Some(SEVENTH_PC_CASE_SIZE),
        _ => None,
    }
}

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
    /// How many pieces of the game's queue were disclosed at this state: its
    /// NEXT5 ends at this position. Undo is located by this position.
    #[serde(default)]
    pub queue_end: usize,
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
    /// A state that does not continue the history was confirmed, by staying
    /// for `DISCONTINUITY_CONFIRM_MS` or by a next state that continues it.
    /// The tracker starts over from it with the same PC number, like a
    /// resync: Undo cannot go back past it.
    Resynced {
        snapshot: Snapshot,
    },
}

#[derive(Debug, Clone)]
pub struct SessionTracker {
    config: TrackerConfig,
    pc_ordinal: PcOrdinal,
    observed_queue_history: Vec<Piece>,
    /// Every piece disclosed since the game start or resync. Unlike
    /// `observed_queue_history` it is not reseeded at a PC.
    game_queue: Vec<Piece>,
    guidance: GuidanceState,
    history: Vec<Snapshot>,
    /// The last state rejected as a discontinuity and when it was first
    /// seen, until it is confirmed or the history continues.
    unconfirmed: Option<(Observation, u64)>,
    /// The last game key pressed and when, until a change is committed.
    key_hint: Option<(GameKey, u64)>,
}

impl SessionTracker {
    pub fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            pc_ordinal: PcOrdinal::default(),
            observed_queue_history: Vec::new(),
            game_queue: Vec::new(),
            guidance: GuidanceState::default(),
            history: Vec::new(),
            unconfirmed: None,
            key_hint: None,
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
    /// the case pieces (`case_size`). Live HOLD/Current come from the latest
    /// snapshot. None on a PC without a case.
    pub fn following_bag_context(&self) -> Option<EvalContext> {
        let start = self.current_pc_start()?;
        let latest = self.history.last()?;
        let case_pieces_in_queue =
            case_size(start.pc_ordinal)? - usize::from(start.observation.hold.is_some());
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
        self.game_queue.clear();
        self.guidance = GuidanceState::default();
        self.history.clear();
        self.unconfirmed = None;
        self.key_hint = None;
    }

    /// Notes a game key pressed at `timestamp_ms` (the clock of the frames),
    /// so the next change committed within `KEY_HINT_MS` is read as its result:
    /// - Retry: an empty board is a new game, even with a HOLD already used.
    /// - Undo: an earlier state is matched by its board and queue alone,
    ///   whether or not TETR.IO restored HOLD, and even when the NEXT seems to
    ///   continue (a 7-bag repeating a window).
    /// - Redo: the state is never taken for an undo.
    pub fn note_key(&mut self, key: GameKey, timestamp_ms: u64) {
        self.key_hint = Some((key, timestamp_ms));
    }

    fn active_key(&self, timestamp_ms: u64) -> Option<GameKey> {
        self.key_hint
            .filter(|(_, at)| timestamp_ms.saturating_sub(*at) <= KEY_HINT_MS)
            .map(|(key, _)| key)
    }

    /// Adopts the PC number derived from the game's own PIECES counter, as a
    /// second source next to the empty-board boundaries counted by `commit`.
    /// Unlike `resync` it keeps the history: the snapshots of the current PC
    /// are relabeled, so Undo and the PC start stay usable. Returns whether
    /// the number changed.
    pub fn align_pc_ordinal(&mut self, ordinal: PcOrdinal) -> bool {
        if ordinal == self.pc_ordinal {
            return false;
        }
        let start = self
            .history
            .iter()
            .rposition(|snapshot| snapshot.pc_ordinal != self.pc_ordinal)
            .map_or(0, |index| index + 1);
        self.pc_ordinal = ordinal;
        // Guidance chosen for the wrong PC does not carry over.
        self.guidance = GuidanceState::default();
        for snapshot in &mut self.history[start..] {
            snapshot.pc_ordinal = ordinal;
        }
        if let Some(last) = self.history.last_mut() {
            last.guidance = GuidanceState::default();
        }
        true
    }

    pub fn commit(
        &mut self,
        observation: Observation,
        recognition_confidence: f32,
        timestamp_ms: u64,
    ) -> CommitOutcome {
        let outcome = self.advance(observation, recognition_confidence, timestamp_ms);
        if !matches!(
            outcome,
            CommitOutcome::Unchanged
                | CommitOutcome::IgnoredLowConfidence { .. }
                | CommitOutcome::RejectedHistoryDiscontinuity
        ) {
            self.unconfirmed = None;
            self.key_hint = None;
        }
        outcome
    }

    fn advance(
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

        let key = self.active_key(timestamp_ms);
        if key == Some(GameKey::Retry) && board_is_empty(&observation.board) {
            return self.start_new_game(observation, recognition_confidence, timestamp_ms);
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
            self.game_queue.truncate(target.queue_end);
            self.guidance = target.guidance.clone();
            return CommitOutcome::RolledBack {
                steps,
                snapshot: target.clone(),
            };
        }

        if key != Some(GameKey::Redo)
            && let Some((index, position)) =
                self.undo_target(&observation, key == Some(GameKey::Undo))
        {
            return self.roll_back_to_unseen(
                index,
                position,
                observation,
                recognition_confidence,
                timestamp_ms,
            );
        }

        if self
            .history
            .last()
            .is_some_and(|last| starts_new_game(&last.observation, &observation))
        {
            return self.start_new_game(observation, recognition_confidence, timestamp_ms);
        }

        let crossed_pc_boundary = self.history.last().is_some_and(|last| {
            !board_is_empty(&last.observation.board) && board_is_empty(&observation.board)
        });

        if crossed_pc_boundary {
            let last = self
                .history
                .last()
                .expect("a PC boundary has a previous state");
            // A PC is counted even when the queue does not continue; the game
            // queue then starts a fresh segment, so a later Undo stops there.
            let placed = match append_revealed(
                &self.game_queue,
                &last.observation.next5,
                &observation.next5,
            ) {
                Some(queue) => {
                    self.game_queue = queue;
                    self.placed_since_pc_start(self.game_queue.len(), observation.hold)
                }
                None => {
                    self.game_queue.extend(seed_queue(&observation));
                    None
                }
            };
            self.pc_ordinal = self.pc_ordinal.after_pieces(placed.unwrap_or(0));
            self.observed_queue_history = seed_queue(&observation);
            self.guidance = GuidanceState::default();
        } else if let Some(last) = self.history.last() {
            let Some(updated) = append_revealed(
                &self.observed_queue_history,
                &last.observation.next5,
                &observation.next5,
            ) else {
                return self.reject_discontinuity(
                    observation,
                    recognition_confidence,
                    timestamp_ms,
                );
            };
            self.game_queue
                .extend_from_slice(&updated[self.observed_queue_history.len()..]);
            self.observed_queue_history = updated;
        } else {
            self.observed_queue_history = seed_queue(&observation);
            self.game_queue = seed_queue(&observation);
        }

        let snapshot = self.push_snapshot(observation, recognition_confidence, timestamp_ms);
        CommitOutcome::Accepted { snapshot }
    }

    fn start_new_game(
        &mut self,
        observation: Observation,
        recognition_confidence: f32,
        timestamp_ms: u64,
    ) -> CommitOutcome {
        // Undo never crosses a restart, so the old game's states are gone.
        self.pc_ordinal = PcOrdinal::default();
        self.observed_queue_history = seed_queue(&observation);
        self.game_queue = seed_queue(&observation);
        self.guidance = GuidanceState::default();
        self.history.clear();
        let snapshot = self.push_snapshot(observation, recognition_confidence, timestamp_ms);
        CommitOutcome::NewGame { snapshot }
    }

    /// A state whose NEXT does not continue the latest snapshot's. It is
    /// rejected, so one misread cannot corrupt the queue history, until it is
    /// confirmed: the same state again `DISCONTINUITY_CONFIRM_MS` after it was
    /// first rejected, or a state whose NEXT continues the rejected one. Then
    /// the history is what was wrong (a restart or an Undo the checks above
    /// miss, or an earlier misread that was accepted), and the tracker starts
    /// over from the confirmed state with the same PC number.
    fn reject_discontinuity(
        &mut self,
        observation: Observation,
        recognition_confidence: f32,
        timestamp_ms: u64,
    ) -> CommitOutcome {
        let confirmed = self.unconfirmed.as_ref().is_some_and(|(seen, since)| {
            if *seen == observation {
                timestamp_ms.saturating_sub(*since) >= DISCONTINUITY_CONFIRM_MS
            } else {
                overlap(&seen.next5, &observation.next5).is_some()
            }
        });
        if !confirmed {
            if self
                .unconfirmed
                .as_ref()
                .is_none_or(|(seen, _)| *seen != observation)
            {
                self.unconfirmed = Some((observation, timestamp_ms));
            }
            return CommitOutcome::RejectedHistoryDiscontinuity;
        }
        self.observed_queue_history = seed_queue(&observation);
        self.game_queue = seed_queue(&observation);
        self.guidance = GuidanceState::default();
        self.history.clear();
        let snapshot = self.push_snapshot(observation, recognition_confidence, timestamp_ms);
        CommitOutcome::Resynced { snapshot }
    }

    /// Undo to a state that never became stable, such as one the player left
    /// within the frames the stability filter needs. Its NEXT5 is an earlier
    /// window of the game queue, and the newest snapshot at or before that
    /// window is where the undone placements began. Returns that snapshot's
    /// index and the queue position the observation's NEXT5 ends at.
    /// `undo_pressed` (Ctrl+Z was seen) drops the NEXT-overlap guard below,
    /// which tells an undo from play forward without the key.
    fn undo_target(&self, observation: &Observation, undo_pressed: bool) -> Option<(usize, usize)> {
        let last = self.history.last()?;
        // One or two placements forward keep three or more NEXT pieces in
        // view. An undo matches all five pieces of an earlier window instead,
        // and a short forward overlap next to such a match is a coincidence.
        if !undo_pressed
            && overlap(&last.observation.next5, &observation.next5).is_some_and(|size| size >= 3)
        {
            return None;
        }
        let earliest = self.history.first()?.queue_end;
        let position = (earliest.max(5)..last.queue_end)
            .rev()
            .find(|end| self.game_queue.get(end - 5..*end) == Some(&observation.next5[..]))?;
        let index = self
            .history
            .iter()
            .rposition(|snapshot| snapshot.queue_end <= position)?;
        // TETR.IO's undo restores HOLD with the rest of the state, and HOLD
        // never empties in play, so an empty HOLD where the target had one
        // is a restart, not an undo, with or without Ctrl+Z.
        if observation.hold.is_none() && self.history[index].observation.hold.is_some() {
            return None;
        }
        Some((index, position))
    }

    fn roll_back_to_unseen(
        &mut self,
        index: usize,
        position: usize,
        observation: Observation,
        recognition_confidence: f32,
        timestamp_ms: u64,
    ) -> CommitOutcome {
        let steps = self.history.len() - 1 - index;
        self.history.truncate(index + 1);
        let target = self.history.last().expect("undo target exists");
        self.pc_ordinal = target.pc_ordinal;
        self.observed_queue_history = target.observed_queue_history.clone();
        self.guidance = target.guidance.clone();
        // Undo can stop just after a PC whose empty board never became
        // stable: the target is still in the previous PC.
        if !board_is_empty(&target.observation.board) && board_is_empty(&observation.board) {
            let placed = self.placed_since_pc_start(position, observation.hold);
            self.pc_ordinal = self.pc_ordinal.after_pieces(placed.unwrap_or(0));
            self.observed_queue_history = seed_queue(&observation);
            self.guidance = GuidanceState::default();
        } else {
            self.observed_queue_history
                .extend_from_slice(&self.game_queue[target.queue_end..position]);
        }
        self.game_queue.truncate(position);
        let snapshot = self.push_snapshot(observation, recognition_confidence, timestamp_ms);
        CommitOutcome::RolledBack { steps, snapshot }
    }

    /// Pieces placed in the current PC up to a state whose NEXT5 ends at
    /// `queue_end` with `hold`: the queue advanced since the PC start, less
    /// the one piece the game's first HOLD takes without a placement. `None`
    /// when the PC start was not seen (a resync or a start mid-PC).
    fn placed_since_pc_start(&self, queue_end: usize, hold: Option<Piece>) -> Option<usize> {
        let start = self.current_pc_start()?;
        if !board_is_empty(&start.observation.board) {
            return None;
        }
        let first_hold = usize::from(start.observation.hold.is_none() && hold.is_some());
        queue_end
            .checked_sub(start.queue_end)?
            .checked_sub(first_hold)
    }

    /// The PC number the game's PIECES counter gives for the latest state:
    /// the pieces before the current PC are the counter less the pieces
    /// placed in it, which a 2-line PC (5 pieces) keeps off the multiples of
    /// 10. Without a seen PC start, the 10-per-PC count. `None` when the
    /// counter and the tracked state disagree (a frame of one and not the
    /// other).
    pub fn ordinal_from_pieces(&self, pieces: u32) -> Option<PcOrdinal> {
        let Some(latest) = self.history.last() else {
            return Some(PcOrdinal::from_pieces_placed(pieces));
        };
        match self.placed_since_pc_start(latest.queue_end, latest.observation.hold) {
            Some(placed) => PcOrdinal::at_pc_start(pieces.checked_sub(placed as u32)?),
            None => Some(PcOrdinal::from_pieces_placed(pieces)),
        }
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
            queue_end: self.game_queue.len(),
            guidance: self.guidance.clone(),
            recognition_confidence,
        };
        self.history.push(snapshot.clone());
        snapshot
    }
}

/// A restarted game (`R` in ZEN) is told from a PC by HOLD, not by the board:
/// once filled, HOLD never empties in play (undo restores the HOLD of the
/// state it returns to), while a restart empties it.
/// Before the first hold, a NEXT queue that does not continue the previous
/// one is a restart too, with or without a HOLD now: the first state of a
/// restart can show one already, when HOLD was pressed before it became
/// stable (or during the countdown). Such a state is in PC 1, so a false
/// positive from skipped frames there costs nothing. Undo to an earlier state of the same
/// game is matched by the rollback before this check.
fn starts_new_game(previous: &Observation, observation: &Observation) -> bool {
    let continues = overlap(&previous.next5, &observation.next5).is_some();
    (observation.hold.is_none() && previous.hold.is_some())
        || (previous.hold.is_none() && !continues)
}

fn seed_queue(observation: &Observation) -> Vec<Piece> {
    std::iter::once(observation.current)
        .chain(observation.next5)
        .collect()
}

/// How many pieces at the end of the previous NEXT5 open the current one.
fn overlap(previous: &[Piece; 5], current: &[Piece; 5]) -> Option<usize> {
    (1..=5)
        .rev()
        .find(|size| previous[5 - size..] == current[..*size])
}

fn append_revealed(
    history: &[Piece],
    previous: &[Piece; 5],
    current: &[Piece; 5],
) -> Option<Vec<Piece>> {
    let overlap = overlap(previous, current)?;
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
    fn seventh_pc_case_is_three_pieces() {
        let mut tracker = tracker();
        tracker.resync(PcOrdinal::new(6).unwrap());
        tracker.commit(
            observation(
                true,
                Piece::T,
                [Piece::Z, Piece::S, Piece::I, Piece::J, Piece::L],
            ),
            1.0,
            1,
        );
        // PC 7 starts with HOLD I, so O and T are the two case pieces in queue.
        tracker.commit(
            observation(
                false,
                Piece::O,
                [Piece::T, Piece::J, Piece::L, Piece::S, Piece::Z],
            ),
            1.0,
            2,
        );

        let start = tracker.current_pc_start().expect("PC start observed");
        assert_eq!(start.pc_ordinal.get(), 7);
        let context = tracker.following_bag_context().expect("context");
        assert_eq!(context.case_queue, [Piece::O, Piece::T]);
        assert_eq!(
            context.queue_history,
            [Piece::J, Piece::L, Piece::S, Piece::Z]
        );
        assert_eq!(case_size(PcOrdinal::new(1).unwrap()), None);
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
    fn aligning_the_ordinal_relabels_the_current_pc_and_keeps_undo() {
        let mut tracker = tracker();
        let first = observation(
            true,
            Piece::I,
            [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
        );
        let second = observation(
            true,
            Piece::J,
            [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
        );
        tracker.commit(first.clone(), 1.0, 1);
        tracker.commit(second, 1.0, 2);

        assert!(!tracker.align_pc_ordinal(PcOrdinal::default()));
        assert!(tracker.align_pc_ordinal(PcOrdinal::new(3).unwrap()));
        assert_eq!(tracker.pc_ordinal().get(), 3);
        assert!(
            tracker
                .snapshots()
                .iter()
                .all(|snapshot| snapshot.pc_ordinal.get() == 3)
        );
        assert_eq!(tracker.current_pc_start().unwrap().timestamp_ms, 1);

        // Undo still finds the earlier state, now labeled with the new PC.
        assert!(matches!(
            tracker.commit(first, 1.0, 3),
            CommitOutcome::RolledBack { steps: 1, .. }
        ));
        assert_eq!(tracker.pc_ordinal().get(), 3);
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
    fn restart_with_a_hold_before_its_first_stable_state_is_a_new_game() {
        let mut tracker = tracker();
        tracker.resync(PcOrdinal::new(3).unwrap());
        let mut start = observation(
            false,
            Piece::I,
            [Piece::J, Piece::L, Piece::O, Piece::S, Piece::T],
        );
        start.hold = None;
        tracker.commit(start, 1.0, 1);
        // Restarted and HOLD pressed at once: the first stable state of the
        // new game already has a HOLD, and its queue does not continue.
        // Previously rejected as a discontinuity, which froze the guidance
        // on the old game's start.
        let restarted = observation(
            false,
            Piece::T,
            [Piece::S, Piece::Z, Piece::I, Piece::O, Piece::L],
        );
        assert!(matches!(
            tracker.commit(restarted, 1.0, 2),
            CommitOutcome::NewGame { .. }
        ));
        assert_eq!(tracker.pc_ordinal().get(), 1);
    }

    /// A tracker in PC 3 whose latest state has a HOLD, and a state that
    /// does not continue it.
    fn tracker_before_a_discontinuity() -> (SessionTracker, Observation) {
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
        let jump = observation(
            true,
            Piece::Z,
            [Piece::I, Piece::O, Piece::L, Piece::J, Piece::Z],
        );
        (tracker, jump)
    }

    #[test]
    fn a_discontinuity_that_stays_on_screen_restarts_tracking_from_it() {
        let (mut tracker, jump) = tracker_before_a_discontinuity();
        assert_eq!(
            tracker.commit(jump.clone(), 1.0, 10),
            CommitOutcome::RejectedHistoryDiscontinuity
        );
        assert_eq!(
            tracker.commit(jump.clone(), 1.0, 10 + DISCONTINUITY_CONFIRM_MS - 1),
            CommitOutcome::RejectedHistoryDiscontinuity
        );
        let outcome = tracker.commit(jump.clone(), 1.0, 10 + DISCONTINUITY_CONFIRM_MS);
        assert!(
            matches!(&outcome, CommitOutcome::Resynced { snapshot } if snapshot.observation == jump),
            "{outcome:?}"
        );
        assert_eq!(tracker.pc_ordinal().get(), 3);
        assert_eq!(tracker.snapshots().len(), 1);
        assert_eq!(tracker.observed_queue_history().len(), 6);
        // Later states continue from it.
        assert!(matches!(
            tracker.commit(
                observation(
                    true,
                    Piece::I,
                    [Piece::O, Piece::L, Piece::J, Piece::Z, Piece::T],
                ),
                1.0,
                1000,
            ),
            CommitOutcome::Accepted { .. }
        ));
    }

    #[test]
    fn a_next_state_continuing_a_rejected_one_confirms_it() {
        let (mut tracker, jump) = tracker_before_a_discontinuity();
        assert_eq!(
            tracker.commit(jump, 1.0, 10),
            CommitOutcome::RejectedHistoryDiscontinuity
        );
        let placed = observation(
            true,
            Piece::I,
            [Piece::O, Piece::L, Piece::J, Piece::Z, Piece::T],
        );
        let outcome = tracker.commit(placed.clone(), 1.0, 20);
        assert!(
            matches!(&outcome, CommitOutcome::Resynced { snapshot } if snapshot.observation == placed),
            "{outcome:?}"
        );
        assert_eq!(tracker.pc_ordinal().get(), 3);
    }

    #[test]
    fn a_short_misread_is_still_rejected() {
        let (mut tracker, jump) = tracker_before_a_discontinuity();
        assert_eq!(
            tracker.commit(jump.clone(), 1.0, 10),
            CommitOutcome::RejectedHistoryDiscontinuity
        );
        // The history continues, so the misread is dropped...
        assert!(matches!(
            tracker.commit(
                observation(
                    true,
                    Piece::J,
                    [Piece::L, Piece::O, Piece::S, Piece::T, Piece::Z],
                ),
                1.0,
                20,
            ),
            CommitOutcome::Accepted { .. }
        ));
        // ...and seen again later it waits for confirmation afresh.
        assert_eq!(
            tracker.commit(jump, 1.0, 10 + DISCONTINUITY_CONFIRM_MS),
            CommitOutcome::RejectedHistoryDiscontinuity
        );
        assert_eq!(tracker.snapshots().len(), 2);
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

    /// Three 7-bags, so NEXT5 windows tell queue positions apart.
    const QUEUE: [Piece; 21] = [
        Piece::I,
        Piece::J,
        Piece::L,
        Piece::O,
        Piece::S,
        Piece::T,
        Piece::Z,
        Piece::T,
        Piece::S,
        Piece::Z,
        Piece::I,
        Piece::O,
        Piece::L,
        Piece::J,
        Piece::L,
        Piece::J,
        Piece::O,
        Piece::I,
        Piece::T,
        Piece::S,
        Piece::Z,
    ];

    /// The state with `QUEUE[index]` as Current.
    fn at(index: usize, filled: bool) -> Observation {
        let mut next5 = [Piece::I; 5];
        next5.copy_from_slice(&QUEUE[index + 1..index + 6]);
        observation(filled, QUEUE[index], next5)
    }

    fn assert_rolled_back(outcome: CommitOutcome, expected_steps: usize) {
        match outcome {
            CommitOutcome::RolledBack { steps, .. } => assert_eq!(steps, expected_steps),
            other => panic!("expected a rollback, got {other:?}"),
        }
    }

    #[test]
    fn undo_before_a_pc_to_an_unstable_state_restores_the_previous_pc() {
        let mut tracker = tracker();
        tracker.commit(at(0, true), 1.0, 1);
        // The last PC piece was placed from a state that never became stable.
        tracker.commit(at(2, false), 1.0, 2);
        assert_eq!(tracker.pc_ordinal().get(), 2);

        assert_rolled_back(tracker.commit(at(1, true), 1.0, 3), 1);
        assert_eq!(tracker.pc_ordinal().get(), 1);
        assert_eq!(tracker.observed_queue_history(), &QUEUE[..7]);
        let start = tracker.current_pc_start().expect("PC 1 start");
        assert_eq!(start.observation, at(0, true));

        // Previously the undo was rejected, and the same queue position with
        // a filled board was then accepted as PC 2, so the next PC read 3.
        let mut misplaced = at(2, true);
        misplaced.board[0][1] = Some(Piece::L);
        assert!(matches!(
            tracker.commit(misplaced, 1.0, 4),
            CommitOutcome::Accepted { .. }
        ));
        assert_eq!(tracker.pc_ordinal().get(), 1);
        tracker.commit(at(3, false), 1.0, 5);
        assert_eq!(tracker.pc_ordinal().get(), 2);
        let start = tracker.current_pc_start().expect("PC 2 start");
        assert_eq!(start.observation, at(3, false));
    }

    #[test]
    fn undo_to_an_unstable_pc_start_does_not_count_the_pc_again() {
        let mut tracker = tracker();
        tracker.commit(at(0, true), 1.0, 1);
        // PC 2 starts with HOLD I and Current J; only the state after holding
        // J (HOLD J, Current I) became stable.
        let pc_start = at(1, false);
        let mut held = pc_start.clone();
        held.hold = Some(QUEUE[1]);
        held.current = Piece::I;
        tracker.commit(held, 1.0, 2);
        assert_eq!(tracker.pc_ordinal().get(), 2);
        tracker.commit(at(2, true), 1.0, 3);
        tracker.commit(at(3, true), 1.0, 4);

        // Previously this empty board after a filled one counted PC 3.
        assert_rolled_back(tracker.commit(pc_start.clone(), 1.0, 5), 2);
        assert_eq!(tracker.pc_ordinal().get(), 2);
        // Seeded at the stable PC start, whose Current was the held-out I.
        assert_eq!(tracker.observed_queue_history()[0], Piece::I);
        assert_eq!(tracker.observed_queue_history()[1..], QUEUE[2..7]);
        let start = tracker.current_pc_start().expect("PC 2 start");
        assert_eq!(start.observation.board, pc_start.board);
        assert_eq!(start.observation.hold, Some(QUEUE[1]));
    }

    #[test]
    fn undo_within_a_pc_to_an_unstable_state_keeps_the_queue() {
        let mut tracker = tracker();
        tracker.commit(at(0, true), 1.0, 1);
        tracker.commit(at(2, true), 1.0, 2);
        tracker.commit(at(4, true), 1.0, 3);

        // Undo past the stable state at 2 to the unstable one at 1.
        assert_rolled_back(tracker.commit(at(1, true), 1.0, 4), 2);
        assert_eq!(tracker.pc_ordinal().get(), 1);
        assert_eq!(tracker.observed_queue_history(), &QUEUE[..7]);

        // Play resumes forward without repeating disclosed pieces.
        assert!(matches!(
            tracker.commit(at(2, true), 1.0, 5),
            CommitOutcome::Accepted { .. }
        ));
        assert!(matches!(
            tracker.commit(at(3, false), 1.0, 6),
            CommitOutcome::Accepted { .. }
        ));
        assert_eq!(tracker.pc_ordinal().get(), 2);
        assert!(matches!(
            tracker.commit(at(4, true), 1.0, 7),
            CommitOutcome::Accepted { .. }
        ));
        assert_eq!(tracker.observed_queue_history(), &QUEUE[3..10]);

        // An exact match still rolls back across the PC.
        assert_rolled_back(tracker.commit(at(2, true), 1.0, 8), 2);
        assert_eq!(tracker.pc_ordinal().get(), 1);
        assert_eq!(tracker.observed_queue_history(), &QUEUE[..8]);
    }

    #[test]
    fn a_two_line_pc_counts_four_pcs_and_undo_restores_it() {
        let mut tracker = tracker();
        tracker.commit(at(0, false), 1.0, 1);
        tracker.commit(at(2, true), 1.0, 2);
        // Five pieces from the empty start: a 2-line PC.
        tracker.commit(at(5, false), 1.0, 3);
        assert_eq!(tracker.pc_ordinal().get(), 5);
        assert_rolled_back(tracker.commit(at(2, true), 1.0, 4), 1);
        assert_eq!(tracker.pc_ordinal().get(), 1);
        // Undo stopping on the empty board after the PC (never stable)
        // counts the same four.
        tracker.commit(at(4, true), 1.0, 5);
        tracker.commit(at(6, true), 1.0, 6);
        assert_rolled_back(tracker.commit(at(5, false), 1.0, 7), 1);
        assert_eq!(tracker.pc_ordinal().get(), 5);
    }

    #[test]
    fn a_pc_start_not_seen_keeps_the_one_step_count() {
        let mut tracker = tracker();
        // Tracking began mid-PC, so the pieces of the PC are unknown.
        tracker.commit(at(0, true), 1.0, 1);
        tracker.commit(at(5, false), 1.0, 2);
        assert_eq!(tracker.pc_ordinal().get(), 2);
    }

    #[test]
    fn pieces_counter_gives_the_ordinal_after_a_two_line_pc() {
        let mut tracker = tracker();
        assert_eq!(tracker.ordinal_from_pieces(25).map(PcOrdinal::get), Some(3));
        tracker.commit(at(0, false), 1.0, 1);
        tracker.commit(at(3, true), 1.0, 2);
        tracker.commit(at(5, false), 1.0, 3);
        tracker.commit(at(7, true), 1.0, 4);
        // PIECES 7: five before this PC and two in it, the 5th PC.
        assert_eq!(tracker.ordinal_from_pieces(7).map(PcOrdinal::get), Some(5));
        // A counter one frame ahead of the tracked state does not align.
        assert_eq!(tracker.ordinal_from_pieces(8), None);
        assert_eq!(tracker.ordinal_from_pieces(1), None);
    }

    #[test]
    fn undo_key_finds_a_state_whose_next_seems_to_continue() {
        // QUEUE repeats T S Z: NEXT at 6 (T S Z I O) opens with the last
        // three pieces of NEXT at 15 (. . T S Z), so the undo looks like
        // play forward.
        let played = || {
            let mut tracker = tracker();
            for (step, index) in [0, 4, 8, 12, 15].into_iter().enumerate() {
                tracker.commit(at(index, true), 1.0, step as u64 + 1);
            }
            tracker
        };
        let mut tracker = played();
        assert!(!matches!(
            tracker.commit(at(6, true), 1.0, 10),
            CommitOutcome::RolledBack { .. }
        ));

        let mut tracker = played();
        tracker.note_key(GameKey::Undo, 9);
        assert_rolled_back(tracker.commit(at(6, true), 1.0, 10), 3);
        assert_eq!(tracker.observed_queue_history(), &QUEUE[..12]);
    }

    #[test]
    fn undo_key_still_reads_an_emptied_hold_as_a_restart() {
        // TETR.IO's undo restores HOLD, so a state matching an earlier one
        // but HOLD is not that state.
        let mut tracker = tracker();
        tracker.commit(at(0, true), 1.0, 1);
        tracker.commit(at(3, true), 1.0, 2);
        let mut restarted = at(1, true);
        restarted.hold = None;
        tracker.note_key(GameKey::Undo, 3);
        assert!(matches!(
            tracker.commit(restarted, 1.0, 4),
            CommitOutcome::NewGame { .. }
        ));
    }

    #[test]
    fn retry_key_makes_an_empty_board_a_new_game_even_with_a_hold() {
        // Restarted from PC 3, with HOLD pressed before the first stable
        // frame: without the key the empty board reads as a PC.
        let restarted = at(4, false);
        let played = || {
            let mut tracker = tracker();
            tracker.resync(PcOrdinal::new(3).unwrap());
            tracker.commit(at(0, true), 1.0, 1);
            tracker
        };
        let mut tracker = played();
        tracker.commit(restarted.clone(), 1.0, 10);
        assert_eq!(tracker.pc_ordinal().get(), 4);

        let mut tracker = played();
        tracker.note_key(GameKey::Retry, 9);
        assert!(matches!(
            tracker.commit(restarted.clone(), 1.0, 10),
            CommitOutcome::NewGame { .. }
        ));
        assert_eq!(tracker.pc_ordinal().get(), 1);

        // A key pressed long before the change is no hint.
        let mut tracker = played();
        tracker.note_key(GameKey::Retry, 9);
        tracker.commit(restarted, 1.0, 10 + KEY_HINT_MS);
        assert_eq!(tracker.pc_ordinal().get(), 4);
    }

    #[test]
    fn a_key_hint_applies_to_one_change_only() {
        let mut tracker = tracker();
        tracker.resync(PcOrdinal::new(3).unwrap());
        tracker.commit(at(0, true), 1.0, 1);
        tracker.note_key(GameKey::Retry, 2);
        // Unchanged frames keep the hint; the placement uses it up.
        assert_eq!(
            tracker.commit(at(0, true), 1.0, 3),
            CommitOutcome::Unchanged
        );
        assert!(matches!(
            tracker.commit(at(1, true), 1.0, 4),
            CommitOutcome::Accepted { .. }
        ));
        tracker.commit(at(2, false), 1.0, 5);
        assert_eq!(tracker.pc_ordinal().get(), 4, "a PC, not a restart");
    }

    #[test]
    fn redo_key_is_never_taken_for_an_undo() {
        let played = || {
            let mut tracker = tracker();
            tracker.commit(at(0, true), 1.0, 1);
            tracker.commit(at(3, true), 1.0, 2);
            tracker
        };
        let mut tracker = played();
        assert_rolled_back(tracker.commit(at(1, true), 1.0, 3), 1);

        let mut tracker = played();
        tracker.note_key(GameKey::Redo, 2);
        assert_eq!(
            tracker.commit(at(1, true), 1.0, 3),
            CommitOutcome::RejectedHistoryDiscontinuity
        );
    }

    #[test]
    fn undo_far_back_within_the_game_restores_the_earlier_pc() {
        let mut tracker = tracker();
        tracker.commit(at(0, true), 1.0, 1);
        tracker.commit(at(3, false), 1.0, 2);
        tracker.commit(at(5, true), 1.0, 3);
        // Six pieces: five would be a 2-line PC, which counts four PCs.
        tracker.commit(at(9, false), 1.0, 4);
        tracker.commit(at(10, true), 1.0, 5);
        assert_eq!(tracker.pc_ordinal().get(), 3);

        // Several undos at once, back into PC 1, as one stable frame.
        assert_rolled_back(tracker.commit(at(2, true), 1.0, 6), 4);
        assert_eq!(tracker.pc_ordinal().get(), 1);
        assert_eq!(tracker.observed_queue_history(), &QUEUE[..8]);
    }

    #[test]
    fn restart_with_an_earlier_queue_window_is_still_a_new_game() {
        let mut tracker = tracker();
        tracker.commit(at(0, true), 1.0, 1);
        tracker.commit(at(3, false), 1.0, 2);
        tracker.commit(at(5, true), 1.0, 3);
        let mut restarted = at(1, false);
        restarted.hold = None;
        assert!(matches!(
            tracker.commit(restarted, 1.0, 4),
            CommitOutcome::NewGame { .. }
        ));
        assert_eq!(tracker.pc_ordinal().get(), 1);
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
