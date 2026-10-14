//! Integration tests for battle-env (registered as a test target of crates/battle-env).
#![allow(clippy::needless_range_loop)]

use battle_env::backend::{Action, Backend, MAX_CHOICES, NUM_ACTIONS};
use battle_env::env::{outcome, Env, EnvConfig};
use battle_env::memory::Memory;
use battle_env::obs::{self, OBS_DIM};
use battle_env::pkmn1::{encode_battle, Gen1Battle, Pkmn1, LOG_BUF};
use battle_env::protocol;
use battle_env::rng::Rng;
use battle_env::team::{builtin_teams, random_legal_team, Team, TeamSource};
use battle_env::{Opponent, StepOut, VecEnv};
use std::path::PathBuf;
use std::process::Command;

fn pool() -> TeamSource {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/teams/gen1ou");
    if dir.exists() {
        TeamSource::from_spec(dir.to_str().unwrap()).expect("team pool loads")
    } else {
        TeamSource::Pool(builtin_teams())
    }
}

fn random_actions(v: &VecEnv<Pkmn1>, rng: &mut Rng) -> Vec<i16> {
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
        }
    }
}

/// Run `steps` random steps; returns a hash of all outputs.
fn rollout_hash(seed: u64, threads: usize, steps: usize) -> u64 {
    let n = 64;
    let mut v = VecEnv::<Pkmn1>::new(n, seed, pool(), EnvConfig::default(), threads);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(seed ^ 0xABCD);
    let mut h = 0xcbf29ce484222325u64;
    let mut mix = |bytes: &[u8]| {
        for &x in bytes {
            h = (h ^ x as u64).wrapping_mul(0x100000001b3);
        }
    };
    for _ in 0..steps {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
        for f in &b.obs {
            mix(&f.to_le_bytes());
        }
        mix(&b.mask.iter().map(|&m| m as u8).collect::<Vec<_>>());
        for f in &b.reward {
            mix(&f.to_le_bytes());
        }
        mix(&b.outcome.iter().map(|&o| o as u8).collect::<Vec<_>>());
    }
    h
}

#[test]
fn same_seed_same_trajectory_independent_of_threads() {
    let a = rollout_hash(42, 1, 300);
    assert_eq!(a, rollout_hash(42, 1, 300));
    assert_eq!(a, rollout_hash(42, 4, 300));
    assert_ne!(a, rollout_hash(43, 4, 300));
}

#[test]
fn clone_restore_round_trip() {
    let n = 16;
    let mut v = VecEnv::<Pkmn1>::new(n, 7, pool(), EnvConfig::default(), 2);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(1);
    for _ in 0..20 {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
    }
    let blobs: Vec<Vec<u8>> = (0..n).map(|i| v.clone_state(i)).collect();
    let rng_saved = rng;
    let mut first = Vec::new();
    for _ in 0..50 {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
        first.push((b.obs.clone(), b.mask.clone(), b.reward.clone(), b.done.clone()));
    }
    for (i, blob) in blobs.iter().enumerate() {
        v.restore_state(i, blob).unwrap();
        assert_eq!(&v.clone_state(i), blob, "blob round-trip");
    }
    rng = rng_saved;
    for f in &first {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
        assert_eq!(f.0, b.obs);
        assert_eq!(f.1, b.mask);
        assert_eq!(f.2, b.reward);
        assert_eq!(f.3, b.done);
    }
    assert!(v.restore_state(0, &[1, 2, 3]).is_err());
}

