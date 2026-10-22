//! Replays TETR.IO `.ttrm` / `.ttr` event streams on the solo engine and
//! checks the result against the replay's serialized end snapshot.
//!
//! This mirrors `reconstruct_stream` in czsmall_nnue's
//! `trainer/ttrm_exact_replay.py`, minus the multiplayer garbage exchange.
//! IGE events are skipped; rows that the Python reconstruction tanked can be
//! fed back through [`StreamRun::garbage`] so a multiplayer stream still
//! replays end to end.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::engine::{
    BoardCell, Engine, EngineBoard, EngineError, EngineSetup, GameOptions, HEIGHT, Handling, Key,
    PieceStream, Spin, WIDTH,
};
use crate::model::Piece;

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("replay JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("replay has no event stream")]
    NoStreams,
    #[error("stream has no {0} event")]
    MissingEvent(&'static str),
    #[error("full event lacks a game snapshot")]
    NoSnapshot,
    #[error("replay has no deterministic seed")]
    NoSeed,
    #[error("unknown board cell {0:?}")]
    UnknownCell(String),
    #[error("piece seed mismatch: snapshot {snapshot:?}, generated {generated:?}")]
    SeedMismatch {
        snapshot: Vec<Piece>,
        generated: Vec<Piece>,
    },
    #[error("input subframe moved backwards at frame {frame}")]
    Backwards { frame: i64 },
    #[error("event time at frame {frame} is outside the {MAX_FRAME}-frame limit")]
    FrameOutOfRange { frame: i64 },
    #[error("unsupported game option: {0}")]
    Unsupported(String),
    #[error("engine: {0}")]
    Engine(#[from] EngineError),
}

/// One player's event stream inside a replay document.
#[derive(Debug, Clone)]
pub struct StreamRef<'a> {
    /// Round index (0 for a solo `.ttr`).
    pub round: usize,
    /// Player index within the round.
    pub player: usize,
    pub events: &'a [Value],
    /// The stream's `replay` object. A solo `.ttr` has no `full` snapshot
    /// and keeps its options and final stats here (`options`, `results`).
    pub replay: Option<&'a Value>,
}

impl<'a> StreamRef<'a> {
    /// A bare event stream with a `full` snapshot (no surrounding document).
    pub fn from_events(events: &'a [Value]) -> Self {
        Self {
            round: 0,
            player: 0,
            events,
            replay: None,
        }
    }
}

/// Lists every player stream. TETRA LEAGUE `.ttrm` files keep them under
/// `replay.rounds[][]`; a solo `.ttr` keeps a single `replay.events`.
pub fn streams(document: &Value) -> Result<Vec<StreamRef<'_>>, ReplayError> {
    let replay = document.get("replay").unwrap_or(document);
    let rounds = replay
        .get("rounds")
        .or_else(|| document.get("rounds"))
        .and_then(Value::as_array);
    let mut out = Vec::new();
    if let Some(rounds) = rounds {
        for (round, round_data) in rounds.iter().enumerate() {
            let players = round_data
                .as_array()
                .or_else(|| round_data.get("replays").and_then(Value::as_array));
            for (player, stream) in players.into_iter().flatten().enumerate() {
                if let Some(events) = stream_events(stream) {
                    out.push(StreamRef {
                        round,
                        player,
                        events,
                        replay: stream.get("replay"),
                    });
                }
            }
        }
    } else if let Some(events) = stream_events(document) {
        out.push(StreamRef {
            round: 0,
            player: 0,
            events,
            replay: document.get("replay"),
        });
    }
    if out.is_empty() {
        return Err(ReplayError::NoStreams);
    }
    Ok(out)
}

