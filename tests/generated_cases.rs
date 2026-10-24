//! Generated case data (tools/build_cases.py) against the Python oracle.
//!
//! `python -I tools/build_cases.py coverage` writes
//! tests/fixtures/cases_coverage.json: for every verified setup, how many of
//! the 5040 following-bag orders some solve can play, for the bag alone (HOLD
//! empty) and for several 2nd-PC starts (HOLD empty or holding a case piece,
//! case pieces in a given queue order). The Rust `solvable` condition must
//! agree exactly on every closed bag.

use std::collections::BTreeMap;
use std::fs;

use pc_overlay_core::rules::{ReviewStatus, RuleSet, SetupRule};
use pc_overlay_core::{EvalContext, EvalResult, Piece};

const PIECES: [Piece; 7] = [
    Piece::I,
    Piece::J,
    Piece::L,
    Piece::O,
    Piece::S,
    Piece::T,
    Piece::Z,
];

fn all_bags() -> Vec<Vec<Piece>> {
    fn go(rest: &mut Vec<Piece>, order: &mut Vec<Piece>, out: &mut Vec<Vec<Piece>>) {
        if rest.is_empty() {
            out.push(order.clone());
            return;
        }
        for index in 0..rest.len() {
            let piece = rest.remove(index);
            order.push(piece);
            go(rest, order, out);
            order.pop();
            rest.insert(index, piece);
        }
    }
    let mut out = Vec::new();
    go(&mut PIECES.to_vec(), &mut Vec::new(), &mut out);
    out
}

fn closed(bag: &[Piece]) -> EvalContext {
    EvalContext {
        queue_history: bag.to_vec(),
        queue_closed: true,
        hold: None,
        current: bag[0],
        accessible: Default::default(),
        case_hold: None,
        case_queue: Vec::new(),
    }
}

fn pieces(letters: &str) -> Vec<Piece> {
    letters
        .chars()
        .map(|letter| match letter {
            'I' => Piece::I,
            'J' => Piece::J,
            'L' => Piece::L,
            'O' => Piece::O,
            'S' => Piece::S,
            'T' => Piece::T,
            'Z' => Piece::Z,
            other => panic!("not a piece: {other}"),
        })
        .collect()
}

fn load_cases() -> BTreeMap<String, RuleSet> {
    let mut cases = BTreeMap::new();
    for entry in fs::read_dir("data/cases").expect("data/cases") {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let rules = RuleSet::load(&path).unwrap_or_else(|error| panic!("{path:?}: {error:?}"));
            cases.insert(rules.case.id.clone(), rules);
        }
    }
    cases
}

#[test]
fn every_generated_case_loads_and_validates() {
    let cases = load_cases();
    assert!(cases.len() >= 40, "only {} cases", cases.len());
    let verified: usize = cases
        .values()
        .flat_map(|rules| &rules.methods)
        .flat_map(|method| &method.setups)
        .filter(|setup| setup.review_status == ReviewStatus::Verified)
        .count();
    assert!(verified > 300, "only {verified} verified setups");
}

#[test]
fn solvable_matches_the_python_oracle_on_every_bag() {
    type Row = (String, String, usize);
    let expected: BTreeMap<String, BTreeMap<String, Vec<Row>>> = serde_json::from_str(
        &fs::read_to_string("tests/fixtures/cases_coverage.json").expect("coverage fixture"),
    )
    .unwrap();
    let cases = load_cases();
    let bags = all_bags();
    let mut rows_to_check = Vec::new();
    for (case_id, setups) in &expected {
        let rules = &cases[case_id];
        for (setup_id, rows) in setups {
            let setup = rules
                .methods
                .iter()
                .flat_map(|method| &method.setups)
                .find(|setup| &setup.id == setup_id)
                .unwrap_or_else(|| panic!("{setup_id} missing"));
            for row in rows {
                rows_to_check.push((case_id, setup, row));
            }
        }
    }
    let check = &|(case_id, setup, (hold, queue, count)): &(&String, &SetupRule, &Row)| {
        let hold = pieces(hold).first().copied();
        let queue = pieces(queue);
        let got = bags
            .iter()
            .filter(|bag| {
                let mut context = closed(bag);
                if !queue.is_empty() || hold.is_some() {
                    context = context.with_case(hold, &queue);
                }
                setup.build_condition.evaluate(&context) == EvalResult::Satisfied
            })
            .count();
        (got != *count).then(|| {
            format!(
                "{case_id}/{} hold={hold:?} queue={queue:?}: rust {got}, python {count}",
                setup.id
            )
        })
    };
    // About 8 million evaluations; the rows are independent, so split them
    // across the cores.
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let chunk = rows_to_check.len().div_ceil(threads).max(1);
    let mismatches: Vec<String> = std::thread::scope(|scope| {
        let workers: Vec<_> = rows_to_check
            .chunks(chunk)
            .map(|rows| scope.spawn(move || rows.iter().filter_map(check).collect::<Vec<_>>()))
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect()
    });
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn solvable_is_pending_until_the_completions_agree() {
    let cases = load_cases();
    // A setup that does not solve every bag must be Pending with nothing
    // disclosed, and decided once the bag is closed.
    let setup = cases
        .values()
        .flat_map(|rules| &rules.methods)
        .flat_map(|method| &method.setups)
        .filter(|setup| setup.review_status == ReviewStatus::Verified)
        .find(|setup| {
            all_bags()
                .iter()
                .any(|bag| setup.build_condition.evaluate(&closed(bag)) == EvalResult::Failed)
        })
        .expect("some setup fails on some bag");
    let open = EvalContext {
        queue_history: vec![],
        queue_closed: false,
        hold: None,
        current: Piece::T,
        accessible: Default::default(),
        case_hold: None,
        case_queue: Vec::new(),
    };
    assert_eq!(setup.build_condition.evaluate(&open), EvalResult::Pending);
}