#[test]
fn masks_agree_with_engine_choices() {
    let source = pool();
    let mut rng = Rng::new(3);
    let cfg = EnvConfig::default();
    for g in 0..300 {
        let mut env = Env::<Pkmn1>::new(1000 + g, &source);
        loop {
            for p in 0..2 {
                let mut raw = [0u8; MAX_CHOICES];
                let n = Pkmn1::choices(&env.battle, p, &mut raw);
                let mut expected = [false; NUM_ACTIONS];
                let view = Pkmn1::view(&env.battle);
                for &c in &raw[..n] {
                    let a = Pkmn1::action_of(&env.battle, p, c);
                    assert!(!expected[a.index()], "duplicate action");
                    expected[a.index()] = true;
                    assert_eq!(env.table[p][a.index()], c);
                    if let Action::Switch(k) = a {
                        let side = &view.sides[p];
                        assert!(view.sides[p].mons[k as usize].hp > 0, "switch to fainted");
                        assert_ne!(side.active.party_idx, k, "switch to active");
                    }
                }
                assert_eq!(expected, env.mask[p]);
                assert!(env.mask[p].iter().any(|&m| m));
            }
            let a1 = battle_env::opponent::random_legal(&env.mask[0], &mut rng);
            let a2 = battle_env::opponent::random_legal(&env.mask[1], &mut rng);
            if env.step(a1, a2, &cfg, &source).done {
                break;
            }
        }
    }
}

#[test]
fn random_games_terminate() {
    let n = 256;
    let mut v = VecEnv::<Pkmn1>::new(n, 11, TeamSource::RandomLegal, EnvConfig::default(), 4);
    let mut b = Bufs::new(n);
    v.reset(None, &mut b.obs, &mut b.mask);
    let mut rng = Rng::new(2);
    let mut finished = 0;
    let mut counts = [0usize; 5];
    for _ in 0..2000 {
        let acts = random_actions(&v, &mut rng);
        v.step(&acts, b.out()).unwrap();
        for i in 0..n {
            if b.done[i] {
                finished += 1;
                counts[b.outcome[i] as usize] += 1;
                assert!(b.turns[i] > 0 && b.turns[i] <= battle_env::MAX_TURNS);
                assert_eq!(b.reward[2 * i], -b.reward[2 * i + 1]);
            } else {
                assert_eq!(b.outcome[i], outcome::ONGOING);
            }
        }
    }
    assert!(finished > n, "only {finished} games finished");
    assert!(counts[1] > 0 && counts[2] > 0);
    eprintln!("outcomes [_, p1, p2, tie, trunc] = {counts:?}");
}

#[test]
fn built_in_opponents_play() {
    for opp in [Opponent::Random, Opponent::MaxDamage] {
        let n = 128;
        let cfg = EnvConfig { opponent: Some(opp), ..Default::default() };
        let mut v = VecEnv::<Pkmn1>::new(n, 5, pool(), cfg, 2);
        let mut b = Bufs::new(n);
        v.reset(None, &mut b.obs, &mut b.mask);
        let mut rng = Rng::new(9);
        let mut wins = 0;
        let mut games = 0;
        for _ in 0..600 {
            let acts = random_actions(&v, &mut rng);
            v.step(&acts, b.out()).unwrap();
            for i in 0..n {
                assert!(b.obs[(2 * i + 1) * OBS_DIM..(2 * i + 2) * OBS_DIM].iter().all(|&x| x == 0.0));
                if b.done[i] {
                    games += 1;
                    wins += (b.reward[2 * i] > 0.0) as usize;
                }
            }
        }
        eprintln!("{opp:?}: random policy won {wins}/{games}");
        if opp == Opponent::MaxDamage {
            assert!((wins as f64) < 0.35 * games as f64, "max_damage should beat random");
        }
    }
}

#[test]
fn illegal_actions_rejected_without_side_effects() {
    let mut v = VecEnv::<Pkmn1>::new(4, 1, pool(), EnvConfig::default(), 1);
    let mut b = Bufs::new(4);
    v.reset(None, &mut b.obs, &mut b.mask);
    let before: Vec<Vec<u8>> = (0..4).map(|i| v.clone_state(i)).collect();
    let mut acts = vec![0i16; 8];
    acts[5] = 99;
    let err = v.step(&acts, b.out()).unwrap_err();
    let battle_env::StepError::Illegal(err) = err else { panic!("expected an illegal-action error") };
    assert_eq!((err.env, err.player), (2, 1));
    for i in 0..4 {
        assert_eq!(v.clone_state(i), before[i]);
    }
}

// ---- Observation / hidden information -------------------------------------------------------

fn fresh(seed: u64) -> (Gen1Battle, [Memory; 2], Team, Team) {
    let teams = builtin_teams();
    let (p1, p2) = (teams[0], teams[1]);
    let mut b = Pkmn1::new_battle(&p1, &p2, seed);
    let mut mem = [Memory::default(); 2];
    Pkmn1::update(&mut b, 0, 0, &mut mem);
    (b, mem, p1, p2)
}

