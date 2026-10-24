//! Time of `konbini::staged_moves` on boards filled with 1-5 pieces' worth
//! of cells (column by column from the left), with random 7-bag queues.
//! `cargo run --release --example staged_time`.

use pc_overlay_core::{
    Observation, empty_board,
    finder::{ALL_PIECES, Finder},
    konbini,
};
use std::time::Instant;
fn main() {
    let mut seed: u64 = 1357;
    let mut random = move |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let mut finder = Finder::new();
    for placed in [1usize, 2, 3, 5] {
        let (mut worst, mut total, mut n) = (0f64, 0f64, 0);
        for _ in 0..20 {
            let mut dealt = Vec::new();
            while dealt.len() < 40 {
                let mut bag = ALL_PIECES.to_vec();
                for i in (1..7).rev() {
                    bag.swap(i, random(i + 1));
                }
                dealt.extend(bag);
            }
            // `placed` pieces' worth of cells, column by column from the left.
            let mut board = empty_board();
            for cell in 0..placed * 4 {
                board[cell % 4][cell / 4] = Some(dealt[0]);
            }
            let first = 10 + placed + 1;
            let obs = Observation {
                board,
                hold: Some(dealt[10]),
                current: dealt[first],
                next5: [
                    dealt[first + 1],
                    dealt[first + 2],
                    dealt[first + 3],
                    dealt[first + 4],
                    dealt[first + 5],
                ],
            };
            let started = Instant::now();
            let moves = konbini::staged_moves(&mut finder, &obs, first);
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            worst = worst.max(ms);
            total += ms;
            n += 1;
            let _ = moves;
            if finder.cached_moves() > 200_000 {
                finder.clear_cache();
            }
        }
        println!(
            "{placed} placed: mean {:.0} ms, worst {:.0} ms",
            total / n as f64,
            worst
        );
    }
}
