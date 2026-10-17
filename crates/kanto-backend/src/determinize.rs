//! State reconstruction for search: a full simulator state consistent with one player's
//! observations (a [`Snapshot`] of a `ClientBattle`), with the hidden parts filled in.
//!
//! The caller supplies both teams: the observer's own (exact: its DVs and stat exp are its own)
//! and a guess of the opponent's, whose first `opp_seen` members are the Pokémon seen so far, in
//! order of appearance, and which must contain every opponent move revealed so far. Everything
//! else hidden is sampled from `seed`:
//!
//! - the opponent's exact HP within its shown percentage;
//! - remaining sleep, confusion and Disable turns, given the turns observed so far (both sides:
//!   Showdown hides the counters from their owner too);
//! - the simulator's PRNG state.
//!
//! The result is an env whose observation for the observer equals the client's, and whose
//! memories are both players' (so a search can play either seat from env observations).
//!
//! States the reconstruction does not support return `Err` (callers fall back to playing without
//! search): an active Pokémon that is transformed, partially trapping or trapped, using Bide, or
//! holding a Mimic copy.

use kanto_env::client::{RequestKind, Snapshot};
use kanto_env::env::Env;
use kanto_env::memory::UNREVEALED;
use kanto_env::obs::{hp_percent, status, vol};
use kanto_env::rng::Rng;
use kanto_env::team::Team;
use kanto_gen1::state::{st, v, Active};
use kanto_gen1::state::Battle;
use kanto_gen1::Rules;

use crate::Native1;

fn sim_status(s: u8) -> u8 {
    match s {
        status::PSN => st::PSN,
        status::TOX => st::TOX,
        status::BRN => st::BRN,
        status::PAR => st::PAR,
        status::SLP => st::SLP,
        status::FRZ => st::FRZ,
        _ => st::NONE,
    }
}

/// `modifyStat`.
fn modify(stat: u16, m: f64) -> u16 {
    ((stat as f64 * m).floor() as i64).max(1) as u16
}

fn boosted(stored: u16, stage: i8) -> u16 {
    let m = if stage >= 0 {
        [1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0][stage as usize]
    } else {
        [100.0, 66.0, 50.0, 40.0, 33.0, 28.0, 25.0][(-stage) as usize] / 100.0
    };
    modify(stored, m).min(999)
}

/// Uniform in `lo..=hi` (`lo` if empty).
fn uniform(rng: &mut Rng, lo: i32, hi: i32) -> i32 {
    if hi <= lo {
        return lo;
    }
    lo + (rng.next_u64() % (hi - lo + 1) as u64) as i32
}

