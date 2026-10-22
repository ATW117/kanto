//! `Backend` implementation for pkmn/engine Generation I in Showdown compatibility mode.
//!
//! Battles are constructed by serialising the engine's documented byte layout
//! (`src/lib/gen1/README.md#layout`, `src/data/layout.json`), as the engine's own TypeScript
//! driver does; libpkmn's C ABI deliberately exposes no constructor.

use crate::backend::{Action, Backend, Outcome};
use crate::data;
use crate::memory::{Memory, UNREVEALED};
use crate::obs::{status, vol, ActiveView, MonView, View};
use crate::protocol;
use crate::team::{Mon, Team};
use pkmn_sys::layout::{self as L, volatiles as V};
use pkmn_sys::{bits, pkmn_gen1_battle, pkmn_gen1_battle_options, pkmn_gen1_log_options};

/// Log buffer size: `PKMN_GEN1_LOGS_SIZE` (MAX_LOGS = 180 rounded up to a power of two).
pub const LOG_BUF: usize = 256;

pub struct Pkmn1;

/// Battle bytes plus the last update's result (which encodes each player's request kind).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gen1Battle {
    pub raw: pkmn_gen1_battle,
    pub result: u8,
}

#[inline(always)]
fn rd16(b: &[u8], off: usize) -> u16 {
    u16::from_ne_bytes([b[off], b[off + 1]])
}

#[inline(always)]
fn wr16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_ne_bytes());
}

#[inline(always)]
fn side_off(player: usize) -> usize {
    L::BATTLE_SIDES + player * L::SIDE_SIZE
}

impl Gen1Battle {
    /// Original party index (0-based) of the Pokémon at 1-based party position `pos`.
    #[inline]
    pub fn party_at(&self, player: usize, pos: u8) -> u8 {
        self.raw.bytes[side_off(player) + L::SIDE_ORDER + pos as usize - 1].wrapping_sub(1)
    }

    /// Original party index of the active Pokémon (`UNREVEALED` before the battle starts).
    #[inline]
    pub fn active_idx(&self, player: usize) -> u8 {
        if self.started() {
            self.party_at(player, 1)
        } else {
            UNREVEALED
        }
    }

    #[inline]
    pub fn started(&self) -> bool {
        self.raw.bytes[side_off(0) + L::SIDE_ACTIVE + L::ACTIVE_SPECIES] != 0
    }

    /// Stored (base) moves of party member `idx` (original index).
    #[inline]
    pub fn stored_moves(&self, player: usize, idx: usize) -> [u8; 4] {
        let o = side_off(player) + L::SIDE_POKEMON + idx.min(5) * L::POKEMON_SIZE + L::POKEMON_MOVES;
        let b = &self.raw.bytes;
        [b[o], b[o + 2], b[o + 4], b[o + 6]]
    }

    /// Moves currently on the active Pokémon (differ from stored when transformed / mimicking).
    #[inline]
    pub fn active_moves(&self, player: usize) -> [u8; 4] {
        let o = side_off(player) + L::SIDE_ACTIVE + L::ACTIVE_MOVES;
        let b = &self.raw.bytes;
        [b[o], b[o + 2], b[o + 4], b[o + 6]]
    }

    #[inline]
    pub fn volatiles(&self, player: usize) -> u64 {
        let o = side_off(player) + L::SIDE_ACTIVE + L::ACTIVE_VOLATILES;
        u64::from_ne_bytes(self.raw.bytes[o..o + 8].try_into().unwrap())
    }

    #[inline]
    pub fn status_byte(&self, player: usize, idx: usize) -> u8 {
        self.raw.bytes[side_off(player) + L::SIDE_POKEMON + idx * L::POKEMON_SIZE + L::POKEMON_STATUS]
    }

    #[inline]
    pub fn request(&self, player: usize) -> u8 {
        if player == 0 {
            bits::result_p1(self.result)
        } else {
            bits::result_p2(self.result)
        }
    }
}

/// Decode the engine's status byte into (visible category, remaining sleep turns, rest flag).
/// `toxic` is the active Pokémon's Toxic volatile: on Showdown, badly poisoned reverts to
/// regular poison when it switches out, so TOX is only shown while the volatile is present.
fn decode_status(s: u8, toxic: bool) -> (u8, u8, bool) {
    if s & 0b111 != 0 {
        return (status::SLP, s & 0b111, s & 0x80 != 0);
    }
    if s & (1 << 3) != 0 {
        return (if toxic { status::TOX } else { status::PSN }, 0, false);
    }
    if s & (1 << 4) != 0 {
        return (status::BRN, 0, false);
    }
    if s & (1 << 5) != 0 {
        return (status::FRZ, 0, false);
    }
    if s & (1 << 6) != 0 {
        return (status::PAR, 0, false);
    }
    (status::NONE, 0, false)
}

