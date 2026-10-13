//! Single-threaded speed benchmark: native simulator vs pkmn/engine on the same random games.
//!
//! `cargo run --release -p sim-backend --example bench -- [battles] [teams] [seed]`
//!
//! Built from this crate (not sim-parity) so the PRNG call tracing feature is compiled out.
#[path = "../../sim-parity/src/bench.rs"]
#[allow(dead_code)]
mod bench;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n = args.first().map(|s| s.parse().expect("battles")).unwrap_or(5000);
    let teams = args.get(1).cloned().unwrap_or_else(|| "randbats".into());
    let seed = args.get(2).map(|s| s.parse().expect("seed")).unwrap_or(1);
    let teams = if teams.contains('/') {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(&teams);
        p.to_string_lossy().into_owned()
    } else {
        teams
    };
    bench::bench(n, seed, &teams);
}
