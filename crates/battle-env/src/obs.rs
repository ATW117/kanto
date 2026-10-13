//! Observation encoding, `OBS_VERSION` 1. docs/OBSERVATION.md is the authoritative layout and
//! docs/OBSERVATION_RULES.md the visibility spec this implements.
//!
//! The encoder takes a backend-agnostic, omniscient [`View`] plus the observing player's
//! [`Memory`]. All hidden-information filtering happens here. Keep the offsets below in sync with
//! docs/OBSERVATION.md and bump `OBS_VERSION` on any change.
//!
//! `ID` fields (species, move, type ids) hold raw integer values as f32, for embedding; all other
//! fields are scaled to roughly [0, 1] or [-1, 1].

use crate::data;
use crate::memory::{Memory, UNREVEALED};

pub const OBS_VERSION: u32 = 1;

/// Species token for an opponent slot that exists but is unrevealed.
pub const SPECIES_UNKNOWN: u8 = 152;
/// Move token for an opponent move not yet revealed.
pub const MOVE_UNKNOWN: u8 = 166;

// ---- Backend-agnostic omniscient view ---------------------------------------------------------

/// Status categories; the one-hot order is `[none, psn, tox, brn, par, slp, frz, fnt]`.
pub mod status {
    pub const NONE: u8 = 0;
    pub const PSN: u8 = 1;
    pub const TOX: u8 = 2;
    pub const BRN: u8 = 3;
    pub const PAR: u8 = 4;
    pub const SLP: u8 = 5;
    pub const FRZ: u8 = 6;
    pub const FNT: u8 = 7;
    pub const COUNT: usize = 8;
}

