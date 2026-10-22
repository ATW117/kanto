//! Backend-agnostic reveal bookkeeping: applying an update's public events to both players'
//! [`Memory`] and advancing the elapsed-turn trackers (docs/OBSERVATION_RULES.md §3).

use crate::data;
use crate::memory::{Memory, TurnEvents, UNREVEALED};
use crate::protocol::{self, Event};

const STRUGGLE: u8 = 165;

/// What the reveal logic needs to know about the battle (after the update).
pub trait RevealSource {
    /// Stored (base) moves of party member `idx` (original index).
    fn stored_moves(&self, player: usize, idx: usize) -> [u8; 4];
    /// Original party index of the active Pokémon (`UNREVEALED` before the start).
    fn active_idx(&self, player: usize) -> u8;
}

/// Apply one update's events to both players' memories (docs/OBSERVATION_RULES.md §3).
pub fn apply_events<R: RevealSource, I: IntoIterator<Item = Event>>(b: &R, events: I, mem: &mut [Memory; 2]) {
    let mut last = [TurnEvents::default(); 2];
    let mut attacker: Option<usize> = None;
    let mut first_mover: Option<usize> = None;
    // The attacker's move started a partial-trapping sequence (a miss ends it at once).
    let mut bind_start = false;
    for ev in events {
        match ev {
            Event::Switch { player, slot } => {
                let obs = &mut mem[1 - player];
                obs.reveal(slot);
                obs.opp_transformed = false;
                for m in mem.iter_mut() {
                    m.track[player] = Default::default();
                }
                // Partial trapping ends when either Pokémon leaves.
                end_binding(mem, 1 - player);
                last[player].switched = true;
            }
            Event::Move { player, slot, mv, from } => {
                attacker = Some(player);
                // Partial trapping lasts 2-5 hits and ends silently: a first hit or a continuation
                // (`[from]`) counts a hit, any other move ends the sequence.
                let binds = is_binding_move(mv);
                bind_start = binds && !from;
                for m in mem.iter_mut() {
                    let t = &mut m.track[player];
                    t.bind_hits = if !binds { 0 } else if from { t.bind_hits.saturating_add(1) } else { 1 };
                    t.binding = matches!(t.bind_hits, 1 | 2);
                    // A Pokémon that moves is not recharging; Gen 1 partial trapping negates the
                    // target's recharge turn, even on a miss.
                    t.recharging = false;
                    if binds {
                        m.track[1 - player].recharging = false;
                    }
                }
                let first = *first_mover.get_or_insert(player) == player;
                let e = &mut last[player];
                *e = TurnEvents {
                    moved: true,
                    moved_first: first,
                    mv,
                    called: from,
                    switched: e.switched,
                    ..Default::default()
                };
                let obs = &mut mem[1 - player];
                // Only selections (no `[from]`) reveal a slot and cost PP. Moves used while
                // transformed are not the user's own, nor are Struggle and the move Mimic copied
                // (a copy of a move the user also knows is indistinguishable: not counted).
                if !from && !obs.opp_transformed && mv != STRUGGLE && obs.track[player].mimic_move != mv {
                    obs.see_move(slot, mv, true);
                }
            }
            Event::LastMiss | Event::Miss => {
                if let Some(p) = attacker {
                    last[p].missed = true;
                    if std::mem::take(&mut bind_start) {
                        end_binding(mem, p);
                    }
                }
            }
            Event::MustRecharge { player } => {
                for m in mem.iter_mut() {
                    m.track[player].recharging = true;
                }
            }
            Event::SleepStart { player } => {
                // Gen 1: sleep induced on a Pokémon that must recharge cancels the recharge.
                for m in mem.iter_mut() {
                    m.track[player].recharging = false;
                }
            }
            Event::Faint { player } => {
                for m in mem.iter_mut() {
                    m.track[player].recharging = false;
                }
                end_binding(mem, 0);
                end_binding(mem, 1);
            }
            Event::ConfusionDamage { player } => end_binding(mem, player),
            Event::TrapEnd { player } => {
                // The foe stops binding (its hit count stands until its next move).
                for m in mem.iter_mut() {
                    m.track[1 - player].binding = false;
                }
            }
            Event::Crit | Event::SuperEffective | Event::Resisted | Event::Immune | Event::Fail => {
                if let Some(p) = attacker {
                    let e = &mut last[p];
                    match ev {
                        Event::Crit => e.crit = true,
                        Event::SuperEffective => e.super_effective = true,
                        Event::Resisted => e.resisted = true,
                        Event::Immune => e.immune = true,
                        _ => e.failed = true,
                    }
                }
            }
            Event::Cant { player, slot, reason, mv } => {
                last[player].cant = reason + 1;
                // Full paralysis ends the paralysed side's partial trapping.
                if reason == protocol::cant::PARALYSIS {
                    end_binding(mem, player);
                }
                if reason == protocol::cant::SLEEP {
                    for m in mem.iter_mut() {
                        let s = &mut m.sleep_obs[player][slot as usize % 6];
                        *s = s.saturating_add(1);
                    }
                }
                if reason == protocol::cant::RECHARGE {
                    for m in mem.iter_mut() {
                        m.track[player].recharging = false;
                    }
                }
                let obs = &mut mem[1 - player];
                if reason == protocol::cant::DISABLE && !obs.opp_transformed && obs.track[player].mimic_move != mv {
                    obs.see_move(slot, mv, false);
                }
            }
            Event::DisableStart { player, slot, mv } => {
                let obs = &mut mem[1 - player];
                if !obs.opp_transformed && obs.track[player].mimic_move != mv {
                    obs.see_move(slot, mv, false);
                }
            }
            Event::MimicStart { player, mv } => {
                for m in mem.iter_mut() {
                    m.track[player].mimic_move = mv;
                }
                // Mimic copies a random move of the foe's moveset: the Mimic user's owner learns it.
                let foe = 1 - player;
                let idx = b.active_idx(foe);
                let own = &mut mem[player];
                if idx != UNREVEALED && !own.opp_transformed && own.track[foe].mimic_move != mv {
                    own.see_move(idx, mv, false);
                }
            }
            Event::Transform { player } => {
                mem[1 - player].opp_transformed = true;
                // The transformer's owner now sees the target's full moveset.
                let foe = 1 - player;
                let idx = b.active_idx(foe);
                if idx != UNREVEALED {
                    for mv in b.stored_moves(foe, idx as usize) {
                        mem[player].see_move(idx, mv, false);
                    }
                }
            }
            Event::Turn => {}
        }
    }
    for m in mem.iter_mut() {
        m.last = last;
    }
}