#[inline]
fn signed4(n: u8) -> i8 {
    if n > 7 {
        n as i8 - 16
    } else {
        n as i8
    }
}

fn encode_mon(out: &mut [u8], m: &Mon) {
    for (k, s) in m.stats.iter().enumerate() {
        wr16(out, L::POKEMON_STATS + 2 * k, *s);
    }
    for k in 0..4 {
        let id = m.moves[k];
        out[L::POKEMON_MOVES + 2 * k] = id;
        out[L::POKEMON_MOVES + 2 * k + 1] = if id == 0 { 0 } else { data::max_pp(id) };
    }
    wr16(out, L::POKEMON_HP, m.stats[0]);
    out[L::POKEMON_STATUS] = 0;
    out[L::POKEMON_SPECIES] = m.species;
    let t = data::SPECIES_TYPES[m.species as usize];
    out[L::POKEMON_TYPES] = (t[1] << 4) | t[0];
    out[L::POKEMON_LEVEL] = m.level;
}

/// Serialise a fresh battle (no active Pokémon, turn 0) per the engine's layout.
pub fn encode_battle(p1: &Team, p2: &Team, seed: u64) -> pkmn_gen1_battle {
    let mut raw = pkmn_gen1_battle::default();
    for (player, team) in [p1, p2].into_iter().enumerate() {
        let side = side_off(player);
        for (i, m) in team.mons().iter().enumerate() {
            let o = side + L::SIDE_POKEMON + i * L::POKEMON_SIZE;
            encode_mon(&mut raw.bytes[o..o + L::POKEMON_SIZE], m);
            raw.bytes[side + L::SIDE_ORDER + i] = i as u8 + 1;
        }
    }
    raw.bytes[L::BATTLE_RNG..L::BATTLE_RNG + 8].copy_from_slice(&seed.to_ne_bytes());
    raw
}

/// Apply one update's protocol log to both players' memories (docs/OBSERVATION_RULES.md §3).
pub fn apply_log(b: &Gen1Battle, log: &[u8], mem: &mut [Memory; 2]) {
    crate::reveal::apply_events(b, protocol::Parser::new(log), mem);
}

impl crate::reveal::RevealSource for Gen1Battle {
    fn stored_moves(&self, player: usize, idx: usize) -> [u8; 4] {
        Gen1Battle::stored_moves(self, player, idx)
    }
    fn active_idx(&self, player: usize) -> u8 {
        Gen1Battle::active_idx(self, player)
    }
}

/// What each player's own request shows (`reveal::apply_requests`): partially trapped by the foe
/// with the forced "Fight" as its only choice (awake, not frozen), and whether it must recharge.
fn request_flags(b: &Gen1Battle) -> [crate::reveal::RequestFlags; 2] {
    std::array::from_fn(|p| {
        let a = b.active_idx(p);
        if !b.started() || a == crate::memory::UNREVEALED {
            return Default::default();
        }
        let s = b.status_byte(p, a as usize);
        let mut raw = [0u8; 16];
        let single = <Pkmn1 as crate::backend::Backend>::choices(b, p, &mut raw) == 1;
        let bound = b.volatiles(1 - p) >> V::BINDING & 1 != 0;
        crate::reveal::RequestFlags {
            lone_fight: bound && s & 0b111 == 0 && s & (1 << 5) == 0 && single,
            recharge: Some(b.volatiles(p) >> V::RECHARGING & 1 != 0),
        }
    })
}

/// Post-update bookkeeping from public state (see `reveal::update_trackers`).
fn update_trackers(b: &Gen1Battle, turn_advanced: bool, mem: &mut [Memory; 2]) {
    if !b.started() {
        return;
    }
    let flags: [crate::reveal::TrackFlags; 2] = std::array::from_fn(|player| {
        let v = b.volatiles(player);
        crate::reveal::TrackFlags {
            confused: (v >> V::CONFUSION) & 1 != 0,
            disabled: (v >> V::DISABLE_MOVE) & 0b111 != 0,
            toxic: (v >> V::TOXIC) & 1 != 0,
            asleep: std::array::from_fn(|i| b.status_byte(player, i) & 0b111 != 0),
        }
    });
    crate::reveal::update_trackers(&flags, turn_advanced, mem);
}

