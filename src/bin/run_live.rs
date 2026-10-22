use std::{
    collections::BTreeMap, error::Error, fs, io::Write, path::PathBuf, thread, time::Duration,
};

use pc_overlay_core::{
    BoardCalibration, CommitOutcome, DiagramSet, RecognizerConfig, RuleSet, SessionTracker,
    StabilityConfig, StabilityFilter, TetrioRecognizer, TrackerConfig, WindowLocator, piece_code,
};

#[derive(Debug)]
struct Args {
    once: bool,
    interval_ms: u64,
    verbose: bool,
    reviewed_only: bool,
    rules: PathBuf,
    diagrams: PathBuf,
    calibration: PathBuf,
    record_dir: Option<PathBuf>,
    record_max: usize,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            once: false,
            interval_ms: 100,
            verbose: false,
            reviewed_only: false,
            rules: PathBuf::from("data/ijlo.json"),
            diagrams: PathBuf::from("data/ijlo/diagram_cells.json"),
            calibration: PathBuf::from("data/overlay-calibration.json"),
            record_dir: None,
            record_max: 40,
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    let mut rules = RuleSet::load(&args.rules)?;
    let diagrams = DiagramSet::load(&args.diagrams)?;
    if let Err(issues) = rules.attach_setup_diagrams(&diagrams) {
        for issue in issues {
            eprintln!("warning: {issue}");
        }
    }

    let calibration = load_calibration(&args.calibration)?;
    let recognizer_config = RecognizerConfig {
        board_calibration: calibration,
        ..Default::default()
    };
    let recognizer = TetrioRecognizer::new(recognizer_config)?;
    let locator = WindowLocator::default();
    let mut stability = StabilityFilter::new(StabilityConfig::default());
    let mut tracker = SessionTracker::new(TrackerConfig::default());
    let mut last_line = String::new();
    // Recording is a calibration aid: every frame's summary goes to log.txt,
    // and frames that did not yield a confident observation are saved as BMP
    // (rate-limited and capped, since each full frame is several megabytes).
    let mut record_log = match &args.record_dir {
        Some(dir) => {
            fs::create_dir_all(dir)?;
            Some(fs::File::create(dir.join("log.txt"))?)
        }
        None => None,
    };
    let mut recorded = 0usize;
    let mut last_recorded_ms = 0u64;
    let mut frame_index = 0u64;

