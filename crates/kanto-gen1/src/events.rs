//! Statuses, volatiles, damage/heal/boost, and the Gen 1 event handlers, written out as plain
//! functions. Where Showdown's `runEvent` would sort several handlers, the handlers present are
//! collected with their (order, priority, speed, subOrder) keys and sorted with Showdown's
//! `speedSort`, so ties consume the PRNG exactly as in Showdown.

use crate::battle::boosted_stat;
use crate::proto::{plog, BOOST_NAMES};
use crate::state::*;
use kanto_core::sort::{key, sort_by_key, KEY_ORDER_UNSET};
use kanto_core::{Event, Player, Sink};
use kanto_data::gen1::{self as data};

/// What caused damage (`effect` in `Battle#damage`); decides whether `lastDamage` is updated
/// and how the `-damage` line is tagged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dmg {
    Move(u8),
    Recoil,
    LeechSeed,
    /// Residual damage from a status (PSN/BRN/TOX): `[from] psn|brn`, plus `[of] source` when
    /// the event source is another Pokémon.
    Status,
}

/// The effect of a `Battle#directDamage` (the `-damage` tag and Gen 1's Substitute quirk).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direct {
    /// Confusion self-hit: `[from] confusion`, hits Substitutes the Gen 1 way.
    Confusion,
    /// Jump Kick / Hi Jump Kick crash damage: untagged, hits Substitutes the Gen 1 way.
    Crash,
    /// Anything else (Substitute's HP cost): untagged.
    Plain,
}

/// How a heal is reported (`Battle#heal`'s effect).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Heal {
    /// A move's own heal (Recover, Soft-Boiled): untagged.
    Move,
    /// Drain: `[from] drain|[of] <the damaged Pokémon>`.
    Drain(Player),
    /// Rest, Leech Seed: `[silent]`.
    Silent,
}

/// `Condition` handlers sort after `Status` ones at equal priority (`subOrder` 2 vs 0).
const SUB_STATUS: u8 = 0;
const SUB_CONDITION: u8 = 2;

/// Volatiles with `DisableMove` handlers.
const DISABLE_MOVE_VOLS: u32 = 1 << v::PARTIALLYTRAPPED | 1 << v::FAKEPARTIALLYTRAPPED | 1 << v::PARTIALTRAPPINGLOCK | 1 << v::BIDE | 1 << v::DISABLE;
/// Volatiles with `AfterMoveSelf` / `AfterMove` handlers.
const AFTER_MOVE_SELF_VOLS: u32 = 1 << v::LEECHSEED | 1 << v::RESIDUALDMG;
const AFTER_MOVE_VOLS: u32 = 1 << v::PARTIALTRAPPINGLOCK | 1 << v::MUSTRECHARGE | 1 << v::LOCKEDMOVE;
/// Volatiles with a `duration` (counted down by `fieldEvent('Residual')`).
const DURATION_VOLS: u32 = 1 << v::FLINCH | 1 << v::PARTIALLYTRAPPED | 1 << v::FAKEPARTIALLYTRAPPED | 1 << v::PARTIALTRAPPINGLOCK;

impl Battle {
    // ---- Volatiles ---------------------------------------------------------------------------

    #[inline]
    pub(crate) fn set_vol(&mut self, p: Player, id: u8) {
        let a = &mut self.sides[p].active;
        if a.next_stamp == u8::MAX {
            renumber_stamps(a);
        }
        a.vols |= 1 << id;
        a.vstamp[id as usize] = a.next_stamp;
        a.next_stamp += 1;
    }

    /// `delete pokemon.volatiles[id]` (no End event, no HP check).
    #[inline]
    pub(crate) fn del_vol(&mut self, p: Player, id: u8) {
        self.sides[p].active.vols &= !(1 << id);
    }

    /// `Pokemon#removeVolatile` (with the Gen 1 `End` messages: confusion, Disable, Substitute).
    pub(crate) fn remove_volatile<S: Sink>(&mut self, p: Player, id: u8, s: &mut S) -> bool {
        if self.sides[p].mon().hp == 0 || !self.sides[p].active.has(id) {
            return false;
        }
        self.del_vol(p, id);
        let what = match id {
            v::CONFUSION => "confusion",
            v::DISABLE => "Disable",
            v::SUBSTITUTE => "Substitute",
            _ => "",
        };
        if !what.is_empty() {
            plog!(s, |l| {
                let i = self.ident(l, p);
                l.add(format!("|-end|{i}|{what}"));
            });
        }
        true
    }

