//! Node-free parity regression: replays parity/fixtures (compact Showdown corpora must match;
//! `divergent/` cases, which pair vanilla Showdown with the pkmn-patched sim, must still be
//! detected with the recorded category).
#[test]
fn parity_fixtures() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_parity")).arg("fixtures").output().expect("run parity");
    let text = String::from_utf8_lossy(&out.stdout);
    eprintln!("{text}");
    assert!(out.status.success(), "parity fixtures failed:\n{text}");
    assert!(text.contains("match"));
}
