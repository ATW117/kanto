//! `battle_env::Backend` implementations backed by the native simulator (`sim-gen1`).
//!
//! - [`Native1`]: Gen 1 with `[Gen 1] OU`'s Sleep Clause Mod and Freeze Clause Mod (pkmn/engine
//!   enforces both clauses in `-Dshowdown` mode, so this is the drop-in replacement).
//! - [`Native1Nc`]: Gen 1 without clauses.
//!
//! Both reproduce vanilla pokemon-showdown 0.11.11 (see docs/SIM_DESIGN.md). The env-level
//! semantics match the pkmn backend: battles start unstarted with `Pass` as the only choice,
//! actions map to `Move(k)` (request index) / `Switch(original team slot)`, and reveals come
//! from the simulator's event stream.

use battle_env::backend::{Action, Backend, Outcome};
use battle_env::memory::{Memory, UNREVEALED};
use battle_env::obs::{status, vol, ActiveView, MonView, View};
use battle_env::protocol::{self, Event as PEvent};
use battle_env::reveal::{self, RevealSource, TrackFlags};
use battle_env::team::Team;
use sim_core::{Cant, Choice, Event, EventBuf};
use sim_data::gen1::SPECIES;
use sim_gen1::state::{st, v, Battle};
use sim_gen1::{MonSpec, Prng, Rules};

mod proto_game;
pub use proto_game::ProtoGame;

/// Gen 1 with Sleep Clause Mod and Freeze Clause Mod (`[Gen 1] OU`).
pub struct Native1;
/// Gen 1 without clauses.
pub struct Native1Nc;

/// Showdown sets for a battle-env team (DVs and stat exp are kept on `battle_env::team::Mon`).
pub fn team_specs(t: &Team) -> Vec<MonSpec> {
    t.mons().iter().map(|m| MonSpec::from_dvs(m.species, m.level, &m.moves[..], m.dvs, m.exp)).collect()
}

/// A native battle from battle-env teams and a 64-bit Gen 5 seed.
///
/// battle-env teams are validated when they are built (`Team::from_spec`, the random
/// generators), so this only fails for a hand-built `Team` with out-of-range ids.
pub fn try_new_battle(p1: &Team, p2: &Team, seed: u64, rules: Rules) -> Result<Battle, sim_gen1::TeamError> {
    Battle::new(&team_specs(p1), &team_specs(p2), Prng::from_u64(seed), rules)
}

/// [`try_new_battle`] for validated teams (panics with the reason otherwise).
pub fn new_battle(p1: &Team, p2: &Team, seed: u64, rules: Rules) -> Battle {
    try_new_battle(p1, p2, seed, rules).unwrap_or_else(|e| panic!("invalid team for the native simulator: {e}"))
}

struct Reveal<'a>(&'a Battle);

impl RevealSource for Reveal<'_> {
    fn stored_moves(&self, player: usize, idx: usize) -> [u8; 4] {
        let m = &self.0.sides[player].mons[idx.min(5)];
        std::array::from_fn(|k| if k < m.n_moves as usize { m.moves[k].id } else { 0 })
    }
    fn active_idx(&self, player: usize) -> u8 {
        if self.0.started {
            self.0.sides[player].order[0]
        } else {
            UNREVEALED
        }
    }
}

fn map_event(e: &Event) -> Option<PEvent> {
    Some(match *e {
        Event::Switch { player, slot } => PEvent::Switch { player, slot },
        Event::Move { player, slot, mv, from } => PEvent::Move { player, slot, mv, from },
        Event::Cant { player, slot, reason, mv } => PEvent::Cant {
            player,
            slot,
            reason: match reason {
                Cant::Sleep => protocol::cant::SLEEP,
                Cant::Freeze => protocol::cant::FREEZE,
                Cant::Paralysis => protocol::cant::PARALYSIS,
                Cant::PartiallyTrapped => protocol::cant::BOUND,
                Cant::Flinch => protocol::cant::FLINCH,
                Cant::Disable => protocol::cant::DISABLE,
                Cant::Recharge => protocol::cant::RECHARGE,
            },
            mv,
        },
        Event::Miss { .. } => PEvent::Miss,
        Event::Crit => PEvent::Crit,
        Event::SuperEffective => PEvent::SuperEffective,
        Event::Resisted => PEvent::Resisted,
        Event::Immune => PEvent::Immune,
        Event::Fail => PEvent::Fail,
        Event::DisableStart { player, slot, mv } => PEvent::DisableStart { player, slot, mv },
        Event::MimicStart { player, mv } => PEvent::MimicStart { player, mv },
        Event::Transform { player } => PEvent::Transform { player },
        Event::Turn { .. } => PEvent::Turn,
        Event::Faint { .. } => return None,
    })
}