    /// `Pokemon#addVolatile(id, source, sourceEffect)`; `effect` is the move that caused it (for
    /// `effectState.move` and friends). Returns Showdown's result: `Some(true)` added/restarted,
    /// `Some(false)` failed, `None` a `null` result (Minimize/Defense Curl restart).
    pub(crate) fn add_volatile<S: Sink>(&mut self, p: Player, id: u8, source: Player, effect: u8, s: &mut S) -> Option<bool> {
        let q = 1 - p;
        if self.sides[p].mon().hp == 0 {
            return Some(false);
        }
        if self.sides[p].active.has(id) {
            return match id {
                v::PARTIALLYTRAPPED => {
                    self.sides[p].active.pt_dur = 2;
                    Some(true)
                }
                v::MINIMIZE | v::DEFENSECURL => None,
                _ => Some(false),
            };
        }
        // runStatusImmunity: only `fainted` matters for volatiles in Gen 1.
        if self.sides[p].mon().fainted() {
            return Some(false);
        }
        self.set_vol(p, id);
        // onStart messages.
        let start = match id {
            v::CONFUSION if effect == 0 => "confusion|[silent]",
            v::CONFUSION => "confusion",
            v::LEECHSEED => "move: Leech Seed",
            v::MIST => "Mist",
            v::LIGHTSCREEN => "Light Screen",
            v::REFLECT => "Reflect",
            v::FOCUSENERGY => "move: Focus Energy",
            v::BIDE => "Bide",
            v::SUBSTITUTE => "Substitute",
            _ => "",
        };
        if !start.is_empty() {
            plog!(s, |l| {
                let i = self.ident(l, p);
                l.add(format!("|-start|{i}|{start}"));
            });
        }
        if id == v::TWOTURNMOVE {
            plog!(s, |l| l.attr_last_move("[still]"));
        }
        match id {
            v::CONFUSION => {
                let t = self.rng.random_range(2, 6, "confusion.onStart") as i8;
                self.sides[p].active.confusion_time = t;
            }
            v::FLINCH => {
                self.sides[p].active.flinch_dur = 1;
                self.remove_volatile(p, v::MUSTRECHARGE, s);
            }
            v::PARTIALLYTRAPPED => {
                let pt_source = self.sides[source].order[0];
                let a = &mut self.sides[p].active;
                a.pt_dur = 2;
                a.pt_maybe_locked = false;
                a.pt_source = pt_source;
                if source == p {
                    // (never happens: the trapper is the foe)
                }
            }
            v::FAKEPARTIALLYTRAPPED => self.sides[p].active.fpt_dur = 2,
            v::PARTIALTRAPPINGLOCK => {
                let dur = [2u8, 2, 2, 3, 3, 3, 4, 5][self.rng.sample(8, "partialtrappinglock.duration") as usize];
                // onStart: needs a foe with HP.
                if self.sides[q].active.species == 0 || self.sides[q].mon().hp == 0 {
                    self.del_vol(p, id);
                    return Some(false);
                }
                let last_damage = self.last_damage;
                let locked = self.sides[q].order[0];
                let a = &mut self.sides[p].active;
                a.ptl_dur = dur;
                a.ptl_total = dur;
                a.ptl_move = effect;
                a.ptl_damage = last_damage;
                a.ptl_locked = locked;
                self.add_volatile(q, v::PARTIALLYTRAPPED, p, effect, s);
            }
            v::LOCKEDMOVE => {
                let t = self.rng.random_range(2, 4, "lockedmove.onStart") as i8;
                let a = &mut self.sides[p].active;
                a.lockedmove_move = effect;
                a.lockedmove_time = t;
                a.lockedmove_acc = 255;
            }
            v::TWOTURNMOVE => self.sides[p].active.twoturn_move = effect,
            v::BIDE => {
                let t = self.rng.random_range(2, 4, "bide.onStart") as i8;
                let a = &mut self.sides[p].active;
                a.bide_damage = 0;
                a.bide_time = t;
            }
            v::DISABLE => {
                // Sample a move slot with PP left.
                let sd = &self.sides[p];
                let mut cand = [0u8; 4];
                let mut n = 0;
                for i in 0..sd.active.n_slots as usize {
                    if sd.slot(i).pp() > 0 {
                        cand[n] = i as u8;
                        n += 1;
                    }
                }
                let k = self.rng.sample(n as u32, "disable.onStart") as usize;
                let slot = cand[k];
                let mv = self.sides[p].slot(slot as usize).id;
                plog!(s, |l| {
                    let i = self.ident(l, p);
                    l.add(format!("|-start|{i}|Disable|{}", crate::proto::move_name(mv)));
                });
                let t = self.rng.random_range(1, 9, "disable.onStart") as i8;
                let a = &mut self.sides[p].active;
                a.disable_move = mv;
                a.disable_slot = slot;
                a.disable_time = t;
            }
            v::SUBSTITUTE => {
                let maxhp = self.sides[p].mon().stats[0];
                self.sides[p].active.sub_hp = (maxhp / 4) as i32 + 1;
                if self.sides[p].active.has(v::PARTIALLYTRAPPED) {
                    s.event(Event::TrapEnd { player: p });
                    // `-end|X|<trapping move>|[partiallytrapped]|[silent]`: the foe's move.
                    let mv = self.sides[1 - p].active.ptl_move;
                    plog!(s, |l| {
                        let i = self.ident(l, p);
                        l.add(format!("|-end|{i}|{}|[partiallytrapped]|[silent]", crate::proto::move_name(mv)));
                    });
                }
                self.del_vol(p, v::PARTIALLYTRAPPED);
            }
            v::RAGE => self.sides[p].active.rage_acc = 255,
            v::RESIDUALDMG => self.sides[p].active.residual_counter = 0,
            _ => {}
        }
        Some(true)
    }