#[test]
fn fresh_battle_reveals_only_opponent_lead() {
    let (b, mem, _p1, p2) = fresh(5);
    let mut o = vec![0f32; OBS_DIM];
    Pkmn1::observe(&b, 0, &mem[0], false, &mut o);
    let opp = |j: usize, f: usize| o[obs::OFF_OPP + j * obs::OPP_MON_DIM + f];
    // Slot 0: the lead, species only (no moves yet), full HP shown as 100%.
    assert_eq!(opp(0, obs::opp::SPECIES), p2.mons[0].species as f32);
    assert_eq!(opp(0, obs::opp::REVEALED), 1.0);
    assert_eq!(opp(0, obs::opp::HP_PCT), 1.0);
    for k in 0..4 {
        assert_eq!(opp(0, obs::opp::MOVES + 3 * k), obs::MOVE_UNKNOWN as f32);
    }
    assert_eq!(opp(0, obs::opp::N_MOVES), 0.0);
    // Slots 1..5: unknown species, everything else zero.
    for j in 1..6 {
        assert_eq!(opp(j, obs::opp::SPECIES), obs::SPECIES_UNKNOWN as f32);
        for f in 1..obs::OPP_MON_DIM {
            assert_eq!(opp(j, f), 0.0, "slot {j} field {f}");
        }
    }
    // No other party species id appears anywhere in the opponent blocks.
    let opp_block = &o[obs::OFF_OPP..obs::OFF_OPP + 6 * obs::OPP_MON_DIM];
    for m in &p2.mons[1..] {
        if m.species != p2.mons[0].species {
            for j in 0..6 {
                assert_ne!(opp_block[j * obs::OPP_MON_DIM + obs::opp::SPECIES], m.species as f32);
            }
        }
    }
    // The privileged block is zero without reveal_hidden.
    assert!(o[obs::OFF_PRIVILEGED..obs::OFF_PRIVILEGED + obs::PRIVILEGED_DIM].iter().all(|&x| x == 0.0));
}

/// Invariant 1 of OBSERVATION_RULES.md: mutating hidden opponent fields does not change the
/// observation.
#[test]
fn observation_invariant_to_hidden_opponent_fields() {
    let (b, mem, _, _) = fresh(9);
    let mut o1 = vec![0f32; OBS_DIM];
    Pkmn1::observe(&b, 0, &mem[0], false, &mut o1);
    let mut b2 = b;
    let side = pkmn_sys::layout::SIDE_SIZE;
    // Opponent party member 3 (unrevealed): species, moves and stats.
    let o = side + 3 * pkmn_sys::layout::POKEMON_SIZE;
    b2.raw.bytes[o + pkmn_sys::layout::POKEMON_SPECIES] = 113; // Chansey
    b2.raw.bytes[o + pkmn_sys::layout::POKEMON_MOVES] = 1;
    b2.raw.bytes[o + pkmn_sys::layout::POKEMON_STATS + 2] ^= 0x11;
    // Opponent lead: unrevealed moves, exact stats and (same percentage bucket) exact HP.
    let lead = side;
    b2.raw.bytes[lead + pkmn_sys::layout::POKEMON_MOVES + 2] = 2;
    b2.raw.bytes[lead + pkmn_sys::layout::POKEMON_STATS + 4] ^= 0x03;
    let mut o2 = vec![0f32; OBS_DIM];
    Pkmn1::observe(&b2, 0, &mem[0], false, &mut o2);
    assert_eq!(o1, o2);
}

