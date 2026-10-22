//! Replays `.ttrm` / `.ttr` files on the Rust engine.
//!
//! `verify-ttrm <file-or-dir>...` checks each stream's end snapshot. Streams
//! that received garbage cannot match without the multiplayer exchange, so
//! they are counted separately. With `--reference <dump.json>` (written by
//! `tools/ttrm_python_dump.py`), the Python reconstruction's tanked garbage
//! is replayed too, and every placement is compared with Python bit for bit.

use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use pc_overlay_core::ttrm::{
    ReferencePlacement, StreamRun, first_reference_difference, garbage_from_reference, run_stream,
    streams,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct ReferenceStream {
    file: String,
    round: usize,
    player: usize,
    placements: Vec<ReferencePlacement>,
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for entry in entries {
            collect(&entry, out);
        }
    } else if matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ttrm" | "ttr")
    ) {
        out.push(path.to_path_buf());
    }
}

fn canonical(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_lowercase()
}

fn main() -> ExitCode {
    let mut inputs = Vec::new();
    let mut reference_path = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--reference" {
            reference_path = args.next();
        } else {
            inputs.push(PathBuf::from(arg));
        }
    }
    if inputs.is_empty() {
        eprintln!("usage: verify-ttrm [--reference dump.json] <file-or-dir>...");
        return ExitCode::FAILURE;
    }

    let mut reference: HashMap<(String, usize, usize), Vec<ReferencePlacement>> = HashMap::new();
    if let Some(path) = &reference_path {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("{path}: {error}");
                return ExitCode::FAILURE;
            }
        };
        let entries: Vec<ReferenceStream> = match serde_json::from_str(&text) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!("{path}: {error}");
                return ExitCode::FAILURE;
            }
        };
        for entry in entries {
            reference.insert(
                (canonical(Path::new(&entry.file)), entry.round, entry.player),
                entry.placements,
            );
        }
    }

    let mut files = Vec::new();
    for input in &inputs {
        collect(input, &mut files);
    }

    let (mut total, mut exact, mut garbage_only, mut failed) = (0, 0, 0, 0);
    let (mut compared, mut placements) = (0, 0usize);
    for file in &files {
        let document: Value = match fs::read_to_string(file)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        {
            Ok(document) => document,
            Err(error) => {
                println!("{}: {error}", file.display());
                failed += 1;
                continue;
            }
        };
        let list = match streams(&document) {
            Ok(list) => list,
            Err(error) => {
                println!("{}: {error}", file.display());
                failed += 1;
                continue;
            }
        };
        for stream in list {
            total += 1;
            let key = (canonical(file), stream.round, stream.player);
            let expected = reference.get(&key);
            let run = expected.map_or_else(StreamRun::default, |r| garbage_from_reference(r));
            let label = format!(
                "{} round {} player {}",
                file.display(),
                stream.round,
                stream.player
            );
            let result = match run_stream(&stream, &run) {
                Ok(result) => result,
                Err(error) => {
                    println!("{label}: error {error}");
                    failed += 1;
                    continue;
                }
            };
            if let Some(expected) = expected {
                compared += 1;
                placements += expected.len();
                if let Some(diff) = first_reference_difference(&result.engine, expected) {
                    println!("{label}: differs from Python: {diff}");
                    failed += 1;
                    continue;
                }
            }
            if result.exact() {
                exact += 1;
            } else if expected.is_none()
                && result
                    .mismatches
                    .iter()
                    .all(|m| m.field.starts_with("board"))
                && document_has_garbage(stream.events)
            {
                garbage_only += 1;
            } else {
                let fields: Vec<_> = result.mismatches.iter().map(|m| m.field).collect();
                println!(
                    "{label}: end mismatch {fields:?} warnings {:?}",
                    result.warnings
                );
                failed += 1;
            }
        }
    }
    println!(
        "streams {total}: end-exact {exact}, garbage-dependent {garbage_only}, failed {failed}"
    );
    if reference_path.is_some() {
        println!("compared with Python: {compared} streams, {placements} placements");
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Whether the stream received any garbage interaction.
fn document_has_garbage(events: &[Value]) -> bool {
    events.iter().any(|event| {
        event.pointer("/data/type").and_then(Value::as_str) == Some("interaction_confirm")
    })
}
