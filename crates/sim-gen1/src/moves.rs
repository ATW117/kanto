//! Move execution: a port of Showdown's Gen 1 `runMove`, `useMove`, `useMoveInner`,
//! `tryMoveHit`, `moveHit` and `getDamage` (data/mods/gen1/scripts.ts) plus the Gen 1 move and
//! condition callbacks (data/mods/gen1/{moves,conditions}.ts) that run inside them.

use crate::events::Dmg;
use crate::state::*;
use sim_core::sort::{sort_by_priority, Priority, ORDER_UNSET};
use sim_core::{Cant, Event, Player, Sink};
use sim_data::gen1::{self as data, mv, target as tgt, MOVES, SPECIES};

const TWO_TURN: [u8; 6] = [mv::DIG, mv::FLY, mv::RAZORWIND, mv::SKULLBASH, mv::SKYATTACK, mv::SOLARBEAM];
const SKIP_LASTDAMAGE: [u8; 24] = [
    mv::CONFUSERAY,
    mv::CONVERSION,
    mv::COUNTER,
    mv::FOCUSENERGY,
    mv::GLARE,
    mv::HAZE,
    mv::LEECHSEED,
    mv::LIGHTSCREEN,
    mv::MIMIC,
    mv::MIST,
    mv::POISONGAS,
    mv::POISONPOWDER,
    mv::RECOVER,
    mv::REFLECT,
    mv::REST,
    mv::SOFTBOILED,
    mv::SPLASH,
    mv::STUNSPORE,
    mv::SUBSTITUTE,
    mv::SUPERSONIC,
    mv::TELEPORT,
    mv::THUNDERWAVE,
    mv::TOXIC,
    mv::TRANSFORM,
];

const TYPE_NORMAL: u8 = 0;
const TYPE_FIGHTING: u8 = 1;
const TYPE_GHOST: u8 = 7;
const TYPE_GRASS: u8 = 10;

/// An `ActiveMove`: the move's data plus the fields Showdown mutates while it runs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mv {
    /// Move id; `pm::NOMOVE_HACK` for Gen 1's "no move" Fissure-alike.
    pub id: u8,
    pub d: sim_data::MoveData,
    /// Accuracy in percent (`None` = `true`).
    pub accuracy: Option<f64>,
    pub hit: u8,
    pub last_hit: bool,
    /// `move.damage` set by multi-hit moves after the first hit.
    pub fixed: Option<u32>,
}

impl Mv {
    pub fn new(id: u8) -> Mv {
        if id == pm::NOMOVE {
            let mut d = MOVES[mv::FISSURE as usize];
            d.base_power = 102;
            d.ohko = false;
            d.type_ = data::TYPELESS;
            d.category = 1;
            d.pp = 10;
            return Mv { id: pm::NOMOVE_HACK, d, accuracy: Some(100.0 * 81.0 / 256.0), hit: 0, last_hit: false, fixed: None };
        }
        let d = MOVES[id as usize];
        let accuracy = if d.accuracy == 0 { None } else { Some(d.accuracy as f64) };
        Mv { id, d, accuracy, hit: 0, last_hit: false, fixed: None }
    }

    /// The move id as `pokemon.lastMove` / `useMove(lastMove.id)` sees it.
    #[inline]
    fn real_id(&self) -> u8 {
        if self.id == pm::NOMOVE_HACK {
            mv::FISSURE
        } else {
            self.id
        }
    }

    #[inline]
    fn is_status(&self) -> bool {
        self.d.category == 2
    }
}

/// Which `moveData` a `moveHit` call applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Md {
    Main,
    /// `move.self` (`{volatileStatus}`).
    SelfEffect,
    Secondary,
    /// Bide's release: `{damage}`.
    Fixed(u32),
}

/// JS-ish results of `moveHit`/`tryMoveHit`/`getDamage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum R {
    False,
    Null,
    Undef,
    N(u32),
}

impl R {
    #[inline]
    fn truthy(self) -> bool {
        matches!(self, R::N(n) if n != 0) || self == R::Undef
    }
}

impl Battle {
    #[inline]
    fn has_type(&self, p: Player, t: u8) -> bool {
        self.sides[p].active.types.contains(&t)
    }

    /// `pokemon.runImmunity(move.type)` (type-based immunity only exists for damaging types).
    fn type_immune(&self, ty: u8, t: Player) -> bool {
        if ty == data::TYPELESS {
            return false;
        }
        let ts = self.sides[t].active.types;
        data::IMMUNE[ty as usize][ts[0] as usize] || data::IMMUNE[ty as usize][ts[1] as usize]
    }

    /// Gen 1 `deductPP` (by `side.lastSelectedMoveSlot`), `amount` may be -1.
    fn deduct_pp(&mut self, p: Player, amount: i32) {
        let sd = &mut self.sides[p];
        let i = sd.last_selected_move_slot as usize;
        if i >= sd.active.n_slots as usize {
            return;
        }
        let slot = sd.slot_mut(i);
        slot.pp = (slot.pp as i32 - amount).rem_euclid(64) as u8;
        let pp = slot.pp;
        if sd.active.virt[i] && !sd.active.transformed && i < sd.mon().n_moves as usize {
            sd.mon_mut().moves[i].pp = pp;
        }
    }

    /// The default target of a move used by `p`.
    #[inline]
    fn target_of(&self, p: Player, id: u8) -> Player {
        if id < 166 && MOVES[id as usize].target == tgt::SELF {
            p
        } else {
            1 - p
        }
    }

