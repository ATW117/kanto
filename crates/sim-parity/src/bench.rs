//! Speed benchmark (native vs pkmn/engine, single-threaded, same random games) and the fast
//! differential check against pkmn/engine.
//!
//! Both engines are driven by the same policy: a uniform choice among the *env actions*
//! (`Move(k)` / `Switch(team slot)`, sorted by action index) from a per-game choice RNG. If the
//! engines agree on mechanics and legal actions they therefore play exactly the same games, which
//! is what the differential mode checks (turn count, winner and every Pokémon's HP and status after
//! every update).

use battle_env::backend::{Action, Backend};
use battle_env::memory::Memory;
use battle_env::obs::View;
use battle_env::pkmn1::{Gen1Battle, Pkmn1};
use battle_env::rng::{derive_seed, Rng};
use battle_env::team::{Team, TeamSource};
use sim_core::Choice;
use sim_gen1::{Battle, Compat, Rules};
use std::time::Instant;

const MAX_TURNS: u32 = 1000;

/// Uniform pick among actions sorted by index; returns the chosen choice's position.
#[inline]
fn pick(rng: &mut Rng, idx: &mut [(usize, usize)]) -> usize {
    idx.sort_unstable();
    idx[rng.below(idx.len() as u64) as usize].1
}

/// (action index, choice position) pairs in a stack buffer.
struct Idx {
    n: usize,
    v: [(usize, usize); 16],
}

impl Idx {
    fn new() -> Idx {
        Idx { n: 0, v: [(0, 0); 16] }
    }
    fn push(&mut self, x: (usize, usize)) {
        self.v[self.n] = x;
        self.n += 1;
    }
    fn pick(&mut self, rng: &mut Rng) -> usize {
        pick(rng, &mut self.v[..self.n])
    }
}

/// One game on libpkmn without logging (pkmn's own fast path). Returns (turns, updates, winner).
fn play_pkmn(b: &mut Gen1Battle, seed: u64, on_update: impl FnMut(&Gen1Battle)) -> (u32, u64, u8) {
    play_pkmn_c(b, seed, on_update, |_, _| {})
}

fn play_pkmn_c(b: &mut Gen1Battle, seed: u64, mut on_update: impl FnMut(&Gen1Battle), mut on_choice: impl FnMut(Action, Action)) -> (u32, u64, u8) {
    let mut rng = Rng::new(seed);
    let mut ch = [0u8; 16];
    let (mut c1, mut c2) = (0u8, 0u8);
    let mut updates = 0;
    loop {
        let r = unsafe { pkmn_sys::pkmn_gen1_battle_update(&mut b.raw, c1, c2, std::ptr::null_mut()) };
        b.result = r;
        updates += 1;
        on_update(b);
        let kind = pkmn_sys::bits::result_kind(r);
        if kind != 0 || Pkmn1::turn(b) >= MAX_TURNS {
            return (Pkmn1::turn(b), updates, kind);
        }
        let mut cs = [0u8; 2];
        for p in 0..2 {
            let n = Pkmn1::choices(b, p, &mut ch);
            let mut idx = Idx::new();
            for i in 0..n {
                idx.push((Pkmn1::action_of(b, p, ch[i]).index(), i));
            }
            cs[p] = ch[idx.pick(&mut rng)];
        }
        on_choice(Pkmn1::action_of(b, 0, cs[0]), Pkmn1::action_of(b, 1, cs[1]));
        c1 = cs[0];
        c2 = cs[1];
    }
}

fn native_action(b: &Battle, p: usize, c: Choice) -> Action {
    match c {
        Choice::Pass => Action::Pass,
        Choice::Move(k) => Action::Move(k.saturating_sub(1)),
        Choice::Switch(k) => Action::Switch(b.sides[p].order[k as usize - 1]),
    }
}

/// One game on the native sim with the no-op event sink.
fn play_native(b: &mut Battle, seed: u64, on_update: impl FnMut(&Battle)) -> (u32, u64, u8) {
    play_native_c(b, seed, on_update, |_, _| {})
}