fn stream_events(stream: &Value) -> Option<&[Value]> {
    stream
        .get("replay")
        .and_then(|replay| replay.get("events"))
        .or_else(|| stream.get("events"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
}

fn kind(event: &Value) -> String {
    event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn first_event<'a>(events: &'a [Value], name: &'static str) -> Result<&'a Value, ReplayError> {
    events
        .iter()
        .find(|event| kind(event) == name)
        .ok_or(ReplayError::MissingEvent(name))
}

fn last_event<'a>(events: &'a [Value], name: &'static str) -> Result<&'a Value, ReplayError> {
    events
        .iter()
        .rev()
        .find(|event| kind(event) == name)
        .ok_or(ReplayError::MissingEvent(name))
}

fn number(value: Option<&Value>, default: f64) -> f64 {
    match value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        _ => default,
    }
}

fn integer(value: Option<&Value>, default: i64) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(default),
        Some(Value::Bool(b)) => i64::from(*b),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        _ => default,
    }
}

fn boolean(value: Option<&Value>, default: bool) -> bool {
    match value {
        None => default,
        Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

pub fn piece_from_letter(text: &str) -> Option<Piece> {
    Some(match text.to_ascii_uppercase().as_str() {
        "I" => Piece::I,
        "J" => Piece::J,
        "L" => Piece::L,
        "O" => Piece::O,
        "S" => Piece::S,
        "T" => Piece::T,
        "Z" => Piece::Z,
        _ => return None,
    })
}

/// Parses a serialized top-down board; the last 40 rows are kept.
pub fn parse_board(rows: &[Value]) -> Result<EngineBoard, ReplayError> {
    let start = rows.len().saturating_sub(HEIGHT);
    let mut bottom_up = Vec::with_capacity(HEIGHT);
    for row in rows[start..].iter().rev() {
        let mut cells = [BoardCell::Empty; WIDTH];
        for (x, cell) in row.as_array().into_iter().flatten().take(WIDTH).enumerate() {
            cells[x] = match cell {
                Value::Null | Value::Bool(false) => BoardCell::Empty,
                Value::Number(n) if n.as_f64() == Some(0.0) => BoardCell::Empty,
                Value::String(s) if s.is_empty() => BoardCell::Empty,
                Value::String(s) => match s.to_ascii_uppercase().as_str() {
                    "GB" | "G" | "GARBAGE" => BoardCell::Garbage,
                    // `get` rather than `[..1]`: a non-ASCII first letter
                    // is an unknown cell, not a panic.
                    other => BoardCell::Piece(
                        other
                            .get(..1)
                            .and_then(piece_from_letter)
                            .ok_or_else(|| ReplayError::UnknownCell(s.clone()))?,
                    ),
                },
                other => return Err(ReplayError::UnknownCell(other.to_string())),
            };
        }
        bottom_up.push(cells);
    }
    Ok(EngineBoard::from_rows(&bottom_up))
}

/// Replay options: `full.data.options`, then `end.data.options`, then the
/// stream's `replay.options` (the only copy in a solo `.ttr`).
fn options<'a>(
    stream: &StreamRef<'a>,
    full: Option<&'a Value>,
    end: &'a Value,
) -> Option<&'a Value> {
    let non_empty =
        |v: Option<&'a Value>| v.filter(|v| v.as_object().is_some_and(|o| !o.is_empty()));
    non_empty(full.and_then(|f| f.pointer("/data/options")))
        .or_else(|| non_empty(end.pointer("/data/options")))
        .or_else(|| non_empty(stream.replay.and_then(|r| r.get("options"))))
}

/// The seed of a stream's piece RNG, if the replay records one.
pub fn stream_seed(stream: &StreamRef<'_>) -> Option<i64> {
    let full = first_event(stream.events, "full").ok();
    let end = last_event(stream.events, "end").ok()?;
    let seed = integer(options(stream, full, end)?.get("seed"), 0);
    (seed != 0).then_some(seed)
}

