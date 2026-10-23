//! Env-level tests of the native backends (mirrors tests/rust/env.rs for the pkmn backend).
#![allow(clippy::needless_range_loop)]

use kanto_env::backend::{Action, Backend, MAX_CHOICES, NUM_ACTIONS};
use kanto_env::env::{outcome, Env, EnvConfig};
use kanto_env::memory::Memory;
use kanto_env::obs::OBS_DIM;
use kanto_env::rng::Rng;
use kanto_env::team::{builtin_teams, TeamSource};
use kanto_env::{Opponent, StepOut, VecEnv};
use kanto_backend::{Native1, Native1Nc};
use std::path::PathBuf;

fn pool() -> TeamSource {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/teams/gen1ou");
    if dir.exists() {
        TeamSource::from_spec(dir.to_str().unwrap()).expect("team pool loads")
    } else {
        TeamSource::Pool(builtin_teams())
    }
}

fn random_actions<B: Backend>(v: &VecEnv<B>, rng: &mut Rng) -> Vec<i16> {
    let mut acts = vec![0i16; 2 * v.num_envs()];
    for (i, e) in v.envs.iter().enumerate() {
        for p in 0..2 {
            acts[2 * i + p] = kanto_env::opponent::random_legal(&e.mask[p], rng) as i16;
        }
    }
    acts
}

struct Bufs {
    obs: Vec<f32>,
    mask: Vec<bool>,
    reward: Vec<f32>,
    done: Vec<bool>,
    truncated: Vec<bool>,
    outcome: Vec<i8>,
    turns: Vec<u32>,
    opp_action: Vec<i16>,
    error: Vec<bool>,
}

impl Bufs {
    fn new(n: usize) -> Self {
        Bufs {
            obs: vec![0.0; n * 2 * OBS_DIM],
            mask: vec![false; n * 2 * NUM_ACTIONS],
            reward: vec![0.0; n * 2],
            done: vec![false; n],
            truncated: vec![false; n],
            outcome: vec![0; n],
            turns: vec![0; n],
            opp_action: vec![0; 2 * n],
            error: vec![false; n],
        }
    }
    fn out(&mut self) -> StepOut<'_> {
        StepOut {
            obs: &mut self.obs,
            mask: &mut self.mask,
            reward: &mut self.reward,
            done: &mut self.done,
            truncated: &mut self.truncated,
            outcome: &mut self.outcome,
            turns: &mut self.turns,
            opp_action: &mut self.opp_action,
            error: &mut self.error,
            final_obs: None,
        }
    }
}

fn rollout_hash<B: Backend>(seed: u64, threads: usize, steps: usize) -> u64 {
    let n = 64;
    let mut v = VecEnv::<B>::new(n, seed, pool(), EnvConfig::default(), threads);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(seed ^ 0xABCD);
    let mut h = 0xcbf29ce484222325u64;
    for _ in 0..steps {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
        let bytes: Vec<u8> = b
            .obs
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .chain(b.mask.iter().map(|&m| m as u8))
            .chain(b.outcome.iter().map(|&o| o as u8))
            .collect();
        for x in bytes {
            h = (h ^ x as u64).wrapping_mul(0x100000001b3);
        }
    }
    h
}

#[test]
fn deterministic_and_thread_independent() {
    let a = rollout_hash::<Native1>(42, 1, 200);
    assert_eq!(a, rollout_hash::<Native1>(42, 4, 200));
    assert_ne!(a, rollout_hash::<Native1>(43, 4, 200));
    assert_ne!(a, rollout_hash::<Native1Nc>(42, 4, 200));
}