/// Engine volatile bits (pkmn Gen 1 `Volatiles` order) used by `ActiveView::volatiles`.
pub mod vol {
    pub const BIDE: u32 = 1 << 0;
    pub const THRASHING: u32 = 1 << 1;
    pub const CHARGING: u32 = 1 << 4;
    pub const BINDING: u32 = 1 << 5;
    pub const INVULNERABLE: u32 = 1 << 6;
    pub const CONFUSION: u32 = 1 << 7;
    pub const MIST: u32 = 1 << 8;
    pub const FOCUS_ENERGY: u32 = 1 << 9;
    pub const SUBSTITUTE: u32 = 1 << 10;
    pub const RECHARGING: u32 = 1 << 11;
    pub const RAGE: u32 = 1 << 12;
    pub const LEECH_SEED: u32 = 1 << 13;
    pub const TOXIC: u32 = 1 << 14;
    pub const LIGHT_SCREEN: u32 = 1 << 15;
    pub const REFLECT: u32 = 1 << 16;
    pub const TRANSFORM: u32 = 1 << 17;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonView {
    pub species: u8,
    pub level: u8,
    pub types: [u8; 2],
    pub hp: u16,
    pub max_hp: u16,
    /// Visible status category (`status::*`, never FNT; derive that from hp).
    pub status: u8,
    /// Hidden: remaining sleep turns.
    pub sleep_turns: u8,
    /// Sleep was self-inflicted (Rest) — public via `-status ... [from] move: Rest`.
    pub rest: bool,
    /// Unmodified [hp, atk, def, spe, spc].
    pub stats: [u16; 5],
    pub moves: [u8; 4],
    pub pp: [u8; 4],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveView {
    /// Original party index of the active Pokémon (`UNREVEALED` before the battle starts).
    pub party_idx: u8,
    /// Current species (differs from the party member's when transformed).
    pub species: u8,
    pub types: [u8; 2],
    /// Modified [hp, atk, def, spe, spc].
    pub stats: [u16; 5],
    /// [atk, def, spe, spc, accuracy, evasion] in -6..=6.
    pub boosts: [i8; 6],
    /// `vol::*` bits.
    pub volatiles: u32,
    pub moves: [u8; 4],
    pub pp: [u8; 4],
    /// Disabled move slot 1..=4 (0 = none).
    pub disabled_slot: u8,
    // Hidden counters (privileged only):
    pub disable_turns: u8,
    pub confusion_turns: u8,
    pub attacks_left: u8,
    pub substitute_hp: u8,
    pub toxic_counter: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SideView {
    pub size: u8,
    pub mons: [MonView; 6],
    pub active: ActiveView,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub turn: u16,
    pub sides: [SideView; 2],
}

// ---- Layout -----------------------------------------------------------------------------------

pub const GLOBAL_DIM: usize = 12;
pub const OWN_MON_DIM: usize = 51;
pub const OPP_MON_DIM: usize = 49;
pub const ACTIVE_DIM: usize = 47;
pub const EVENTS_DIM: usize = 18;
pub const PRIVILEGED_DIM: usize = 6;
pub const RESERVED_DIM: usize = 4;

pub const OFF_GLOBAL: usize = 0;
pub const OFF_OWN: usize = OFF_GLOBAL + GLOBAL_DIM;
pub const OFF_OPP: usize = OFF_OWN + 6 * OWN_MON_DIM;
pub const OFF_OWN_ACTIVE: usize = OFF_OPP + 6 * OPP_MON_DIM;
pub const OFF_OPP_ACTIVE: usize = OFF_OWN_ACTIVE + ACTIVE_DIM;
pub const OFF_OWN_EVENTS: usize = OFF_OPP_ACTIVE + ACTIVE_DIM;
pub const OFF_OPP_EVENTS: usize = OFF_OWN_EVENTS + EVENTS_DIM;
pub const OFF_PRIVILEGED: usize = OFF_OPP_EVENTS + EVENTS_DIM;
pub const OFF_RESERVED: usize = OFF_PRIVILEGED + PRIVILEGED_DIM;
pub const OBS_DIM: usize = OFF_RESERVED + RESERVED_DIM;
const _: () = assert!(OBS_DIM == 752);

pub mod global {
    pub const TURN: usize = 0; // turn / 1000
    pub const OWN_SIZE: usize = 1; // /6
    pub const OWN_ALIVE: usize = 2; // /6
    pub const OPP_SIZE: usize = 3; // /6 (public via |teamsize|)
    pub const OPP_REVEALED: usize = 4; // /6
    pub const OPP_FAINTED: usize = 5; // /6
    pub const SLP_CLAUSE_BY_ME: usize = 6; // an opp Pokémon is asleep (non-Rest)
    pub const FRZ_CLAUSE_BY_ME: usize = 7; // an opp Pokémon is frozen
    pub const SLP_CLAUSE_BY_OPP: usize = 8;
    pub const FRZ_CLAUSE_BY_OPP: usize = 9;
    pub const OWN_ACTIVE_FAINTED: usize = 10; // forced switch pending
    pub const OPP_ACTIVE_FAINTED: usize = 11;
}

/// Own party block, one per original team slot (matches switch action `4 + k`).
pub mod own {
    pub const SPECIES: usize = 0; // ID; 0 = empty slot
    pub const ACTIVE: usize = 1;
    pub const FAINTED: usize = 2;
    pub const HP_FRAC: usize = 3; // exact hp / maxhp
    pub const MAX_HP: usize = 4; // maxhp / 704
    pub const LEVEL: usize = 5; // /100
    pub const STATS: usize = 6; // stored atk, def, spc, spe / 999
    pub const TYPES: usize = 10; // 15 multi-hot
    pub const STATUS: usize = 25; // 8 one-hot
    pub const SLEEP_OBS: usize = 33; // observed sleep turns / 7
    pub const REST: usize = 34;
    pub const MOVES: usize = 35; // 4 x [move ID (0 = empty), pp/maxpp, maxpp/64, disabled]
    pub const MOVE_STRIDE: usize = 4;
}

/// Opponent party block, one per slot in order of first appearance.
pub mod opp {
    pub const SPECIES: usize = 0; // ID; 0 = beyond team size, 152 = unknown
    pub const REVEALED: usize = 1;
    pub const ACTIVE: usize = 2;
    pub const FAINTED: usize = 3;
    pub const HP_PCT: usize = 4; // Showdown percentage / 100
    pub const LEVEL: usize = 5; // /100
    pub const ASSUMED_MAX_HP: usize = 6; // public prior (max DVs/exp) / 704
    pub const ASSUMED_STATS: usize = 7; // public prior atk, def, spc, spe / 999
    pub const TYPES: usize = 11; // 15 multi-hot (species typing)
    pub const STATUS: usize = 26; // 8 one-hot
    pub const SLEEP_OBS: usize = 34; // /7
    pub const REST: usize = 35;
    pub const MOVES: usize = 36; // 4 x [move ID (166 = unknown), uses/maxpp, est pp left/64]
    pub const MOVE_STRIDE: usize = 3;
    pub const N_MOVES: usize = 48; // revealed moves / 4
}

/// Active block (own first, then opponent; same shape, opponent omits hidden fields).
pub mod active {
    pub const BOOSTS: usize = 0; // atk, def, spc, spe, acc, eva / 6
    pub const STATS: usize = 6; // own only: effective atk, def, spc, spe / 999
    pub const TYPES: usize = 10; // 15 multi-hot (current typing)
    pub const VOLATILES: usize = 25; // 16 flags, see VOLATILE_NAMES
    pub const ELAPSED: usize = 41; // confusion/5, disable/8, trap/5, toxic/15 elapsed
    pub const TRANSFORM_SPECIES: usize = 45; // ID, 0 = not transformed
    pub const MIMIC_MOVE: usize = 46; // ID, 0 = none
}

pub const VOLATILE_NAMES: [&str; 16] = [
    "confusion",
    "leech_seed",
    "substitute",
    "reflect",
    "light_screen",
    "mist",
    "focus_energy",
    "must_recharge",
    "partially_trapped",
    "trapping",
    "bide",
    "locked",
    "rage",
    "transformed",
    "disabled_any",
    "mimic_active",
];

/// Last-update events block (own side first, then opponent).
pub mod events {
    pub const MOVED: usize = 0;
    pub const MOVED_FIRST: usize = 1;
    pub const MOVE: usize = 2; // ID
    pub const CALLED: usize = 3;
    pub const MISSED: usize = 4;
    pub const CRIT: usize = 5;
    pub const SUPER_EFFECTIVE: usize = 6;
    pub const RESISTED: usize = 7;
    pub const IMMUNE: usize = 8;
    pub const FAILED: usize = 9;
    pub const SWITCHED: usize = 10;
    pub const CANT: usize = 11; // 7 one-hot: slp, frz, par, partiallytrapped, flinch, disable, recharge
}

/// Privileged block (zeros unless `reveal_hidden`): own active's true hidden counters.
pub mod privileged {
    pub const SLEEP_LEFT: usize = 0; // /7
    pub const CONFUSION_LEFT: usize = 1; // /5
    pub const DISABLE_LEFT: usize = 2; // /8
    pub const ATTACKS_LEFT: usize = 3; // thrash/bide/binding turns /5
    pub const SUBSTITUTE_HP: usize = 4; // / maxhp
    pub const TOXIC_COUNTER: usize = 5; // /15
}

// ---- Encoder ----------------------------------------------------------------------------------

/// Showdown's HP Percentage Mod: `ceil(100*hp/maxhp)`, capped at 99 unless full.
#[inline]
pub fn hp_percent(hp: u16, max_hp: u16) -> u8 {
    if hp == 0 || max_hp == 0 {
        return 0;
    }
    let pct = (100 * hp as u32).div_ceil(max_hp as u32);
    if pct == 100 && hp < max_hp {
        99
    } else {
        pct as u8
    }
}

/// Public prior on stats: max DVs and stat exp at the given level. [hp, atk, def, spe, spc].
#[inline]
pub fn assumed_stats(species: u8, level: u8) -> [u16; 5] {
    let base = data::SPECIES_BASE[species as usize];
    let mut s = [0u16; 5];
    for i in 0..5 {
        s[i] = data::calc_stat(i == 0, base[i], 15, 65535, level);
    }
    s
}

#[inline]
fn write_types(out: &mut [f32], types: [u8; 2]) {
    out[types[0] as usize] = 1.0;
    out[types[1] as usize] = 1.0;
}

#[inline]
fn category(m: &MonView) -> u8 {
    if m.hp == 0 {
        status::FNT
    } else {
        m.status
    }
}

/// Stats in the observation order (atk, def, spc, spe) from [hp, atk, def, spe, spc].
#[inline]
fn write_stats(out: &mut [f32], s: &[u16; 5]) {
    out[0] = s[1] as f32 / 999.0;
    out[1] = s[2] as f32 / 999.0;
    out[2] = s[4] as f32 / 999.0;
    out[3] = s[3] as f32 / 999.0;
}

/// Encode `player`'s observation of `v` into `out[..OBS_DIM]`. With `privileged`, the own
/// active's true hidden counters are written to the privileged block (debug only).
pub fn encode(v: &View, player: usize, mem: &Memory, privileged: bool, out: &mut [f32]) {
    let out = &mut out[..OBS_DIM];
    out.fill(0.0);
    let me = &v.sides[player];
    let foe = &v.sides[1 - player];
    let my_active = me.active.party_idx;
    let foe_active = foe.active.party_idx;

    // Global.
    {
        let g = &mut out[OFF_GLOBAL..OFF_GLOBAL + GLOBAL_DIM];
        let own_alive = me.mons[..me.size as usize].iter().filter(|m| m.hp > 0).count();
        let opp_fainted =
            (0..mem.n_revealed as usize).filter(|&j| foe.mons[mem.reveal_order[j] as usize].hp == 0).count();
        // Clause state is public: the inflicting side is always the sleeper's opponent.
        let clause = |s: &SideView| {
            let mons = &s.mons[..s.size as usize];
            let slp = mons.iter().any(|m| m.hp > 0 && m.status == status::SLP && !m.rest);
            let frz = mons.iter().any(|m| m.hp > 0 && m.status == status::FRZ);
            (slp, frz)
        };
        let (slp_me, frz_me) = clause(foe);
        let (slp_opp, frz_opp) = clause(me);
        g[global::TURN] = v.turn.min(1000) as f32 / 1000.0;
        g[global::OWN_SIZE] = me.size as f32 / 6.0;
        g[global::OWN_ALIVE] = own_alive as f32 / 6.0;
        g[global::OPP_SIZE] = foe.size as f32 / 6.0;
        g[global::OPP_REVEALED] = mem.n_revealed as f32 / 6.0;
        g[global::OPP_FAINTED] = opp_fainted as f32 / 6.0;
        g[global::SLP_CLAUSE_BY_ME] = slp_me as u8 as f32;
        g[global::FRZ_CLAUSE_BY_ME] = frz_me as u8 as f32;
        g[global::SLP_CLAUSE_BY_OPP] = slp_opp as u8 as f32;
        g[global::FRZ_CLAUSE_BY_OPP] = frz_opp as u8 as f32;
        g[global::OWN_ACTIVE_FAINTED] = (my_active != UNREVEALED && me.mons[my_active as usize].hp == 0) as u8 as f32;
        g[global::OPP_ACTIVE_FAINTED] =
            (foe_active != UNREVEALED && foe.mons[foe_active as usize].hp == 0) as u8 as f32;
    }

    // Own party, original team order.
    for i in 0..me.size as usize {
        let m = &me.mons[i];
        let b = &mut out[OFF_OWN + i * OWN_MON_DIM..OFF_OWN + (i + 1) * OWN_MON_DIM];
        let is_active = i as u8 == my_active;
        b[own::SPECIES] = m.species as f32;
        b[own::ACTIVE] = is_active as u8 as f32;
        b[own::FAINTED] = (m.hp == 0) as u8 as f32;
        b[own::HP_FRAC] = m.hp as f32 / m.max_hp.max(1) as f32;
        b[own::MAX_HP] = m.max_hp as f32 / 704.0;
        b[own::LEVEL] = m.level as f32 / 100.0;
        write_stats(&mut b[own::STATS..own::STATS + 4], &m.stats);
        write_types(&mut b[own::TYPES..own::TYPES + 15], m.types);
        b[own::STATUS + category(m) as usize] = 1.0;
        b[own::SLEEP_OBS] = mem.sleep_obs[player][i] as f32 / 7.0;
        b[own::REST] = (m.status == status::SLP && m.rest) as u8 as f32;
        // The active Pokémon's usable moves/PP live on the active struct (Mimic/Transform).
        let (moves, pp) = if is_active { (me.active.moves, me.active.pp) } else { (m.moves, m.pp) };
        for k in 0..4 {
            let mv = moves[k];
            if mv == 0 {
                continue;
            }
            let maxpp = data::max_pp(mv).max(1) as f32;
            let o = own::MOVES + k * own::MOVE_STRIDE;
            b[o] = mv as f32;
            b[o + 1] = (pp[k] as f32 / maxpp).min(1.0);
            b[o + 2] = maxpp / 64.0;
            b[o + 3] = (is_active && me.active.disabled_slot as usize == k + 1) as u8 as f32;
        }
    }

    // Opponent party in order of first appearance; public information only.
    for j in 0..foe.size as usize {
        let b = &mut out[OFF_OPP + j * OPP_MON_DIM..OFF_OPP + (j + 1) * OPP_MON_DIM];
        if j >= mem.n_revealed as usize {
            b[opp::SPECIES] = SPECIES_UNKNOWN as f32;
            continue;
        }
        let idx = mem.reveal_order[j] as usize;
        let m = &foe.mons[idx];
        b[opp::SPECIES] = m.species as f32;
        b[opp::REVEALED] = 1.0;
        b[opp::ACTIVE] = (idx as u8 == foe_active) as u8 as f32;
        b[opp::FAINTED] = (m.hp == 0) as u8 as f32;
        b[opp::HP_PCT] = hp_percent(m.hp, m.max_hp) as f32 / 100.0;
        b[opp::LEVEL] = m.level as f32 / 100.0;
        let prior = assumed_stats(m.species, m.level);
        b[opp::ASSUMED_MAX_HP] = prior[0] as f32 / 704.0;
        write_stats(&mut b[opp::ASSUMED_STATS..opp::ASSUMED_STATS + 4], &prior);
        write_types(&mut b[opp::TYPES..opp::TYPES + 15], data::SPECIES_TYPES[m.species as usize]);
        b[opp::STATUS + category(m) as usize] = 1.0;
        b[opp::SLEEP_OBS] = mem.sleep_obs[1 - player][idx] as f32 / 7.0;
        b[opp::REST] = (m.status == status::SLP && m.rest) as u8 as f32;
        let mut n = 0;
        for k in 0..4 {
            let o = opp::MOVES + k * opp::MOVE_STRIDE;
            let mv = mem.moves[idx][k];
            if mv == 0 {
                b[o] = MOVE_UNKNOWN as f32;
                continue;
            }
            n += 1;
            let maxpp = data::max_pp(mv).max(1);
            let uses = mem.move_uses[idx][k];
            b[o] = mv as f32;
            b[o + 1] = (uses as f32 / maxpp as f32).min(1.0);
            b[o + 2] = maxpp.saturating_sub(uses) as f32 / 64.0;
        }
        b[opp::N_MOVES] = n as f32 / 4.0;
    }

    // Active blocks.
    encode_active(me, &mem.track[player], true, &mut out[OFF_OWN_ACTIVE..OFF_OWN_ACTIVE + ACTIVE_DIM]);
    encode_active(foe, &mem.track[1 - player], false, &mut out[OFF_OPP_ACTIVE..OFF_OPP_ACTIVE + ACTIVE_DIM]);
    // `partially_trapped` = the other side's active is using a binding move.
    if foe.active.party_idx != UNREVEALED && foe.active.volatiles & vol::BINDING != 0 {
        out[OFF_OWN_ACTIVE + active::VOLATILES + 8] = 1.0;
    }
    if me.active.party_idx != UNREVEALED && me.active.volatiles & vol::BINDING != 0 {
        out[OFF_OPP_ACTIVE + active::VOLATILES + 8] = 1.0;
    }

    // Last-update events.
    encode_events(&mem.last[player], &mut out[OFF_OWN_EVENTS..OFF_OWN_EVENTS + EVENTS_DIM]);
    encode_events(&mem.last[1 - player], &mut out[OFF_OPP_EVENTS..OFF_OPP_EVENTS + EVENTS_DIM]);

    if privileged && my_active != UNREVEALED {
        let a = &me.active;
        let p = &mut out[OFF_PRIVILEGED..OFF_PRIVILEGED + PRIVILEGED_DIM];
        p[privileged::SLEEP_LEFT] = me.mons[my_active as usize].sleep_turns as f32 / 7.0;
        p[privileged::CONFUSION_LEFT] = a.confusion_turns as f32 / 5.0;
        p[privileged::DISABLE_LEFT] = a.disable_turns as f32 / 8.0;
        p[privileged::ATTACKS_LEFT] = a.attacks_left as f32 / 5.0;
        p[privileged::SUBSTITUTE_HP] = a.substitute_hp as f32 / a.stats[0].max(1) as f32;
        p[privileged::TOXIC_COUNTER] = a.toxic_counter as f32 / 15.0;
    }
}

fn encode_active(side: &SideView, t: &crate::memory::SideTrack, own_side: bool, b: &mut [f32]) {
    let a = &side.active;
    if a.party_idx == UNREVEALED {
        return;
    }
    // Boost order: atk, def, spc, spe, acc, eva.
    let order = [0usize, 1, 3, 2, 4, 5];
    for (k, &src) in order.iter().enumerate() {
        b[active::BOOSTS + k] = a.boosts[src] as f32 / 6.0;
    }
    if own_side {
        write_stats(&mut b[active::STATS..active::STATS + 4], &a.stats);
    }
    write_types(&mut b[active::TYPES..active::TYPES + 15], a.types);
    let v = a.volatiles;
    let has = |bit: u32| (v & bit != 0) as u8 as f32;
    let f = &mut b[active::VOLATILES..active::VOLATILES + 16];
    f[0] = has(vol::CONFUSION);
    f[1] = has(vol::LEECH_SEED);
    f[2] = has(vol::SUBSTITUTE);
    f[3] = has(vol::REFLECT);
    f[4] = has(vol::LIGHT_SCREEN);
    f[5] = has(vol::MIST);
    f[6] = has(vol::FOCUS_ENERGY);
    f[7] = has(vol::RECHARGING);
    // f[8] partially_trapped is set by the caller from the other side.
    f[9] = has(vol::BINDING);
    f[10] = has(vol::BIDE);
    f[11] = (v & (vol::THRASHING | vol::RAGE | vol::CHARGING | vol::INVULNERABLE) != 0) as u8 as f32;
    f[12] = has(vol::RAGE);
    f[13] = has(vol::TRANSFORM);
    f[14] = (a.disabled_slot != 0) as u8 as f32;
    f[15] = (t.mimic_move != 0) as u8 as f32;
    let e = &mut b[active::ELAPSED..active::ELAPSED + 4];
    e[0] = t.confusion_elapsed as f32 / 5.0;
    e[1] = t.disable_elapsed as f32 / 8.0;
    e[2] = t.trap_elapsed as f32 / 5.0;
    e[3] = t.toxic_elapsed as f32 / 15.0;
    if v & vol::TRANSFORM != 0 {
        b[active::TRANSFORM_SPECIES] = a.species as f32;
    }
    b[active::MIMIC_MOVE] = t.mimic_move as f32;
}

fn encode_events(e: &crate::memory::TurnEvents, b: &mut [f32]) {
    let f = |x: bool| x as u8 as f32;
    b[events::MOVED] = f(e.moved);
    b[events::MOVED_FIRST] = f(e.moved_first);
    b[events::MOVE] = e.mv as f32;
    b[events::CALLED] = f(e.called);
    b[events::MISSED] = f(e.missed);
    b[events::CRIT] = f(e.crit);
    b[events::SUPER_EFFECTIVE] = f(e.super_effective);
    b[events::RESISTED] = f(e.resisted);
    b[events::IMMUNE] = f(e.immune);
    b[events::FAILED] = f(e.failed);
    b[events::SWITCHED] = f(e.switched);
    if e.cant > 0 && (e.cant as usize) <= 7 {
        b[events::CANT + e.cant as usize - 1] = 1.0;
    }
}

/// (name, offset, size) of each top-level block, exported to Python.
pub fn layout() -> Vec<(String, usize, usize)> {
    let mut v = vec![("global".to_string(), OFF_GLOBAL, GLOBAL_DIM)];
    for i in 0..6 {
        v.push((format!("own_mon{i}"), OFF_OWN + i * OWN_MON_DIM, OWN_MON_DIM));
    }
    for j in 0..6 {
        v.push((format!("opp_mon{j}"), OFF_OPP + j * OPP_MON_DIM, OPP_MON_DIM));
    }
    v.push(("own_active".into(), OFF_OWN_ACTIVE, ACTIVE_DIM));
    v.push(("opp_active".into(), OFF_OPP_ACTIVE, ACTIVE_DIM));
    v.push(("own_events".into(), OFF_OWN_EVENTS, EVENTS_DIM));
    v.push(("opp_events".into(), OFF_OPP_EVENTS, EVENTS_DIM));
    v.push(("privileged".into(), OFF_PRIVILEGED, PRIVILEGED_DIM));
    v.push(("reserved".into(), OFF_RESERVED, RESERVED_DIM));
    v
}

/// Absolute offsets of all ID fields, with their vocabulary ("species": 0..=152,
/// "move": 0..=166).
pub fn id_offsets() -> Vec<(usize, &'static str)> {
    let mut v = Vec::new();
    for i in 0..6 {
        let base = OFF_OWN + i * OWN_MON_DIM;
        v.push((base + own::SPECIES, "species"));
        for k in 0..4 {
            v.push((base + own::MOVES + k * own::MOVE_STRIDE, "move"));
        }
    }
    for j in 0..6 {
        let base = OFF_OPP + j * OPP_MON_DIM;
        v.push((base + opp::SPECIES, "species"));
        for k in 0..4 {
            v.push((base + opp::MOVES + k * opp::MOVE_STRIDE, "move"));
        }
    }
    for base in [OFF_OWN_ACTIVE, OFF_OPP_ACTIVE] {
        v.push((base + active::TRANSFORM_SPECIES, "species"));
        v.push((base + active::MIMIC_MOVE, "move"));
    }
    v.push((OFF_OWN_EVENTS + events::MOVE, "move"));
    v.push((OFF_OPP_EVENTS + events::MOVE, "move"));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Showdown's `getHealth` (pokemon.ts L2080-2086), written independently.
    fn showdown_pct(hp: u32, max: u32) -> u32 {
        if hp == 0 {
            return 0;
        }
        let mut pct = ((100.0 * hp as f64) / max as f64).ceil() as u32;
        if pct == 100 && hp < max {
            pct = 99;
        }
        pct
    }

    #[test]
    fn hp_percent_matches_showdown_exhaustively() {
        for max in 1..=704u32 {
            for hp in 0..=max {
                assert_eq!(hp_percent(hp as u16, max as u16) as u32, showdown_pct(hp, max), "{hp}/{max}");
            }
        }
    }
}
