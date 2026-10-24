//! Times Setup Konbini candidate search from each PC's start with random
//! 7-bag queues and a cold move cache (`cargo run --release --example
//! konbini_timing`). Needs the cache from `cargo run --bin fetch-konbini`.

use std::{path::Path, time::Instant};

use pc_overlay_core::{
    KonbiniLibrary, Observation, Piece, empty_board,
    finder::{ALL_PIECES, Finder},
    konbini::BuildQuery,
};

const ROUNDS: usize = 50;

fn main() {
    let library = KonbiniLibrary::load_dir(Path::new("data/konbini")).expect("fetched cache");
    let mut seed: u64 = 12345;
    let mut random = move |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    for stage in 1..=7u8 {
        let (mut worst, mut total, mut shown) = (0f64, 0f64, 0usize);
        for _ in 0..ROUNDS {
            // A perfect-clear loop has dealt 10 pieces per earlier PC.
            let start = 10 * usize::from(stage - 1);
            let mut dealt = Vec::new();
            while dealt.len() < start + 14 {
                let mut bag = ALL_PIECES.to_vec();
                for index in (1..7).rev() {
                    bag.swap(index, random(index + 1));
                }
                dealt.extend(bag);
            }
            let hold = (stage > 1).then(|| dealt[start - 1]);
            let queue = &dealt[start..];
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
                current_index: Some(start),
            };
            let started = Instant::now();
            let candidates = library.candidates(stage, &query, &mut Finder::new());
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            worst = worst.max(elapsed);
            total += elapsed;
            shown += candidates.len();
        }
        println!(
            "PC {stage}: {} setups, mean {:.2} ms, worst {:.2} ms, {:.1} shown on average",
            library.stage_count(stage),
            total / ROUNDS as f64,
            worst,
            shown as f64 / ROUNDS as f64
        );
    }
}
