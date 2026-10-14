//! Node-free parity regression test: replay every fixture under parity/fixtures.
//!
//! - `corpus-*.jsonl`: battles recorded from Pokémon Showdown (spec + per-step legal choices and
//!   state hashes); the native simulator must reproduce every step.
//! - `divergent/*.json`: shrunk cases where the native sim runs in vanilla mode against the
//!   pkmn-patched Showdown reference; the harness must still flag them with the recorded
//!   category, which guards the harness itself.
//! - `passing/*.json` (full traces) must match; `open/*.json` are known divergences (reported).

use std::process::Command;

#[test]
fn fixtures_replay() {
    let out = Command::new(env!("CARGO_BIN_EXE_parity")).arg("fixtures").output().expect("run parity fixtures");
    let stdout = String::from_utf8_lossy(&out.stdout);
    println!("{stdout}");
    assert!(out.status.success(), "fixture regression:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("corpus-gen1ou.jsonl"), "corpus not found");
}
