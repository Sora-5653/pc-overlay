//! The perfect-clear search rediscovers the solves drawn in the source.
//!
//! Every verified solve of `data/ijlo.json` and `data/cases/*.json` is a set
//! of placements on top of its setup, drawn without line clears. The search
//! starts from the finished setup with the solve's pieces in any order and
//! must list that set among its perfect clears (searched with each piece
//! kept to the cells of its type, which prunes the other solutions only). The source solves were checked
//! with SRS 90-degree movement; the search uses SRS+, which reaches more.

use std::{fs, path::PathBuf};

use pc_overlay_core::{
    DiagramSet, Placement, RuleSet,
    finder::{ALL_PIECES, Finder, Mask, Step, Supply, Target, bit},
    rules::ReviewStatus,
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn mask(placement: &Placement) -> Mask {
    placement
        .cells
        .iter()
        .fold(0, |acc, cell| acc | bit(cell.x, cell.y))
}

fn rule_sets() -> Vec<RuleSet> {
    let mut ijlo = RuleSet::load(root().join("data/ijlo.json")).unwrap();
    let diagrams = DiagramSet::load(root().join("data/ijlo/diagram_cells.json")).unwrap();
    ijlo.attach_setup_diagrams(&diagrams).unwrap();
    let mut sets = vec![ijlo];
    let mut paths: Vec<PathBuf> = fs::read_dir(root().join("data/cases"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    sets.extend(paths.iter().map(|path| RuleSet::load(path).unwrap()));
    sets
}

/// (case, solve id, found) for every verified solve with a drawn setup.
fn rediscover(sets: &[RuleSet]) -> Vec<(String, String, bool)> {
    let mut finder = Finder::new();
    let mut results = Vec::new();
    for rules in sets {
        for method in &rules.methods {
            for setup in &method.setups {
                if setup.placements.is_empty() {
                    continue;
                }
                let placed: Mask = setup.placements.iter().map(mask).fold(0, |a, b| a | b);
                for solve in &setup.solves {
                    if solve.review_status != ReviewStatus::Verified || solve.target.is_empty() {
                        continue;
                    }
                    let mut counts = [0u8; 7];
                    let mut expected: Vec<Step> = solve
                        .target
                        .iter()
                        .map(|placement| {
                            let index = ALL_PIECES
                                .iter()
                                .position(|piece| *piece == placement.piece)
                                .unwrap();
                            counts[index] += 1;
                            (placement.piece, mask(placement))
                        })
                        .collect();
                    expected.sort();
                    // The perfect clear with each piece kept to the cells the
                    // source gives that piece type: the search is exhaustive,
                    // so this finds the source solve exactly when the full
                    // search lists it, without enumerating the others.
                    let cells =
                        setup
                            .placements
                            .iter()
                            .chain(&solve.target)
                            .flat_map(|placement| {
                                placement
                                    .cells
                                    .iter()
                                    .map(|cell| (cell.x, cell.y, Some(placement.piece)))
                            });
                    let target = Target::new(cells).unwrap();
                    let full = Target::perfect_clear(target.piece_count() * 4 / 10).unwrap();
                    let found = target.cells == full.cells
                        && finder
                            .solutions(&target, placed, Supply::AnyOrder(counts), usize::MAX)
                            .contains(&expected);
                    results.push((rules.case.id.clone(), solve.id.clone(), found));
                }
            }
        }
    }
    results
}

#[test]
fn every_verified_source_solve_is_found() {
    let results = rediscover(&rule_sets());
    let missing: Vec<_> = results.iter().filter(|(_, _, found)| !found).collect();
    eprintln!(
        "{} of {} verified solves rediscovered",
        results.len() - missing.len(),
        results.len()
    );
    assert!(results.len() > 100, "{}", results.len());
    assert!(missing.is_empty(), "not found: {missing:?}");
}
