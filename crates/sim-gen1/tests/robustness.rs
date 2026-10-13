//! Robustness of the public API: no input (teams, choices, `clone_state` blobs) may panic.
//!
//! - A1: Showdown's "moveSlot is null" state (a Pokémon whose `lastSelectedMoveSlot` points past
//!   its current move slots must "Fight") is handled, not a panic.
//! - A2: `apply_choices` / `Generation::update` reject illegal choices and leave the battle as is.
//! - A3: `from_bytes` validates every field; corrupted blobs are rejected or step safely.
//! - A4: `Battle::new` returns errors for malformed teams.

use sim_core::{Choice, Generation, Outcome, Request};
use sim_data::gen1::{mv, SPECIES};
use sim_gen1::state::st;
use sim_gen1::{Battle, MonSpec, Prng, Rules};
use std::panic::{catch_unwind, AssertUnwindSafe};

fn sp(name: &str) -> u8 {
    (1..=151u8).find(|&i| SPECIES[i as usize].name == name).unwrap()
}

fn mon(species: u8, moves: [u8; 4]) -> MonSpec {
    MonSpec { species, level: 100, moves, ivs: [30; 6], evs: [255; 6] }
}

/// Tiny deterministic generator (xorshift64*).
struct R(u64);
impl R {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A random team with any (not necessarily learnable) moves, so every mechanic shows up.
fn random_team(r: &mut R) -> Vec<MonSpec> {
    let n = 1 + r.below(6) as usize;
    (0..n)
        .map(|_| {
            let species = 1 + r.below(151) as u8;
            let k = 1 + r.below(4) as usize;
            let mut moves = [0u8; 4];
            let mut i = 0;
            while i < k {
                let m = 1 + r.below(164) as u8;
                if !moves.contains(&m) {
                    moves[i] = m;
                    i += 1;
                }
            }
            let level = if r.below(4) == 0 { 1 + r.below(100) as u8 } else { 100 };
            let ivs = std::array::from_fn(|_| 2 * r.below(16) as u8);
            let evs = std::array::from_fn(|_| r.below(256) as u8);
            MonSpec { species, level, moves, ivs, evs }
        })
        .collect()
}

fn random_choice(b: &Battle, p: usize, r: &mut R) -> Choice {
    let mut buf = [Choice::Pass; 16];
    let n = Generation::choices(b, p, &mut buf);
    buf[r.below(n as u64) as usize]
}

/// Step with random legal choices through the checked API (which must accept them).
fn step_random(b: &mut Battle, r: &mut R) {
    let (c1, c2) = (random_choice(b, 0, r), random_choice(b, 1, r));
    b.apply_choices(c1, c2, &mut ()).expect("a legal choice was rejected");
}

/// Random mid-game states (decision points), including fresh and finished battles.
fn sample_states(seed: u64, games: usize) -> Vec<Battle> {
    let mut r = R(seed | 1);
    let mut out = Vec::new();
    for g in 0..games {
        let (t1, t2) = (random_team(&mut r), random_team(&mut r));
        let rules = if g % 3 == 0 { Rules::NO_CLAUSES } else { Rules::OU };
        let mut b = Battle::new(&t1, &t2, Prng::from_u64(r.next()), rules).unwrap();
        out.push(b);
        let stop = r.below(120) as usize;
        for i in 0..=stop {
            if b.outcome() != Outcome::Ongoing {
                break;
            }
            step_random(&mut b, &mut r);
            if i % 7 == 0 || i == stop {
                out.push(b);
            }
        }
    }
    out
}

// ---- A1 --------------------------------------------------------------------------------------

/// Mew uses Transform (its 4th move) on a Jigglypuff that only knows Sing, then is put to sleep:
/// `lastSelectedMoveSlot` (3) points past its single move slot when it must "Fight". Showdown
/// 0.11.11 throws "moveSlot is null"; we read slot 0 (as pkmn/engine does).
#[test]
fn move_slot_null_does_not_panic() {
    let mew = mon(sp("Mew"), [mv::PSYCHIC, mv::SOFTBOILED, mv::THUNDERWAVE, mv::TRANSFORM]);
    let jig = mon(sp("Jigglypuff"), [mv::SING, 0, 0, 0]);
    let mut reached = 0;
    for seed in 0..300u64 {
        for rules in [Rules::OU, Rules::NO_CLAUSES] {
            let mut b = Battle::new(&[mew], &[jig], Prng::from_u64(seed), rules).unwrap();
            b.apply_choices(Choice::Pass, Choice::Pass, &mut ()).unwrap();
            for _ in 0..12 {
                if b.outcome() != Outcome::Ongoing {
                    break;
                }
                let sd = &b.sides[0];
                let asleep = matches!(sd.mon().status, st::SLP | st::FRZ);
                if asleep && sd.last_selected_move_slot >= sd.active.n_slots {
                    reached += 1;
                }
                let c1 = if b.is_legal(0, Choice::Move(4)) { Choice::Move(4) } else { Choice::Move(1) };
                let r = catch_unwind(AssertUnwindSafe(|| b.apply_choices(c1, Choice::Move(1), &mut ())));
                assert!(r.is_ok(), "seed {seed}: panicked");
                r.unwrap().expect("legal choice rejected");
                // The fallback reads slot 0: Jigglypuff's Sing.
                if asleep && b.sides[0].active.transformed {
                    assert_eq!(b.sides[0].last_enemy_selected_move, mv::SING);
                }
            }
        }
    }
    assert!(reached > 50, "the moveSlot-null state was reached only {reached} times");
}

// ---- A4 --------------------------------------------------------------------------------------

#[test]
fn new_rejects_malformed_teams() {
    let ok = mon(sp("Tauros"), [mv::BODYSLAM, 0, 0, 0]);
    let t = vec![ok];
    let cases: Vec<(&str, Vec<MonSpec>)> = vec![
        ("empty", vec![]),
        ("7 Pokémon", vec![ok; 7]),
        ("species 0", vec![MonSpec { species: 0, ..ok }]),
        ("species 152", vec![MonSpec { species: 152, ..ok }]),
        ("species 255", vec![MonSpec { species: 255, ..ok }]),
        ("move 166", vec![MonSpec { moves: [166, 0, 0, 0], ..ok }]),
        ("move 255", vec![MonSpec { moves: [mv::TACKLE, 255, 0, 0], ..ok }]),
        ("Struggle", vec![MonSpec { moves: [165, 0, 0, 0], ..ok }]),
        ("no moves", vec![MonSpec { moves: [0; 4], ..ok }]),
        ("duplicate moves", vec![MonSpec { moves: [mv::TACKLE, mv::TACKLE, 0, 0], ..ok }]),
        ("level 0", vec![MonSpec { level: 0, ..ok }]),
        ("level 101", vec![MonSpec { level: 101, ..ok }]),
        ("iv 32", vec![MonSpec { ivs: [32; 6], ..ok }]),
    ];
    for (what, team) in cases {
        for (a, b, player) in [(&team, &t, 0), (&t, &team, 1)] {
            let r = catch_unwind(|| Battle::new(a, b, Prng::from_u64(1), Rules::OU));
            let err = r.unwrap_or_else(|_| panic!("{what}: panicked")).err().unwrap_or_else(|| panic!("{what}: accepted"));
            assert_eq!(err.player, player, "{what}");
        }
    }
    assert!(Battle::new(&[ok; 6], &t, Prng::from_u64(1), Rules::OU).is_ok());
    let gappy = MonSpec { moves: [0, mv::TACKLE, 0, mv::GROWL], ..ok };
    let b = Battle::new(&[gappy], &t, Prng::from_u64(1), Rules::OU).unwrap();
    assert_eq!(b.sides[0].mons[0].n_moves, 2);
}

// ---- A2 --------------------------------------------------------------------------------------

fn bytes(b: &Battle) -> Vec<u8> {
    let mut v = Vec::new();
    b.to_bytes(&mut v);
    v
}

fn assert_rejected(b: &Battle, c1: Choice, c2: Choice, what: &str) {
    let mut x = *b;
    let before = bytes(&x);
    let r = catch_unwind(AssertUnwindSafe(|| x.apply_choices(c1, c2, &mut ())));
    let r = r.unwrap_or_else(|_| panic!("{what}: panicked"));
    assert!(r.is_err(), "{what}: accepted");
    assert_eq!(before, bytes(&x), "{what}: battle changed");
    let r = catch_unwind(AssertUnwindSafe(|| Generation::update(&mut x, c1, c2, &mut ())));
    assert!(r.unwrap_or_else(|_| panic!("{what}: panicked")).is_err(), "{what}: Generation::update accepted");
}

#[test]
fn illegal_choices_are_rejected() {
    let tauros = mon(sp("Tauros"), [mv::BODYSLAM, mv::HYPERBEAM, 0, 0]);
    let chansey = mon(sp("Chansey"), [mv::SOFTBOILED, 0, 0, 0]);
    let snorlax = mon(sp("Snorlax"), [mv::REST, mv::BODYSLAM, 0, 0]);
    let t = vec![tauros, chansey, snorlax];
    let mut b0 = Battle::new(&t, &t, Prng::from_u64(42), Rules::OU).unwrap();
    // Before the start only Pass is legal.
    assert_rejected(&b0, Choice::Move(1), Choice::Pass, "move before start");
    b0.apply_choices(Choice::Pass, Choice::Pass, &mut ()).unwrap();
    let m = Choice::Move(1);
    assert_rejected(&b0, Choice::Pass, m, "pass in a move request");
    assert_rejected(&b0, Choice::Move(0), m, "move 0");
    assert_rejected(&b0, Choice::Move(3), m, "move 3 of 2");
    assert_rejected(&b0, Choice::Move(5), m, "move 5");
    assert_rejected(&b0, Choice::Switch(0), m, "switch 0");
    assert_rejected(&b0, Choice::Switch(1), m, "switch to self");
    assert_rejected(&b0, Choice::Switch(4), m, "switch beyond the team");
    assert_rejected(&b0, Choice::Switch(7), m, "switch 7");
    assert_rejected(&b0, m, Choice::Switch(1), "p2 switch to self");
    let mut b = b0;
    b.sides[0].mons[1].hp = 0;
    b.sides[0].mons[1].set_fainted(true);
    b.sides[0].mons[1].set_faint_queued(true);
    b.sides[0].left -= 1;
    assert_rejected(&b, Choice::Switch(2), m, "switch to a fainted Pokémon");
    assert!(b.apply_choices(Choice::Switch(3), m, &mut ()).is_ok());
    let mut b = b0;
    b.sides[0].mons[0].moves[1].set_pp(0);
    assert_rejected(&b, Choice::Move(2), m, "move with 0 PP");
    assert!(b.apply_choices(Choice::Move(1), m, &mut ()).is_ok());
    let mut b = b0;
    b.sides[0].mons[0].moves[0].set_disabled(true);
    assert_rejected(&b, Choice::Move(1), m, "disabled move");
    // Every legal choice is accepted.
    let mut buf = [Choice::Pass; 16];
    let n = Generation::choices(&b0, 0, &mut buf);
    for &c in &buf[..n] {
        assert!(b0.clone().apply_choices(c, m, &mut ()).is_ok(), "{c:?} rejected");
    }
}

/// `is_legal` agrees with `choices` on every choice in a superset of the action space, over
/// many states; forced switches only accept switches; finished battles accept nothing.
#[test]
fn is_legal_matches_choices() {
    let mut universe = vec![Choice::Pass];
    for k in 0..=7 {
        universe.push(Choice::Move(k));
        universe.push(Choice::Switch(k));
    }
    let (mut forced, mut ended) = (0, 0);
    for b in sample_states(7, 300) {
        for p in 0..2 {
            let mut buf = [Choice::Pass; 16];
            let n = Generation::choices(&b, p, &mut buf);
            for &c in &universe {
                assert_eq!(b.is_legal(p, c), buf[..n].contains(&c), "{c:?} for p{p}");
            }
            forced += (b.sides[p].request == Request::Switch) as usize;
        }
        if b.ended {
            ended += 1;
            assert!(b.clone().apply_choices(Choice::Pass, Choice::Pass, &mut ()).is_err());
        }
    }
    assert!(forced > 20 && ended > 20, "forced {forced} ended {ended}");
}

// ---- A3 --------------------------------------------------------------------------------------

#[test]
fn every_reachable_state_round_trips() {
    for b in sample_states(11, 300) {
        if let Err(e) = b.validate() {
            panic!("reachable state fails validation: {e}");
        }
        let blob = bytes(&b);
        assert_eq!(Battle::from_bytes(&blob), Some(b));
    }
}

/// Play up to `updates` random legal updates; returns whether it panicked.
fn steps_without_panic(b: Battle, seed: u64, updates: usize) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let mut b = b;
        let mut r = R(seed | 1);
        for _ in 0..updates {
            if b.outcome() != Outcome::Ongoing {
                break;
            }
            step_random(&mut b, &mut r);
            let _ = bytes(&b);
        }
    }))
    .is_ok()
}