fn clone_restore<B: Backend>() {
    let n = 16;
    let mut v = VecEnv::<B>::new(n, 7, pool(), EnvConfig::default(), 2);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(1);
    for _ in 0..20 {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
    }
    let blobs: Vec<Vec<u8>> = (0..n).map(|i| v.clone_state(i)).collect();
    let saved = rng;
    let mut first = Vec::new();
    for _ in 0..50 {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
        first.push((b.obs.clone(), b.mask.clone(), b.reward.clone(), b.done.clone()));
    }
    for (i, blob) in blobs.iter().enumerate() {
        v.restore_state(i, blob, None).unwrap();
        assert_eq!(&v.clone_state(i), blob);
    }
    rng = saved;
    for f in &first {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
        assert_eq!(f.0, b.obs);
        assert_eq!(f.1, b.mask);
        assert_eq!(f.2, b.reward);
        assert_eq!(f.3, b.done);
    }
    assert!(v.restore_state(0, &[1, 2, 3], None).is_err());
    let mut bad = blobs[0].clone();
    bad[100] = 0xFF;
    // Corrupting a byte must either be rejected or decode to *some* valid battle.
    let _ = v.restore_state(0, &bad, None);
}

#[test]
fn clone_restore_round_trip() {
    clone_restore::<Native1>();
    clone_restore::<Native1Nc>();
}

/// `restore_state(.., Some(seed))`: one seed replays one future, different seeds sample different
/// ones (the action sequence is a function of the states, so only chance makes them differ).
#[test]
fn restore_with_seed_samples_independent_futures() {
    let n = 32;
    let mut v = VecEnv::<Native1>::new(n, 5, pool(), EnvConfig::default(), 2);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(3);
    for _ in 0..10 {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
    }
    let blob = v.clone_state(0);
    let play = |v: &mut VecEnv<Native1>, b: &mut Bufs, seed: Option<u64>| -> Vec<u8> {
        for i in 0..n {
            v.restore_state(i, &blob, seed).unwrap();
        }
        let mut r = Rng::new(9);
        for _ in 0..8 {
            let acts = random_actions(v, &mut r);
            v.step(&acts, b.out()).unwrap();
        }
        v.clone_state(0)
    };
    let a = play(&mut v, &mut b, Some(100));
    assert_eq!(play(&mut v, &mut b, Some(100)), a, "same seed, same future");
    let futures: std::collections::HashSet<Vec<u8>> = (0..8u64).map(|s| play(&mut v, &mut b, Some(1000 * s))).collect();
    assert!(futures.len() > 1, "different seeds should not all replay one future");
    let none = play(&mut v, &mut b, None);
    assert_eq!(play(&mut v, &mut b, None), none, "without a seed a restore replays the blob's stream");
}

fn terminate<B: Backend>() {
    let n = 128;
    let mut v = VecEnv::<B>::new(n, 11, TeamSource::RandomLegal, EnvConfig::default(), 4);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(2);
    let mut finished = 0;
    let mut counts = [0usize; 5];
    for _ in 0..1500 {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
        for i in 0..n {
            if b.done[i] {
                finished += 1;
                counts[b.outcome[i] as usize] += 1;
                assert!(b.turns[i] > 0 && b.turns[i] <= kanto_env::MAX_TURNS);
            } else {
                assert_eq!(b.outcome[i], outcome::ONGOING);
            }
        }
    }
    assert!(finished > n, "only {finished} games finished");
    assert!(counts[1] > 0 && counts[2] > 0);
}

#[test]
fn random_games_terminate() {
    terminate::<Native1>();
    terminate::<Native1Nc>();
}

#[test]
fn max_damage_beats_random() {
    let n = 128;
    let cfg = EnvConfig { opponent: Some(Opponent::MaxDamage), ..Default::default() };
    let mut v = VecEnv::<Native1>::new(n, 5, pool(), cfg, 2);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(9);
    let (mut wins, mut games) = (0, 0);
    for _ in 0..600 {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
        for i in 0..n {
            if b.done[i] {
                games += 1;
                wins += (b.reward[2 * i] > 0.0) as usize;
            }
        }
    }
    assert!(games > 50);
    assert!((wins as f64) < 0.35 * games as f64, "max_damage won only {}/{games}", games - wins);
}

