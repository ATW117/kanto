//! Single-threaded speed benchmark: native simulator vs pkmn/engine on the same random games.
//!
//! `cargo run --release -p kanto-backend --example bench -- [battles] [teams] [seed] [mode]`
//!
//! `mode`:
//! - `compare` (default): native (vanilla and pkmn-patched) turns/s, and pkmn/engine with
//!   `--features pkmn`.
//! - `breakdown`: native timing breakdown (update, choices, state copy, blob encode/decode).
//! - `h2h` (needs `--features pkmn`; optional 5th arg = rounds): native vs pkmn/engine,
//!   interleaved round by round so both see the same machine load; median/min/max.
//! - `native`: only the native games (vanilla), e.g. under `valgrind --tool=callgrind`.
//!
//! Built from this crate (not kanto-parity) so the PRNG call tracing feature is compiled out.
#[path = "../../kanto-parity/src/bench.rs"]
#[allow(dead_code)]
mod bench;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n = args.first().map(|s| s.parse().expect("battles")).unwrap_or(5000);
    let teams = args.get(1).cloned().unwrap_or_else(|| "randbats".into());
    let seed = args.get(2).map(|s| s.parse().expect("seed")).unwrap_or(1);
    let mode = args.get(3).map(String::as_str).unwrap_or("compare");
    let teams = if teams.contains('/') {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(&teams);
        p.to_string_lossy().into_owned()
    } else {
        teams
    };
    match mode {
        "compare" => bench::bench(n, seed, &teams),
        "breakdown" => {
            #[cfg(feature = "pkmn")]
            bench::breakdown_pkmn(n, seed, &teams, 5);
            bench::breakdown(n, seed, &teams, kanto_gen1::Compat::Vanilla, 5);
            bench::breakdown(n, seed, &teams, kanto_gen1::Compat::Pkmn, 5);
        }
        "native" => bench::native_only(n, seed, &teams),
        #[cfg(feature = "pkmn")]
        "h2h" => bench::h2h(n, seed, &teams, args.get(4).map(|s| s.parse().expect("reps")).unwrap_or(11)),
        m => panic!("unknown mode {m} (compare | breakdown | native)"),
    }
}