#[test]
fn corrupted_blobs_are_rejected_or_safe() {
    std::panic::set_hook(Box::new(|_| {}));
    let states = sample_states(23, 150);
    let mut r = R(0x9E3779B97F4A7C15);
    let (mut tried, mut accepted, mut panicked) = (0, 0, 0);
    let mut first_panic = None;
    let encoded = {
        let mut v = Vec::new();
        use sim_gen1::ser::Bin;
        states[0].put(&mut v);
        v.len()
    };
    for round in 0..40_000 {
        let b = states[r.below(states.len() as u64) as usize];
        let mut blob = bytes(&b);
        let flips = 1 + r.below(4);
        for _ in 0..flips {
            let i = r.below(encoded as u64) as usize;
            blob[i] = match r.below(4) {
                0 => blob[i] ^ (1 << r.below(8)),
                1 => blob[i].wrapping_add(1),
                2 => blob[i].wrapping_sub(1),
                _ => r.next() as u8,
            };
        }
        tried += 1;
        let Some(nb) = catch_unwind(|| Battle::from_bytes(&blob)).expect("from_bytes panicked") else { continue };
        accepted += 1;
        if !steps_without_panic(nb, round, 40) {
            panicked += 1;
            first_panic.get_or_insert(round);
        }
    }
    // Fully random blobs are rejected without panicking.
    for _ in 0..2000 {
        let blob: Vec<u8> = (0..sim_gen1::ser::BATTLE_BYTES).map(|_| r.next() as u8).collect();
        assert!(catch_unwind(|| Battle::from_bytes(&blob)).expect("from_bytes panicked").is_none());
    }
    let _ = std::panic::take_hook();
    eprintln!("blob fuzz: {tried} corruptions, {accepted} accepted, {panicked} panicked");
    assert_eq!(panicked, 0, "accepted blobs panicked when stepped (first at round {first_panic:?})");
    assert!(accepted > 1000, "fuzzing accepted only {accepted} blobs");
    assert!(Battle::from_bytes(&[]).is_none());
    assert!(Battle::from_bytes(&bytes(&states[0])[..100]).is_none());
}