    // ---- runMove ---------------------------------------------------------------------------------

    /// Gen 1 `BattleActions#runMove` for the active of `p` executing `id`.
    pub(crate) fn run_move<S: Sink>(&mut self, p: Player, id: u8, s: &mut S) {
        if id == pm::CANNOTMOVE {
            self.after_move_self(p, s);
            return;
        }
        let target = self.target_of(p, id);
        self.sides[target].mon_mut().sub_fainted = false;
        if self.sides[p].active.move_this_turn != 0 || !self.before_move(p, target, id, s) {
            self.after_move_self(p, s);
            return;
        }
        if id == mv::BIDE && self.sides[p].active.has(v::BIDE) {
            self.after_move_self(p, s);
            return;
        }
        let mut source_effect = None;
        if id != pm::STRUGGLE {
            let locked = self.locked_move(p).or_else(|| self.semi_locked_move(p, false));
            if locked.is_some() {
                source_effect = Some(id);
            }
            if (locked.is_none() && !TWO_TURN.contains(&id)) || self.sides[p].active.has(v::TWOTURNMOVE) {
                self.deduct_pp(p, 1);
            }
        }
        let m = Mv::new(id);
        self.use_move(p, m, Some(target), source_effect, s);
    }

    /// `runEvent('BeforeMove', pokemon, target, move)`; false = the move is aborted.
    fn before_move<S: Sink>(&mut self, p: Player, target: Player, id: u8, s: &mut S) -> bool {
        let mut list = [(Priority::default(), 0u8); 12];
        let mut n = 0;
        let speed = self.sides[p].mon().speed as i64;
        let key = |prio: i32, sub: i32| Priority::new(ORDER_UNSET, prio, speed, sub);
        match self.sides[p].mon().status {
            st::FRZ => {
                list[n] = (key(12, 0), 100);
                n += 1;
            }
            st::SLP => {
                list[n] = (key(10, 0), 101);
                n += 1;
            }
            st::PAR => {
                list[n] = (key(2, 0), 102);
                n += 1;
            }
            _ => {}
        }
        for vid in self.vol_order(p) {
            let prio = match vid {
                v::PARTIALLYTRAPPED => 9,
                v::FLINCH => 8,
                v::MUSTRECHARGE => 7,
                v::DISABLE => 6,
                v::CONFUSION => 3,
                v::PARTIALTRAPPINGLOCK | v::BIDE => 0,
                _ => continue,
            };
            list[n] = (key(prio, 2), vid);
            n += 1;
        }
        sort_by_priority(&mut self.rng, &mut list[..n], "BeforeMove.speedSort");
        let slot = self.sides[p].order[0];
        for &(_, h) in &list[..n] {
            match h {
                100 => {
                    if self.sides[p].mon().status != st::FRZ {
                        continue;
                    }
                    s.event(Event::Cant { player: p, slot, reason: Cant::Freeze, mv: 0 });
                    self.sides[p].active.last_move = 0;
                    return false;
                }
                101 => {
                    if self.sides[p].mon().status != st::SLP {
                        continue;
                    }
                    let m = self.sides[p].mon_mut();
                    m.slp_time -= 1;
                    if m.slp_time > 0 {
                        s.event(Event::Cant { player: p, slot, reason: Cant::Sleep, mv: 0 });
                    }
                    self.sides[p].active.last_move = 0;
                    return false;
                }
                102 => {
                    if self.sides[p].mon().status != st::PAR {
                        continue;
                    }
                    if self.rng.chance(63, 256, "par.onBeforeMove") {
                        s.event(Event::Cant { player: p, slot, reason: Cant::Paralysis, mv: 0 });
                        self.remove_volatile(p, v::BIDE);
                        self.remove_volatile(p, v::TWOTURNMOVE);
                        self.remove_volatile(p, v::PARTIALTRAPPINGLOCK);
                        self.remove_volatile(p, v::LOCKEDMOVE);
                        return false;
                    }
                }
                _ if !self.sides[p].active.has(h) => continue,
                v::PARTIALLYTRAPPED => {
                    s.event(Event::Cant { player: p, slot, reason: Cant::PartiallyTrapped, mv: 0 });
                    return false;
                }
                v::FLINCH => {
                    s.event(Event::Cant { player: p, slot, reason: Cant::Flinch, mv: 0 });
                    return false;
                }
                v::MUSTRECHARGE => {
                    s.event(Event::Cant { player: p, slot, reason: Cant::Recharge, mv: 0 });
                    self.remove_volatile(p, v::MUSTRECHARGE);
                    return false;
                }
                v::DISABLE => {
                    let a = &mut self.sides[p].active;
                    a.disable_time -= 1;
                    if a.disable_time == 0 {
                        self.remove_volatile(p, v::DISABLE);
                        continue;
                    }
                    let used = if a.has(v::BIDE) { mv::BIDE } else { id };
                    if used == a.disable_move {
                        let dm = a.disable_move;
                        s.event(Event::Cant { player: p, slot, reason: Cant::Disable, mv: dm });
                        self.remove_volatile(p, v::TWOTURNMOVE);
                        return false;
                    }
                }
                v::CONFUSION => {
                    let a = &mut self.sides[p].active;
                    a.confusion_time -= 1;
                    if a.confusion_time == 0 {
                        self.remove_volatile(p, v::CONFUSION);
                        continue;
                    }
                    if !self.rng.chance(128, 256, "confusion.onBeforeMove") {
                        let lvl = self.sides[p].mon().level as u64;
                        let atk = self.sides[p].active.modified[0] as u64;
                        let def = self.sides[p].active.modified[1] as u64;
                        let d = ((lvl * 2 / 5 + 2) * atk * 40 / def) / 50 + 2;
                        self.direct_damage(d as u32, p, true);
                        self.remove_volatile(p, v::BIDE);
                        self.remove_volatile(p, v::TWOTURNMOVE);
                        self.remove_volatile(p, v::INVULNERABILITY);
                        self.remove_volatile(p, v::PARTIALTRAPPINGLOCK);
                        self.remove_volatile(p, v::LOCKEDMOVE);
                        return false;
                    }
                }
                v::PARTIALTRAPPINGLOCK => {
                    let q = 1 - p;
                    let locked = self.sides[p].active.ptl_locked;
                    if target != q || self.sides[q].order[0] != locked {
                        self.remove_volatile(p, v::PARTIALTRAPPINGLOCK);
                    }
                }
                v::BIDE => {
                    let ld = self.last_damage;
                    let a = &mut self.sides[p].active;
                    a.bide_damage = a.bide_damage.wrapping_add(ld);
                    a.bide_time -= 1;
                    if a.bide_time == 0 {
                        let dmg = a.bide_damage;
                        if dmg == 0 {
                            s.event(Event::Fail);
                            self.remove_volatile(p, v::BIDE);
                            return false;
                        }
                        let mut bm = Mv::new(mv::BIDE);
                        let q = 1 - p;
                        self.move_hit(Some(q), p, &mut bm, Md::Fixed(dmg.wrapping_mul(2)), false, false, s);
                        self.remove_volatile(p, v::BIDE);
                        return false;
                    }
                    return false;
                }
                _ => {}
            }
        }
        true
    }

