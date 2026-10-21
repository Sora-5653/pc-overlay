//! Finds TETR.IO piece seeds.
//!
//! `recover-seed <pieces> [--limit N] [--next N]` lists the seeds whose spawn
//! order starts with `<pieces>` (letters IJLOSTZ, e.g. `TSZOJLI IOZ...`;
//! spaces are ignored) and previews the following pieces of each.
//! `recover-seed --replay <file>` prints the seed every stream records.

use std::{env, fs, process::ExitCode, time::Instant};

use pc_overlay_core::{
    engine::piece_sequence,
    model::Piece,
    seed::recover_seeds,
    ttrm::{piece_from_letter, stream_seed, streams},
};
use serde_json::Value;

fn letters(pieces: &[Piece]) -> String {
    pieces.iter().map(|p| format!("{p:?}")).collect()
}

fn replay_seeds(path: &str) -> ExitCode {
    let document: Value = match fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(document) => document,
        Err(error) => {
            eprintln!("{path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    match streams(&document) {
        Ok(list) => {
            for stream in list {
                match stream_seed(&stream) {
                    Some(seed) => println!(
                        "round {} player {}: seed {seed} first bags {}",
                        stream.round,
                        stream.player,
                        letters(&piece_sequence(seed, 21))
                    ),
                    None => println!("round {} player {}: no seed", stream.round, stream.player),
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{path}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let mut pieces = String::new();
    let mut limit = 20usize;
    let mut next = 14usize;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--replay" => {
                let Some(path) = args.next() else {
                    eprintln!("--replay needs a file");
                    return ExitCode::FAILURE;
                };
                return replay_seeds(&path);
            }
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(limit),
            "--next" => next = args.next().and_then(|v| v.parse().ok()).unwrap_or(next),
            other => pieces.push_str(other),
        }
    }
    let mut observed = Vec::new();
    for c in pieces.chars().filter(|c| !c.is_whitespace()) {
        match piece_from_letter(&c.to_string()) {
            Some(piece) => observed.push(piece),
            None => {
                eprintln!("not a piece letter: {c}");
                return ExitCode::FAILURE;
            }
        }
    }
    if observed.is_empty() {
        eprintln!("usage: recover-seed <pieces> [--limit N] [--next N] | --replay <file>");
        return ExitCode::FAILURE;
    }

    let started = Instant::now();
    let search = recover_seeds(&observed, limit);
    println!(
        "{} piece(s) observed: {} seed(s){} in {:.1}s",
        observed.len(),
        search.seeds.len(),
        if search.truncated { "+" } else { "" },
        started.elapsed().as_secs_f64()
    );
    for seed in &search.seeds {
        let sequence = piece_sequence(i64::from(*seed), observed.len() + next);
        println!("seed {seed}: next {}", letters(&sequence[observed.len()..]));
    }
    if search.seeds.is_empty() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