/// Every legal choice maps to a distinct action, and actions refer to real moves/team slots.
#[test]
fn masks_are_consistent() {
    let source = TeamSource::RandomLegal;
    let mut rng = Rng::new(3);
    for g in 0..40 {
        let mut env = Env::<Native1>::new(1000 + g, &source);
        for _ in 0..300 {
            let view = Native1::view(&env.battle);
            for p in 0..2 {
                let mut raw = [kanto_core::Choice::Pass; MAX_CHOICES];
                let n = Native1::choices(&env.battle, p, &mut raw);
                assert!(n > 0);
                let mut seen = [false; NUM_ACTIONS];
                for &c in &raw[..n] {
                    let a = Native1::action_of(&env.battle, p, c);
                    let idx = a.index();
                    assert!(!seen[idx], "two choices map to action {idx}");
                    seen[idx] = true;
                    if let Action::Switch(k) = a {
                        assert!(k < view.sides[p].size && view.sides[p].mons[k as usize].hp > 0);
                        assert_ne!(k, view.sides[p].active.party_idx);
                    }
                }
                assert_eq!(seen, env.mask[p]);
            }
            let a1 = kanto_env::opponent::random_legal(&env.mask[0], &mut rng);
            let a2 = kanto_env::opponent::random_legal(&env.mask[1], &mut rng);
            let r = env.step(a1, a2, &EnvConfig::default(), &source);
            if r.done {
                break;
            }
        }
    }
}

#[test]
fn fresh_battle_reveals_only_leads() {
    let teams = builtin_teams();
    let mut b = Native1::new_battle(&teams[0], &teams[1], 7);
    let mut mem = [Memory::default(); 2];
    let mut out = [kanto_core::Choice::Pass; 16];
    assert_eq!(Native1::choices(&b, 0, &mut out), 1);
    Native1::update(&mut b, out[0], out[0], &mut mem);
    assert_eq!(Native1::turn(&b), 1);
    assert_eq!(mem[0].n_revealed, 1);
    assert_eq!(mem[1].n_revealed, 1);
    assert!(mem[0].last[0].switched && mem[0].last[1].switched);
    let v = Native1::view(&b);
    assert_eq!(v.sides[0].active.species, teams[0].mons[0].species);
    // Same max-stat sets give the same stats as kanto-env's calculator.
    for i in 0..teams[0].size as usize {
        assert_eq!(v.sides[0].mons[i].stats, teams[0].mons[i].stats);
    }
    assert_eq!(Native1::choices(&b, 0, &mut out), 9);
}

// ---- Robustness ----------------------------------------------------------------------------

fn crash_teams() -> TeamSource {
    // Showdown's "moveSlot is null" scenario: Mew transforms (4th move) into a 1-move
    // Jigglypuff and is put to sleep (docs/SIM_STATUS.md, "Showdown crashes").
    let text = r#"{"name":"mew","pokemon":[{"species":"Mew","moves":["Psychic","Soft-Boiled","Thunder Wave","Transform"]}]}
{"name":"jiggly","pokemon":[{"species":"Jigglypuff","moves":["Sing"]}]}"#;
    let specs = kanto_env::team::parse_teams(text).unwrap();
    TeamSource::Pool(specs.iter().map(|s| kanto_env::Team::from_spec(s).unwrap()).collect())
}

/// The auditor's repro at env level: prefer the 4th move whenever legal. Before the fix about
/// half of these games panicked; now none may.
#[test]
fn move_slot_null_scenario_runs_clean() {
    let n = 64;
    let mut v = VecEnv::<Native1>::new(n, 1, crash_teams(), EnvConfig::default(), 2);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(3);
    let mut games = 0;
    for _ in 0..1500 {
        let mut acts = random_actions(&v, &mut rng);
        for (i, e) in v.envs.iter().enumerate() {
            for p in 0..2 {
                if e.mask[p][3] {
                    acts[2 * i + p] = 3;
                }
            }
        }
        v.step(&acts, b.out()).unwrap();
        assert!(v.errors.is_empty(), "simulator panicked: {:?}", v.errors);
        games += b.done.iter().filter(|&&d| d).count();
    }
    assert!(games > 100, "only {games} games finished");
    assert_eq!(v.total_errors, 0);
}