    // ---- useMove / useMoveInner ------------------------------------------------------------------

    /// Gen 1 `BattleActions#useMove`. `target` None = `getRandomTarget` for the move.
    pub(crate) fn use_move<S: Sink>(&mut self, p: Player, mut m: Mv, target: Option<Player>, source_effect: Option<u8>, s: &mut S) {
        let mut target = target.unwrap_or_else(|| self.target_of(p, m.id.min(165)));
        if m.d.target == tgt::SELF {
            target = p;
        }
        if matches!(source_effect, Some(mv::METRONOME) | Some(mv::MIRRORMOVE)) {
            if TWO_TURN.contains(&m.id) {
                self.deduct_pp(p, -1);
            }
            let sd = &mut self.sides[p];
            let i = sd.last_selected_move_slot as usize;
            if sd.active.transformed && i < sd.mon().n_moves as usize {
                let mm = sd.mon_mut();
                mm.moves[i].pp = (mm.moves[i].pp + 1) % 64;
            }
        }
        let side_id = if m.id == pm::NOMOVE_HACK { pm::NOMOVE_HACK } else { m.id };
        self.sides[p].last_move = side_id;
        self.sides[p].last_enemy_move = side_id;
        if !TWO_TURN.contains(&m.id) || self.sides[p].active.has(v::TWOTURNMOVE) {
            let a = &mut self.sides[p].active;
            a.last_move = m.real_id();
            a.move_this_turn = m.real_id();
        }
        self.use_move_inner(p, &mut m, target, source_effect, s);
        if m.id != mv::METRONOME {
            let q = 1 - p;
            let foe_last = self.sides[q].active.last_move;
            if m.id != mv::MIRRORMOVE || foe_last == 0 || foe_last == mv::MIRRORMOVE {
                self.after_move(p, target, s);
                if self.sides[target].mon().hp > 0 {
                    self.after_move_self(p, s);
                }
            }
        }
    }

    fn use_move_inner<S: Sink>(&mut self, p: Player, m: &mut Mv, target: Player, source_effect: Option<u8>, s: &mut S) {
        if self.sides[p].mon().fainted {
            return;
        }
        let slot = self.sides[p].order[0];
        s.event(Event::Move { player: p, slot, mv: m.real_id(), from: source_effect.is_some() });
        // TryMove (move callbacks).
        match m.id {
            mv::RAZORWIND | mv::SOLARBEAM | mv::SKULLBASH | mv::SKYATTACK | mv::DIG | mv::FLY => {
                if self.remove_volatile(p, v::TWOTURNMOVE) {
                    self.remove_volatile(p, v::INVULNERABILITY);
                } else {
                    self.add_volatile(p, v::TWOTURNMOVE, target, m.id);
                    if matches!(m.id, mv::DIG | mv::FLY) {
                        self.add_volatile(p, v::INVULNERABILITY, target, m.id);
                    }
                    return;
                }
            }
            mv::BIND | mv::WRAP | mv::CLAMP | mv::FIRESPIN => {
                if self.sides[target].active.has(v::MUSTRECHARGE) {
                    self.remove_volatile(target, v::MUSTRECHARGE);
                }
            }
            _ => {}
        }
        if self.sides[target].mon().fainted {
            return;
        }
        if !SKIP_LASTDAMAGE.contains(&m.id) {
            self.last_damage = 0;
        }
        let damage = self.try_move_hit(target, p, m, s);
        // Disable and Self-Destruct/Explosion build Rage even when they miss or fail.
        if self.sides[target].active.boosts[0] < 6 && (m.d.selfdestruct || m.id == mv::DISABLE) && self.sides[target].active.has(v::RAGE) {
            self.boost([1, 0, 0, 0, 0, 0, 0], target, Some(p), None);
        }
        if damage == R::False {
            if matches!(m.id, mv::JUMPKICK | mv::HIGHJUMPKICK) {
                self.direct_damage(1, p, true);
            }
        }
    }