/// Invariants 2 and 3: revealed opponent moves are real (outside Transform) and use counts never
/// exceed actual PP consumption.
#[test]
fn revealed_moves_are_real_and_uses_bounded() {
    let source = TeamSource::RandBats;
    let cfg = EnvConfig::default();
    let mut rng = Rng::new(4);
    for g in 0..300 {
        let mut env = Env::<Pkmn1>::new(77 + g, &source);
        loop {
            let view = Pkmn1::view(&env.battle);
            for p in 0..2 {
                let mem = &env.mem[p];
                let foe = &view.sides[1 - p];
                for idx in 0..6u8 {
                    if !mem.is_revealed(idx) {
                        continue;
                    }
                    let m = &foe.mons[idx as usize];
                    for k in 0..mem.n_moves(idx) {
                        let mv = mem.moves[idx as usize][k];
                        let slot = m.moves.iter().position(|&x| x == mv);
                        assert!(slot.is_some(), "revealed move {mv} not in real set");
                        let used = battle_env::data::max_pp(mv) - m.pp[slot.unwrap()];
                        // The active's PP lives on the active struct and syncs on switch-out.
                        // Two-turn moves are logged on the charge turn but the engine deducts PP
                        // when the attack executes, so an interrupted charge counts as a use.
                        let info = &battle_env::data::MOVES[mv as usize];
                        let charge = battle_env::data::EFFECT_NAMES[info.effect as usize] == "Charge";
                        if foe.active.party_idx != idx && !charge {
                            assert!(
                                mem.move_uses[idx as usize][k] <= used,
                                "uses exceed PP consumed: mv {} uses {} used {} game {g} turn {}",
                                battle_env::data::MOVE_NAMES[mv as usize],
                                mem.move_uses[idx as usize][k],
                                used,
                                view.turn
                            );
                        }
                    }
                }
            }
            let a1 = battle_env::opponent::random_legal(&env.mask[0], &mut rng);
            let a2 = battle_env::opponent::random_legal(&env.mask[1], &mut rng);
            if env.step(a1, a2, &cfg, &source).done {
                break;
            }
        }
    }
}

/// Every log the engine writes parses with our message-size table and ends at its terminator.
#[test]
fn protocol_parser_consumes_whole_logs() {
    let mut rng = Rng::new(8);
    for g in 0..500 {
        let (p1, p2) = (random_legal_team(&mut rng), random_legal_team(&mut rng));
        let mut raw = encode_battle(&p1, &p2, g);
        let (mut c1, mut c2) = (0u8, 0u8);
        loop {
            let mut buf = [0u8; LOG_BUF];
            let mut opts = pkmn_sys::pkmn_gen1_battle_options::default();
            let log = pkmn_sys::pkmn_gen1_log_options { buf: buf.as_mut_ptr(), len: LOG_BUF };
            let r = unsafe {
                pkmn_sys::pkmn_gen1_battle_options_set(&mut opts, &log, std::ptr::null(), std::ptr::null());
                pkmn_sys::pkmn_gen1_battle_update(&mut raw, c1, c2, &mut opts)
            };
            let len = protocol::log_len(&buf).expect("log parses to terminator");
            assert!(len > 0 && len <= 180);
            if pkmn_sys::bits::result_kind(r) != 0 {
                break;
            }
            let b = Gen1Battle { raw, result: r };
            let mut ch = [0u8; MAX_CHOICES];
            let n = Pkmn1::choices(&b, 0, &mut ch);
            c1 = ch[rng.below(n as u64) as usize];
            let n = Pkmn1::choices(&b, 1, &mut ch);
            c2 = ch[rng.below(n as u64) as usize];
        }
    }
}

// ---- Cross-check against a C driver linked to the same libpkmn -------------------------------

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &x in bytes {
        h = (h ^ x as u64).wrapping_mul(0x100000001b3);
    }
    h
}