    loop {
        let line = match locator.locate() {
            Ok(Some(window)) => match pc_overlay_core::capture::capture_client(&window) {
                Ok(frame) => {
                    let recognition = recognizer.recognize(&frame)?;
                    frame_index += 1;
                    let mut saved = None;
                    if let Some(dir) = &args.record_dir {
                        let failed =
                            recognition.observation.is_none() || recognition.confidence < 0.70;
                        if failed
                            && recorded < args.record_max
                            && frame.captured_at_ms.saturating_sub(last_recorded_ms) >= 700
                        {
                            let name = format!("frame_{frame_index:06}.bmp");
                            frame.write_bmp(dir.join(&name))?;
                            recorded += 1;
                            last_recorded_ms = frame.captured_at_ms;
                            saved = Some(name);
                        }
                    }
                    let stable = if let Some(observation) = recognition.observation.clone() {
                        Some(stability.ingest(observation, recognition.confidence))
                    } else {
                        stability.clear_pending();
                        None
                    };
                    let commit = match stable.as_ref() {
                        Some(pc_overlay_core::StabilityOutcome::Stable {
                            observation,
                            confidence,
                        }) => Some(tracker.commit(
                            (**observation).clone(),
                            *confidence,
                            frame.captured_at_ms,
                        )),
                        _ => None,
                    };
                    let line = format_line(
                        &window.title,
                        &recognition,
                        &stable,
                        commit.as_ref(),
                        &tracker,
                        &rules,
                        args.reviewed_only,
                        args.verbose,
                    );
                    if let Some(log) = record_log.as_mut() {
                        let detail = format_line(
                            &window.title,
                            &recognition,
                            &stable,
                            commit.as_ref(),
                            &tracker,
                            &rules,
                            args.reviewed_only,
                            true,
                        );
                        let saved = saved
                            .map(|name| format!(" saved={name}"))
                            .unwrap_or_default();
                        writeln!(
                            log,
                            "#{frame_index} t={} {detail}{saved}",
                            frame.captured_at_ms
                        )?;
                    }
                    line
                }
                Err(error) => format!("capture_error={error}"),
            },
            Ok(None) => "window=not_found".to_string(),
            Err(error) => format!("window_error={error}"),
        };

        if args.once || args.verbose || line != last_line {
            println!("{line}");
            last_line = line;
        }
        if args.once {
            break;
        }
        thread::sleep(Duration::from_millis(args.interval_ms.max(20)));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn format_line(
    title: &str,
    recognition: &pc_overlay_core::RecognitionResult,
    stable: &Option<pc_overlay_core::StabilityOutcome>,
    commit: Option<&CommitOutcome>,
    tracker: &SessionTracker,
    rules: &RuleSet,
    reviewed_only: bool,
    verbose: bool,
) -> String {
    let hold = recognition.hold.map(piece_code).unwrap_or('-');
    let current = recognition.current.map(piece_code).unwrap_or('-');
    let next = recognition
        .next5
        .iter()
        .map(|piece| piece.map(piece_code).unwrap_or('-'))
        .collect::<String>();
    let stable_label = match stable {
        Some(pc_overlay_core::StabilityOutcome::Stable { .. }) => "stable",
        Some(pc_overlay_core::StabilityOutcome::Pending { consecutive_frames }) => {
            return format!(
                "window={title:?} recognition=current:{current} hold:{hold} next:{next} board_cells={} confidence={:.3} stable=pending({consecutive_frames})",
                recognition.board_cells, recognition.confidence
            );
        }
        Some(pc_overlay_core::StabilityOutcome::IgnoredLowConfidence { .. }) => "low_confidence",
        None => "unknown",
    };

    let (selection, statuses) = if let (Some(snapshot), Some(context)) =
        (tracker.snapshots().last(), tracker.following_bag_context())
    {
        let report = if reviewed_only {
            rules.select_for_board(&context, &snapshot.observation.board)
        } else {
            rules.select_preview_for_board(&context, &snapshot.observation.board)
        };
        let selection = report
            .selected
            .as_ref()
            .map(|selected| selected.setup_id.as_str())
            .unwrap_or("none");
        let statuses = status_counts(&report.candidates);
        (selection.to_string(), statuses)
    } else {
        ("none".to_string(), "none".to_string())
    };
    let commit_label = commit.map(commit_name).unwrap_or("no_commit").to_string();
    let mut line = format!(
        "window={title:?} recognition=current:{current} hold:{hold} next:{next} board_cells={} confidence={:.3} stable={stable_label} commit={commit_label} pc={} queue_len={} selection={selection} statuses={statuses}",
        recognition.board_cells,
        recognition.confidence,
        tracker.pc_ordinal().get(),
        tracker.observed_queue_history().len(),
    );
    if recognition.ambiguous_board_cells > 0 || recognition.weak_board_cells > 0 {
        line.push_str(&format!(
            " weak_cells={} ambiguous_cells={}",
            recognition.weak_board_cells, recognition.ambiguous_board_cells
        ));
    }
    if verbose {
        line.push_str(&format!(
            " region_confidence=current:{:.3} hold:{:.3} next:{:?}",
            recognition.current_confidence,
            recognition.hold_confidence,
            recognition
                .next_confidences
                .iter()
                .map(|confidence| format!("{confidence:.3}"))
                .collect::<Vec<_>>()
        ));
    }
    line
}

fn status_counts(candidates: &[pc_overlay_core::rules::CandidateReport]) -> String {
    if candidates.is_empty() {
        return "none".to_string();
    }
    let mut counts = BTreeMap::<String, usize>::new();
    for candidate in candidates {
        *counts.entry(format!("{:?}", candidate.status)).or_default() += 1;
    }
    let mut entries = counts
        .into_iter()
        .map(|(status, count)| format!("{status}:{count}"))
        .collect::<Vec<_>>();
    entries.sort();
    entries.join(",")
}

fn commit_name(commit: &CommitOutcome) -> &'static str {
    match commit {
        CommitOutcome::Accepted { .. } => "accepted",
        CommitOutcome::Unchanged => "unchanged",
        CommitOutcome::IgnoredLowConfidence { .. } => "ignored_low_confidence",
        CommitOutcome::RejectedHistoryDiscontinuity => "history_discontinuity",
        CommitOutcome::RolledBack { .. } => "rolled_back",
        CommitOutcome::NewGame { .. } => "new_game",
    }
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = Args::default();
    let mut values = std::env::args().skip(1);
    while let Some(arg) = values.next() {
        match arg.as_str() {
            "--once" => args.once = true,
            "--verbose" => args.verbose = true,
            "--reviewed-only" => args.reviewed_only = true,
            "--interval-ms" => {
                args.interval_ms = values
                    .next()
                    .ok_or("--interval-ms requires a value")?
                    .parse()?;
            }
            "--rules" => {
                args.rules = PathBuf::from(values.next().ok_or("--rules requires a path")?);
            }
            "--diagrams" => {
                args.diagrams = PathBuf::from(values.next().ok_or("--diagrams requires a path")?);
            }
            "--calibration" => {
                args.calibration =
                    PathBuf::from(values.next().ok_or("--calibration requires a path")?);
            }
            "--record-dir" => {
                args.record_dir = Some(PathBuf::from(
                    values.next().ok_or("--record-dir requires a path")?,
                ));
            }
            "--record-max" => {
                args.record_max = values
                    .next()
                    .ok_or("--record-max requires a value")?
                    .parse()?;
            }
            "--help" | "-h" => {
                println!(
                    "run-live [--once] [--verbose] [--reviewed-only] [--interval-ms N] [--rules PATH] [--diagrams PATH] [--calibration PATH]"
                );
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument: {unknown}").into()),
        }
    }
    Ok(args)
}

fn load_calibration(path: &std::path::Path) -> Result<BoardCalibration, Box<dyn Error>> {
    if !path.exists() {
        eprintln!(
            "warning: calibration file {} not found; using the default 1920x1009 profile",
            path.display()
        );
        return Ok(BoardCalibration::default());
    }
    let text = fs::read_to_string(path)?;
    let calibration: BoardCalibration = serde_json::from_str(&text)?;
    calibration
        .validate()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(calibration)
}