fn play_native_c(b: &mut Battle, seed: u64, mut on_update: impl FnMut(&Battle), mut on_choice: impl FnMut(Action, Action)) -> (u32, u64, u8) {
    use sim_core::Generation;
    let mut rng = Rng::new(seed);
    let mut ch = [Choice::Pass; 16];
    let (mut c1, mut c2) = (Choice::Pass, Choice::Pass);
    let mut updates = 0;
    loop {
        b.apply_choices_unchecked(c1, c2, &mut ());
        updates += 1;
        on_update(b);
        if b.ended || b.turn as u32 >= MAX_TURNS {
            return (b.turn as u32, updates, b.winner);
        }
        let mut cs = [Choice::Pass; 2];
        for p in 0..2 {
            let n = b.choices(p, &mut ch);
            let mut idx = Idx::new();
            for i in 0..n {
                idx.push((native_action(b, p, ch[i]).index(), i));
            }
            cs[p] = ch[idx.pick(&mut rng)];
        }
        on_choice(native_action(b, 0, cs[0]), native_action(b, 1, cs[1]));
        c1 = cs[0];
        c2 = cs[1];
    }
}

fn games(source: &TeamSource, n: usize, seed: u64) -> Vec<(Team, Team, u64, u64)> {
    (0..n)
        .map(|i| {
            let mut rng = Rng::new(derive_seed(seed, i as u64));
            let (a, b) = (source.sample(&mut rng), source.sample(&mut rng));
            (a, b, rng.next_u64(), derive_seed(seed ^ 0xBEEF, i as u64))
        })
        .collect()
}

pub fn bench(n: usize, seed: u64, teams: &str) {
    let source = TeamSource::from_spec(teams).unwrap_or_else(|e| panic!("{e}"));
    let gs = games(&source, n, seed);
    println!("== bench: {n} random games (teams={teams}), single thread, uniform random actions");
    // Warm up caches/branch predictors briefly.
    for g in gs.iter().take(50) {
        let mut b = Pkmn1::new_battle(&g.0, &g.1, g.2);
        play_pkmn(&mut b, g.3, |_| {});
        let mut nb = sim_backend::new_battle(&g.0, &g.1, g.2, Rules::OU);
        play_native(&mut nb, g.3, |_| {});
    }
    let mut results = Vec::new();
    for (name, compat) in [("pkmn/engine (libpkmn, -Dshowdown, no log)", None), ("native (vanilla Showdown)", Some(Compat::Vanilla)), ("native (pkmn-patched Showdown)", Some(Compat::Pkmn))] {
        let starts_native: Vec<Battle> = gs
            .iter()
            .map(|g| sim_backend::new_battle(&g.0, &g.1, g.2, Rules { compat: compat.unwrap_or_default(), ..Rules::OU }))
            .collect();
        let starts_pkmn: Vec<Gen1Battle> = gs.iter().map(|g| Pkmn1::new_battle(&g.0, &g.1, g.2)).collect();
        let t0 = Instant::now();
        let (mut turns, mut updates) = (0u64, 0u64);
        for (i, g) in gs.iter().enumerate() {
            let (t, u, _) = match compat {
                None => {
                    let mut b = starts_pkmn[i];
                    play_pkmn(&mut b, g.3, |_| {})
                }
                Some(_) => {
                    let mut b = starts_native[i];
                    play_native(&mut b, g.3, |_| {})
                }
            };
            turns += t as u64;
            updates += u;
        }
        let secs = t0.elapsed().as_secs_f64();
        println!(
            "  {name:45} {secs:7.3}s  {:>10.0} turns/s  {:>10.0} updates/s  ({turns} turns)",
            turns as f64 / secs,
            updates as f64 / secs
        );
        results.push(turns as f64 / secs);
    }
    println!("  native/pkmn speed ratio: {:.2}x (vanilla), {:.2}x (pkmn-patched)", results[1] / results[0], results[2] / results[0]);
    println!("  state size: native {} bytes, pkmn {} bytes", std::mem::size_of::<Battle>(), pkmn_sys::GEN1_BATTLE_SIZE);
}