#[test]
fn cross_check_against_c_driver() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let exe = std::env::temp_dir().join(format!("pkmn_driver_{}", std::process::id()));
    let status = Command::new("cc")
        .args(["-O2", "-std=c99", "-o"])
        .arg(&exe)
        .arg(root.join("tests/c/driver.c"))
        .arg(format!("-I{}", pkmn_sys::INCLUDE_DIR))
        .arg(format!("{}/libpkmn-showdown.a", pkmn_sys::LIB_DIR))
        .status();
    match status {
        Ok(s) if s.success() => {}
        other => {
            eprintln!("skipping cross-check: could not build C driver ({other:?})");
            return;
        }
    }
    let source = pool();
    for seed in 0..20u64 {
        // Our env path: build the battle via Env, then drive it through the action table using
        // the same PSRNG choice rule as the C driver.
        let mut env_rng = Rng::new(seed);
        let (p1, p2) = (source.sample(&mut env_rng), source.sample(&mut env_rng));
        let battle = Pkmn1::new_battle(&p1, &p2, 1000 + seed);
        let hex: String = battle.raw.bytes.iter().map(|b| format!("{b:02x}")).collect();

        let mut env = Env::<Pkmn1> {
            battle,
            mem: [Memory::default(); 2],
            rng: Rng::new(0),
            table: [[0; NUM_ACTIONS]; 2],
            mask: [[false; NUM_ACTIONS]; 2],
            steps: 0,
            inject_panic: false,
        };
        let mut psrng = pkmn_sys::pkmn_psrng::default();
        unsafe { pkmn_sys::pkmn_psrng_init(&mut psrng, seed) };
        let mut h = 0xcbf29ce484222325u64;
        let mut c = (0u8, 0u8);
        let result;
        loop {
            let mut buf = [0u8; LOG_BUF];
            let mut opts = pkmn_sys::pkmn_gen1_battle_options::default();
            let log = pkmn_sys::pkmn_gen1_log_options { buf: buf.as_mut_ptr(), len: LOG_BUF };
            let r = unsafe {
                pkmn_sys::pkmn_gen1_battle_options_set(&mut opts, &log, std::ptr::null(), std::ptr::null());
                pkmn_sys::pkmn_gen1_battle_update(&mut env.battle.raw, c.0, c.1, &mut opts)
            };
            env.battle.result = r;
            h = fnv(h, &buf);
            if pkmn_sys::bits::result_kind(r) != 0 {
                result = pkmn_sys::bits::result_kind(r);
                break;
            }
            env.refresh();
            // Pick the same raw choice as the C driver, then go through the env action table.
            let mut pick = |p: usize| {
                let mut raw = [0u8; MAX_CHOICES];
                let n = Pkmn1::choices(&env.battle, p, &mut raw) as u64;
                let x = unsafe { pkmn_sys::pkmn_psrng_next(&mut psrng) } as u64;
                let chosen = raw[((x * n) >> 32) as usize];
                let a = Pkmn1::action_of(&env.battle, p, chosen).index();
                assert!(env.mask[p][a]);
                env.table[p][a]
            };
            c = (pick(0), pick(1));
        }
        let turns = Pkmn1::turn(&env.battle);
        let ours = format!("{} {} {:016x} {:016x}", result, turns, h, fnv(0xcbf29ce484222325, &env.battle.raw.bytes));

        let mut child = Command::new(&exe)
            .arg(seed.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(hex.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let theirs = String::from_utf8(out.stdout).unwrap();
        assert_eq!(ours, theirs.trim(), "seed {seed}");
    }
    let _ = std::fs::remove_file(&exe);
}

/// Reset must stop after the leads are sent out, also when a player's only choice in turn 1 is
/// a real move (1 Pokémon with 1 move): only the backend's pass choice counts as a pass.
#[test]
fn reset_plays_no_turns_with_one_move_pkmn() {
    use battle_env::team::Mon;
    // Snorlax (143) with Tackle (33) vs Chansey (113) with Pound (1): neither can faint in the
    // first few turns, so a reset that played real turns would show turn > 1.
    let t1 = Team::new(&[Mon::new(143, &[33], 100, [15; 4], [65535; 5])]);
    let t2 = Team::new(&[Mon::new(113, &[1], 100, [15; 4], [65535; 5])]);
    let src = TeamSource::Pool(vec![t1, t2]);
    for seed in 0..20 {
        let mut env = Env::<Pkmn1>::new(seed, &src);
        for round in 0..2 {
            assert_eq!(Pkmn1::turn(&env.battle), 1, "seed {seed} round {round}: reset played real turns");
            assert_eq!(env.steps, 0);
            for p in 0..2 {
                assert!(env.mask[p][0] && env.mask[p][1..].iter().all(|&x| !x));
                assert_eq!(Pkmn1::action_of(&env.battle, p, env.table[p][0]), Action::Move(0));
            }
            env.reset(&src);
        }
    }
}
