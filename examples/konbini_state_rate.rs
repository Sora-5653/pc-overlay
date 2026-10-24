//! The PC rate of the shown Setup Konbini candidates for the state on
//! screen (`konbini::state_pc_rate`) at each PC's start, with random 7-bag
//! queues: how often a sheet 100% setup is below 100% here, and the time
//! it takes. `cargo run --release --example konbini_state_rate [-- --rounds N]`.
//! Needs `cargo run --bin fetch-konbini`.

use std::{path::Path, time::Instant};

use pc_overlay_core::{
    KonbiniLibrary, Observation, Piece, empty_board,
    finder::{ALL_PIECES, Finder},
    konbini::{self, BuildQuery},
};

fn main() {
    let rounds: usize = std::env::args()
        .skip_while(|arg| arg != "--rounds")
        .nth(1)
        .map_or(10, |value| value.parse().unwrap());
    let library = KonbiniLibrary::load_dir(Path::new("data/konbini")).expect("fetched cache");
    let mut seed: u64 = 24680;
    let mut random = move |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let mut finder = Finder::new();
    for stage in 1..=7u8 {
        let (mut rated, mut sheet_full, mut below, mut worst, mut total_ms) = (0, 0, 0, 0f64, 0f64);
        let mut per_state_worst = 0f64;
        for _ in 0..rounds {
            let before = 10 * usize::from(stage - 1);
            let mut dealt = Vec::new();
            while dealt.len() < before + 30 {
                let mut bag = ALL_PIECES.to_vec();
                for index in (1..7).rev() {
                    bag.swap(index, random(index + 1));
                }
                dealt.extend(bag);
            }
            let hold = (stage > 1).then(|| dealt[before]);
            let current = before + usize::from(hold.is_some());
            let queue = &dealt[current..];
            let observation = Observation {
                board: empty_board(),
                hold,
                current: queue[0],
                next5: [queue[1], queue[2], queue[3], queue[4], queue[5]],
            };
            let pc_pieces: Vec<Piece> =
                hold.into_iter().chain(queue[..6].iter().copied()).collect();
            let query = BuildQuery {
                pc_pieces: &pc_pieces,
                observation: &observation,
                pieces_before_pc: Some(before),
            };
            let candidates = library.candidates(stage, &query, &mut finder);
            let state_started = Instant::now();
            for candidate in &candidates {
                let started = Instant::now();
                let rate = konbini::state_pc_rate(
                    &mut finder,
                    candidate.setup,
                    &candidate.progress,
                    &observation,
                    current,
                );
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                worst = worst.max(elapsed);
                total_ms += elapsed;
                rated += 1;
                if candidate.setup.sol_percent == Some(100.0) {
                    sheet_full += 1;
                    if rate.is_none_or(|rate| rate < 100.0) {
                        below += 1;
                    }
                }
            }
            per_state_worst = per_state_worst.max(state_started.elapsed().as_secs_f64() * 1000.0);
            if finder.cached_moves() > 200_000 {
                finder.clear_cache();
            }
        }
        println!(
            "PC {stage}: {rated} candidates rated, mean {:.1} ms, worst {:.1} ms, worst state (all candidates) {:.0} ms; sheet 100%: {sheet_full}, below 100% here: {below}",
            total_ms / rated.max(1) as f64,
            worst,
            per_state_worst
        );
    }
}