    // ---- tryMoveHit ------------------------------------------------------------------------------

    fn try_move_hit<S: Sink>(&mut self, t: Player, p: Player, m: &mut Mv, s: &mut S) -> R {
        let mut damage = R::N(0);
        if m.d.self_volatile == data::vol::LOCKEDMOVE {
            if self.sides[p].active.has(v::LOCKEDMOVE) {
                let a = &mut self.sides[p].active;
                a.lockedmove_time -= 1;
                if a.lockedmove_time == 0 {
                    self.del_vol(p, v::CONFUSION);
                    self.add_volatile(p, v::CONFUSION, p, 0);
                }
            } else {
                self.add_volatile(p, v::LOCKEDMOVE, p, m.id);
            }
        }
        // Invulnerability (Dig/Fly).
        if self.sides[t].active.has(v::INVULNERABILITY) && t != p && !matches!(m.id, mv::SWIFT | mv::TRANSFORM) {
            s.event(Event::Miss { player: p });
            if m.d.selfdestruct {
                self.faint(p);
            }
            return R::False;
        }
        if !m.d.ignore_immunity && self.type_immune(m.d.type_, t) {
            s.event(Event::Immune);
            if m.d.selfdestruct {
                self.faint(p);
            }
            return R::False;
        }
        // TryImmunity.
        let immune = match m.id {
            mv::LEECHSEED => self.has_type(t, TYPE_GRASS),
            mv::DREAMEATER => !(self.sides[t].mon().status == st::SLP && !self.sides[t].active.has(v::SUBSTITUTE)),
            _ => false,
        };
        if immune {
            s.event(Event::Immune);
            return R::False;
        }
        let mut accuracy = m.accuracy;
        if m.d.status == st::SLP && self.sides[t].active.has(v::MUSTRECHARGE) {
            accuracy = None;
        }
        if m.d.ohko && self.sides[t].active.modified[4] > self.sides[p].active.modified[4] {
            s.event(Event::Immune);
            return R::False;
        }
        let mut acc_int: Option<i64> = None;
        if let Some(a) = accuracy {
            const TABLE: [f64; 13] = [25.0, 28.0, 33.0, 40.0, 50.0, 66.0, 100.0, 150.0, 200.0, 250.0, 300.0, 350.0, 400.0];
            let mut acc = (a * 255.0 / 100.0).floor() as i64;
            let ua = self.sides[p].active;
            if ua.has(v::LOCKEDMOVE) {
                acc = ua.lockedmove_acc as i64;
            }
            if ua.has(v::RAGE) {
                acc = ua.rage_acc as i64;
            }
            let ab = (ua.boosts[5] + 6) as usize;
            acc = (acc as f64 * (TABLE[ab] / 100.0)).floor() as i64;
            let eb = (6 - self.sides[t].active.boosts[6]) as usize;
            acc = (acc as f64 * (TABLE[eb] / 100.0)).floor() as i64;
            acc = acc.clamp(1, 255);
            let ua = &mut self.sides[p].active;
            if ua.has(v::LOCKEDMOVE) {
                ua.lockedmove_acc = acc as i32;
            }
            if ua.has(v::RAGE) {
                ua.rage_acc = acc as i32;
            }
            acc_int = Some(acc);
        }
        // runEvent('Accuracy'): partiallytrapped on the target lets its trapper always hit.
        let ta = &self.sides[t].active;
        if t != p && ta.has(v::PARTIALLYTRAPPED) && ta.pt_source == self.sides[p].order[0] {
            acc_int = None;
        }
        if m.d.target == tgt::SELF {
            if let Some(a) = acc_int.as_mut() {
                *a += 1;
            }
        }
        if let Some(a) = acc_int {
            if !self.rng.chance(a as u32, 256, "tryMoveHit.accuracy") {
                s.event(Event::Miss { player: p });
                damage = R::False;
                self.last_damage = 0;
            }
        }
        if damage != R::False {
            if m.d.multihit != 0 {
                let hits = if m.d.multihit == 25 {
                    [2u8, 2, 2, 3, 3, 3, 4, 5][self.rng.sample(8, "multihit") as usize]
                } else {
                    m.d.multihit
                };
                let mut i = 0u8;
                while i < hits && self.sides[t].mon().hp > 0 && self.sides[p].mon().hp > 0 {
                    m.hit = i + 1;
                    m.last_hit = m.hit == hits;
                    let md = self.move_hit(Some(t), p, m, Md::Main, false, false, s);
                    if md == R::False {
                        break;
                    }
                    let d = match md {
                        R::N(n) => n,
                        _ => 0,
                    };
                    damage = R::N(d);
                    if i == 0 {
                        m.fixed = Some(d);
                    }
                    if self.sides[t].mon().sub_fainted {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                m.fixed = None;
                if i == 0 {
                    return R::N(1);
                }
            } else {
                damage = self.move_hit(Some(t), p, m, Md::Main, false, false, s);
            }
        }
        if m.d.selfdestruct && !self.sides[t].mon().sub_fainted {
            self.faint(p);
        }
        if damage == R::False {
            self.del_vol(p, v::PARTIALTRAPPINGLOCK);
            return R::False;
        }
        // AfterMoveSecondary: a burning move thaws a frozen target.
        if self.sides[t].mon().status == st::FRZ {
            if let Some(sec) = m.d.secondary {
                if sec.status == st::BRN {
                    self.cure_status(t);
                }
            }
        }
        damage
    }

    // ---- moveHit ---------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn move_hit<S: Sink>(&mut self, target: Option<Player>, p: Player, m: &mut Mv, md: Md, is_secondary: bool, is_self: bool, s: &mut S) -> R {
        let mut damage = R::N(0);
        let mut target = target;
        let target_had_sub = target.is_some_and(|t| self.sides[t].active.has(v::SUBSTITUTE));
        let mut target_has_sub: Option<bool> = None;
        if let Some(t) = target {
            // singleEvent('TryHit', moveData)
            if md == Md::Main {
                let r = self.move_try_hit(t, p, m, s);
                if !r.truthy() {
                    if r == R::False {
                        s.event(Event::Fail);
                    }
                    return R::False;
                }
            }
            let mut hit = R::Undef;
            if !is_self && !is_secondary {
                if self.sides[t].active.has(v::SUBSTITUTE) {
                    hit = self.substitute_try_hit(t, p, m, md, s);
                }
                if !hit.truthy() {
                    if hit == R::False {
                        s.event(Event::Fail);
                    }
                    if hit != R::N(0) {
                        return R::False;
                    }
                }
            }
            if hit == R::N(0) {
                target_has_sub = Some(self.sides[t].active.has(v::SUBSTITUTE));
                target = None;
            }
        }
        if let Some(t) = target {
            let mut did_something = false;
            damage = self.get_damage(p, t, m, md, s);
            if let R::N(d) = damage {
                let hp = self.sides[t].mon().hp as u32;
                if d > 0 && d > hp {
                    damage = R::N(hp);
                }
            }
            if let R::N(d) = damage {
                if !self.sides[t].mon().fainted {
                    let dealt = self.damage(d, t, Some(p), Dmg::Move(m.real_id()));
                    if dealt > 0 {
                        self.apply_recoil(dealt, m.real_id(), p);
                    }
                    damage = R::N(dealt);
                    did_something = true;
                }
            }
            if matches!(damage, R::False | R::Null) {
                return R::False;
            }
            let (boosts, status, volatile) = match md {
                Md::Main => (m.d.boosts, m.d.status, m.d.volatile),
                Md::SelfEffect => ([0; 7], 0, m.d.self_volatile),
                Md::Secondary => {
                    let sec = m.d.secondary.expect("secondary");
                    (sec.boosts, sec.status, sec.volatile)
                }
                Md::Fixed(_) => ([0; 7], 0, 0),
            };
            if boosts != [0; 7] && self.sides[t].mon().hp > 0 {
                let will = self.boost(boosts, t, Some(p), Some(!m.is_status()));
                if !will {
                    s.event(Event::Fail);
                    return R::False;
                }
                did_something = true;
                let q = 1 - p;
                match self.sides[q].mon().status {
                    st::PAR => {
                        let a = &mut self.sides[q].active;
                        a.modified[4] = crate::battle::modify_stat(a.modified[4], 0.25);
                    }
                    st::BRN => {
                        let a = &mut self.sides[q].active;
                        a.modified[0] = crate::battle::modify_stat(a.modified[0], 0.5);
                    }
                    _ => {}
                }
            }
            if status != 0 {
                if status == st::SLP && self.sides[t].active.has(v::MUSTRECHARGE) {
                    if self.set_status(t, st::SLP, p, false) {
                        self.remove_volatile(t, v::MUSTRECHARGE);
                    }
                } else if self.sides[t].mon().status == st::NONE {
                    if self.set_status(t, status, p, false) {
                        let a = &mut self.sides[t].active;
                        if status == st::BRN {
                            a.modified[0] = crate::battle::modify_stat(a.modified[0], 0.5);
                        }
                        if status == st::PAR {
                            a.modified[4] = crate::battle::modify_stat(a.modified[4], 0.25);
                        }
                    }
                } else if !is_secondary {
                    s.event(Event::Fail);
                }
                did_something = true;
            }
            if volatile != 0 {
                let vid = data_vol(volatile);
                if self.add_volatile(t, vid, p, m.real_id()) == Some(true) {
                    did_something = true;
                    if vid == v::DISABLE {
                        let a = &self.sides[t].active;
                        s.event(Event::DisableStart { player: t, slot: self.sides[t].order[0], mv: a.disable_move });
                    }
                }
            }
            // singleEvent('Hit', moveData)
            let hit_result = if md == Md::Main { self.move_on_hit(t, p, m, s) } else { true };
            // runEvent('Hit'): Rage.
            if !is_self && !is_secondary && self.sides[t].active.has(v::RAGE) {
                let a = &self.sides[t].active;
                if a.boosts[0] < 6 && !m.is_status() && !m.d.selfdestruct {
                    self.boost([1, 0, 0, 0, 0, 0, 0], t, Some(p), None);
                }
            }
            if !hit_result && !did_something {
                s.event(Event::Fail);
                return R::False;
            }
        }
        let has_sub = target_has_sub.unwrap_or_else(|| target.is_some_and(|t| self.sides[t].active.has(v::SUBSTITUTE)));
        let do_self = (target_had_sub && has_sub) || !target_had_sub;
        if md == Md::Main && m.d.self_volatile != 0 && m.d.self_volatile != data::vol::LOCKEDMOVE && (do_self || m.d.self_volatile == data::vol::PARTIALTRAPPINGLOCK) {
            self.move_hit(Some(p), p, m, Md::SelfEffect, is_secondary, true, s);
        }
        if md == Md::Main {
            if let (Some(sec), Some(t)) = (m.d.secondary, target) {
                if self.sides[t].mon().hp > 0 && (m.d.multihit == 0 || m.last_hit) {
                    let blocked = matches!(sec.status, st::PAR | st::BRN | st::FRZ) && self.has_type(t, m.d.type_);
                    if !blocked {
                        if sec.chance == 0 {
                            self.move_hit(Some(t), p, m, Md::Secondary, true, is_self, s);
                        } else {
                            let mut chance = (sec.chance as u32 * 256).div_ceil(100);
                            if sec.volatile == data::vol::CONFUSION {
                                chance -= 1;
                            }
                            if self.rng.chance(chance, 256, "moveHit.secondary") {
                                self.move_hit(Some(t), p, m, Md::Secondary, true, is_self, s);
                            }
                        }
                    }
                }
            }
        }
        damage
    }

    /// The move's own `onTryHit` (singleEvent on the main moveData).
    fn move_try_hit<S: Sink>(&mut self, t: Player, _p: Player, m: &Mv, _s: &mut S) -> R {
        match m.id {
            mv::DISABLE => {
                let sd = &self.sides[t];
                if (0..sd.active.n_slots as usize).any(|i| sd.slot(i).pp > 0) {
                    R::Undef
                } else {
                    R::False
                }
            }
            mv::LIGHTSCREEN => {
                if self.sides[t].active.has(v::LIGHTSCREEN) {
                    R::False
                } else {
                    R::Undef
                }
            }
            mv::REFLECT => {
                if self.sides[t].active.has(v::REFLECT) {
                    R::False
                } else {
                    R::Undef
                }
            }
            mv::SUBSTITUTE => {
                if self.sides[t].active.has(v::SUBSTITUTE) {
                    return R::Null;
                }
                let m = self.sides[t].mon();
                if (m.hp as f64) < m.stats[0] as f64 / 4.0 {
                    return R::Null;
                }
                R::Undef
            }
            _ => R::Undef,
        }
    }

    /// Gen 1 `substitute.onTryHit` on target `t` hit by `p`.
    fn substitute_try_hit<S: Sink>(&mut self, t: Player, p: Player, m: &mut Mv, md: Md, s: &mut S) -> R {
        if m.is_status() {
            let blocked = matches!(m.d.status, st::PSN | st::TOX) || (m.d.boosts != [0; 7] && t != p) || m.d.volatile == data::vol::CONFUSION;
            return if blocked { R::False } else { R::Undef };
        }
        if m.d.volatile != 0 && t == p {
            return R::Undef;
        }
        let mut uncapped = if m.hit > 1 { R::N(self.last_damage) } else { self.get_damage(p, t, m, md, s) };
        if m.id == mv::BIDE {
            uncapped = R::N(self.sides[p].active.bide_damage.wrapping_mul(2));
        }
        let R::N(u) = uncapped else {
            return R::Null;
        };
        self.last_damage = u;
        let a = &mut self.sides[t].active;
        a.sub_hp -= if u as i64 > a.sub_hp as i64 { a.sub_hp } else { u as i32 };
        if a.sub_hp <= 0 {
            self.remove_volatile(t, v::SUBSTITUTE);
            self.sides[t].mon_mut().sub_fainted = true;
        }
        if self.sides[t].active.has(v::SUBSTITUTE) {
            if u > 0 {
                self.apply_recoil(u, m.real_id(), p);
            }
            if m.d.drain {
                let amount = (u / 2).max(1);
                self.last_damage = amount;
                self.heal(amount, p);
            }
            if let Some(sec) = m.d.secondary {
                if sec.volatile == data::vol::CONFUSION {
                    let ok = sec.chance == 0 || self.rng.chance((sec.chance as u32 * 256).div_ceil(100) - 1, 256, "substitute.confusion");
                    if ok {
                        self.add_volatile(t, v::CONFUSION, p, m.real_id());
                    }
                }
            }
        }
        R::N(0)
    }

    // ---- getDamage -------------------------------------------------------------------------------

    fn get_damage<S: Sink>(&mut self, p: Player, t: Player, m: &mut Mv, md: Md, s: &mut S) -> R {
        // Non-main moveData objects are plain {status/volatileStatus/boosts/damage} objects.
        match md {
            Md::Fixed(d) => return R::N(d),
            Md::SelfEffect | Md::Secondary => {
                if self.sides[p].active.has(v::PARTIALTRAPPINGLOCK) && t != p && self.sides[t].order[0] == self.sides[p].active.ptl_locked {
                    return R::N(self.sides[p].active.ptl_damage);
                }
                return R::Undef;
            }
            Md::Main => {}
        }
        if !m.d.ignore_immunity && self.type_immune(m.d.type_, t) {
            return R::False;
        }
        if m.d.ohko {
            return R::N(65535);
        }
        match m.id {
            mv::COUNTER => return self.counter_damage(p, t, s),
            mv::PSYWAVE => {
                let lvl = self.sides[p].mon().level as u32;
                if ((lvl + (lvl >> 1)) & 0xff) < 2 {
                    return R::False;
                }
                let hi = (1.5 * lvl as f64) as u32;
                return R::N(self.rng.random_range(1, hi, "psywave"));
            }
            mv::SUPERFANG => return R::N((self.sides[t].mon().hp as u32 / 2).max(1)),
            _ => {}
        }
        if m.d.damage == 255 {
            return R::N(self.sides[p].mon().level as u32);
        }
        if let Some(f) = m.fixed {
            return R::N(f);
        }
        if m.d.damage != 0 {
            return R::N(m.d.damage as u32);
        }
        if m.d.self_volatile == data::vol::PARTIALTRAPPINGLOCK && m.d.type_ == TYPE_NORMAL && self.has_type(t, TYPE_GHOST) {
            return R::N(0);
        }
        let ua = self.sides[p].active;
        if ua.has(v::PARTIALTRAPPINGLOCK) && t != p && self.sides[t].order[0] == ua.ptl_locked {
            return R::N(ua.ptl_damage);
        }
        let bp = m.d.base_power as u64;
        if bp == 0 {
            return R::Undef;
        }
        // Critical hit (Gen 1: from the user's *original* species' base Speed).
        let mut crit = m.d.will_crit == 1;
        if !crit {
            let base_spe = SPECIES[self.sides[p].mon().species as usize].base[5] as u32;
            let mut cc = base_spe / 2;
            if ua.has(v::FOCUSENERGY) {
                cc /= 2;
            } else {
                cc = (cc * 2).clamp(1, 255);
            }
            if m.d.crit_ratio == 1 {
                cc /= 2;
            } else if m.d.crit_ratio == 2 {
                cc = (cc * 4).clamp(1, 255);
            }
            if cc > 0 {
                crit = self.rng.chance(cc, 256, "getDamage.crit");
            }
        }
        let physical = m.d.category == 0;
        let (ai, di) = if physical { (0usize, 1usize) } else { (2, 3) };
        let mut level = self.sides[p].mon().level as u64;
        let mut attack = ua.modified[ai] as u64;
        let ta = self.sides[t].active;
        let mut defense = ta.modified[di] as u64;
        if (di == 1 && ta.has(v::REFLECT)) || (di == 3 && ta.has(v::LIGHTSCREEN)) {
            defense *= 2;
        }
        if crit {
            level *= 2;
            s.event(Event::Crit);
            attack = self.sides[p].mon().stats[ai + 1] as u64;
            defense = self.sides[t].mon().stats[di + 1] as u64;
        }
        level %= 256;
        if attack >= 256 || defense >= 256 {
            attack = ((attack / 4) % 256).max(1);
            defense = (defense / 4) % 256;
            if defense == 0 {
                defense = 1;
            }
        }
        if m.d.selfdestruct && di == 1 {
            defense = (defense / 2).max(1);
        }
        let mut d = level * 2 / 5 + 2;
        d = d * bp * attack / defense;
        d = (d / 50).min(997) + 2;
        let ty = m.d.type_;
        if ty != data::TYPELESS && self.has_type(p, ty) {
            d += d / 2;
        }
        let ts = ta.types;
        let mut total = 0i32;
        for (k, &tt) in ts.iter().enumerate() {
            if k == 1 && ts[1] == ts[0] {
                break;
            }
            let e = if ty == data::TYPELESS { 0 } else { data::EFFECTIVENESS[ty as usize][tt as usize] };
            total += e as i32;
            if e > 0 {
                d = d * 20 / 10;
            }
            if e < 0 {
                d = d * 5 / 10;
            }
        }
        if total > 0 {
            s.event(Event::SuperEffective);
        } else if total < 0 {
            s.event(Event::Resisted);
        }
        if d == 0 {
            return R::N(0);
        }
        if d > 1 {
            d = d * self.rng.random_range(217, 256, "getDamage.random") as u64 / 255;
        }
        R::N(d as u32)
    }

    /// Gen 1 Counter `damageCallback` (with Desync Clause Mod semantics).
    fn counter_damage<S: Sink>(&mut self, _p: Player, t: Player, s: &mut S) -> R {
        let foe = &self.sides[t];
        let selected_ok = foe.last_enemy_selected_move != mv::COUNTER;
        let last = foe.last_enemy_move;
        let counterable = if last == 0 || last >= 166 {
            false
        } else {
            let d = &MOVES[last as usize];
            (d.type_ == TYPE_NORMAL || d.type_ == TYPE_FIGHTING) && d.base_power > 0
        };
        if selected_ok && counterable && self.last_damage > 0 {
            R::N(2 * self.last_damage)
        } else {
            s.event(Event::Fail);
            R::False
        }
    }

    // ---- Move callbacks (onHit) ---------------------------------------------------------------

    /// `singleEvent('Hit', move)`: returns the truthiness of the result.
    fn move_on_hit<S: Sink>(&mut self, t: Player, p: Player, m: &mut Mv, s: &mut S) -> bool {
        match m.id {
            mv::CONVERSION => {
                self.sides[p].active.types = self.sides[t].active.types;
                true
            }
            mv::HAZE => {
                self.haze(p, s);
                true
            }
            mv::MIMIC => {
                let n = self.sides[t].active.n_slots as u32;
                if n == 0 {
                    return false;
                }
                let k = self.rng.sample(n, "mimic") as usize;
                let id = self.sides[t].slot(k).id;
                if id == 0 {
                    return false;
                }
                let sd = &mut self.sides[p];
                let i = sd.last_selected_move_slot as usize;
                let pp = sd.slot(i).pp;
                let ups = if i < sd.mon().n_moves as usize { 3 } else { 0 };
                sd.active.virt[i] = true;
                sd.active.vslots[i] = MoveSlot { id, pp, maxpp: crate::battle::calc_pp(MOVES[id as usize].pp, ups), disabled: false };
                s.event(Event::MimicStart { player: p, mv: id });
                true
            }
            mv::MIRRORMOVE => {
                let q = 1 - p;
                let last = self.sides[q].active.last_move;
                if last == 0 || last == mv::MIRRORMOVE {
                    return false;
                }
                self.sides[p].last_selected_move = last;
                self.sides[p].last_enemy_selected_move = last;
                self.use_move(p, Mv::new(last), None, Some(mv::MIRRORMOVE), s);
                true
            }
            mv::METRONOME => {
                let k = self.rng.sample(data::METRONOME.len() as u32, "metronome") as usize;
                let id = data::METRONOME[k];
                self.sides[p].last_selected_move = id;
                self.sides[p].last_enemy_selected_move = id;
                self.use_move(p, Mv::new(id), None, Some(mv::METRONOME), s);
                true
            }
            mv::RECOVER | mv::SOFTBOILED | mv::REST => {
                let mon = self.sides[p].mon();
                let (hp, max) = (mon.hp as i32, mon.stats[0] as i32);
                if hp == max {
                    return false;
                }
                if (hp == max - 255 || hp == max - 511) && hp % 256 != 0 {
                    return false;
                }
                if m.id == mv::REST {
                    if !self.set_status(p, st::SLP, p, false) {
                        return false;
                    }
                    let mm = self.sides[p].mon_mut();
                    mm.slp_time = 2;
                    mm.slp_start = 2;
                    self.heal(max as u32, p);
                } else {
                    self.heal((max / 2) as u32, p);
                }
                true
            }
            mv::TRANSFORM => self.transform(p, t, s),
            mv::SUBSTITUTE => {
                let max = self.sides[p].mon().stats[0];
                if max > 3 {
                    self.direct_damage((max / 4) as u32, p, false);
                }
                true
            }
            _ => true,
        }
    }

    /// Gen 1 Haze `onHit`.
    fn haze<S: Sink>(&mut self, source: Player, _s: &mut S) {
        for p in 0..2 {
            if self.sides[p].active.species == 0 || self.sides[p].mon().fainted {
                continue;
            }
            self.clear_boosts(p);
            if p != source {
                if matches!(self.sides[p].mon().status, st::FRZ | st::SLP) {
                    self.sides[p].last_selected_move = pm::CANNOTMOVE;
                    self.sides[p].last_enemy_selected_move = pm::CANNOTMOVE;
                    if self.will_move(p) {
                        self.change_action(p, pm::CANNOTMOVE);
                    }
                }
                self.cure_status(p);
            }
            if self.sides[p].mon().status == st::TOX {
                self.set_status(p, st::PSN, source, true);
            }
            self.update_speed_one(p);
            for vid in [v::DISABLE, v::CONFUSION, v::MIST, v::FOCUSENERGY, v::LEECHSEED, v::LIGHTSCREEN, v::REFLECT, v::RESIDUALDMG] {
                self.remove_volatile(p, vid);
            }
        }
    }

    /// `Pokemon#transformInto` (Gen 1).
    fn transform<S: Sink>(&mut self, p: Player, t: Player, s: &mut S) -> bool {
        if self.sides[t].mon().fainted {
            return false;
        }
        let foe = self.sides[t].active;
        let foe_slots: [MoveSlot; 4] = std::array::from_fn(|i| if i < foe.n_slots as usize { self.sides[t].slot(i) } else { MoveSlot::default() });
        // setSpecies(species, effect, isTransform): stats from the user's set and the new species.
        let me = *self.sides[p].mon();
        let base = SPECIES[foe.species as usize].base;
        let stats = crate::battle::calc_stats(base, me.ivs, me.evs, me.level);
        let sd = &mut self.sides[p];
        sd.mon_mut().speed = stats[5];
        let a = &mut sd.active;
        a.species = foe.species;
        a.transformed = true;
        a.types = foe.types;
        a.stored = foe.stored;
        a.modified = foe.modified;
        a.n_slots = foe.n_slots;
        for i in 0..4 {
            if i < foe.n_slots as usize {
                let id = foe_slots[i].id;
                let ups = if i < me.n_moves as usize { 3 } else { 0 };
                a.virt[i] = true;
                a.vslots[i] = MoveSlot {
                    id,
                    pp: MOVES[id as usize].pp.min(5),
                    maxpp: crate::battle::calc_pp(MOVES[id as usize].pp, ups),
                    disabled: false,
                };
            } else {
                a.virt[i] = false;
            }
        }
        a.boosts = foe.boosts;
        s.event(Event::Transform { player: p });
        true
    }
}

/// Map a `sim_data` volatile id to the internal one.
fn data_vol(d: u8) -> u8 {
    use sim_data::gen1::vol;
    match d {
        vol::PARTIALLYTRAPPED => v::PARTIALLYTRAPPED,
        vol::PARTIALTRAPPINGLOCK => v::PARTIALTRAPPINGLOCK,
        vol::CONFUSION => v::CONFUSION,
        vol::FLINCH => v::FLINCH,
        vol::DISABLE => v::DISABLE,
        vol::MIST => v::MIST,
        vol::LEECHSEED => v::LEECHSEED,
        vol::LIGHTSCREEN => v::LIGHTSCREEN,
        vol::REFLECT => v::REFLECT,
        vol::FOCUSENERGY => v::FOCUSENERGY,
        vol::BIDE => v::BIDE,
        vol::SUBSTITUTE => v::SUBSTITUTE,
        vol::MINIMIZE => v::MINIMIZE,
        vol::DEFENSECURL => v::DEFENSECURL,
        vol::LOCKEDMOVE => v::LOCKEDMOVE,
        vol::MUSTRECHARGE => v::MUSTRECHARGE,
        vol::RAGE => v::RAGE,
        _ => panic!("unknown volatile {d}"),
    }
}