/// Per-update fingerprint: turn, each side's active slot, and every Pokémon's (hp, status
/// category) from the env view.
fn fingerprint(v: &View) -> Vec<(u16, u8)> {
    let mut out = Vec::with_capacity(16);
    for s in &v.sides {
        out.push((s.active.party_idx as u16, 255));
        for m in &s.mons[..s.size as usize] {
            // pkmn clears a fainted Pokémon's status; Showdown keeps it until it is replaced.
            out.push((m.hp, if m.hp == 0 { 0 } else { m.status }));
        }
    }
    out.push((v.turn, 0));
    out
}

/// Sorted env action indices of a player's legal choices.
fn pkmn_actions(b: &Gen1Battle, p: usize) -> Vec<usize> {
    let mut ch = [0u8; 16];
    let n = Pkmn1::choices(b, p, &mut ch);
    let mut v: Vec<usize> = ch[..n].iter().map(|&c| Pkmn1::action_of(b, p, c).index()).collect();
    v.sort();
    v
}

fn native_actions(b: &Battle, p: usize) -> Vec<usize> {
    use sim_core::Generation;
    let mut ch = [Choice::Pass; 16];
    let n = b.choices(p, &mut ch);
    let mut v: Vec<usize> = ch[..n].iter().map(|&c| native_action(b, p, c).index()).collect();
    v.sort();
    v
}

/// Differential test against pkmn/engine: step both engines in lockstep on the same games. Each
/// player's action is drawn uniformly from the actions *both* engines allow (Showdown 0.11.11
/// offers a single "Fight" option to a sleeping/frozen Pokémon where pkmn's older Showdown
/// target lists every move), and the fingerprints (active slots, every HP and status, turn) are
/// compared after every update. Returns the number of games that diverged.
pub fn diff_pkmn(n: usize, seed: u64, teams: &str, compat: Compat, verbose: bool) -> usize {
    let source = TeamSource::from_spec(teams).unwrap_or_else(|e| panic!("{e}"));
    let gs = games(&source, n, seed);
    let mut diverged = 0;
    let mut first_updates = Vec::new();
    let mut total_updates = 0u64;
    let mut action_mismatch = 0u64;
    let mut empty = 0;
    for (gi, g) in gs.iter().enumerate() {
        let mut pk = Pkmn1::new_battle(&g.0, &g.1, g.2);
        let mut nb = sim_backend::new_battle(&g.0, &g.1, g.2, Rules { compat, ..Rules::OU });
        let mut rng = Rng::new(g.3);
        let mut mem = [Memory::default(); 2];
        let (mut c_p, mut c_n) = ([0u8; 2], [Choice::Pass; 2]);
        let mut updates = 0;
        loop {
            Pkmn1::update(&mut pk, c_p[0], c_p[1], &mut mem);
            nb.apply_choices_unchecked(c_n[0], c_n[1], &mut ());
            updates += 1;
            let (va, vb) = (Pkmn1::view(&pk), sim_backend::view(&nb));
            if fingerprint(&va) != fingerprint(&vb) {
                diverged += 1;
                first_updates.push(updates);
                if verbose && diverged <= 8 {
                    println!("game {gi}: state differs after update {updates} (turn {})", vb.turn);
                    println!("  pkmn   {:?}\n  native {:?}", fingerprint(&va), fingerprint(&vb));
                }
                break;
            }
            let pk_over = pkmn_sys::bits::result_kind(pk.result) != 0;
            if pk_over || nb.ended || nb.turn as u32 >= MAX_TURNS {
                if pk_over != nb.ended && (nb.turn as u32) < MAX_TURNS {
                    diverged += 1;
                    first_updates.push(updates);
                }
                break;
            }
            let mut ok = true;
            for p in 0..2 {
                let (a, b) = (pkmn_actions(&pk, p), native_actions(&nb, p));
                if a != b {
                    action_mismatch += 1;
                }
                // A single forced option (Showdown's "Fight" for a sleeping/frozen Pokémon vs pkmn's
                // only move with PP) is the same decision even if the indices differ.
                let (act_p, act_n) = if a.len() == 1 && b.len() == 1 {
                    (a[0], b[0])
                } else {
                    let both: Vec<usize> = a.iter().copied().filter(|x| b.contains(x)).collect();
                    if both.is_empty() {
                        ok = false;
                        break;
                    }
                    let act = both[rng.below(both.len() as u64) as usize];
                    (act, act)
                };
                let mut ch = [0u8; 16];
                let k = Pkmn1::choices(&pk, p, &mut ch);
                c_p[p] = *ch[..k].iter().find(|&&c| Pkmn1::action_of(&pk, p, c).index() == act_p).unwrap();
                let mut chn = [Choice::Pass; 16];
                let k = sim_core::Generation::choices(&nb, p, &mut chn);
                c_n[p] = *chn[..k].iter().find(|&&c| native_action(&nb, p, c).index() == act_n).unwrap();
            }
            if !ok {
                empty += 1;
                diverged += 1;
                first_updates.push(updates);
                if verbose && diverged <= 8 {
                    println!("game {gi}: no common action after update {updates}: p1 pkmn {:?} native {:?} / p2 pkmn {:?} native {:?}", pkmn_actions(&pk, 0), native_actions(&nb, 0), pkmn_actions(&pk, 1), native_actions(&nb, 1));
                    let (va, vb) = (Pkmn1::view(&pk), sim_backend::view(&nb));
                    for p in 0..2 { println!("   p{} pkmn {:?}\n      native {:?}", p + 1, va.sides[p].active, vb.sides[p].active); }
                }
                break;
            }
        }
        total_updates += updates as u64;
    }
    println!(
        "== diff vs pkmn/engine ({teams}, native compat {compat:?}): {n} games, {total_updates} lockstep updates, {diverged} diverged ({:.2}%), {empty} without a common action; action sets differed {action_mismatch} times",
        100.0 * diverged as f64 / n as f64
    );
    if !first_updates.is_empty() {
        first_updates.sort();
        println!("  median update of first divergence: {}", first_updates[first_updates.len() / 2]);
    }
    diverged
}