impl Backend for Pkmn1 {
    type Battle = Gen1Battle;
    type Choice = u8;
    const BATTLE_BYTES: usize = pkmn_sys::GEN1_BATTLE_SIZE + 1;

    fn new_battle(p1: &Team, p2: &Team, seed: u64) -> Gen1Battle {
        Gen1Battle { raw: encode_battle(p1, p2, seed), result: 0 }
    }

    #[inline]
    fn choices(b: &Gen1Battle, player: usize, out: &mut [u8]) -> usize {
        debug_assert!(out.len() >= 16);
        unsafe {
            pkmn_sys::pkmn_gen1_battle_choices(&b.raw, player as u8, b.request(player), out.as_mut_ptr(), out.len())
                as usize
        }
    }

    #[inline]
    fn action_of(b: &Gen1Battle, player: usize, c: u8) -> Action {
        let data = bits::choice_data(c);
        match bits::choice_kind(c) {
            // Showdown mode uses `move 1` for forced turns and `move 0` for Struggle; both map to
            // action 0 and never coexist with another move choice.
            pkmn_sys::PKMN_CHOICE_MOVE => Action::Move(data.saturating_sub(1)),
            pkmn_sys::PKMN_CHOICE_SWITCH => Action::Switch(b.party_at(player, data)),
            _ => Action::Pass,
        }
    }

    fn choice_move(b: &Gen1Battle, player: usize, c: u8) -> u8 {
        if bits::choice_kind(c) != pkmn_sys::PKMN_CHOICE_MOVE {
            return 0;
        }
        let data = bits::choice_data(c);
        if data == 0 {
            return 165; // Struggle
        }
        // Showdown mode encodes forced turns as `move 1`: name no move while locked in.
        let view = Self::view(b);
        let a = &view.sides[player].active;
        let locked = crate::obs::vol::RECHARGING
            | crate::obs::vol::THRASHING
            | crate::obs::vol::CHARGING
            | crate::obs::vol::BIDE
            | crate::obs::vol::BINDING;
        if a.volatiles & locked != 0 {
            return 0;
        }
        a.moves.get(data as usize - 1).copied().unwrap_or(0) // data >= 1 here
    }

    #[inline]
    fn update(b: &mut Gen1Battle, c1: u8, c2: u8, mem: &mut [Memory; 2]) -> Outcome {
        let mut buf = [0u8; LOG_BUF];
        // The options struct points into itself once set, so it is set right before the update.
        let mut options = pkmn_gen1_battle_options::default();
        let log = pkmn_gen1_log_options { buf: buf.as_mut_ptr(), len: LOG_BUF };
        let turn_before = Self::turn(b);
        let result = unsafe {
            pkmn_sys::pkmn_gen1_battle_options_set(&mut options, &log, std::ptr::null(), std::ptr::null());
            pkmn_sys::pkmn_gen1_battle_update(&mut b.raw, c1, c2, &mut options)
        };
        assert!(!unsafe { pkmn_sys::pkmn_error(result) }, "libpkmn log buffer overflow");
        b.result = result;
        apply_log(b, &buf, mem);
        crate::reveal::apply_requests(request_flags(b), mem);
        update_trackers(b, Self::turn(b) > turn_before && turn_before > 0, mem);
        match bits::result_kind(result) {
            pkmn_sys::PKMN_RESULT_NONE => Outcome::Ongoing,
            pkmn_sys::PKMN_RESULT_WIN => Outcome::P1Win,
            pkmn_sys::PKMN_RESULT_LOSE => Outcome::P2Win,
            pkmn_sys::PKMN_RESULT_TIE => Outcome::Tie,
            r => panic!("libpkmn returned error result {r}"),
        }
    }

    fn battle_to_bytes(b: &Gen1Battle, out: &mut Vec<u8>) {
        out.extend_from_slice(&b.raw.bytes);
        out.push(b.result);
    }

    fn battle_from_bytes(bytes: &[u8]) -> Option<Gen1Battle> {
        if bytes.len() != Self::BATTLE_BYTES {
            return None;
        }
        let mut raw = pkmn_gen1_battle::default();
        raw.bytes.copy_from_slice(&bytes[..pkmn_sys::GEN1_BATTLE_SIZE]);
        Some(Gen1Battle { raw, result: bytes[pkmn_sys::GEN1_BATTLE_SIZE] })
    }

    #[inline]
    fn turn(b: &Gen1Battle) -> u32 {
        rd16(&b.raw.bytes, L::BATTLE_TURN) as u32
    }