/// Rejects options whose mechanics the engine does not implement, instead of
/// replaying them approximately.
fn check_supported(opts: &Value) -> Result<(), ReplayError> {
    for (key, expected) in [
        ("kickset", "SRS+"),
        ("bagtype", "7-bag"),
        ("spinbonuses", "all-mini+"),
    ] {
        if let Some(value) = opts.get(key).and_then(Value::as_str)
            && !value.eq_ignore_ascii_case(expected)
        {
            return Err(ReplayError::Unsupported(format!("{key} = {value}")));
        }
    }
    for (key, expected) in [("boardwidth", 10), ("boardheight", 20)] {
        let value = integer(opts.get(key), expected);
        if value != expected {
            return Err(ReplayError::Unsupported(format!("{key} = {value}")));
        }
    }
    if !boolean(opts.get("allow180"), true) {
        return Err(ReplayError::Unsupported("allow180 = false".to_owned()));
    }
    Ok(())
}

/// Engine setup from a stream's `full` snapshot, or for a solo `.ttr`
/// (which has none) from `replay.options` with an empty board.
pub fn setup_for_stream(stream: &StreamRef<'_>) -> Result<EngineSetup, ReplayError> {
    let full = first_event(stream.events, "full").ok();
    let end = last_event(stream.events, "end")?;
    let empty = Value::Object(Default::default());
    let opts = options(stream, full, end).unwrap_or(&empty);
    let game = match full {
        Some(full) => full
            .pointer("/data/game")
            .filter(|g| g.as_object().is_some_and(|o| !o.is_empty()))
            .ok_or(ReplayError::NoSnapshot)?,
        None if stream.replay.and_then(|r| r.get("options")).is_some() => &empty,
        None => return Err(ReplayError::MissingEvent("full")),
    };
    check_supported(opts)?;

    let seed = integer(opts.get("seed"), 0);
    if seed == 0 {
        return Err(ReplayError::NoSeed);
    }
    let snapshot_bag: Vec<Piece> = game
        .get("bag")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().and_then(piece_from_letter))
        .collect();
    if !snapshot_bag.is_empty() {
        let mut stream = PieceStream::new(seed);
        let generated: Vec<Piece> = (0..snapshot_bag.len()).map(|_| stream.pop()).collect();
        if generated != snapshot_bag {
            return Err(ReplayError::SeedMismatch {
                snapshot: snapshot_bag,
                generated,
            });
        }
    }
    // `no_szo` changes the first bag when it would start with S, Z or O. How
    // the client rearranges it is not confirmed, so such games are rejected.
    if boolean(opts.get("no_szo"), false)
        && matches!(PieceStream::new(seed).pop(), Piece::S | Piece::Z | Piece::O)
    {
        return Err(ReplayError::Unsupported(
            "no_szo with an S, Z or O first piece".to_owned(),
        ));
    }

    let handling_value = game
        .get("handling")
        .filter(|h| h.as_object().is_some_and(|o| !o.is_empty()))
        .or_else(|| opts.get("handling"))
        .unwrap_or(&empty);
    let handling = Handling {
        arr: number(handling_value.get("arr"), 0.0).max(0.0),
        das: number(handling_value.get("das"), 0.0).max(0.0),
        dcd: number(handling_value.get("dcd"), 0.0).max(0.0),
        sdf: number(handling_value.get("sdf"), 41.0).max(0.0),
        cancel: boolean(handling_value.get("cancel"), false),
        safelock: boolean(handling_value.get("safelock"), false),
        may20g: boolean(handling_value.get("may20g"), true),
    };
    let options = GameOptions {
        gravity: number(opts.get("g"), number(game.get("g"), 0.02)).max(0.0),
        gravity_increase: number(opts.get("gincrease"), 0.0).max(0.0),
        gravity_margin: number(opts.get("gmargin"), 0.0).max(0.0),
        gravity_may20g: boolean(opts.get("gravitymay20g"), true),
        lock_delay: integer(opts.get("locktime"), 30).max(0) as f64,
        lock_reset_limit: integer(opts.get("lockresets"), 15).max(0) as u32,
        clutch: boolean(opts.get("clutch"), true),
    };
    let board = parse_board(
        game.get("board")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice),
    )?;
    let hold = game
        .pointer("/hold/piece")
        .and_then(Value::as_str)
        .and_then(piece_from_letter);
    let hold_locked = boolean(game.pointer("/hold/locked"), false);
    Ok(EngineSetup {
        seed,
        handling,
        options,
        board,
        hold,
        hold_locked,
    })
}

