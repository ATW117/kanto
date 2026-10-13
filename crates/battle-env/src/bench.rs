//! Engine-only throughput: random self-play straight through libpkmn (no logs, observations or
//! masks), mirroring pkmn's own benchmark. Used by bench/bench.py.

use crate::backend::Backend;
use crate::pkmn1::{Gen1Battle, Pkmn1};
use crate::rng::{derive_seed, Rng};
use crate::team::TeamSource;
use rayon::prelude::*;
use std::time::Instant;

/// Play `battles` random games on `threads` threads; returns (seconds, total updates, total turns).
/// Teams are sampled before timing starts.
pub fn engine_only(source: &TeamSource, battles: usize, threads: usize, seed: u64) -> (f64, u64, u64) {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads.max(1)).build().unwrap();
    let starts: Vec<Gen1Battle> = (0..battles)
        .map(|i| {
            let mut rng = Rng::new(derive_seed(seed, i as u64));
            let (a, b) = (source.sample(&mut rng), source.sample(&mut rng));
            Pkmn1::new_battle(&a, &b, rng.next_u64())
        })
        .collect();
    let t0 = Instant::now();
    let (updates, turns) = pool.install(|| {
        starts
            .into_par_iter()
            .enumerate()
            .with_min_len(16)
            .map(|(i, mut b)| play(&mut b, derive_seed(seed ^ 0xBEEF, i as u64)))
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    });
    (t0.elapsed().as_secs_f64(), updates, turns)
}

fn play(b: &mut Gen1Battle, seed: u64) -> (u64, u64) {
    let mut rng = Rng::new(seed);
    let mut choices = [0u8; 16];
    let (mut c1, mut c2) = (0u8, 0u8);
    let mut updates = 0u64;
    loop {
        let r = unsafe { pkmn_sys::pkmn_gen1_battle_update(&mut b.raw, c1, c2, std::ptr::null_mut()) };
        b.result = r;
        updates += 1;
        if pkmn_sys::bits::result_kind(r) != 0 || Pkmn1::turn(b) >= crate::env::MAX_TURNS {
            return (updates, Pkmn1::turn(b) as u64);
        }
        let n = Pkmn1::choices(b, 0, &mut choices);
        c1 = choices[rng.below(n as u64) as usize];
        let n = Pkmn1::choices(b, 1, &mut choices);
        c2 = choices[rng.below(n as u64) as usize];
    }
}
