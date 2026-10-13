//! Env-level tests of the native backends (mirrors tests/rust/env.rs for the pkmn backend).
#![allow(clippy::needless_range_loop)]

use battle_env::backend::{Action, Backend, MAX_CHOICES, NUM_ACTIONS};
use battle_env::env::{outcome, Env, EnvConfig};
use battle_env::memory::Memory;
use battle_env::obs::OBS_DIM;
use battle_env::rng::Rng;
use battle_env::team::{builtin_teams, TeamSource};
use battle_env::{Opponent, StepOut, VecEnv};
use sim_backend::{Native1, Native1Ou};
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
            acts[2 * i + p] = battle_env::opponent::random_legal(&e.mask[p], rng) as i16;
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
    assert_ne!(a, rollout_hash::<Native1Ou>(42, 4, 200));
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
        v.restore_state(i, blob).unwrap();
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
    assert!(v.restore_state(0, &[1, 2, 3]).is_err());
    let mut bad = blobs[0].clone();
    bad[100] = 0xFF;
    // Corrupting a byte must either be rejected or decode to *some* valid battle.
    let _ = v.restore_state(0, &bad);
}

#[test]
fn clone_restore_round_trip() {
    clone_restore::<Native1>();
    clone_restore::<Native1Ou>();
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
                assert!(b.turns[i] > 0 && b.turns[i] <= battle_env::MAX_TURNS);
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
    terminate::<Native1Ou>();
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
                let mut raw = [sim_core::Choice::Pass; MAX_CHOICES];
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
            let a1 = battle_env::opponent::random_legal(&env.mask[0], &mut rng);
            let a2 = battle_env::opponent::random_legal(&env.mask[1], &mut rng);
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
    let mut out = [sim_core::Choice::Pass; 16];
    assert_eq!(Native1::choices(&b, 0, &mut out), 1);
    Native1::update(&mut b, out[0], out[0], &mut mem);
    assert_eq!(Native1::turn(&b), 1);
    assert_eq!(mem[0].n_revealed, 1);
    assert_eq!(mem[1].n_revealed, 1);
    assert!(mem[0].last[0].switched && mem[0].last[1].switched);
    let v = Native1::view(&b);
    assert_eq!(v.sides[0].active.species, teams[0].mons[0].species);
    // Same max-stat sets give the same stats as battle-env's calculator.
    for i in 0..teams[0].size as usize {
        assert_eq!(v.sides[0].mons[i].stats, teams[0].mons[i].stats);
    }
    assert_eq!(Native1::choices(&b, 0, &mut out), 9);
}