/// One field of the end snapshot that the engine did not reproduce.
#[derive(Debug, Clone, PartialEq)]
pub struct EndMismatch {
    pub field: &'static str,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Clone)]
pub struct StreamResult {
    pub engine: Engine,
    pub mismatches: Vec<EndMismatch>,
    pub warnings: Vec<String>,
}

impl StreamResult {
    pub fn exact(&self) -> bool {
        self.mismatches.is_empty() && self.warnings.is_empty()
    }
}

/// Options for [`run_stream`].
#[derive(Debug, Clone, Default)]
pub struct StreamRun {
    /// Garbage holes to tank after each placement index.
    pub garbage: BTreeMap<usize, Vec<u8>>,
}

/// The latest frame a replay may reach: 24 hours at 60 frames a second.
/// The engine steps frame by frame, so an unbounded frame number from the
/// file would never finish. Real games are minutes long.
pub const MAX_FRAME: i64 = 24 * 60 * 60 * 60;

/// An event's time in subframe ticks (10 per frame), within [`MAX_FRAME`].
fn event_time(frame: i64, subframe: Option<&Value>) -> Result<i64, ReplayError> {
    let ticks = subframe.map_or(0, |sub| {
        (number(Some(sub), 0.0) * 10.0).round_ties_even() as i64
    });
    (0..=MAX_FRAME)
        .contains(&frame)
        .then(|| (frame * 10).checked_add(ticks))
        .flatten()
        .filter(|at| (0..=(MAX_FRAME + 1) * 10).contains(at))
        .ok_or(ReplayError::FrameOutOfRange { frame })
}

const SKIPPED: [&str; 7] = [
    "start",
    "full",
    "end",
    "target",
    "targets",
    "allow_targeting",
    "sizzle",
];

/// Plays a stream's inputs and compares the final state with its `end` event.
pub fn run_stream(stream: &StreamRef<'_>, run: &StreamRun) -> Result<StreamResult, ReplayError> {
    let setup = setup_for_stream(stream)?;
    let end = last_event(stream.events, "end")?;
    let mut engine = Engine::new(setup);
    for (&index, holes) in &run.garbage {
        engine.schedule_garbage(index, holes.clone());
    }

    let mut warnings = Vec::new();
    let mut last_gameplay: Option<&Value> = None;
    for event in stream.events {
        let name = kind(event);
        if SKIPPED.contains(&name.as_str()) {
            continue;
        }
        let frame = integer(event.get("frame"), 0);
        let data = event.get("data");
        // Inputs carry their subframe; an IGE runs at whatever subframe the
        // frame has already reached, so it never moves time backwards.
        let at = match data.and_then(|d| d.get("subframe")) {
            Some(sub) => {
                let at = event_time(frame, Some(sub))?;
                if at < engine.time() {
                    return Err(ReplayError::Backwards { frame });
                }
                at
            }
            None => event_time(frame, None)?.max(engine.time()),
        };
        engine.advance_to(at)?;
        last_gameplay = Some(event);
        let key = data
            .and_then(|d| d.get("key"))
            .and_then(Value::as_str)
            .and_then(Key::from_replay_name);
        match name.as_str() {
            "keydown" => {
                if let Some(key) = key {
                    let hoisted = boolean(data.and_then(|d| d.get("hoisted")), false);
                    engine.key_down(key, at, hoisted)?;
                }
            }
            "keyup" => {
                if let Some(key) = key {
                    engine.key_up(key);
                }
            }
            // Garbage exchange is not simulated; see `StreamRun::garbage`.
            "ige" => {}
            other => warnings.push(format!("unhandled event type {other} at frame {frame}")),
        }
    }

    // The end frame's normal frame-tail physics has completed in the snapshot.
    let end_frame = integer(end.get("frame"), 0);
    let end_tail = event_time(end_frame, None)? + 10;
    if end_tail >= engine.time() {
        engine.advance_to(end_tail)?;
    }

    // If Hold itself tops out, v19 serializes the failed held piece as both
    // the falling piece and HOLD.
    let end_hold = end
        .pointer("/data/game/hold/piece")
        .and_then(Value::as_str)
        .and_then(piece_from_letter);
    if end.pointer("/data/gameoverreason").and_then(Value::as_str) == Some("topout")
        && let Some(last) = last_gameplay
    {
        let falling = end
            .pointer("/data/game/falling/type")
            .and_then(Value::as_str)
            .and_then(piece_from_letter);
        let active = engine.active().kind;
        if kind(last) == "keydown"
            && last.pointer("/data/key").and_then(Value::as_str) == Some("hold")
            && integer(last.get("frame"), -1) == end_frame
            && end_hold == Some(active)
            && falling == Some(active)
        {
            engine.set_hold(Some(active));
        }
    }

    let mismatches = match stream.replay.and_then(|r| r.pointer("/results/stats")) {
        Some(stats) if end.pointer("/data/game").is_none() => verify_results(&engine, stats),
        _ => verify_end(&engine, end, end_hold)?,
    };
    Ok(StreamResult {
        engine,
        mismatches,
        warnings,
    })
}

/// One placement as dumped from the Python reconstruction by
/// `tools/ttrm_python_dump.py`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ReferencePlacement {
    pub subframe: i64,
    pub piece: String,
    pub rotation: u8,
    pub x: i32,
    pub y: f64,
    pub hold: Option<String>,
    pub used_hold: bool,
    pub lines: u32,
    /// Bottom-up occupancy masks after the lock and any tanked garbage.
    pub rows: Vec<u16>,
    /// Garbage holes tanked right after this placement.
    pub holes: Vec<u8>,
}