    fn view(b: &Gen1Battle) -> View {
        let bytes = &b.raw.bytes;
        let mut v = View { turn: rd16(bytes, L::BATTLE_TURN), ..Default::default() };
        let started = b.started();
        for player in 0..2 {
            let side = side_off(player);
            let sv = &mut v.sides[player];
            let a = side + L::SIDE_ACTIVE;
            let volat = b.volatiles(player);
            let active_idx = b.active_idx(player);
            let toxic = started && (volat >> V::TOXIC) & 1 != 0;
            for i in 0..6 {
                let o = side + L::SIDE_POKEMON + i * L::POKEMON_SIZE;
                let species = bytes[o + L::POKEMON_SPECIES];
                if species == 0 {
                    break;
                }
                sv.size += 1;
                let (st, slp, rest) = decode_status(bytes[o + L::POKEMON_STATUS], toxic && i as u8 == active_idx);
                let t = bytes[o + L::POKEMON_TYPES];
                let m = &mut sv.mons[i];
                *m = MonView {
                    species,
                    level: bytes[o + L::POKEMON_LEVEL],
                    types: [t & 0xF, t >> 4],
                    hp: rd16(bytes, o + L::POKEMON_HP),
                    max_hp: rd16(bytes, o + L::POKEMON_STATS),
                    status: st,
                    sleep_turns: slp,
                    rest,
                    ..Default::default()
                };
                for k in 0..5 {
                    m.stats[k] = rd16(bytes, o + L::POKEMON_STATS + 2 * k);
                }
                for k in 0..4 {
                    m.moves[k] = bytes[o + L::POKEMON_MOVES + 2 * k];
                    m.pp[k] = bytes[o + L::POKEMON_MOVES + 2 * k + 1];
                }
            }
            if !started {
                sv.active.party_idx = UNREVEALED;
                continue;
            }
            let t = bytes[a + L::ACTIVE_TYPES];
            let field = |off: u32, width: u32| ((volat >> off) & ((1 << width) - 1)) as u8;
            let flags = (volat & ((1 << 18) - 1)) as u32;
            let mut act = ActiveView {
                party_idx: active_idx,
                species: bytes[a + L::ACTIVE_SPECIES],
                types: [t & 0xF, t >> 4],
                volatiles: flags,
                disabled_slot: field(V::DISABLE_MOVE, 3),
                disable_turns: field(V::DISABLE_DURATION, 4),
                confusion_turns: field(V::CONFUSION_TURNS, 3),
                attacks_left: if flags & (vol::THRASHING | vol::BIDE | vol::BINDING) != 0 {
                    field(V::ATTACKS, 3)
                } else {
                    0
                },
                substitute_hp: field(V::SUBSTITUTE_HP, 8),
                toxic_counter: field(V::TOXIC_COUNTER, 5),
                ..Default::default()
            };
            for k in 0..5 {
                act.stats[k] = rd16(bytes, a + L::ACTIVE_STATS + 2 * k);
            }
            for k in 0..3 {
                let byte = bytes[a + L::ACTIVE_BOOSTS + k];
                act.boosts[2 * k] = signed4(byte & 0xF);
                act.boosts[2 * k + 1] = signed4(byte >> 4);
            }
            for k in 0..4 {
                act.moves[k] = bytes[a + L::ACTIVE_MOVES + 2 * k];
                act.pp[k] = bytes[a + L::ACTIVE_MOVES + 2 * k + 1];
            }
            sv.active = act;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::builtin_teams;

    #[test]
    fn start_reveals_leads() {
        let teams = builtin_teams();
        let mut b = Pkmn1::new_battle(&teams[0], &teams[1], 7);
        let mut mem = [Memory::default(); 2];
        let mut out = [0u8; 16];
        assert_eq!(Pkmn1::choices(&b, 0, &mut out), 1);
        assert_eq!(out[0], 0);
        let o = Pkmn1::update(&mut b, 0, 0, &mut mem);
        assert_eq!(o, Outcome::Ongoing);
        assert_eq!(Pkmn1::turn(&b), 1);
        assert_eq!(mem[0].n_revealed, 1);
        assert_eq!(mem[0].reveal_order[0], 0);
        assert_eq!(mem[1].n_revealed, 1);
        assert!(mem[0].last[0].switched && mem[0].last[1].switched);
        let v = Pkmn1::view(&b);
        assert_eq!(v.sides[0].active.species, teams[0].mons[0].species);
        assert_eq!(v.sides[1].active.party_idx, 0);
        assert_eq!(v.sides[0].mons[0].hp, teams[0].mons[0].stats[0]);
        // Both players now have 4 moves + 5 switches.
        assert_eq!(Pkmn1::choices(&b, 0, &mut out), 9);
    }
}