/// Median of a few samples.
fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// Timing breakdown of the native simulator (single thread, `compat` rules), by ablation:
/// every game's choices are recorded once, then the same games are replayed
/// - with updates only (`update` ns per update),
/// - with updates plus `choices()` for both players at every decision (`choices` = difference),
/// - and a sample of decision-point states is copied (`Battle` memcpy, the search clone),
///   serialised (`to_bytes`, the env's `clone_state`) and decoded + validated (`from_bytes`,
///   `restore_state`).
///
/// Each figure is the median of `reps` runs. For a per-function profile use callgrind on
/// `bench N teams seed native` (only the native games are played).
pub fn breakdown(n: usize, seed: u64, teams: &str, compat: Compat, reps: usize) {
    use sim_core::Generation;
    use std::hint::black_box;
    let source = TeamSource::from_spec(teams).unwrap_or_else(|e| panic!("{e}"));
    let gs = games(&source, n, seed);
    let rules = Rules { compat, ..Rules::OU };
    let starts: Vec<Battle> = gs.iter().map(|g| sim_backend::new_battle(&g.0, &g.1, g.2, rules)).collect();
    // Record the choice sequences and a sample of decision-point states.
    let mut traj: Vec<Vec<(Choice, Choice)>> = Vec::with_capacity(n);
    let mut sample: Vec<Battle> = Vec::new();
    let mut turns = 0u64;
    for (i, g) in gs.iter().enumerate() {
        let mut b = starts[i];
        let mut t = Vec::new();
        let mut rng = Rng::new(g.3);
        let mut ch = [Choice::Pass; 16];
        let (mut c1, mut c2) = (Choice::Pass, Choice::Pass);
        loop {
            b.apply_choices_unchecked(c1, c2, &mut ());
            t.push((c1, c2));
            if t.len() % 16 == 0 && sample.len() < 20_000 {
                sample.push(b);
            }
            if b.ended || b.turn as u32 >= MAX_TURNS {
                break;
            }
            let mut cs = [Choice::Pass; 2];
            for p in 0..2 {
                let n = b.choices(p, &mut ch);
                let mut idx = Idx::new();
                for i in 0..n {
                    idx.push((native_action(&b, p, ch[i]).index(), i));
                }
                cs[p] = ch[idx.pick(&mut rng)];
            }
            c1 = cs[0];
            c2 = cs[1];
        }
        turns += b.turn as u64;
        traj.push(t);
    }
    let updates: usize = traj.iter().map(|t| t.len()).sum();
    let time = |f: &mut dyn FnMut()| {
        median(
            (0..reps)
                .map(|_| {
                    let t0 = Instant::now();
                    f();
                    t0.elapsed().as_secs_f64()
                })
                .collect(),
        )
    };
    let replay = time(&mut || {
        for (i, t) in traj.iter().enumerate() {
            let mut b = starts[i];
            for &(c1, c2) in t {
                b.apply_choices_unchecked(black_box(c1), black_box(c2), &mut ());
            }
            black_box(&b);
        }
    });
    let with_choices = time(&mut || {
        let mut ch = [Choice::Pass; 16];
        for (i, t) in traj.iter().enumerate() {
            let mut b = starts[i];
            for &(c1, c2) in t {
                b.apply_choices_unchecked(black_box(c1), black_box(c2), &mut ());
                for p in 0..2 {
                    black_box(b.choices(p, &mut ch));
                    black_box(&ch);
                }
            }
        }
    });
    // In-cache copies (a search clones from a hot state): 64 states, many times over.
    const COPIES: usize = 20_000;
    let hot = &sample[..64];
    let copy = time(&mut || {
        let mut dst = sample[0];
        for _ in 0..COPIES {
            for s in hot {
                dst = black_box(*black_box(s));
            }
        }
        black_box(&dst);
    });
    let mut blobs = Vec::with_capacity(sample.len() * sim_gen1::ser::BATTLE_BYTES);
    let to_bytes = time(&mut || {
        blobs.clear();
        for s in &sample {
            s.to_bytes(&mut blobs);
        }
        black_box(&blobs);
    });
    let from_bytes = time(&mut || {
        for c in blobs.chunks(sim_gen1::ser::BATTLE_BYTES) {
            black_box(Battle::from_bytes(c).expect("valid blob"));
        }
    });
    let ns = |s: f64, k: usize| s * 1e9 / k as f64;
    println!("== native breakdown ({compat:?}, {n} games, {updates} updates, {turns} turns; median of {reps})");
    println!("  update only:         {:7.1} ns/update  {:>10.0} turns/s", ns(replay, updates), turns as f64 / replay);
    println!("  choices (2 players): {:7.1} ns/update  ({:.1} ns/player)", ns(with_choices - replay, updates), ns(with_choices - replay, 2 * updates));
    println!("  Battle copy:         {:7.1} ns         ({} bytes)", ns(copy, COPIES * hot.len()), std::mem::size_of::<Battle>());
    println!("  to_bytes:            {:7.1} ns         (clone_state blob, {} bytes)", ns(to_bytes, sample.len()), sim_gen1::ser::BATTLE_BYTES);
    println!("  from_bytes+validate: {:7.1} ns         (restore_state)", ns(from_bytes, sample.len()));
}

/// Only the native games (vanilla rules), for profilers.
pub fn native_only(n: usize, seed: u64, teams: &str) {
    let source = TeamSource::from_spec(teams).unwrap_or_else(|e| panic!("{e}"));
    let gs = games(&source, n, seed);
    let starts: Vec<Battle> = gs.iter().map(|g| sim_backend::new_battle(&g.0, &g.1, g.2, Rules::OU)).collect();
    let t0 = Instant::now();
    let mut turns = 0u64;
    for (i, g) in gs.iter().enumerate() {
        let mut b = starts[i];
        turns += play_native(&mut b, g.3, |_| {}).0 as u64;
    }
    let secs = t0.elapsed().as_secs_f64();
    println!("native only: {turns} turns, {:.0} turns/s", turns as f64 / secs);
}
