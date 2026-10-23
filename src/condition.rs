use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::model::Piece;

/// Three-valued evaluation is essential while NEXT is still being disclosed.
/// Pending never excludes a candidate and is never treated as satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalResult {
    Satisfied,
    Failed,
    Pending,
}

impl EvalResult {
    fn not(self) -> Self {
        match self {
            Self::Satisfied => Self::Failed,
            Self::Failed => Self::Satisfied,
            Self::Pending => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalContext {
    /// Pieces observed in order from the start of the current PC interval.
    pub queue_history: Vec<Piece>,
    /// True only for bounded fixtures/imports. Live sessions normally remain open.
    #[serde(default)]
    pub queue_closed: bool,
    pub hold: Option<Piece>,
    pub current: Piece,
    /// Recognition/tracking computes this set; the DSL does not guess reachability.
    #[serde(default)]
    pub accessible: BTreeSet<Piece>,
    /// HOLD when the 2nd PC started, and the case pieces that were in
    /// Current/NEXT, in queue order. `solvable` replays from there, so a HOLD
    /// that is still occupied when the following bag starts is modelled.
    /// Empty when unknown; `solvable` then assumes an empty HOLD at the bag.
    #[serde(default)]
    pub case_hold: Option<Piece>,
    #[serde(default)]
    pub case_queue: Vec<Piece>,
}

pub const BAG_SIZE: usize = 7;

impl EvalContext {
    /// Source conditions such as `[TJ]!,S` describe the bag that follows the
    /// 2nd-PC case pieces, not the case pieces themselves. `case_pieces_in_queue`
    /// is how many case pieces were in Current/NEXT (rather than HOLD) when the
    /// PC started. The window is limited to one bag and closes once that bag is
    /// fully revealed, so order conditions resolve instead of staying pending.
    pub fn for_following_bag(
        queue_history: &[Piece],
        case_pieces_in_queue: usize,
        hold: Option<Piece>,
        current: Piece,
    ) -> Self {
        let bag: Vec<Piece> = queue_history
            .iter()
            .skip(case_pieces_in_queue)
            .take(BAG_SIZE)
            .copied()
            .collect();
        Self {
            queue_closed: bag.len() == BAG_SIZE,
            queue_history: bag,
            hold,
            current,
            accessible: BTreeSet::new(),
            case_hold: None,
            case_queue: Vec::new(),
        }
    }

    /// Adds the 2nd-PC start: HOLD then and the case pieces in queue order.
    pub fn with_case(mut self, case_hold: Option<Piece>, case_queue: &[Piece]) -> Self {
        self.case_hold = case_hold;
        self.case_queue = case_queue.to_vec();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueuePattern {
    /// Matches a prefix containing exactly these pieces in any order, optionally
    /// followed by one exact piece. This represents source notation such as
    /// `[TJ]!,S` without assigning meaning to a free-form string at runtime.
    PermutationPrefix {
        pieces: Vec<Piece>,
        #[serde(default)]
        then: Option<Piece>,
    },
    ExactPrefix {
        pieces: Vec<Piece>,
    },
}

impl QueuePattern {
    pub fn evaluate(&self, history: &[Piece], queue_closed: bool) -> EvalResult {
        let (required_len, prefix_matches) = match self {
            Self::PermutationPrefix { pieces, then } => {
                let required_len = pieces.len() + usize::from(then.is_some());
                if history.len() < pieces.len() {
                    return if queue_closed {
                        EvalResult::Failed
                    } else {
                        EvalResult::Pending
                    };
                }

                let mut expected = pieces.clone();
                let mut actual = history[..pieces.len()].to_vec();
                expected.sort_unstable();
                actual.sort_unstable();
                let prefix_matches = expected == actual
                    && then
                        .map(|piece| history.get(pieces.len()).is_some_and(|seen| *seen == piece))
                        .unwrap_or(true);
                (required_len, prefix_matches)
            }
            Self::ExactPrefix { pieces } => {
                let available = history.len().min(pieces.len());
                if history[..available] != pieces[..available] {
                    return EvalResult::Failed;
                }
                (pieces.len(), history.len() >= pieces.len())
            }
        };

        if history.len() >= required_len {
            if prefix_matches {
                EvalResult::Satisfied
            } else {
                EvalResult::Failed
            }
        } else if queue_closed {
            EvalResult::Failed
        } else {
            EvalResult::Pending
        }
    }

    pub fn mirrored(&self) -> Self {
        match self {
            Self::PermutationPrefix { pieces, then } => Self::PermutationPrefix {
                pieces: pieces.iter().copied().map(Piece::mirrored).collect(),
                then: then.map(Piece::mirrored),
            },
            Self::ExactPrefix { pieces } => Self::ExactPrefix {
                pieces: pieces.iter().copied().map(Piece::mirrored).collect(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Condition {
    Always,
    All {
        conditions: Vec<Condition>,
    },
    Any {
        conditions: Vec<Condition>,
    },
    Not {
        condition: Box<Condition>,
    },
    Seen {
        piece: Piece,
    },
    NotSeen {
        piece: Piece,
    },
    Before {
        first: Piece,
        second: Piece,
    },
    After {
        first: Piece,
        second: Piece,
    },
    LastAmong {
        piece: Piece,
        pieces: Vec<Piece>,
    },
    InFirstN {
        piece: Piece,
        n: usize,
    },
    BeforeAny {
        piece: Piece,
        candidates: Vec<Piece>,
        count: usize,
    },
    QueueMatches {
        pattern: QueuePattern,
    },
    HoldIs {
        piece: Piece,
    },
    CurrentIs {
        piece: Piece,
    },
    Accessible {
        piece: Piece,
    },
    /// The following bag can be played into a solve with one HOLD slot.
    /// Each solve piece may be placed only after its prerequisites; `saved`
    /// is never placed, so once it arrives it occupies HOLD. Satisfied when
    /// every undisclosed completion of the bag works, Failed when none does.
    /// The prerequisites are derived from SRS reachability and checked
    /// against it by `tools/ijlo_solves.py verify`.
    OrderedWithHold {
        prerequisites: Vec<Prerequisite>,
        saved: Piece,
    },
    /// Generated by tools/build_cases.py: `prefix` (case pieces the setup
    /// leaves over) followed by the following bag can be played into one of
    /// `solves` with one HOLD slot. Each solve carries its SRS placement
    /// table, so no queue rule is read from the prose. Satisfied when every
    /// undisclosed completion of the bag works, Failed when none does.
    Solvable {
        #[serde(default)]
        prefix: Vec<Piece>,
        /// The case pieces the setup uses. With the case start known, they
        /// are played from the queue (any order, before every other piece).
        #[serde(default)]
        setup: Vec<Piece>,
        solves: Vec<SolvableSolve>,
    },
}

/// One solve's placement table: bit i of a mask is `pieces[i]`; each
/// `[placed, allowed]` pair lists the pieces SRS can place next from that
/// state (setup pieces from the following bag come first).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SolvableSolve {
    pub solve: String,
    pub pieces: Vec<Piece>,
    pub transitions: Vec<(u32, u32)>,
    /// The table also holds the setup's pieces (after the solve's own), for
    /// a queue-based solve that places a piece before the setup is done.
    #[serde(default)]
    pub setup_in_table: bool,
}

impl SolvableSolve {
    fn allowed(&self, placed: u32) -> u32 {
        self.transitions
            .binary_search_by_key(&placed, |(mask, _)| *mask)
            .map(|index| self.transitions[index].1)
            .unwrap_or(0)
    }
}

/// `first` must already be placed before `then` can be placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prerequisite {
    pub first: Piece,
    pub then: Piece,
}

impl Condition {
    pub fn evaluate(&self, context: &EvalContext) -> EvalResult {
        use EvalResult::{Failed, Pending, Satisfied};

        match self {
            Self::Always => Satisfied,
            Self::All { conditions } => {
                let results = conditions.iter().map(|item| item.evaluate(context));
                fold_all(results)
            }
            Self::Any { conditions } => {
                let results = conditions.iter().map(|item| item.evaluate(context));
                fold_any(results)
            }
            Self::Not { condition } => condition.evaluate(context).not(),
            Self::Seen { piece } => {
                if context.queue_history.contains(piece) {
                    Satisfied
                } else if context.queue_closed {
                    Failed
                } else {
                    Pending
                }
            }
            Self::NotSeen { piece } => {
                if context.queue_history.contains(piece) {
                    Failed
                } else if context.queue_closed {
                    Satisfied
                } else {
                    Pending
                }
            }
            Self::Before { first, second } => before(context, *first, *second),
            Self::After { first, second } => before(context, *second, *first),
            Self::LastAmong { piece, pieces } => last_among(context, *piece, pieces),
            Self::InFirstN { piece, n } => {
                let window_len = context.queue_history.len().min(*n);
                if context.queue_history[..window_len].contains(piece) {
                    Satisfied
                } else if context.queue_history.len() >= *n || context.queue_closed {
                    Failed
                } else {
                    Pending
                }
            }
            Self::BeforeAny {
                piece,
                candidates,
                count,
            } => before_any(context, *piece, candidates, *count),
            Self::QueueMatches { pattern } => {
                pattern.evaluate(&context.queue_history, context.queue_closed)
            }
            Self::HoldIs { piece } => {
                if context.hold == Some(*piece) {
                    Satisfied
                } else {
                    Failed
                }
            }
            Self::CurrentIs { piece } => {
                if context.current == *piece {
                    Satisfied
                } else {
                    Failed
                }
            }
            Self::Accessible { piece } => {
                if context.accessible.contains(piece) {
                    Satisfied
                } else {
                    Failed
                }
            }
            Self::OrderedWithHold {
                prerequisites,
                saved,
            } => ordered_with_hold(context, prerequisites, *saved),
            Self::Solvable {
                prefix,
                setup,
                solves,
            } => solvable(context, prefix, setup, solves),
        }
    }

    /// For `solvable`: how many undisclosed completions of the bag some solve
    /// can play, out of how many. `None` for every other condition.
    pub fn success_ratio(&self, context: &EvalContext) -> Option<(usize, usize)> {
        match self {
            Self::Solvable {
                prefix,
                setup,
                solves,
            } => Some(solvable_counts(context, prefix, setup, solves)),
            _ => None,
        }
    }

    /// Earliest disclosed prefix at which the condition becomes satisfied.
    /// Non-queue context stays fixed. `None` means it is not yet satisfied.
    pub fn satisfied_at(&self, context: &EvalContext) -> Option<usize> {
        (0..=context.queue_history.len()).find(|prefix_len| {
            let mut prefix_context = context.clone();
            prefix_context.queue_history.truncate(*prefix_len);
            prefix_context.queue_closed = false;
            self.evaluate(&prefix_context) == EvalResult::Satisfied
        })
    }

    pub fn mirrored(&self) -> Self {
        match self {
            Self::Always => Self::Always,
            Self::All { conditions } => Self::All {
                conditions: conditions.iter().map(Self::mirrored).collect(),
            },
            Self::Any { conditions } => Self::Any {
                conditions: conditions.iter().map(Self::mirrored).collect(),
            },
            Self::Not { condition } => Self::Not {
                condition: Box::new(condition.mirrored()),
            },
            Self::Seen { piece } => Self::Seen {
                piece: piece.mirrored(),
            },
            Self::NotSeen { piece } => Self::NotSeen {
                piece: piece.mirrored(),
            },
            Self::Before { first, second } => Self::Before {
                first: first.mirrored(),
                second: second.mirrored(),
            },
            Self::After { first, second } => Self::After {
                first: first.mirrored(),
                second: second.mirrored(),
            },
            Self::LastAmong { piece, pieces } => Self::LastAmong {
                piece: piece.mirrored(),
                pieces: pieces.iter().copied().map(Piece::mirrored).collect(),
            },
            Self::InFirstN { piece, n } => Self::InFirstN {
                piece: piece.mirrored(),
                n: *n,
            },
            Self::BeforeAny {
                piece,
                candidates,
                count,
            } => Self::BeforeAny {
                piece: piece.mirrored(),
                candidates: candidates.iter().copied().map(Piece::mirrored).collect(),
                count: *count,
            },
            Self::QueueMatches { pattern } => Self::QueueMatches {
                pattern: pattern.mirrored(),
            },
            Self::HoldIs { piece } => Self::HoldIs {
                piece: piece.mirrored(),
            },
            Self::CurrentIs { piece } => Self::CurrentIs {
                piece: piece.mirrored(),
            },
            Self::Accessible { piece } => Self::Accessible {
                piece: piece.mirrored(),
            },
            Self::Solvable {
                prefix,
                setup,
                solves,
            } => Self::Solvable {
                prefix: prefix.iter().map(|piece| piece.mirrored()).collect(),
                setup: setup.iter().map(|piece| piece.mirrored()).collect(),
                // The SRS kick table is not symmetric; build_cases.py writes
                // mirrored setups with their own tables, so this mirror is
                // only for completeness.
                solves: solves
                    .iter()
                    .map(|solve| SolvableSolve {
                        solve: solve.solve.clone(),
                        pieces: solve.pieces.iter().map(|piece| piece.mirrored()).collect(),
                        transitions: solve.transitions.clone(),
                        setup_in_table: solve.setup_in_table,
                    })
                    .collect(),
            },
            Self::OrderedWithHold {
                prerequisites,
                saved,
            } => Self::OrderedWithHold {
                prerequisites: prerequisites
                    .iter()
                    .map(|rule| Prerequisite {
                        first: rule.first.mirrored(),
                        then: rule.then.mirrored(),
                    })
                    .collect(),
                saved: saved.mirrored(),
            },
        }
    }
}

fn fold_all(results: impl Iterator<Item = EvalResult>) -> EvalResult {
    let mut pending = false;
    for result in results {
        match result {
            EvalResult::Failed => return EvalResult::Failed,
            EvalResult::Pending => pending = true,
            EvalResult::Satisfied => {}
        }
    }
    if pending {
        EvalResult::Pending
    } else {
        EvalResult::Satisfied
    }
}

fn fold_any(results: impl Iterator<Item = EvalResult>) -> EvalResult {
    let mut pending = false;
    for result in results {
        match result {
            EvalResult::Satisfied => return EvalResult::Satisfied,
            EvalResult::Pending => pending = true,
            EvalResult::Failed => {}
        }
    }
    if pending {
        EvalResult::Pending
    } else {
        EvalResult::Failed
    }
}

fn before(context: &EvalContext, first: Piece, second: Piece) -> EvalResult {
    let first_index = context.queue_history.iter().position(|seen| *seen == first);
    let second_index = context
        .queue_history
        .iter()
        .position(|seen| *seen == second);
    match (first_index, second_index) {
        (Some(a), Some(b)) if a < b => EvalResult::Satisfied,
        (Some(_), Some(_)) => EvalResult::Failed,
        (Some(_), None) => EvalResult::Satisfied,
        (None, Some(_)) => EvalResult::Failed,
        (None, None) if context.queue_closed => EvalResult::Failed,
        (None, None) => EvalResult::Pending,
    }
}

fn last_among(context: &EvalContext, piece: Piece, pieces: &[Piece]) -> EvalResult {
    let required: BTreeSet<_> = pieces.iter().copied().chain([piece]).collect();
    let missing: Vec<_> = required
        .iter()
        .filter(|required_piece| !context.queue_history.contains(required_piece))
        .collect();
    if !missing.is_empty() {
        if context.queue_closed {
            return EvalResult::Failed;
        }
        // The window is one bag, so every undisclosed piece still arrives,
        // after everything disclosed. With `piece` the only one missing it
        // will be last; if `piece` is disclosed and another is not, it won't.
        return if missing == [&piece] {
            EvalResult::Satisfied
        } else if !missing.contains(&&piece) {
            EvalResult::Failed
        } else {
            EvalResult::Pending
        };
    }
    let last = context
        .queue_history
        .iter()
        .enumerate()
        .filter(|(_, seen)| required.contains(seen))
        .max_by_key(|(index, _)| *index)
        .map(|(_, seen)| *seen);
    if last == Some(piece) {
        EvalResult::Satisfied
    } else {
        EvalResult::Failed
    }
}

fn before_any(
    context: &EvalContext,
    piece: Piece,
    candidates: &[Piece],
    count: usize,
) -> EvalResult {
    if count == 0 {
        return EvalResult::Satisfied;
    }
    let Some(piece_index) = context.queue_history.iter().position(|seen| *seen == piece) else {
        return if context.queue_closed {
            EvalResult::Failed
        } else {
            EvalResult::Pending
        };
    };
    let after_count = candidates
        .iter()
        .filter(|candidate| {
            context
                .queue_history
                .iter()
                .position(|seen| seen == *candidate)
                .is_some_and(|index| piece_index < index)
        })
        .count();
    if after_count >= count {
        EvalResult::Satisfied
    } else if context.queue_closed {
        EvalResult::Failed
    } else {
        EvalResult::Pending
    }
}

const ALL_PIECES: [Piece; BAG_SIZE] = [
    Piece::I,
    Piece::J,
    Piece::L,
    Piece::O,
    Piece::S,
    Piece::T,
    Piece::Z,
];

fn piece_bit(piece: Piece) -> u8 {
    1 << ALL_PIECES
        .iter()
        .position(|candidate| *candidate == piece)
        .unwrap_or(0)
}

fn ordered_with_hold(
    context: &EvalContext,
    prerequisites: &[Prerequisite],
    saved: Piece,
) -> EvalResult {
    // Ignore repeats: within one bag a piece can only arrive once.
    let mut disclosed = Vec::with_capacity(BAG_SIZE);
    for piece in &context.queue_history {
        if !disclosed.contains(piece) {
            disclosed.push(*piece);
        }
    }
    let mut remaining: Vec<Piece> = ALL_PIECES
        .iter()
        .copied()
        .filter(|piece| !disclosed.contains(piece))
        .collect();
    let mut solvable = false;
    let mut unsolvable = false;
    for_each_permutation(&mut remaining, 0, &mut |completion| {
        let order: Vec<Piece> = disclosed.iter().chain(completion).copied().collect();
        if playable_with_hold(&order, prerequisites, saved) {
            solvable = true;
        } else {
            unsolvable = true;
        }
        !(solvable && unsolvable)
    });
    match (solvable, unsolvable) {
        (true, false) => EvalResult::Satisfied,
        (false, true) => EvalResult::Failed,
        _ => EvalResult::Pending,
    }
}

/// (hash of prefix and solves, disclosed bag) -> (completions solved, completions).
type SolvableCache = HashMap<(u64, Vec<Piece>), (usize, usize)>;

thread_local! {
    /// The overlay evaluates the same conditions every frame while the queue
    /// barely changes; a bag has at most 5040 completions per setup.
    static SOLVABLE_CACHE: RefCell<SolvableCache> = RefCell::new(HashMap::new());
}

fn solvable(
    context: &EvalContext,
    prefix: &[Piece],
    setup: &[Piece],
    solves: &[SolvableSolve],
) -> EvalResult {
    match solvable_counts(context, prefix, setup, solves) {
        (works, total) if works == total => EvalResult::Satisfied,
        (0, _) => EvalResult::Failed,
        _ => EvalResult::Pending,
    }
}

/// (completions some solve can play, completions) for the disclosed bag.
fn solvable_counts(
    context: &EvalContext,
    prefix: &[Piece],
    setup: &[Piece],
    solves: &[SolvableSolve],
) -> (usize, usize) {
    let mut disclosed = Vec::with_capacity(BAG_SIZE);
    for piece in &context.queue_history {
        if !disclosed.contains(piece) {
            disclosed.push(*piece);
        }
    }
    // From the case start when it is known and covers the setup; otherwise
    // the leftover case pieces, then the bag, with HOLD empty.
    let from_case_start = !setup.is_empty()
        && context.case_queue.len() + usize::from(context.case_hold.is_some())
            == setup.len() + prefix.len();
    let (start_hold, lead, base): (Option<Piece>, &[Piece], &[Piece]) = if from_case_start {
        (context.case_hold, &context.case_queue, setup)
    } else {
        (None, prefix, &[])
    };
    // Keyed by content, not address: rule sets are reloaded in tests.
    let identity = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        start_hold.hash(&mut hasher);
        lead.hash(&mut hasher);
        base.hash(&mut hasher);
        solves.hash(&mut hasher);
        hasher.finish()
    };
    let key = (identity, disclosed.clone());
    if let Some(counts) = SOLVABLE_CACHE.with(|cache| cache.borrow().get(&key).copied()) {
        return counts;
    }
    let mut remaining: Vec<Piece> = ALL_PIECES
        .iter()
        .copied()
        .filter(|piece| !disclosed.contains(piece))
        .collect();
    let mut works = 0;
    let mut total = 0;
    let mut queue = Vec::with_capacity(lead.len() + BAG_SIZE);
    // Without the case start, a solve whose table holds the setup pieces gets
    // them in front of the leftover case pieces.
    let mut setup_first = Vec::with_capacity(setup.len() + lead.len() + BAG_SIZE);
    for_each_permutation(&mut remaining, 0, &mut |completion| {
        queue.clear();
        queue.extend_from_slice(lead);
        queue.extend_from_slice(&disclosed);
        queue.extend_from_slice(completion);
        total += 1;
        let solved = solves.iter().any(|solve| {
            if !solve.setup_in_table {
                Replay::new(&queue, base, solve).playable(start_hold)
            } else if from_case_start {
                Replay::new(&queue, &[], solve).playable(start_hold)
            } else {
                setup_first.clear();
                setup_first.extend_from_slice(setup);
                setup_first.extend_from_slice(&queue);
                Replay::new(&setup_first, &[], solve).playable(None)
            }
        });
        if solved {
            works += 1;
        }
        true
    });
    SOLVABLE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 4096 {
            cache.clear();
        }
        cache.insert(key, (works, total));
    });
    (works, total)
}

/// Mirrors `playable` in tools/build_cases.py: place Current, move it to an
/// empty HOLD (locked until the next placement), or swap by placing the held
/// piece. `base` pieces (the setup's case pieces) may be placed in any order
/// but all before any solve piece; solve pieces follow the SRS table. Bits
/// `0..solve.pieces.len()` are solve pieces, the bits above them `base`.
struct Replay<'a> {
    queue: &'a [Piece],
    base: &'a [Piece],
    solve: &'a SolvableSolve,
    goal: u32,
    seen: std::collections::HashSet<(usize, Option<Piece>, u32, bool)>,
}

impl<'a> Replay<'a> {
    fn new(queue: &'a [Piece], base: &'a [Piece], solve: &'a SolvableSolve) -> Self {
        Self {
            queue,
            base,
            solve,
            goal: (1u32 << (solve.pieces.len() + base.len())) - 1,
            seen: std::collections::HashSet::new(),
        }
    }

    fn playable(&mut self, start_hold: Option<Piece>) -> bool {
        self.go(0, start_hold, 0, false)
    }

    /// Bits that may be placed next and the piece each stands for.
    fn allowed(&self, placed: u32) -> u32 {
        let solve_bits = self.solve.pieces.len();
        let base_mask = ((1u32 << self.base.len()) - 1) << solve_bits;
        if placed & base_mask != base_mask {
            !placed & base_mask
        } else {
            self.solve.allowed(placed & ((1u32 << solve_bits) - 1))
        }
    }

    fn piece(&self, bit: usize) -> Piece {
        let solve_bits = self.solve.pieces.len();
        if bit < solve_bits {
            self.solve.pieces[bit]
        } else {
            self.base[bit - solve_bits]
        }
    }

    fn place(&mut self, piece: Piece, index: usize, hold: Option<Piece>, placed: u32) -> bool {
        let allowed = self.allowed(placed);
        (0..self.solve.pieces.len() + self.base.len()).any(|bit| {
            allowed >> bit & 1 == 1
                && self.piece(bit) == piece
                && self.go(index + 1, hold, placed | 1 << bit, false)
        })
    }

    fn go(&mut self, index: usize, hold: Option<Piece>, placed: u32, locked: bool) -> bool {
        if placed == self.goal {
            return true;
        }
        let Some(&current) = self.queue.get(index) else {
            // The bag after the queue opens: its first piece can be swapped
            // for the held piece, which goes in last. A 7th PC places all
            // ten of its pieces ("hold S until everything is placed"); a 2nd
            // PC has a piece to spare, so this never decides one.
            return match hold {
                Some(held) if !locked && index == self.queue.len() => {
                    self.place(held, index, None, placed)
                }
                _ => false,
            };
        };
        if !self.seen.insert((index, hold, placed, locked)) {
            return false;
        }
        if self.place(current, index, hold, placed) {
            return true;
        }
        if locked {
            return false;
        }
        match hold {
            None => self.go(index + 1, Some(current), placed, true),
            Some(held) => self.place(held, index, Some(current), placed),
        }
    }
}

/// Calls `visit` with each ordering of `items[start..]`; stops when it
/// returns false.
fn for_each_permutation(
    items: &mut [Piece],
    start: usize,
    visit: &mut dyn FnMut(&[Piece]) -> bool,
) -> bool {
    if start + 1 >= items.len() {
        return visit(items);
    }
    for index in start..items.len() {
        items.swap(start, index);
        let keep_going = for_each_permutation(items, start + 1, visit);
        items.swap(start, index);
        if !keep_going {
            return false;
        }
    }
    true
}

/// Mirrors `play` in tools/ijlo_solves.py: place Current, move it to an
/// empty HOLD (then HOLD is locked until the next placement), or swap with
/// HOLD by placing the held piece.
fn playable_with_hold(order: &[Piece], prerequisites: &[Prerequisite], saved: Piece) -> bool {
    let goal = ALL_PIECES
        .iter()
        .filter(|piece| **piece != saved)
        .fold(0u8, |mask, piece| mask | piece_bit(*piece));
    let can_place = |piece: Piece, placed: u8| {
        piece != saved
            && placed & piece_bit(piece) == 0
            && prerequisites
                .iter()
                .filter(|rule| rule.then == piece)
                .all(|rule| placed & piece_bit(rule.first) != 0)
    };
    fn go(
        order: &[Piece],
        index: usize,
        hold: Option<Piece>,
        placed: u8,
        locked: bool,
        goal: u8,
        can_place: &dyn Fn(Piece, u8) -> bool,
    ) -> bool {
        if placed == goal {
            return true;
        }
        let Some(&current) = order.get(index) else {
            return false;
        };
        if can_place(current, placed)
            && go(
                order,
                index + 1,
                hold,
                placed | piece_bit(current),
                false,
                goal,
                can_place,
            )
        {
            return true;
        }
        if locked {
            return false;
        }
        match hold {
            None => go(
                order,
                index + 1,
                Some(current),
                placed,
                true,
                goal,
                can_place,
            ),
            Some(held) => {
                can_place(held, placed)
                    && go(
                        order,
                        index + 1,
                        Some(current),
                        placed | piece_bit(held),
                        false,
                        goal,
                        can_place,
                    )
            }
        }
    }
    go(order, 0, None, 0, false, goal, &can_place)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(queue_history: Vec<Piece>) -> EvalContext {
        EvalContext {
            queue_history,
            queue_closed: false,
            hold: Some(Piece::I),
            current: Piece::J,
            accessible: [Piece::I, Piece::J].into_iter().collect(),
            case_hold: None,
            case_queue: Vec::new(),
        }
    }

    #[test]
    fn seen_remains_pending_until_disclosed() {
        assert_eq!(
            Condition::Seen { piece: Piece::S }.evaluate(&context(vec![Piece::T])),
            EvalResult::Pending
        );
    }

    #[test]
    fn before_can_be_decided_as_pieces_arrive() {
        let condition = Condition::Before {
            first: Piece::S,
            second: Piece::J,
        };
        assert_eq!(condition.evaluate(&context(vec![])), EvalResult::Pending);
        assert_eq!(
            condition.evaluate(&context(vec![Piece::S])),
            EvalResult::Satisfied
        );
        assert_eq!(
            condition.evaluate(&context(vec![Piece::J, Piece::S])),
            EvalResult::Failed
        );
    }

    #[test]
    fn source_permutation_prefix_notation_is_structured() {
        let pattern = QueuePattern::PermutationPrefix {
            pieces: vec![Piece::T, Piece::J],
            then: Some(Piece::S),
        };
        assert_eq!(
            pattern.evaluate(&[Piece::J, Piece::T], false),
            EvalResult::Pending
        );
        assert_eq!(
            pattern.evaluate(&[Piece::J, Piece::T, Piece::S], false),
            EvalResult::Satisfied
        );
        assert_eq!(
            pattern.evaluate(&[Piece::T, Piece::J, Piece::Z], false),
            EvalResult::Failed
        );
    }

    fn save_o_left() -> Condition {
        let rule = |first, then| Prerequisite { first, then };
        Condition::OrderedWithHold {
            prerequisites: vec![
                rule(Piece::J, Piece::I),
                rule(Piece::T, Piece::Z),
                rule(Piece::T, Piece::L),
                rule(Piece::J, Piece::L),
            ],
            saved: Piece::O,
        }
    }

    fn bag(text: &str) -> Vec<Piece> {
        text.chars()
            .map(|code| match code {
                'I' => Piece::I,
                'J' => Piece::J,
                'L' => Piece::L,
                'O' => Piece::O,
                'S' => Piece::S,
                'T' => Piece::T,
                _ => Piece::Z,
            })
            .collect()
    }

    #[test]
    fn last_among_is_decided_once_the_rest_of_the_group_is_disclosed() {
        let fails_on_tj_s = Condition::LastAmong {
            piece: Piece::S,
            pieces: vec![Piece::T, Piece::J],
        };
        // S has not arrived after T and J, so within the bag it comes last.
        assert_eq!(
            fails_on_tj_s.evaluate(&context(bag("TJ"))),
            EvalResult::Satisfied
        );
        // S arrived while J is still to come: S cannot be last.
        assert_eq!(
            fails_on_tj_s.evaluate(&context(bag("TS"))),
            EvalResult::Failed
        );
        assert_eq!(
            fails_on_tj_s.evaluate(&context(bag("T"))),
            EvalResult::Pending
        );
    }

    #[test]
    fn ordered_with_hold_uses_hold_to_reorder_one_piece() {
        // I before J is fixed by holding I until J is placed.
        let mut ready = context(bag("IJTLSZO"));
        ready.queue_closed = true;
        assert_eq!(save_o_left().evaluate(&ready), EvalResult::Satisfied);
        // O arrives first and fills HOLD, so I before J cannot be fixed.
        let mut blocked = context(bag("OIJTLSZ"));
        blocked.queue_closed = true;
        assert_eq!(save_o_left().evaluate(&blocked), EvalResult::Failed);
    }

    #[test]
    fn ordered_with_hold_stays_pending_until_completions_agree() {
        assert_eq!(
            save_o_left().evaluate(&context(vec![])),
            EvalResult::Pending
        );
        // T and J first: every completion works.
        assert_eq!(
            save_o_left().evaluate(&context(bag("TJ"))),
            EvalResult::Satisfied
        );
        // O fills HOLD at once, then I and L both precede J and T.
        assert_eq!(
            save_o_left().evaluate(&context(bag("OIL"))),
            EvalResult::Failed
        );
    }

    #[test]
    fn mirror_is_an_involution_for_nested_conditions() {
        let condition = Condition::All {
            conditions: vec![
                Condition::Before {
                    first: Piece::S,
                    second: Piece::J,
                },
                Condition::HoldIs { piece: Piece::L },
            ],
        };
        assert_eq!(condition.mirrored().mirrored(), condition);
    }
}
