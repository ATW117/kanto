//! Backend-agnostic reveal bookkeeping: applying an update's public events to both players'
//! [`Memory`] and advancing the elapsed-turn trackers (docs/OBSERVATION_RULES.md §3).

use crate::memory::{Memory, TurnEvents, UNREVEALED};
use crate::protocol::{self, Event};

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
    for ev in events {
        match ev {
            Event::Switch { player, slot } => {
                let obs = &mut mem[1 - player];
                obs.reveal(slot);
                obs.opp_transformed = false;
                for m in mem.iter_mut() {
                    m.track[player] = Default::default();
                }
                last[player].switched = true;
            }
            Event::Move { player, slot, mv, from } => {
                attacker = Some(player);
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
                // transformed are not the user's own; the stored-moveset check also filters
                // Mimic-copied moves and Struggle.
                if !from && !obs.opp_transformed && b.stored_moves(player, slot as usize).contains(&mv) {
                    obs.see_move(slot, mv, true);
                }
            }
            Event::LastMiss => {
                if let Some(p) = attacker {
                    last[p].missed = true;
                }
            }
            Event::Miss => {
                if let Some(p) = attacker {
                    last[p].missed = true;
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
                if reason == protocol::cant::SLEEP {
                    for m in mem.iter_mut() {
                        let s = &mut m.sleep_obs[player][slot as usize % 6];
                        *s = s.saturating_add(1);
                    }
                }
                if reason == protocol::cant::DISABLE && b.stored_moves(player, slot as usize).contains(&mv) {
                    mem[1 - player].see_move(slot, mv, false);
                }
            }
            Event::DisableStart { player, slot, mv } => {
                if b.stored_moves(player, slot as usize).contains(&mv) {
                    mem[1 - player].see_move(slot, mv, false);
                }
            }
            Event::MimicStart { player, mv } => {
                for m in mem.iter_mut() {
                    m.track[player].mimic_move = mv;
                }
                // Mimic copies a random move of the foe's moveset: the Mimic user's owner learns it.
                let foe = 1 - player;
                let idx = b.active_idx(foe);
                if idx != UNREVEALED && b.stored_moves(foe, idx as usize).contains(&mv) {
                    mem[player].see_move(idx, mv, false);
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
    /// Either side's active is partially trapping (binding).
    pub trapped: bool,
    /// Per original party index.
    pub asleep: [bool; 6],
}

/// Post-update bookkeeping from public state: elapsed counters advance once per new turn and
/// reset when the condition ends; observed sleep resets once a Pokémon is no longer asleep.
pub fn update_trackers(flags: &[TrackFlags; 2], turn_advanced: bool, mem: &mut [Memory; 2]) {
    for player in 0..2 {
        let f = flags[player];
        for m in mem.iter_mut() {
            let t = &mut m.track[player];
            let step = |c: &mut u8, on: bool| *c = if on { c.saturating_add(turn_advanced as u8) } else { 0 };
            step(&mut t.confusion_elapsed, f.confused);
            step(&mut t.disable_elapsed, f.disabled);
            step(&mut t.trap_elapsed, f.trapped);
            step(&mut t.toxic_elapsed, f.toxic);
            for i in 0..6 {
                if !f.asleep[i] {
                    m.sleep_obs[player][i] = 0;
                }
            }
        }
    }
}