/// A panic inside one env's step ends that game as ERROR, resets the env, and leaves every
/// other env exactly as an un-poisoned run would.
#[test]
fn panics_are_isolated_per_env() {
    let n = 48;
    let poisoned = [7usize, 31];
    let mut a = VecEnv::<Native1>::new(n, 5, pool(), EnvConfig::default(), 4);
    let mut r = VecEnv::<Native1>::new(n, 5, pool(), EnvConfig::default(), 4);
    let (mut ba, mut br) = (Bufs::new(n), Bufs::new(n));
    a.reset(None, &mut ba.obs, &mut ba.mask);
    r.reset(None, &mut br.obs, &mut br.mask);
    let mut rng = Rng::new(9);
    for t in 0..60 {
        let acts = random_actions(&r, &mut rng);
        if t == 10 {
            for &i in &poisoned {
                a.envs[i].inject_panic = true;
            }
        }
        // After the poisoned step the poisoned envs play their own (new) games.
        let mut acts_a = acts.clone();
        if t > 10 {
            let mut rng2 = Rng::new(t as u64);
            for &i in &poisoned {
                for p in 0..2 {
                    acts_a[2 * i + p] = kanto_env::opponent::random_legal(&a.envs[i].mask[p], &mut rng2) as i16;
                }
            }
        }
        a.step(&acts_a, ba.out()).unwrap();
        r.step(&acts, br.out()).unwrap();
        if t == 10 {
            assert_eq!(a.errors.iter().map(|e| e.0).collect::<Vec<_>>(), poisoned);
            assert!(a.errors[0].1.contains("injected panic"));
            for &i in &poisoned {
                assert!(ba.error[i] && ba.done[i] && ba.truncated[i]);
                assert_eq!(ba.outcome[i], outcome::ERROR);
                assert_eq!(ba.reward[2 * i], 0.0);
                // The env was reset: a fresh game at turn 1 with a usable mask.
                assert_eq!(Native1::turn(&a.envs[i].battle), 1);
                assert!(ba.mask[i * 2 * NUM_ACTIONS..(i * 2 + 1) * NUM_ACTIONS].iter().any(|&m| m));
            }
        } else {
            assert!(a.errors.is_empty() && !ba.error.iter().any(|&e| e));
        }
        for i in (0..n).filter(|i| !poisoned.contains(i)) {
            assert_eq!(a.clone_state(i), r.clone_state(i), "env {i} diverged at step {t}");
            assert_eq!(ba.obs[i * 2 * OBS_DIM..(i + 1) * 2 * OBS_DIM], br.obs[i * 2 * OBS_DIM..(i + 1) * 2 * OBS_DIM]);
            assert_eq!(ba.done[i], br.done[i]);
        }
    }
    assert_eq!(a.total_errors, 2);
}

/// With a built-in opponent, player 2's observation and mask rows are zero.
#[test]
fn builtin_opponent_rows_are_zero() {
    let n = 32;
    let cfg = EnvConfig { opponent: Some(Opponent::MaxDamage), ..EnvConfig::default() };
    let mut v = VecEnv::<Native1>::new(n, 2, pool(), cfg, 2);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(1);
    for _ in 0..50 {
        for i in 0..n {
            assert!(b.obs[(2 * i + 1) * OBS_DIM..(2 * i + 2) * OBS_DIM].iter().all(|&x| x == 0.0));
            assert!(b.mask[(2 * i + 1) * NUM_ACTIONS..(2 * i + 2) * NUM_ACTIONS].iter().all(|&m| !m));
            assert!(b.mask[2 * i * NUM_ACTIONS..(2 * i + 1) * NUM_ACTIONS].iter().any(|&m| m));
        }
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
    }
}