/// Garbage schedule implied by a reference run.
pub fn garbage_from_reference(placements: &[ReferencePlacement]) -> StreamRun {
    StreamRun {
        garbage: placements
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.holes.is_empty())
            .map(|(i, p)| (i, p.holes.clone()))
            .collect(),
    }
}

/// Describes the first placement where the engine and the reference differ,
/// or a length difference; `None` when they agree exactly (Y bit for bit).
pub fn first_reference_difference(
    engine: &Engine,
    reference: &[ReferencePlacement],
) -> Option<String> {
    let actual = engine.placements();
    for (index, (a, r)) in actual.iter().zip(reference).enumerate() {
        let letter = |p: Option<Piece>| p.map(|p| format!("{p:?}"));
        let same = a.subframe == r.subframe
            && format!("{:?}", a.piece) == r.piece.to_ascii_uppercase()
            && a.rotation == r.rotation
            && a.x == r.x
            && a.y.to_bits() == r.y.to_bits()
            && letter(a.hold) == r.hold.as_ref().map(|h| h.to_ascii_uppercase())
            && a.used_hold == r.used_hold
            && a.lines_cleared == r.lines
            && a.rows == r.rows;
        if !same {
            return Some(format!(
                "placement {index}: rust=({:?} r{} x{} y{} t{} hold {:?} lines {}) \
                 python=({} r{} x{} y{} t{} hold {:?} lines {}) rows_equal={}",
                a.piece,
                a.rotation,
                a.x,
                a.y,
                a.subframe,
                a.hold,
                a.lines_cleared,
                r.piece,
                r.rotation,
                r.x,
                r.y,
                r.subframe,
                r.hold,
                r.lines,
                a.rows == r.rows
            ));
        }
    }
    (actual.len() != reference.len()).then(|| {
        format!(
            "placement count: rust={} python={}",
            actual.len(),
            reference.len()
        )
    })
}