/// Build the env state. `own` is the observer's team in its original order; `opp` the opponent
/// guess (appearance order first).
pub fn determinize(snap: &Snapshot, own: &Team, opp: &Team, seed: u64) -> Result<Env<Native1>, String> {
    let me = snap.me;
    let foe = 1 - me;
    let v = &snap.view;
    let mut rng = Rng::new(seed);
    if !matches!(snap.request, RequestKind::Move | RequestKind::Switch) {
        return Err("no decision to make".into());
    }
    let own_mons = own.mons();
    let opp_mons = opp.mons();
    if own_mons.len() != v.sides[me].size as usize {
        return Err(format!("own team has {} Pokémon, the battle {}", own_mons.len(), v.sides[me].size));
    }
    if opp_mons.len() < snap.opp_seen as usize || opp_mons.len() > 6 {
        return Err("opponent guess smaller than the Pokémon seen".into());
    }
    for (i, m) in own_mons.iter().enumerate() {
        if m.species != v.sides[me].mons[i].species {
            return Err(format!("own team slot {i} species differs from the battle"));
        }
    }
    let om = &snap.mem[me];
    for j in 0..snap.opp_seen as usize {
        if opp_mons[j].species != v.sides[foe].mons[j].species {
            return Err(format!("opponent guess slot {j} is not the Pokémon seen there"));
        }
        for &mv in om.moves[j].iter().filter(|&&m| m != 0) {
            if !opp_mons[j].moves.contains(&mv) {
                return Err(format!("opponent guess slot {j} lacks revealed move {mv}"));
            }
        }
    }
    for p in 0..2 {
        let a = &v.sides[p].active;
        if a.party_idx == UNREVEALED {
            return Err("battle not started".into());
        }
        if a.volatiles & (vol::TRANSFORM | vol::BINDING | vol::BIDE) != 0 {
            return Err("unsupported volatile (Transform, partial trapping or Bide)".into());
        }
        if snap.mem[me].track[p].mimic_move != 0 || snap.bind_hits[p] != 0 {
            return Err("unsupported volatile (Mimic or partial trapping)".into());
        }
    }

    let (p1, p2) = if me == 0 { (own, opp) } else { (opp, own) };
    let mut b: Battle = crate::try_new_battle(p1, p2, rng.next_u64(), Rules::OU).map_err(|e| e.to_string())?;
    b.start(&mut ());
    b.turn = snap.turn;
    b.mid_turn = false;
    b.last_damage = 0;

    for p in 0..2 {
        let sv = &v.sides[p];
        let active = sv.active.party_idx as usize;
        let sd = &mut b.sides[p];
        // Showdown's side order: the observer's from its request; the opponent's with the active
        // first (only the observer's positions matter for its choices).
        if p == me {
            if snap.n_order as usize != sd.n as usize {
                return Err("request order does not cover the team".into());
            }
            sd.order[..snap.n_order as usize].copy_from_slice(&snap.order[..snap.n_order as usize]);
        } else {
            let mut k = 0;
            sd.order[k] = active as u8;
            k += 1;
            for i in 0..sd.n as usize {
                if i != active {
                    sd.order[k] = i as u8;
                    k += 1;
                }
            }
        }
        if sd.order[0] as usize != active {
            return Err("the active Pokémon is not first in the request order".into());
        }
        let mem = &snap.mem[me];
        for i in 0..sd.n as usize {
            let m = &mut sd.mons[i];
            let seen = p == me || i < snap.opp_seen as usize;
            let view_mon = &sv.mons[i];
            if seen {
                m.hp = if p == me {
                    view_mon.hp
                } else if view_mon.hp == 0 {
                    0
                } else {
                    // An HP consistent with the shown percentage.
                    let pct = view_mon.hp.min(100) as u8;
                    let max = m.stats[0];
                    let ok: Vec<u16> = (1..=max).filter(|&h| hp_percent(h, max) == pct).collect();
                    if ok.is_empty() {
                        return Err("no HP matches the shown percentage".into());
                    }
                    ok[(rng.next_u64() % ok.len() as u64) as usize]
                };
                let raw = snap.raw_status[p][i];
                m.status = if m.hp == 0 { st::NONE } else { sim_status(raw) };
                m.set_fainted(m.hp == 0);
                m.set_faint_queued(m.hp == 0);
                m.set_status_self(m.status == st::SLP && view_mon.rest);
                m.slp_time = 0;
                m.slp_start = 0;
                if m.status == st::SLP {
                    let seen_turns = mem.sleep_obs[p][i] as i32;
                    let total = if view_mon.rest { 2 } else { uniform(&mut rng, (seen_turns + 1).min(7), 7) };
                    m.slp_start = total as u8;
                    m.slp_time = (total - seen_turns).max(1) as i8;
                }
                m.set_bench_par(snap.bench_par[p][i]);
                m.set_bench_brn(snap.bench_brn[p][i]);
                // PP: exact for the observer; the opponent's revealed moves lose one per observed use.
                for k in 0..m.n_moves as usize {
                    let id = m.moves[k].id;
                    let pp = if p == me {
                        view_mon.pp[k]
                    } else {
                        let maxpp = m.moves[k].pp();
                        let uses = (0..4).find(|&r| mem.moves[i][r] == id).map(|r| mem.move_uses[i][r]).unwrap_or(0);
                        maxpp.saturating_sub(uses)
                    };
                    m.moves[k].set_pp(pp);
                }
            }
        }
        sd.left = sd.mons[..sd.n as usize].iter().filter(|m| m.hp > 0).count() as u8;

        // The active Pokémon's volatile state.
        let a_view = sv.active;
        let mon = sd.mons[active];
        let mut a = Active {
            species: mon.species,
            types: a_view.types,
            stored: [mon.stats[1], mon.stats[2], mon.stats[3], mon.stats[4], mon.stats[5]],
            n_slots: mon.n_moves,
            ..Active::default()
        };
        a.boosts = snap.boosts[p];
        if p == me {
            a.stored = snap.own_stored;
            a.modified = snap.own_modified;
        } else {
            for k in 0..5 {
                a.modified[k] = boosted(a.stored[k], a.boosts[k]);
            }
            match mon.status {
                st::PAR => a.modified[4] = modify(a.modified[4], 0.25),
                st::BRN => a.modified[0] = modify(a.modified[0], 0.5),
                _ => {}
            }
        }
        a.speed = a.modified[4];
        let track = &mem.track[p];
        let last_mv = mem.last[p].mv;
        let add = |a: &mut Active, id: u8| {
            a.vols |= 1 << id;
            a.vstamp[id as usize] = a.next_stamp;
            a.next_stamp += 1;
        };
        let bits = a_view.volatiles;
        if bits & vol::CONFUSION != 0 {
            add(&mut a, v::CONFUSION);
            // Confusion lasts 2-5 turns.
            let e = track.confusion_elapsed as i32;
            a.confusion_time = (uniform(&mut rng, (e + 1).clamp(2, 5), 5) - e).max(1) as i8;
        }
        if bits & vol::RECHARGING != 0 {
            add(&mut a, v::MUSTRECHARGE);
        }
        if bits & vol::THRASHING != 0 {
            add(&mut a, v::LOCKEDMOVE);
            a.lockedmove_move = last_mv;
            a.lockedmove_time = uniform(&mut rng, 1, 2) as i8;
            a.lockedmove_acc = 255;
        }
        if bits & (vol::CHARGING | vol::INVULNERABLE) != 0 {
            add(&mut a, v::TWOTURNMOVE);
            a.twoturn_move = last_mv;
            if bits & vol::INVULNERABLE != 0 {
                add(&mut a, v::INVULNERABILITY);
            }
        }
        if a_view.disabled_slot != 0 {
            add(&mut a, v::DISABLE);
            a.disable_slot = a_view.disabled_slot - 1;
            a.disable_move = mon.moves[a.disable_slot as usize].id;
            // Disable lasts 1-8 turns.
            let e = track.disable_elapsed as i32;
            a.disable_time = (uniform(&mut rng, (e + 1).min(8), 8) - e).max(1) as i8;
        }
        for (bit, id) in [
            (vol::FOCUS_ENERGY, v::FOCUSENERGY),
            (vol::MIST, v::MIST),
            (vol::LIGHT_SCREEN, v::LIGHTSCREEN),
            (vol::REFLECT, v::REFLECT),
            (vol::LEECH_SEED, v::LEECHSEED),
            (vol::SUBSTITUTE, v::SUBSTITUTE),
            (vol::RAGE, v::RAGE),
            (vol::TOXIC, v::RESIDUALDMG),
        ] {
            if bits & bit != 0 {
                add(&mut a, id);
            }
        }
        if bits & vol::SUBSTITUTE != 0 {
            a.sub_hp = (mon.stats[0] / 4) as i32 + 1;
        }
        if bits & vol::RAGE != 0 {
            a.rage_acc = 255;
        }
        if bits & vol::TOXIC != 0 {
            a.residual_counter = track.toxic_elapsed;
        }
        a.last_move = last_mv;
        a.switch_flag = mon.hp == 0;
        sd.active = a;
        sd.last_move = last_mv;
        sd.request = kanto_core::Request::None;
    }
    // Requests: the observer's from its own; the opponent must replace a fainted Pokémon,
    // waits while only the observer switches, and otherwise moves.
    for p in 0..2 {
        b.sides[p].last_enemy_move = b.sides[1 - p].last_move;
    }
    let foe_fainted = b.sides[foe].mon().hp == 0;
    match snap.request {
        RequestKind::Move => {
            if foe_fainted {
                return Err("the opponent's active fainted but the observer has a move request".into());
            }
            b.set_move_request(me);
            b.set_move_request(foe);
        }
        RequestKind::Switch => {
            b.sides[me].request = kanto_core::Request::Switch;
            b.sides[foe].request =
                if foe_fainted && b.sides[foe].left > 0 { kanto_core::Request::Switch } else { kanto_core::Request::Wait };
        }
        _ => unreachable!(),
    }
    b.validate().map_err(|e| format!("reconstructed state fails validation: {e}"))?;
    let mut env = Env::<Native1> {
        battle: b,
        mem: snap.mem,
        rng: Rng::new(seed ^ 0x9e37_79b9_7f4a_7c15),
        table: Default::default(),
        mask: Default::default(),
        steps: 0,
        inject_panic: false,
    };
    env.refresh();
    Ok(env)
}