/// `opp_action[i, p]` is player 1-p's choice in p's frame: the chosen move's id (or 0 for
/// unnamed forced moves), 166 + appearance slot for switches, -1 for a pass.
#[test]
fn opponent_action_labels() {
    use kanto_env::env::opp_action;
    let n = 64;
    let mut v = VecEnv::<Native1>::new(n, 4, pool(), EnvConfig::default(), 4);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(17);
    let (mut moves, mut unnamed, mut switches, mut passes) = (0, 0, 0, 0);
    for _ in 0..400 {
        let acts = random_actions(&v, &mut rng);
        let before: Vec<Env<Native1>> = v.envs.clone();
        v.step(&acts, b.out()).unwrap();
        for i in 0..n {
            for p in 0..2 {
                let q = 1 - p;
                let label = b.opp_action[2 * i + p];
                let e = &before[i];
                let a = acts[2 * i + q] as usize;
                let view = Native1::view(&e.battle);
                match Native1::action_of(&e.battle, q, e.table[q][a]) {
                    Action::Pass => {
                        assert_eq!(label, opp_action::NONE);
                        passes += 1;
                    }
                    Action::Move(k) => {
                        assert!((0..=165).contains(&label), "move label {label}");
                        let n_legal = e.mask[q].iter().filter(|&&m| m).count();
                        if label > 0 && label != 165 && n_legal > 1 && k > 0 {
                            assert_eq!(view.sides[q].active.moves[k as usize] as i16, label, "env {i}");
                        }
                        moves += 1;
                        unnamed += (label == 0) as usize;
                    }
                    Action::Switch(slot) => {
                        assert!((opp_action::SWITCH_BASE..opp_action::CLASSES).contains(&label));
                        if !b.done[i] {
                            let mem = &v.envs[i].mem[p];
                            assert_eq!(mem.slot_of[slot as usize] as i16, label - opp_action::SWITCH_BASE);
                        }
                        switches += 1;
                    }
                }
            }
        }
    }
    assert!(moves > 1000 && switches > 100 && passes > 10 && unnamed > 10, "moves {moves} ({unnamed} unnamed) switches {switches} passes {passes}");
}

/// Env-level `clone_state` blobs (battle + reveal memories + RNG): random corruptions are either
/// rejected or restore to an env that steps and observes without panicking.
#[test]
fn corrupted_env_blobs_are_rejected_or_safe() {
    let n = 32;
    let mut v = VecEnv::<Native1>::new(n, 8, pool(), EnvConfig::default(), 2);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(5);
    let mut blobs = Vec::new();
    for t in 0..120 {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
        if t % 10 == 0 {
            blobs.extend((0..n).map(|i| v.clone_state(i)));
        }
    }
    let cfg = EnvConfig::default();
    let source = pool();
    let (mut accepted, mut panicked) = (0, 0);
    for round in 0..20_000u64 {
        let mut blob = blobs[rng.below(blobs.len() as u64) as usize].clone();
        for _ in 0..1 + rng.below(3) {
            let i = rng.below(blob.len() as u64) as usize;
            blob[i] ^= 1 << rng.below(8);
        }
        let restored = std::panic::catch_unwind(|| Env::<Native1>::from_bytes(&blob)).expect("from_bytes panicked");
        let Some(env) = restored else { continue };
        accepted += 1;
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut env = env;
            let mut r = Rng::new(round);
            let mut obs = vec![0f32; OBS_DIM];
            for _ in 0..30 {
                env.observe(0, &cfg, &mut obs);
                env.observe(1, &cfg, &mut obs);
                let a1 = kanto_env::opponent::random_legal(&env.mask[0], &mut r);
                let a2 = kanto_env::opponent::random_legal(&env.mask[1], &mut r);
                env.step(a1, a2, &cfg, &source);
            }
        }));
        panicked += ok.is_err() as usize;
    }
    assert_eq!(panicked, 0, "{panicked} of {accepted} accepted env blobs panicked");
    assert!(accepted > 1000, "only {accepted} accepted");
}