fn track_flags(b: &Battle) -> [TrackFlags; 2] {
    let binding = [b.sides[0].active.has(v::PARTIALTRAPPINGLOCK), b.sides[1].active.has(v::PARTIALTRAPPINGLOCK)];
    std::array::from_fn(|p| {
        let sd = &b.sides[p];
        let a = &sd.active;
        TrackFlags {
            confused: a.has(v::CONFUSION),
            disabled: a.has(v::DISABLE),
            toxic: a.has(v::RESIDUALDMG) && sd.mon().status == st::TOX,
            trapped: binding[0] || binding[1],
            asleep: std::array::from_fn(|i| sd.mons[i].status == st::SLP),
        }
    })
}

/// The observation-encoder view of a native battle (pkmn ids and stat orders).
pub fn view(b: &Battle) -> View {
    let mut out = View { turn: b.turn, ..Default::default() };
    for p in 0..2 {
        let sd = &b.sides[p];
        let sv = &mut out.sides[p];
        sv.size = sd.n;
        let active_slot = sd.order[0] as usize;
        let a = &sd.active;
        let toxic_active = b.started && a.has(v::RESIDUALDMG);
        for i in 0..sd.n as usize {
            let m = &sd.mons[i];
            let is_active = b.started && i == active_slot;
            let stat = match m.status {
                st::SLP => status::SLP,
                st::PSN => status::PSN,
                st::TOX => {
                    if is_active && toxic_active {
                        status::TOX
                    } else {
                        status::PSN
                    }
                }
                st::BRN => status::BRN,
                st::FRZ => status::FRZ,
                st::PAR => status::PAR,
                _ => status::NONE,
            };
            let mut mv = MonView {
                species: m.species,
                level: m.level,
                types: SPECIES[m.species as usize].types,
                hp: m.hp,
                max_hp: m.stats[0],
                status: stat,
                sleep_turns: if m.status == st::SLP { m.slp_time.max(0) as u8 } else { 0 },
                rest: m.status == st::SLP && m.status_self(),
                stats: [m.stats[0], m.stats[1], m.stats[2], m.stats[5], m.stats[3]],
                ..Default::default()
            };
            for k in 0..m.n_moves as usize {
                mv.moves[k] = m.moves[k].id;
                mv.pp[k] = m.moves[k].pp();
            }
            sv.mons[i] = mv;
        }
        if !b.started {
            sv.active.party_idx = UNREVEALED;
            continue;
        }
        let mut bits = 0u32;
        let mut set = |cond: bool, bit: u32| {
            if cond {
                bits |= bit;
            }
        };
        set(a.has(v::BIDE), vol::BIDE);
        set(a.has(v::LOCKEDMOVE), vol::THRASHING);
        set(a.has(v::TWOTURNMOVE), vol::CHARGING);
        set(a.has(v::PARTIALTRAPPINGLOCK), vol::BINDING);
        set(a.has(v::INVULNERABILITY), vol::INVULNERABLE);
        set(a.has(v::CONFUSION), vol::CONFUSION);
        set(a.has(v::MIST), vol::MIST);
        set(a.has(v::FOCUSENERGY), vol::FOCUS_ENERGY);
        set(a.has(v::SUBSTITUTE), vol::SUBSTITUTE);
        set(a.has(v::MUSTRECHARGE), vol::RECHARGING);
        set(a.has(v::RAGE), vol::RAGE);
        set(a.has(v::LEECHSEED), vol::LEECH_SEED);
        set(a.has(v::RESIDUALDMG) && sd.mon().status == st::TOX, vol::TOXIC);
        set(a.has(v::LIGHTSCREEN), vol::LIGHT_SCREEN);
        set(a.has(v::REFLECT), vol::REFLECT);
        set(a.transformed, vol::TRANSFORM);
        let mut act = ActiveView {
            party_idx: active_slot as u8,
            species: a.species,
            types: a.types,
            stats: [sd.mon().stats[0], a.modified[0], a.modified[1], a.modified[4], a.modified[2]],
            boosts: [a.boosts[0], a.boosts[1], a.boosts[4], a.boosts[2], a.boosts[5], a.boosts[6]],
            volatiles: bits,
            disabled_slot: if a.has(v::DISABLE) { a.disable_slot + 1 } else { 0 },
            disable_turns: if a.has(v::DISABLE) { a.disable_time.max(0) as u8 } else { 0 },
            confusion_turns: if a.has(v::CONFUSION) { a.confusion_time.max(0) as u8 } else { 0 },
            attacks_left: if a.has(v::LOCKEDMOVE) {
                a.lockedmove_time.max(0) as u8
            } else if a.has(v::BIDE) {
                a.bide_time.max(0) as u8
            } else if a.has(v::PARTIALTRAPPINGLOCK) {
                a.ptl_dur
            } else {
                0
            },
            substitute_hp: if a.has(v::SUBSTITUTE) { a.sub_hp.clamp(0, 255) as u8 } else { 0 },
            toxic_counter: if a.has(v::RESIDUALDMG) { a.residual_counter } else { 0 },
            ..Default::default()
        };
        for k in 0..a.n_slots as usize {
            let s = sd.slot(k);
            act.moves[k] = s.id;
            act.pp[k] = s.pp();
        }
        sv.active = act;
    }
    out
}