/// Checks a solo `.ttr`, which records final statistics instead of an end
/// snapshot: pieces placed, lines, holds, and the line clears by kind.
fn verify_results(engine: &Engine, stats: &Value) -> Vec<EndMismatch> {
    let mut out = Vec::new();
    let mut check = |field: &'static str, expected: Option<&Value>, actual: i64| {
        if let Some(expected) = expected {
            let expected = integer(Some(expected), -1);
            if expected != actual {
                out.push(EndMismatch {
                    field,
                    expected: expected.to_string(),
                    actual: actual.to_string(),
                });
            }
        }
    };
    let placements = engine.placements();
    check(
        "stats.piecesplaced",
        stats.get("piecesplaced"),
        placements.len() as i64,
    );
    check(
        "stats.lines",
        stats.get("lines"),
        i64::from(engine.lines_cleared()),
    );
    check("stats.holds", stats.get("holds"), i64::from(engine.holds()));

    // A clear is filed as a T-spin (full or mini) or as a plain clear by its
    // line count; non-T `all-mini+` spins are assumed to be plain clears.
    let count = |lines: u32, t_spin: Spin| {
        placements
            .iter()
            .filter(|p| p.lines_cleared == lines)
            .filter(|p| {
                let spin = if p.piece == Piece::T {
                    p.spin
                } else {
                    Spin::None
                };
                spin == t_spin
            })
            .count() as i64
    };
    let clears = stats.get("clears");
    let kinds = [
        ("stats.clears.singles", "singles", 1, Spin::None),
        ("stats.clears.doubles", "doubles", 2, Spin::None),
        ("stats.clears.triples", "triples", 3, Spin::None),
        ("stats.clears.quads", "quads", 4, Spin::None),
        ("stats.clears.tspinsingles", "tspinsingles", 1, Spin::Full),
        ("stats.clears.tspindoubles", "tspindoubles", 2, Spin::Full),
        ("stats.clears.tspintriples", "tspintriples", 3, Spin::Full),
        (
            "stats.clears.minitspinsingles",
            "minitspinsingles",
            1,
            Spin::Mini,
        ),
        (
            "stats.clears.minitspindoubles",
            "minitspindoubles",
            2,
            Spin::Mini,
        ),
    ];
    for (label, name, lines, t_spin) in kinds {
        check(
            label,
            clears.and_then(|c| c.get(name)),
            count(lines, t_spin),
        );
    }
    let all_clears = placements
        .iter()
        .filter(|p| p.lines_cleared > 0 && p.rows.iter().all(|row| *row == 0))
        .count() as i64;
    check(
        "stats.clears.allclear",
        clears.and_then(|c| c.get("allclear")),
        all_clears,
    );
    out
}

fn verify_end(
    engine: &Engine,
    end: &Value,
    end_hold: Option<Piece>,
) -> Result<Vec<EndMismatch>, ReplayError> {
    let mut out = Vec::new();
    let mut check = |field: &'static str, expected: String, actual: String| {
        if expected != actual {
            out.push(EndMismatch {
                field,
                expected,
                actual,
            });
        }
    };
    let expected_board = parse_board(
        end.pointer("/data/game/board")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice),
    )?;
    check(
        "board.occupancy",
        format!("{:?}", expected_board.occupancy_masks()),
        format!("{:?}", engine.board().occupancy_masks()),
    );
    check(
        "board.garbage",
        format!("{:?}", expected_board.garbage_masks()),
        format!("{:?}", engine.board().garbage_masks()),
    );
    check(
        "hold",
        format!("{end_hold:?}"),
        format!("{:?}", engine.hold()),
    );
    let stats = end.pointer("/data/stats");
    let placed = engine.placements().len() as i64;
    check(
        "stats.piecesplaced",
        integer(stats.and_then(|s| s.get("piecesplaced")), placed).to_string(),
        placed.to_string(),
    );
    let lines = i64::from(engine.lines_cleared());
    check(
        "stats.lines",
        integer(stats.and_then(|s| s.get("lines")), lines).to_string(),
        lines.to_string(),
    );
    Ok(out)
}