/// `ProtoGame` (protocol output + env observations) plays exactly the env's game: same masks and
/// same observations after every step, for the same teams, seed and actions.
#[test]
fn proto_game_matches_env() {
    let source = pool();
    let cfg = EnvConfig::default();
    for seed in 0..40u64 {
        let mut env = Env::<Native1>::new(seed, &source);
        // Env::new: two team samples, then the battle seed, from Rng::new(seed).
        let mut rng = Rng::new(seed);
        let (p1, p2) = (source.sample(&mut rng), source.sample(&mut rng));
        let b = kanto_backend::new_battle(&p1, &p2, rng.next_u64(), kanto_gen1::Rules::OU);
        let mut g = kanto_backend::ProtoGame::new(b, ["p1", "p2"]);
        g.start();
        let mut arng = Rng::new(seed ^ 0xabc);
        let (mut o1, mut o2) = (vec![0f32; OBS_DIM], vec![0f32; OBS_DIM]);
        for _ in 0..400 {
            for p in 0..2 {
                assert_eq!(env.mask[p], g.mask[p], "seed {seed}: masks differ");
                env.observe(p, &cfg, &mut o1);
                g.observe(p, &mut o2);
                assert_eq!(o1, o2, "seed {seed}: observations differ for player {p}");
                let (mut s1, mut m1, mut s2, mut m2) = ([0u8; 6], [0u8; 24], [0u8; 6], [0u8; 24]);
                env.opp_team_targets(p, &mut s1, &mut m1);
                g.opp_team_targets(p, &mut s2, &mut m2);
                assert_eq!((s1, m1), (s2, m2), "seed {seed}: hidden-team targets differ for player {p}");
            }
            let a1 = kanto_env::opponent::random_legal(&env.mask[0], &mut arng);
            let a2 = kanto_env::opponent::random_legal(&env.mask[1], &mut arng);
            let r = env.step(a1, a2, &cfg, &source);
            let out = g.step_actions(a1, a2).unwrap();
            assert_eq!(r.opp_action, g.opp_action, "seed {seed}: opponent-action labels differ");
            if r.done {
                assert!(g.battle().ended || g.battle().turn as u32 >= cfg.max_turns);
                assert!(out.requests[0].is_none() || !g.battle().ended);
                break;
            }
        }
    }
}

fn step_out<'a>(
    obs: &'a mut [f32], mask: &'a mut [bool], reward: &'a mut [f32], done: &'a mut [bool], truncated: &'a mut [bool],
    outcome: &'a mut [i8], turns: &'a mut [u32], opp_action: &'a mut [i16], error: &'a mut [bool],
) -> StepOut<'a> {
    StepOut { obs, mask, reward, done, truncated, outcome, turns, opp_action, error, final_obs: None }
}

/// `lookahead` plays legal moves, and as player 2 beats `max_damage` (player 1) more often than not.
#[test]
fn lookahead_beats_max_damage() {
    let n = 64;
    let cfg = EnvConfig { opponent: Some(Opponent::Lookahead), ..Default::default() };
    let mut v = VecEnv::<Native1>::new(n, 11, pool(), cfg, 4);
    let mut obs = vec![0f32; n * 2 * OBS_DIM];
    let mut mask = vec![false; n * 2 * NUM_ACTIONS];
    v.reset(None, &mut obs, &mut mask);
    let (mut reward, mut done, mut trunc) = (vec![0f32; 2 * n], vec![false; n], vec![false; n]);
    let (mut oc, mut turns, mut opp, mut err) = (vec![0i8; n], vec![0u32; n], vec![0i16; 2 * n], vec![false; n]);
    let mut finished = vec![false; n];
    let (mut wins, mut losses) = (0, 0);
    let seats = vec![0u8; n];
    for _ in 0..3000 {
        let mut acts16 = vec![0i16; n];
        v.builtin_actions(Opponent::MaxDamage, &seats, &mut acts16);
        let acts: Vec<i16> = acts16.iter().flat_map(|&a| [a, 0]).collect();
        let out = step_out(&mut obs, &mut mask, &mut reward, &mut done, &mut trunc, &mut oc, &mut turns, &mut opp, &mut err);
        v.step(&acts, out).unwrap();
        for i in 0..n {
            if done[i] && !finished[i] {
                finished[i] = true;
                wins += (oc[i] == outcome::P2_WIN) as usize;
                losses += (oc[i] == outcome::P1_WIN) as usize;
            }
        }
        if finished.iter().all(|&f| f) {
            break;
        }
    }
    eprintln!("lookahead (p2) vs max_damage: W{wins} L{losses} of {n}");
    assert!(wins > losses, "lookahead should beat max_damage");
}