macro_rules! native_backend {
    ($t:ty, $rules:expr) => {
        impl Backend for $t {
            type Battle = Battle;
            type Choice = Choice;
            const BATTLE_BYTES: usize = sim_gen1::ser::BATTLE_BYTES;

            fn new_battle(p1: &Team, p2: &Team, seed: u64) -> Battle {
                new_battle(p1, p2, seed, $rules)
            }

            #[inline]
            fn choices(b: &Battle, player: usize, out: &mut [Choice]) -> usize {
                sim_core::Generation::choices(b, player, out)
            }

            #[inline]
            fn action_of(b: &Battle, player: usize, c: Choice) -> Action {
                match c {
                    Choice::Pass => Action::Pass,
                    Choice::Move(k) => Action::Move(k.saturating_sub(1)),
                    Choice::Switch(k) => Action::Switch(b.sides[player].order[k as usize - 1]),
                }
            }

            fn choice_move(b: &Battle, player: usize, c: Choice) -> u8 {
                let Choice::Move(k) = c else { return 0 };
                if b.sides[player].request != sim_core::Request::Move {
                    return 0;
                }
                let (moves, n, _) = b.request_moves(player);
                // Request entries are real moves, Struggle, or the Recharge/"Fight" pseudo moves.
                match (k as usize).checked_sub(1).and_then(|i| moves.get(i)) {
                    Some(m) if (k as usize) <= n && (1..=165).contains(&m.id) => m.id,
                    _ => 0,
                }
            }

            fn update(b: &mut Battle, c1: Choice, c2: Choice, mem: &mut [Memory; 2]) -> Outcome {
                let mut buf = EventBuf::default();
                let turn_before = b.turn;
                b.apply_choices_unchecked(c1, c2, &mut buf);
                reveal::apply_events(&Reveal(b), buf.as_slice().iter().filter_map(map_event), mem);
                if b.started {
                    reveal::update_trackers(&track_flags(b), b.turn > turn_before && turn_before > 0, mem);
                }
                match b.outcome() {
                    sim_core::Outcome::Ongoing => Outcome::Ongoing,
                    sim_core::Outcome::P1Win => Outcome::P1Win,
                    sim_core::Outcome::P2Win => Outcome::P2Win,
                    sim_core::Outcome::Tie => Outcome::Tie,
                }
            }

            fn battle_to_bytes(b: &Battle, out: &mut Vec<u8>) {
                b.to_bytes(out);
            }

            fn battle_from_bytes(bytes: &[u8]) -> Option<Battle> {
                Battle::from_bytes(bytes)
            }

            #[inline]
            fn turn(b: &Battle) -> u32 {
                b.turn as u32
            }

            fn view(b: &Battle) -> View {
                view(b)
            }
        }
    };
}

native_backend!(Native1, Rules::OU);
native_backend!(Native1Nc, Rules::NO_CLAUSES);