/// Public per-side flags after an update, for [`update_trackers`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TrackFlags {
    pub confused: bool,
    pub disabled: bool,
    pub toxic: bool,
    /// Per original party index.
    pub asleep: [bool; 6],
}

/// Post-update bookkeeping from public state: elapsed counters advance once per new turn and
/// reset when the condition ends; observed sleep resets once a Pokémon is no longer asleep.
pub fn update_trackers(flags: &[TrackFlags; 2], turn_advanced: bool, mem: &mut [Memory; 2]) {
    // Partial trapping as each observer sees it (`SideTrack::binding`), not the hidden state.
    let trapped = mem.map(|m| m.track[0].binding || m.track[1].binding);
    for player in 0..2 {
        let f = flags[player];
        for (m, &trapped) in mem.iter_mut().zip(&trapped) {
            let t = &mut m.track[player];
            let step = |c: &mut u8, on: bool| *c = if on { c.saturating_add(turn_advanced as u8) } else { 0 };
            step(&mut t.confusion_elapsed, f.confused);
            step(&mut t.disable_elapsed, f.disabled);
            step(&mut t.trap_elapsed, trapped);
            step(&mut t.toxic_elapsed, f.toxic);
            for i in 0..6 {
                if !f.asleep[i] {
                    m.sleep_obs[player][i] = 0;
                }
            }
        }
    }
}

/// Wrap, Bind, Fire Spin or Clamp.
pub fn is_binding_move(mv: u8) -> bool {
    matches!(data::MOVE_NAMES.get(mv as usize).copied(), Some("Wrap" | "Bind" | "Fire Spin" | "Clamp"))
}

/// `player`'s partial-trapping sequence ended (in every observer's memory).
fn end_binding(mem: &mut [Memory; 2], player: usize) {
    for m in mem.iter_mut() {
        m.track[player].bind_hits = 0;
        m.track[player].binding = false;
    }
}

/// After an update's events: what each player's own request shows (a player whose request is a
/// lone "Fight" while awake and not frozen is partially trapped: it knows its foe is binding).
pub fn apply_requests(req: [RequestFlags; 2], mem: &mut [Memory; 2]) {
    for p in 0..2 {
        if req[p].lone_fight {
            mem[p].track[1 - p].binding = true;
        }
        // A move request shows whether the observer's own active must recharge.
        if let Some(r) = req[p].recharge {
            mem[p].track[p].recharging = r;
        }
    }
}

/// What a player's request (sent after the update) shows about its own active.
#[derive(Clone, Copy, Debug, Default)]
pub struct RequestFlags {
    /// A lone "Fight" while awake and not frozen: the foe is binding.
    pub lone_fight: bool,
    /// For a move request: whether it is a lone "Recharge".
    pub recharge: Option<bool>,
}
