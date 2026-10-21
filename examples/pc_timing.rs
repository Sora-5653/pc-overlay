//! Times the perfect-clear search the overlay runs once a setup is built.
//!
//! For every verified source solve (`data/ijlo.json`, `data/cases/*.json`),
//! the finished setup is the field and the solve's pieces in a shuffled
//! order are the queue, HOLD empty, as `run-overlay` searches it (one HOLD
//! slot, SRS+, cold move cache per state). Reports the search time, how
//! many perfect clears were found, and how often the source solve is among
//! them (only when the queue order allows it, so below 100% is expected).
//!
//! `cargo run --release --example pc_timing`

use std::{fs, path::PathBuf, time::Instant};

use pc_overlay_core::{
    DiagramSet, Placement, RuleSet,
    finder::{Finder, Mask, Step, Supply, Target, bit},
    rules::ReviewStatus,
};

fn mask(placement: &Placement) -> Mask {
    placement
        .cells
        .iter()
        .fold(0, |acc, cell| acc | bit(cell.x, cell.y))
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut ijlo = RuleSet::load(root.join("data/ijlo.json")).unwrap();
    let diagrams = DiagramSet::load(root.join("data/ijlo/diagram_cells.json")).unwrap();
    ijlo.attach_setup_diagrams(&diagrams).unwrap();
    let mut sets = vec![ijlo];
    let mut paths: Vec<PathBuf> = fs::read_dir(root.join("data/cases"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    sets.extend(paths.iter().map(|path| RuleSet::load(path).unwrap()));

    // As `run-overlay` keeps; `--limit N` changes it.
    let limit: usize = std::env::args()
        .skip_while(|arg| arg != "--limit")
        .nth(1)
        .map_or(100, |value| value.parse().unwrap());
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut times = Vec::new();
    let mut live_times = Vec::new();
    let (mut searched, mut with_solution, mut source_found, mut comparable) = (0, 0, 0, 0);
    let mut counts = Vec::new();
    for rules in &sets {
        for method in &rules.methods {
            for setup in &method.setups {
                if setup.placements.is_empty() {
                    continue;
                }
                let drawn: Mask = setup.placements.iter().map(mask).fold(0, |a, b| a | b);
                let setup_target = Target::new(setup.placements.iter().flat_map(|placement| {
                    placement
                        .cells
                        .iter()
                        .map(|cell| (cell.x, cell.y, Some(placement.piece)))
                }))
                .unwrap();
                let field = setup_target.field(drawn);
                for solve in &setup.solves {
                    if solve.review_status != ReviewStatus::Verified || solve.target.is_empty() {
                        continue;
                    }
                    let mut queue: Vec<_> = solve.target.iter().map(|p| p.piece).collect();
                    for index in (1..queue.len()).rev() {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        queue.swap(index, (seed % (index as u64 + 1)) as usize);
                    }
                    let mut finder = Finder::new();
                    let started = Instant::now();
                    let found = finder.perfect_clears(
                        field,
                        Supply::Queue {
                            hold: None,
                            queue: &queue,
                        },
                        limit,
                    );
                    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                    if elapsed > 1000.0 {
                        eprintln!(
                            "slow: {} {} {elapsed:.0} ms {queue:?} moves={}",
                            rules.case.id,
                            solve.id,
                            finder.cached_moves()
                        );
                    }
                    times.push(elapsed);
                    // The overlay sees at most HOLD + Current + NEXT5.
                    if queue.len() <= 7 {
                        live_times.push(elapsed);
                    }
                    searched += 1;
                    let Some((_, solutions)) = found else {
                        continue;
                    };
                    with_solution += 1;
                    counts.push(solutions.len());
                    // Drawing and field rows agree only when the setup
                    // cleared no row.
                    if field == drawn {
                        comparable += 1;
                        let mut expected: Vec<Step> =
                            solve.target.iter().map(|p| (p.piece, mask(p))).collect();
                        expected.sort();
                        if solutions.contains(&expected) {
                            source_found += 1;
                        }
                    }
                }
            }
        }
    }
    times.sort_by(f64::total_cmp);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    counts.sort_unstable();
    live_times.sort_by(f64::total_cmp);
    println!(
        "{} searches of at most 7 pieces: mean {:.1} ms, max {:.1} ms",
        live_times.len(),
        live_times.iter().sum::<f64>() / live_times.len() as f64,
        live_times.last().copied().unwrap_or(0.0)
    );
    println!(
        "{searched} searches: mean {mean:.1} ms, median {:.1} ms, max {:.1} ms",
        times[times.len() / 2],
        times[times.len() - 1]
    );
    println!(
        "{with_solution} with a perfect clear (median {} / max {} solutions); source solve found in {source_found} of {comparable} comparable",
        counts.get(counts.len() / 2).copied().unwrap_or(0),
        counts.last().copied().unwrap_or(0)
    );
}