/// Hidden-team targets: revealed slots match the observation's species ids and the true moves,
/// and the whole target is a permutation of the opponent's true team.
#[test]
fn opp_team_targets_match_observation_and_team() {
    let n = 16;
    let mut v = VecEnv::<Native1>::new(n, 3, pool(), EnvConfig::default(), 2);
    let mut obs = vec![0f32; n * 2 * OBS_DIM];
    let mut mask = vec![false; n * 2 * NUM_ACTIONS];
    v.reset(None, &mut obs, &mut mask);
    let mut rng = Rng::new(1);
    let opp_mon = kanto_env::obs::layout().iter().find(|(k, _, _)| k == "opp_mon0").unwrap().1;
    let (mut reward, mut done, mut trunc) = (vec![0f32; 2 * n], vec![false; n], vec![false; n]);
    let (mut oc, mut turns, mut opp, mut err) = (vec![0i8; n], vec![0u32; n], vec![0i16; 2 * n], vec![false; n]);
    let mut checked = 0;
    for _ in 0..60 {
        let mut sp = vec![0u8; n * 12];
        let mut mv = vec![0u8; n * 48];
        v.opp_team_targets(&mut sp, &mut mv);
        for i in 0..n {
            for p in 0..2 {
                let view = Native1::view(&v.envs[i].battle);
                let side = &view.sides[1 - p];
                let mut want: Vec<u8> = side.mons[..side.size as usize].iter().map(|m| m.species).collect();
                let mut got: Vec<u8> = sp[12 * i + 6 * p..12 * i + 6 * p + 6].iter().copied().filter(|&s| s != 0).collect();
                want.sort();
                got.sort();
                assert_eq!(want, got);
                let o = &obs[(2 * i + p) * OBS_DIM..];
                for j in 0..v.envs[i].mem[p].n_revealed as usize {
                    assert_eq!(o[opp_mon + 49 * j] as u8, sp[12 * i + 6 * p + j]);
                    let k = v.envs[i].mem[p].reveal_order[j] as usize;
                    assert_eq!(&mv[48 * i + 24 * p + 4 * j..48 * i + 24 * p + 4 * j + 4], &side.mons[k].moves[..]);
                    checked += 1;
                }
            }
        }
        let acts = random_actions(&v, &mut rng);
        let out = step_out(&mut obs, &mut mask, &mut reward, &mut done, &mut trunc, &mut oc, &mut turns, &mut opp, &mut err);
        v.step(&acts, out).unwrap();
    }
    assert!(checked > 0);
}

/// Reset stops after the leads are sent out with 1-Pokémon, 1-move teams (only the pass choice
/// counts as a pass, not "only action 0 legal").
#[test]
fn reset_plays_no_turns_with_one_move_native() {
    use kanto_env::team::{Mon, Team};
    let t1 = Team::new(&[Mon::new(143, &[33], 100, [15; 4], [65535; 5])]);
    let t2 = Team::new(&[Mon::new(113, &[1], 100, [15; 4], [65535; 5])]);
    let src = TeamSource::Pool(vec![t1, t2]);
    for seed in 0..20 {
        let mut env = Env::<Native1>::new(seed, &src);
        for _ in 0..2 {
            assert_eq!(Native1::turn(&env.battle), 1, "seed {seed}: reset played real turns");
            assert_eq!(env.steps, 0);
            for p in 0..2 {
                assert!(env.mask[p][0] && env.mask[p][1..].iter().all(|&x| !x));
                assert_eq!(Native1::action_of(&env.battle, p, env.table[p][0]), Action::Move(0));
            }
            env.reset(&src);
        }
    }
}