    // ---- Status ------------------------------------------------------------------------------

    /// `Pokemon#setStatus(status, source, sourceEffect, ignoreImmunities)` for the active of `p`.
    /// `status_move` says whether the source effect is a move with a `status` of its own (a
    /// status move such as Toxic, not a secondary effect): only then does Showdown print `-fail`
    /// (status already set) or `-immune` (Poison types vs poison).
    /// `effect_move` is the source effect when it is a move (`[from] move: X` on sleep), else 0.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_status<S: Sink>(&mut self, p: Player, status: u8, source: Player, ignore_immunities: bool, status_move: bool, effect_move: u8, s: &mut S) -> bool {
        let sd = &self.sides[p];
        if sd.mon().hp == 0 {
            return false;
        }
        if sd.mon().status == status {
            if status_move {
                s.event(Event::Fail);
                plog!(s, |l| {
                    let i = self.ident(l, p);
                    l.add(format!("|-fail|{i}|{}", data::status::IDS[status as usize]));
                });
            }
            return false;
        }
        if !ignore_immunities {
            let immune = sd.mon().fainted() || (matches!(status, st::PSN | st::TOX) && sd.active.types.contains(&data_type_poison()));
            if immune {
                if status_move {
                    s.event(Event::Immune);
                    plog!(s, |l| {
                        let i = self.ident(l, p);
                        l.add(format!("|-immune|{i}"));
                    });
                }
                return false;
            }
        }
        if !self.set_status_event(p, status, source, s) {
            return false;
        }
        let self_inflicted = source == p;
        let m = self.sides[p].mon_mut();
        m.status = status;
        m.set_status_self(self_inflicted);
        m.slp_time = 0;
        m.slp_start = 0;
        if status == st::SLP {
            s.event(Event::SleepStart { player: p });
        }
        plog!(s, |l| {
            let i = self.ident(l, p);
            if status == st::SLP && effect_move != 0 {
                l.add(format!("|-status|{i}|slp|[from] move: {}", crate::proto::move_name(effect_move)));
            } else {
                l.add(format!("|-status|{i}|{}", data::status::IDS[status as usize]));
            }
        });
        match status {
            st::SLP => {
                let t = self.rng.random_range(1, 8, "slp.onStart") as u8;
                let m = self.sides[p].mon_mut();
                m.slp_start = t;
                m.slp_time = t as i8;
            }
            st::TOX => {
                if !self.sides[p].active.has(v::RESIDUALDMG) {
                    self.add_volatile(p, v::RESIDUALDMG, p, 0, s);
                }
                self.sides[p].active.residual_counter = 0;
            }
            _ => {}
        }
        true
    }

    /// `runEvent('SetStatus')`: Sleep/Freeze Clause Mod handlers (field pseudo-weathers in
    /// 0.11.11). With both clauses their handlers tie on (priority 0, subOrder 5) in vanilla
    /// Showdown and get shuffled.
    fn set_status_event<S: Sink>(&mut self, p: Player, status: u8, source: Player, s: &mut S) -> bool {
        let r = self.rules;
        if r.sleep_clause && r.freeze_clause && r.compat == Compat::Vanilla {
            let k = key(KEY_ORDER_UNSET, 0, 0, 5);
            let mut l = [(k, 0u8), (k, 1u8)];
            sort_by_key(&mut self.rng, &mut l, "SetStatus.speedSort");
        }
        if source == p {
            return true;
        }
        let sd = &self.sides[p];
        if r.sleep_clause && status == st::SLP {
            for i in 0..sd.n as usize {
                let m = &sd.mons[i];
                if m.hp > 0 && m.status == st::SLP && !m.status_self() {
                    plog!(s, |l| {
                        l.add("|-message|Sleep Clause Mod activated.".into());
                        l.hint("Sleep Clause Mod prevents players from putting more than one of their opponent's Pokémon to sleep at a time", false, None);
                    });
                    return false;
                }
            }
        }
        if r.freeze_clause && status == st::FRZ {
            for i in 0..sd.n as usize {
                if sd.mons[i].status == st::FRZ {
                    plog!(s, |l| l.add("|-message|Freeze Clause activated.".into()));
                    return false;
                }
            }
        }
        true
    }

    /// `Pokemon#cureStatus(silent)`.
    pub(crate) fn cure_status<S: Sink>(&mut self, p: Player, silent: bool, s: &mut S) -> bool {
        let m = self.sides[p].mon();
        if m.hp == 0 || m.status == st::NONE {
            return false;
        }
        let old = m.status;
        plog!(s, |l| {
            let i = self.ident(l, p);
            let tag = if silent { "[silent]" } else { "[msg]" };
            l.add(format!("|-curestatus|{i}|{}|{tag}", data::status::IDS[(old as usize).min(6)]));
        });
        let m = self.sides[p].mon_mut();
        m.status = st::NONE;
        m.slp_time = 0;
        m.slp_start = 0;
        m.set_status_self(false);
        true
    }

    // ---- Damage / heal -----------------------------------------------------------------------

    /// `Pokemon#damage`: subtract HP, queue a faint at 0. Returns HP lost.
    pub(crate) fn mon_damage(&mut self, p: Player, d: u32) -> u32 {
        let hp = self.sides[p].mon().hp as i64;
        if hp == 0 || d == 0 {
            return 0;
        }
        let new = hp - d as i64;
        if new <= 0 {
            self.sides[p].mon_mut().hp = 0;
            self.faint(p);
            hp as u32
        } else {
            self.sides[p].mon_mut().hp = new as u16;
            d
        }
    }

    /// `Battle#damage` (`spreadDamage` for one target). Returns the damage dealt.
    pub(crate) fn damage<S: Sink>(&mut self, d: u32, p: Player, source: Option<Player>, effect: Dmg, s: &mut S) -> u32 {
        if self.sides[p].mon().hp == 0 {
            return 0;
        }
        // (isActive is always true for the targets Gen 1 damages.)
        let d = if d != 0 { d.max(1) } else { 0 };
        if !matches!(effect, Dmg::Recoil | Dmg::LeechSeed | Dmg::Status) {
            self.last_damage = d;
        }
        // (The status tag is read before the damage.)
        let status = self.sides[p].mon().status;
        let dealt = self.mon_damage(p, d);
        plog!(s, |l| {
            let rest = match effect {
                Dmg::Move(_) => String::new(),
                Dmg::Recoil => "|[from] Recoil".into(),
                Dmg::LeechSeed => format!("|[from] Leech Seed|[of] {}", self.ident(l, source.unwrap_or(1 - p))),
                Dmg::Status => {
                    let name = if status == st::BRN { "brn" } else { "psn" };
                    match source {
                        Some(src) if src != p => format!("|[from] {name}|[of] {}", self.ident(l, src)),
                        _ => format!("|[from] {name}"),
                    }
                }
            };
            self.add_health(l, "-damage", p, &rest);
        });
        if dealt != 0 {
            if let (Dmg::Move(m), Some(src)) = (effect, source) {
                if data::MOVES[m as usize].drain {
                    let amount = (dealt / 2).max(1);
                    self.last_damage = amount;
                    self.heal(amount, src, Heal::Drain(p), s);
                }
            }
        }
        dealt
    }

    /// `Battle#directDamage` with Gen 1's confusion/Jump Kick Substitute behaviour when `special`.
    pub(crate) fn direct_damage<S: Sink>(&mut self, d: u32, p: Player, kind: Direct, s: &mut S) -> u32 {
        if self.sides[p].mon().hp == 0 || d == 0 {
            return 0;
        }
        let d = d.max(1);
        if kind != Direct::Plain {
            self.last_damage = d;
            if self.sides[p].active.has(v::SUBSTITUTE) {
                const HINT: &str = "In Gen 1, if a Pokemon with a Substitute hurts itself due to confusion or Jump Kick/Hi Jump Kick recoil and the target";
                let q = 1 - p;
                if self.sides[q].active.has(v::SUBSTITUTE) {
                    self.sides[q].active.sub_hp -= d as i32;
                    if self.sides[q].active.sub_hp <= 0 {
                        self.remove_volatile(q, v::SUBSTITUTE, s);
                        self.sides[q].mon_mut().set_sub_fainted(true);
                    } else {
                        plog!(s, |l| {
                            let i = self.ident(l, q);
                            l.add(format!("|-activate|{i}|Substitute|[damage]"));
                        });
                    }
                    plog!(s, |l| l.hint(&format!("{HINT} has a Substitute, the target's Substitute takes the damage."), false, None));
                    return d;
                }
                plog!(s, |l| l.hint(&format!("{HINT} does not have a Substitute there is no damage dealt."), false, None));
                return 0;
            }
        }
        let dealt = self.mon_damage(p, d);
        if kind == Direct::Confusion {
            s.event(Event::ConfusionDamage { player: p });
        }
        plog!(s, |l| self.add_health(l, "-damage", p, if kind == Direct::Confusion { "|[from] confusion" } else { "" }));
        dealt
    }

    /// `Battle#heal`. Returns the HP restored (0 = failed).
    pub(crate) fn heal<S: Sink>(&mut self, d: u32, p: Player, how: Heal, s: &mut S) -> u32 {
        let d = if d != 0 && d <= 1 { 1 } else { d };
        if d == 0 {
            return 0;
        }
        let m = self.sides[p].mon();
        if m.hp == 0 || m.hp >= m.stats[0] {
            return 0;
        }
        let max = m.stats[0] as u32;
        let new = (m.hp as u32 + d).min(max);
        let healed = new - m.hp as u32;
        self.sides[p].mon_mut().hp = new as u16;
        plog!(s, |l| {
            let rest = match how {
                Heal::Move => String::new(),
                Heal::Drain(of) => format!("|[from] drain|[of] {}", self.ident(l, of)),
                Heal::Silent => "|[silent]".into(),
            };
            self.add_health(l, "-heal", p, &rest);
        });
        healed
    }

    /// `applyRecoilDamage` (Gen 4 mod, inherited by Gen 1: rounds down).
    pub(crate) fn apply_recoil<S: Sink>(&mut self, dealt: u32, mv: u8, p: Player, s: &mut S) {
        let r = data::MOVES[mv as usize].recoil;
        if r[1] == 0 {
            return;
        }
        let recoil = (dealt as u64 * r[0] as u64 / r[1] as u64).max(1) as u32;
        self.damage(recoil, p, Some(p), Dmg::Recoil, s);
    }

    // ---- Boosts ------------------------------------------------------------------------------

    /// Gen 1 `Battle#boost`. `effect_is_move_damaging` describes the source effect for Mist:
    /// `Some(true)` a damaging move, `Some(false)` a status move, `None` a non-move effect (Rage).
    /// Returns `success` (`true` or `null` = false).
    pub(crate) fn boost<S: Sink>(&mut self, boosts: [i8; 7], p: Player, source: Option<Player>, effect_damaging: Option<bool>, s: &mut S) -> bool {
        if self.sides[p].mon().hp == 0 {
            return false;
        }
        let mut b = boosts;
        // runEvent('TryBoost'): Mist.
        if self.sides[p].active.has(v::MIST) && effect_damaging != Some(true) {
            if let Some(src) = source {
                if src != p {
                    let mut shown = false;
                    for x in b.iter_mut() {
                        if *x < 0 {
                            *x = 0;
                            shown = true;
                        }
                    }
                    // (Gen 1 status moves have no secondaries, so the message always shows.)
                    if shown {
                        plog!(s, |l| {
                            let i = self.ident(l, p);
                            l.add(format!("|-activate|{i}|move: Mist"));
                        });
                    }
                }
            }
        }
        let mut success = false;
        let from = if effect_damaging.is_none() { "|[from] Rage" } else { "" };
        for (i, &delta) in b.iter().enumerate() {
            if delta == 0 {
                continue;
            }
            let r = self.boost_by(p, i, delta);
            if r == BoostResult::Changed {
                success = true;
            }
            if r != BoostResult::No {
                plog!(s, |l| {
                    let id = self.ident(l, p);
                    let (msg, second) = if delta < 0 { ("-unboost", "-boost") } else { ("-boost", "-unboost") };
                    l.add(format!("|{msg}|{id}|{}|{}{from}", BOOST_NAMES[i], delta.unsigned_abs()));
                    if r == BoostResult::Zero {
                        l.add(format!("|{second}|{id}|{}|1", BOOST_NAMES[i]));
                        if delta > 0 {
                            l.hint("In Gen 1, boosting a stat at 999 will apply a -1 drop afterwards, and the stat remains at 999.", true, None);
                        } else {
                            l.hint("In Gen 1, dropping a stat at 1 will apply a +1 boost afterwards, and the stat remains at 1.", true, None);
                        }
                    }
                });
            }
        }
        success
    }

    /// Gen 1 `Pokemon#boostBy` for one stat.
    fn boost_by(&mut self, p: Player, i: usize, delta: i8) -> BoostResult {
        let a = &mut self.sides[p].active;
        if delta > 0 && a.boosts[i] >= 6 {
            return BoostResult::No;
        }
        if delta < 0 && a.boosts[i] <= -6 {
            return BoostResult::No;
        }
        if i >= 5 {
            a.boosts[i] = (a.boosts[i] + delta).clamp(-6, 6);
            return BoostResult::Changed;
        }
        let mut changed = BoostResult::No;
        if delta > 0 {
            if a.modified[i] == 999 {
                a.boosts[i] = (a.boosts[i] + delta).min(6) - 1;
                changed = BoostResult::Zero;
            } else {
                a.boosts[i] = (a.boosts[i] + delta).min(6);
                changed = BoostResult::Changed;
            }
        }
        if delta < 0 {
            if a.modified[i] == 1 {
                a.boosts[i] = (a.boosts[i] + delta).max(-6) + 1;
                changed = BoostResult::Zero;
            } else {
                a.boosts[i] = (a.boosts[i] + delta).max(-6);
                changed = BoostResult::Changed;
            }
        }
        if changed == BoostResult::Changed {
            a.modified[i] = a.stored[i];
            let bst = a.boosts[i];
            a.modified[i] = boosted_stat(a.modified[i], bst);
            if delta > 0 && a.modified[i] > 999 {
                a.modified[i] = 999;
            }
        }
        changed
    }

    /// Gen 1 `Pokemon#clearBoosts`.
    pub(crate) fn clear_boosts(&mut self, p: Player) {
        let a = &mut self.sides[p].active;
        a.boosts = [0; 7];
        a.modified = a.stored;
    }

    // ---- Event dispatch ----------------------------------------------------------------------

    /// Sort key for a handler on `p`'s active Pokémon.
    #[inline]
    fn hkey(&self, p: Player, priority: i8, sub: u8) -> u64 {
        key(KEY_ORDER_UNSET, priority, self.sides[p].active.speed, sub)
    }

    /// `runEvent('DisableMove', pokemon)` at end of turn.
    #[inline(always)]
    pub(crate) fn disable_move_event(&mut self, p: Player) {
        if !matches!(self.sides[p].mon().status, st::SLP | st::FRZ) && self.sides[p].active.vols & DISABLE_MOVE_VOLS == 0 {
            return;
        }
        self.disable_move_handlers(p);
    }

    #[inline(never)]
    fn disable_move_handlers(&mut self, p: Player) {
        let mut list = [(0u64, 0u8); 8];
        let mut n = 0;
        let status = self.sides[p].mon().status;
        if matches!(status, st::SLP | st::FRZ) {
            list[n] = (self.hkey(p, 0, SUB_STATUS), 100);
            n += 1;
        }
        let disable_prio = if self.rules.compat == Compat::Pkmn { 7 } else { 0 };
        for id in self.vols_in_order(p, DISABLE_MOVE_VOLS) {
            let prio = match id {
                v::PARTIALLYTRAPPED | v::FAKEPARTIALLYTRAPPED => 1,
                v::PARTIALTRAPPINGLOCK | v::BIDE => 0,
                v::DISABLE => disable_prio,
                _ => continue,
            };
            list[n] = (self.hkey(p, prio, SUB_CONDITION), id);
            n += 1;
        }
        sort_by_key(&mut self.rng, &mut list[..n], "DisableMove.speedSort");
        for &(_, h) in &list[..n] {
            let a = &self.sides[p].active;
            let (pt_maybe_locked, ptl_total, ptl_dur, disable_slot, n_slots) = (a.pt_maybe_locked, a.ptl_total, a.ptl_dur, a.disable_slot, a.n_slots);
            let mon = self.sides[p].mon_mut();
            match h {
                100 | v::BIDE => mon.set_maybe_locked(false),
                v::PARTIALLYTRAPPED => {
                    if pt_maybe_locked {
                        mon.set_maybe_locked(true);
                    }
                }
                v::FAKEPARTIALLYTRAPPED => mon.set_maybe_locked(true),
                v::PARTIALTRAPPINGLOCK => {
                    if ptl_total as i32 - ptl_dur as i32 > 1 {
                        mon.set_maybe_locked(true);
                    }
                }
                v::DISABLE => {
                    let slot = disable_slot as usize;
                    if (n_slots as usize) > slot {
                        self.sides[p].slot_mut(slot).set_disabled(true);
                    }
                }
                _ => {}
            }
        }
    }

    /// The active's volatiles in insertion order.
    pub fn vol_order(&self, p: Player) -> VolIter {
        assert!(p < 2, "player {p} out of range (0 or 1)");
        self.vols_in_order(p, u32::MAX)
    }

    /// The active's volatiles among `mask` (`1 << v::*` bits) in insertion order: the sequence
    /// [`Battle::vol_order`] gives, filtered to `mask`, so handler lists keep Showdown's
    /// discovery order.
    #[inline]
    pub(crate) fn vols_in_order(&self, p: Player, mask: u32) -> VolIter {
        let a = &self.sides[p].active;
        let mut ids = [0u8; v::COUNT];
        let mut n = 0;
        let mut bits = a.vols & mask;
        while bits != 0 {
            let id = bits.trailing_zeros() as u8;
            bits &= bits - 1;
            ids[n] = id;
            n += 1;
        }
        // Stamps increase with insertion ([`renumber_stamps`] keeps them below 255) and are
        // distinct among present volatiles: an insertion sort by stamp (lists are short).
        for i in 1..n {
            let x = ids[i];
            let k = a.vstamp[x as usize];
            let mut j = i;
            while j > 0 && a.vstamp[ids[j - 1] as usize] > k {
                ids[j] = ids[j - 1];
                j -= 1;
            }
            ids[j] = x;
        }
        VolIter { ids, n, i: 0 }
    }

    /// `fieldEvent('Residual')`: duration countdown of Gen 1's duration-based volatiles.
    #[inline(always)]
    pub(crate) fn residual_event<S: Sink>(&mut self, s: &mut S) {
        // (An unstarted side has no volatiles.)
        if (self.sides[0].active.vols | self.sides[1].active.vols) & DURATION_VOLS == 0 {
            return;
        }
        self.residual_handlers(s);
    }

    #[inline(never)]
    fn residual_handlers<S: Sink>(&mut self, s: &mut S) {
        let mut list = [(0u64, (0u8, 0u8)); 8];
        let mut n = 0;
        for p in 0..2 {
            if self.sides[p].active.species == 0 {
                continue;
            }
            for id in self.vols_in_order(p, DURATION_VOLS) {
                let dur = self.vol_duration(p, id);
                if dur == 0 {
                    continue;
                }
                list[n] = (self.hkey(p, 0, SUB_CONDITION), (p as u8, id));
                n += 1;
            }
        }
        if self.rules.compat == Compat::Vanilla {
            sort_by_key(&mut self.rng, &mut list[..n], "Residual.speedSort");
        }
        for &(_, (p, id)) in &list[..n] {
            let p = p as usize;
            if self.sides[p].mon().fainted() || !self.sides[p].active.has(id) {
                continue;
            }
            let dur = self.vol_duration(p, id);
            if dur == 0 {
                continue;
            }
            let dur = dur - 1;
            self.set_vol_duration(p, id, dur);
            if dur == 0 {
                self.remove_volatile(p, id, s);
            }
            self.faint_messages(s);
            if self.ended {
                return;
            }
        }
    }

    fn vol_duration(&self, p: Player, id: u8) -> u8 {
        let a = &self.sides[p].active;
        match id {
            v::FLINCH => a.flinch_dur,
            v::PARTIALLYTRAPPED => a.pt_dur,
            v::FAKEPARTIALLYTRAPPED => a.fpt_dur,
            v::PARTIALTRAPPINGLOCK => a.ptl_dur,
            _ => 0,
        }
    }

    fn set_vol_duration(&mut self, p: Player, id: u8, d: u8) {
        let a = &mut self.sides[p].active;
        match id {
            v::FLINCH => a.flinch_dur = d,
            v::PARTIALLYTRAPPED => a.pt_dur = d,
            v::FAKEPARTIALLYTRAPPED => a.fpt_dur = d,
            v::PARTIALTRAPPINGLOCK => a.ptl_dur = d,
            _ => {}
        }
    }

    /// `runEvent('AfterSwitchInSelf')`: residual damage on switch-in.
    pub(crate) fn after_switch_in_self<S: Sink>(&mut self, p: Player, s: &mut S) {
        let m = self.sides[p].mon();
        if matches!(m.status, st::PSN | st::BRN | st::TOX) {
            let d = (m.stats[0] / 16).max(1) as u32;
            self.damage(d, p, None, Dmg::Status, s);
        }
    }

    /// `runEvent('AfterMoveSelf', pokemon)`: residualdmg counter (100), sleep wake-up (3),
    /// PSN/BRN/TOX damage (2), Leech Seed (1).
    /// `target` is the event source (the move's target): `[of]` on PSN/BRN damage.
    #[inline(always)]
    pub(crate) fn after_move_self<S: Sink>(&mut self, p: Player, target: Option<Player>, s: &mut S) {
        if !matches!(self.sides[p].mon().status, st::SLP | st::PSN | st::BRN | st::TOX) && self.sides[p].active.vols & AFTER_MOVE_SELF_VOLS == 0 {
            return;
        }
        self.after_move_self_handlers(p, target, s);
    }

    #[inline(never)]
    fn after_move_self_handlers<S: Sink>(&mut self, p: Player, target: Option<Player>, s: &mut S) {
        const VOLS: u32 = AFTER_MOVE_SELF_VOLS;
        let mut list = [(0u64, 0u8); 4];
        let mut n = 0;
        let status = self.sides[p].mon().status;
        match status {
            st::SLP => {
                list[n] = (self.hkey(p, 3, SUB_STATUS), 101);
                n += 1;
            }
            st::PSN | st::BRN | st::TOX => {
                list[n] = (self.hkey(p, 2, SUB_STATUS), 102);
                n += 1;
            }
            _ => {}
        }
        for id in self.vols_in_order(p, VOLS) {
            let prio = match id {
                v::LEECHSEED => 1,
                v::RESIDUALDMG => 100,
                _ => continue,
            };
            list[n] = (self.hkey(p, prio, SUB_CONDITION), id);
            n += 1;
        }
        sort_by_key(&mut self.rng, &mut list[..n], "AfterMoveSelf.speedSort");
        for &(_, h) in &list[..n] {
            match h {
                101 => {
                    // slp: wake up at the end of the turn the counter hits 0.
                    if self.sides[p].mon().status != st::SLP {
                        continue;
                    }
                    if self.sides[p].mon().slp_time <= 0 {
                        self.cure_status(p, false, s);
                    }
                }
                102 => {
                    let m = self.sides[p].mon();
                    if !matches!(m.status, st::PSN | st::BRN | st::TOX) || m.status != status {
                        continue;
                    }
                    let base = (m.stats[0] / 16).max(1) as u32;
                    let a = &self.sides[p].active;
                    let counter = a.has(v::RESIDUALDMG);
                    let d = if status == st::TOX {
                        base * a.residual_counter as u32
                    } else if counter {
                        base * a.residual_counter as u32
                    } else {
                        base
                    };
                    // Toxic passes the Pokémon itself as the source; PSN/BRN take the event's.
                    let src = if status == st::TOX { Some(p) } else { target };
                    self.damage(d, p, src, Dmg::Status, s);
                    if status != st::TOX && counter {
                        plog!(s, |l| l.hint("In Gen 1, Toxic's counter is retained after Rest and applies to PSN/BRN.", true, None));
                    }
                }
                v::RESIDUALDMG => {
                    if !self.sides[p].active.has(v::RESIDUALDMG) {
                        continue;
                    }
                    if matches!(self.sides[p].mon().status, st::BRN | st::PSN | st::TOX) {
                        let a = &mut self.sides[p].active;
                        a.residual_counter = a.residual_counter.wrapping_add(1);
                    }
                }
                v::LEECHSEED => {
                    if !self.sides[p].active.has(v::LEECHSEED) {
                        continue;
                    }
                    let q = 1 - p;
                    let lm = self.sides[q].mon();
                    if self.sides[q].active.species == 0 || lm.fainted() || lm.hp == 0 {
                        continue;
                    }
                    let mut counter = 1u32;
                    if self.sides[p].active.has(v::RESIDUALDMG) {
                        let a = &mut self.sides[p].active;
                        a.residual_counter = a.residual_counter.wrapping_add(1);
                        counter = a.residual_counter as u32;
                    }
                    let to_leech = (self.sides[p].mon().stats[0] / 16).max(1) as u32 * counter;
                    let dealt = self.damage(to_leech, p, Some(q), Dmg::LeechSeed, s);
                    let had_counter = self.sides[p].active.has(v::RESIDUALDMG);
                    plog!(s, |l| {
                        if had_counter {
                            l.hint("In Gen 1, Leech Seed's damage is affected by Toxic's counter.", true, None);
                        }
                        if dealt == 0 || to_leech > dealt {
                            l.hint("In Gen 1, Leech Seed recovery is not limited by the remaining HP of the seeded Pokemon.", true, None);
                        }
                    });
                    self.heal(to_leech, q, Heal::Silent, s);
                }
                _ => {}
            }
        }
    }

    /// `runEvent('AfterMove', pokemon, target)`: Wrap lock bookkeeping, Hyper Beam recharge
    /// message, Thrash end. `target_alive` is `target.hp > 0`; the target is always the foe.
    #[inline(always)]
    pub(crate) fn after_move<S: Sink>(&mut self, p: Player, target: Player, s: &mut S) {
        if self.sides[p].active.vols & AFTER_MOVE_VOLS == 0 {
            return;
        }
        self.after_move_handlers(p, target, s);
    }

    #[inline(never)]
    fn after_move_handlers<S: Sink>(&mut self, p: Player, target: Player, s: &mut S) {
        const VOLS: u32 = AFTER_MOVE_VOLS;
        let mut list = [(0u64, 0u8); 4];
        let mut n = 0;
        for id in self.vols_in_order(p, VOLS) {
            list[n] = (self.hkey(p, 0, SUB_CONDITION), id);
            n += 1;
        }
        sort_by_key(&mut self.rng, &mut list[..n], "AfterMove.speedSort");
        for &(_, id) in &list[..n] {
            if !self.sides[p].active.has(id) {
                continue;
            }
            let target_hp = self.sides[target].mon().hp;
            match id {
                v::PARTIALTRAPPINGLOCK => {
                    if target_hp == 0 {
                        self.del_vol(p, id);
                        continue;
                    }
                    let a = self.sides[p].active;
                    if a.ptl_dur == 1 {
                        if a.ptl_total != 5 {
                            self.add_volatile(p, v::FAKEPARTIALLYTRAPPED, p, 0, s);
                            let t_slot = self.sides[target].order[0];
                            if self.sides[p].active.has(v::FAKEPARTIALLYTRAPPED) {
                                self.sides[p].active.fpt_counterpart = t_slot;
                            }
                            self.add_volatile(target, v::FAKEPARTIALLYTRAPPED, target, 0, s);
                            let p_slot = self.sides[p].order[0];
                            if self.sides[target].active.has(v::FAKEPARTIALLYTRAPPED) {
                                self.sides[target].active.fpt_counterpart = p_slot;
                            }
                        }
                    } else {
                        self.add_volatile(target, v::PARTIALLYTRAPPED, p, a.ptl_move, s);
                        if a.ptl_total as i32 - a.ptl_dur as i32 > 0 && self.sides[target].active.has(v::PARTIALLYTRAPPED) {
                            self.sides[target].active.pt_maybe_locked = true;
                        }
                    }
                }
                v::MUSTRECHARGE => {
                    if target_hp == 0 {
                        self.del_vol(p, id);
                    } else {
                        s.event(Event::MustRecharge { player: p });
                        plog!(s, |l| {
                            let i = self.ident(l, p);
                            l.add(format!("|-mustrecharge|{i}"));
                        });
                    }
                }
                v::LOCKEDMOVE => {
                    if self.sides[p].active.lockedmove_time <= 0 {
                        self.remove_volatile(p, id, s);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Renumber the insertion stamps of the present volatiles to `0..n` (same order) when the 8-bit
/// counter runs out; absent volatiles' stamps are never read.
#[cold]
fn renumber_stamps(a: &mut Active) {
    let mut ids = [0u8; v::COUNT];
    let mut n = 0;
    let mut bits = a.vols;
    while bits != 0 {
        ids[n] = bits.trailing_zeros() as u8;
        bits &= bits - 1;
        n += 1;
    }
    ids[..n].sort_unstable_by_key(|&id| a.vstamp[id as usize]);
    for (k, &id) in ids[..n].iter().enumerate() {
        a.vstamp[id as usize] = k as u8;
    }
    a.next_stamp = n as u8;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoostResult {
    No,
    Changed,
    /// Boosting at 999 / dropping at 1 (`changed = 0`).
    Zero,
}

/// Iterator over an active Pokémon's volatile ids (`state::v`) in insertion order.
pub struct VolIter {
    ids: [u8; v::COUNT],
    n: usize,
    i: usize,
}

impl Iterator for VolIter {
    type Item = u8;
    fn next(&mut self) -> Option<u8> {
        if self.i < self.n {
            self.i += 1;
            Some(self.ids[self.i - 1])
        } else {
            None
        }
    }
}

#[inline(always)]
pub(crate) fn data_type_poison() -> u8 {
    3
}
