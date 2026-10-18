//! Safe, fixed-size binary serialisation of [`Battle`] (for `clone_state` blobs). Every field is
//! written explicitly and validated on read, so a malformed blob is rejected instead of being
//! transmuted into invalid enums/bools.

use crate::state::*;
use kanto_core::{Prng, Request};

/// Serialised battle size: the encoding is padded to this many bytes.
pub const BATTLE_BYTES: usize = 2048;

pub trait Bin: Sized {
    fn put(&self, o: &mut Vec<u8>);
    fn get(i: &mut &[u8]) -> Option<Self>;
}

fn take<'a>(i: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if i.len() < n {
        return None;
    }
    let (a, b) = i.split_at(n);
    *i = b;
    Some(a)
}

macro_rules! bin_int {
    ($($t:ty),*) => {$(
        impl Bin for $t {
            fn put(&self, o: &mut Vec<u8>) {
                o.extend_from_slice(&self.to_le_bytes());
            }
            fn get(i: &mut &[u8]) -> Option<Self> {
                Some(<$t>::from_le_bytes(take(i, std::mem::size_of::<$t>())?.try_into().ok()?))
            }
        }
    )*};
}
bin_int!(u8, i8, u16, i32, u32, u64, i64);

impl Bin for bool {
    fn put(&self, o: &mut Vec<u8>) {
        o.push(*self as u8);
    }
    fn get(i: &mut &[u8]) -> Option<Self> {
        match take(i, 1)?[0] {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

impl<T: Bin + Copy + Default, const N: usize> Bin for [T; N] {
    fn put(&self, o: &mut Vec<u8>) {
        for x in self {
            x.put(o);
        }
    }
    fn get(i: &mut &[u8]) -> Option<Self> {
        let mut out = [T::default(); N];
        for x in out.iter_mut() {
            *x = T::get(i)?;
        }
        Some(out)
    }
}

impl Bin for Prng {
    fn put(&self, o: &mut Vec<u8>) {
        match self {
            Prng::Gen5(s) => {
                o.push(0);
                s.put(o);
                o.extend_from_slice(&[0; 24]);
            }
            Prng::Sodium(k) => {
                o.push(1);
                o.extend_from_slice(k);
            }
        }
    }
    fn get(i: &mut &[u8]) -> Option<Self> {
        let kind = u8::get(i)?;
        let b = take(i, 32)?;
        match kind {
            0 => Some(Prng::Gen5(u64::from_le_bytes(b[..8].try_into().ok()?))),
            1 => Some(Prng::Sodium(b.try_into().ok()?)),
            _ => None,
        }
    }
}

macro_rules! bin_enum {
    ($t:ty { $($v:path = $n:expr),* $(,)? }) => {
        impl Bin for $t {
            fn put(&self, o: &mut Vec<u8>) {
                o.push(match self { $($v => $n,)* });
            }
            fn get(i: &mut &[u8]) -> Option<Self> {
                match u8::get(i)? { $($n => Some($v),)* _ => None }
            }
        }
    };
}
bin_enum!(Request { Request::None = 0, Request::Move = 1, Request::Switch = 2, Request::Wait = 3 });
bin_enum!(Compat { Compat::Vanilla = 0, Compat::Pkmn = 1 });
bin_enum!(ActionKind {
    ActionKind::BeforeTurn = 0,
    ActionKind::Move = 1,
    ActionKind::Switch = 2,
    ActionKind::InstaSwitch = 3,
    ActionKind::RunSwitch = 4,
    ActionKind::Residual = 5,
});

macro_rules! bin_struct {
    ($t:ident { $($f:ident),* $(,)? }) => {
        impl Bin for $t {
            fn put(&self, o: &mut Vec<u8>) {
                $(self.$f.put(o);)*
            }
            fn get(i: &mut &[u8]) -> Option<Self> {
                Some($t { $($f: Bin::get(i)?,)* })
            }
        }
    };
}

impl Bin for MoveSlot {
    fn put(&self, o: &mut Vec<u8>) {
        o.push(self.id);
        o.push(self.bits());
    }
    fn get(i: &mut &[u8]) -> Option<Self> {
        let b = take(i, 2)?;
        MoveSlot::from_bits(b[0], b[1])
    }
}
bin_struct!(Mon { species, level, hp, stats, status, slp_time, slp_start, n_moves, moves, flags, spe_dv, spe_ev });
bin_struct!(Active {
    species, types, n_slots, virt, transformed, switch_flag, pt_maybe_locked, boosts, speed, stored, modified, vslots,
    vols, vstamp, next_stamp, confusion_time, flinch_dur, pt_dur, pt_source, fpt_dur, fpt_counterpart, ptl_dur,
    ptl_total, ptl_move, ptl_locked, lockedmove_move, lockedmove_time, lockedmove_acc, rage_acc, twoturn_move,
    bide_time, disable_move, disable_slot, disable_time, residual_counter, last_move, move_this_turn, ptl_damage,
    bide_damage, sub_hp,
});
bin_struct!(Side {
    mons, n, order, active, left, last_selected_move, last_enemy_selected_move, last_selected_move_slot, last_move,
    last_enemy_move, fainted_this_turn, request,
});
bin_struct!(Rules { sleep_clause, freeze_clause, compat });
impl Bin for Action {
    fn put(&self, o: &mut Vec<u8>) {
        self.kind().put(o);
        self.order().put(o);
        self.priority().put(o);
        self.speed().put(o);
        self.side().put(o);
        self.data().put(o);
    }
    fn get(i: &mut &[u8]) -> Option<Self> {
        let kind = ActionKind::get(i)?;
        let order = u16::get(i)?;
        let priority = i8::get(i)?;
        let speed = u16::get(i)?;
        let side = u8::get(i)?;
        let data = u8::get(i)?;
        // (The packed form keeps one bit of `side`; actions are only ever on side 0 or 1.)
        if side > 1 {
            return None;
        }
        Some(Action::with(kind, side, data, order, priority, speed))
    }
}
bin_struct!(Queue { len, list });
bin_struct!(Faint { side, slot });
impl Bin for Sides {
    fn put(&self, o: &mut Vec<u8>) {
        self.0.put(o);
    }
    fn get(i: &mut &[u8]) -> Option<Self> {
        Some(Sides(Bin::get(i)?))
    }
}
bin_struct!(Battle {
    sides, rng, turn, last_damage, rules, queue, mid_turn, started, ended, winner, faint_queue, n_faint,
});

impl Battle {
    /// Append exactly [`BATTLE_BYTES`] bytes.
    pub fn to_bytes(&self, out: &mut Vec<u8>) {
        let start = out.len();
        out.reserve(BATTLE_BYTES);
        self.put(out);
        let n = out.len() - start;
        assert!(n <= BATTLE_BYTES, "battle encoding is {n} bytes");
        out.resize(start + BATTLE_BYTES, 0);
    }

    /// Inverse of [`Battle::to_bytes`]; `None` if malformed or if the decoded state fails
    /// [`Battle::validate`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Battle> {
        Battle::from_bytes_checked(bytes).ok()
    }

    /// [`Battle::from_bytes`] with the reason a blob was rejected.
    pub fn from_bytes_checked(bytes: &[u8]) -> Result<Battle, String> {
        if bytes.len() != BATTLE_BYTES {
            return Err(format!("blob is {} bytes, expected {BATTLE_BYTES}", bytes.len()));
        }
        let mut i = bytes;
        let b = Battle::get(&mut i).ok_or("truncated or bad enum/bool encoding")?;
        if i.iter().any(|&x| x != 0) {
            return Err("non-zero padding".into());
        }
        b.validate()?;
        Ok(b)
    }

    /// Check every id, count, range and cross-field invariant the simulator relies on, for a
    /// battle at a decision point (as produced by [`Battle::new`] and stepping, which is the only
    /// kind of state `to_bytes` is called on). A state that passes can be stepped without
    /// panicking; blobs that fail are rejected by [`Battle::from_bytes`].
    pub fn validate(&self) -> Result<(), String> {
        let b = self;
        macro_rules! ensure {
            ($cond:expr, $($fmt:tt)*) => {
                if !$cond {
                    return Err(format!($($fmt)*));
                }
            };
        }
        ensure!(b.winner <= 3, "winner {}", b.winner);
        ensure!(b.ended == (b.winner != 0), "ended {} but winner {}", b.ended, b.winner);
        ensure!(b.turn <= 1001, "turn {}", b.turn);
        ensure!(b.started || (b.turn == 0 && !b.ended && !b.mid_turn), "unstarted battle with progress");
        // Decision points never have queued actions; faints are processed before any request.
        ensure!(b.queue.len == 0, "queue holds {} actions at a decision point", b.queue.len);
        ensure!(b.n_faint as usize <= b.faint_queue.len(), "n_faint {}", b.n_faint);
        ensure!(b.n_faint == 0 || b.ended, "unprocessed faints at a decision point");
        for (p, sd) in b.sides.iter().enumerate() {
            validate_side(b, p, sd).map_err(|e| format!("side {}: {e}", p + 1))?;
        }
        Ok(())
    }
}

/// Real move ids (1..=165, 165 = Struggle).
fn real_move(id: u8) -> bool {
    (1..=165).contains(&id)
}

/// Valid `lastSelectedMove`-style ids: real moves plus `nomove` / `cannotmove`.
fn selected_move(id: u8) -> bool {
    real_move(id) || id == pm::NOMOVE || id == pm::CANNOTMOVE
}

fn valid_slot(s: &MoveSlot) -> bool {
    // (PP is 6 bits by construction.)
    real_move(s.id)
}

/// Whether `stat` is what Showdown computes for this base stat at `level` with some IV (even,
/// 0..=30) and EV (0..=255): the state keeps the stats, not the set.
fn attainable_stat(base: u8, level: u8, stat: u16, hp: bool) -> bool {
    // The stat formula depends on IV + EV / 4, which takes every value in 0..=93; as a function
    // of it the stat never decreases and steps by at most 1 (level <= 100), so the attainable
    // stats are exactly the range between the extremes.
    let lo = crate::battle::calc_stat(base, 0, 0, level, hp);
    let hi = crate::battle::calc_stat(base, 30, 255, level, hp);
    (lo..=hi).contains(&stat)
}

fn validate_side(b: &Battle, p: usize, sd: &Side) -> Result<(), String> {
    use kanto_data::gen1::{NUM_TYPES, SPECIES};
    macro_rules! ensure {
        ($cond:expr, $($fmt:tt)*) => {
            if !$cond {
                return Err(format!($($fmt)*));
            }
        };
    }
    let n = sd.n as usize;
    ensure!((1..=MAX_TEAM).contains(&n), "team size {n}");
    // `order` is a permutation of 0..6 whose first n entries are the team members.
    let mut seen = [false; MAX_TEAM];
    for (i, &o) in sd.order.iter().enumerate() {
        ensure!((o as usize) < MAX_TEAM && !seen[o as usize], "order {:?} is not a permutation", sd.order);
        seen[o as usize] = true;
        ensure!((i < n) == ((o as usize) < n), "order {:?} mixes team and empty slots", sd.order);
    }
    let mut alive = 0;
    for (i, m) in sd.mons.iter().enumerate() {
        if i >= n {
            ensure!(*m == Mon::default(), "empty team slot {i} is not zeroed");
            continue;
        }
        ensure!((1..=151).contains(&m.species), "mon {i}: species {}", m.species);
        ensure!((1..=100).contains(&m.level), "mon {i}: level {}", m.level);
        ensure!(m.spe_dv <= 15, "mon {i}: Speed DV {}", m.spe_dv);
        let base = SPECIES[m.species as usize].base;
        let spe = crate::battle::calc_stat(base[5], m.spe_dv * 2, m.spe_ev, m.level, false);
        ensure!(m.stats[5] == spe, "mon {i}: Speed {} does not match its set ({spe})", m.stats[5]);
        for k in 0..5 {
            ensure!(attainable_stat(base[k], m.level, m.stats[k], k == 0), "mon {i}: stat {k} = {} is not attainable", m.stats[k]);
        }
        ensure!(m.flags & !mf::ALL == 0, "mon {i}: flags {:#x}", m.flags);
        ensure!(m.hp <= m.stats[0], "mon {i}: hp {} > max {}", m.hp, m.stats[0]);
        ensure!(m.status <= st::FNT, "mon {i}: status {}", m.status);
        ensure!((-8..=8).contains(&m.slp_time) && m.slp_start <= 8, "mon {i}: sleep counters");
        let nm = m.n_moves as usize;
        ensure!((1..=4).contains(&nm), "mon {i}: {nm} moves");
        for (k, s) in m.moves.iter().enumerate() {
            if k < nm {
                ensure!(valid_slot(s) && s.id != pm::STRUGGLE, "mon {i}: move slot {k} {s:?}");
            } else {
                ensure!(*s == MoveSlot::default(), "mon {i}: empty move slot {k} is not zeroed");
            }
        }
        ensure!(!m.fainted() || m.hp == 0, "mon {i}: fainted with hp {}", m.hp);
        ensure!(b.ended || m.faint_queued() == m.fainted(), "mon {i}: faint queued but not processed");
        alive += !m.fainted() as usize;
    }
    ensure!(sd.left as usize == alive, "pokemonLeft {} but {alive} alive", sd.left);
    ensure!(selected_move(sd.last_selected_move), "lastSelectedMove {}", sd.last_selected_move);
    ensure!(selected_move(sd.last_enemy_selected_move), "lastEnemySelectedMove {}", sd.last_enemy_selected_move);
    ensure!(sd.last_selected_move_slot <= 3, "lastSelectedMoveSlot {}", sd.last_selected_move_slot);
    for (what, id) in [("lastMove", sd.last_move), ("lastEnemyMove", sd.last_enemy_move)] {
        ensure!(id == 0 || real_move(id) || id == pm::NOMOVE_HACK, "side.{what} {id}");
    }
    let pending = matches!(sd.request, Request::Move | Request::Switch);
    ensure!(!pending || (b.started && !b.ended), "request {:?} outside a running battle", sd.request);
    if sd.request == Request::Switch {
        ensure!(b.can_switch(p) > 0, "switch request with nothing to switch in");
    }
    // The active Pokémon.
    let a = &sd.active;
    if a.species == 0 {
        ensure!(!b.started, "started battle without an active Pokémon");
        ensure!(*a == Active::default(), "unstarted side has active state");
        return Ok(());
    }
    ensure!(b.started, "active Pokémon before the start");
    let m = sd.mon();
    ensure!((1..=151).contains(&a.species), "active species {}", a.species);
    if !a.transformed {
        ensure!(a.species == m.species && a.n_slots == m.n_moves, "active does not match its team member");
    }
    ensure!(a.types.iter().all(|&t| (t as usize) < NUM_TYPES), "types {:?}", a.types);
    for (what, s) in [("stored", a.stored), ("modified", a.modified)] {
        ensure!(s.iter().all(|&x| (1..=999).contains(&x)), "{what} stats {s:?}");
    }
    ensure!(a.boosts.iter().all(|&x| (-6..=6).contains(&x)), "boosts {:?}", a.boosts);
    ensure!((1..=4).contains(&a.n_slots), "{} move slots", a.n_slots);
    for i in 0..4 {
        let s = if a.is_virt(i) { a.vslots[i] } else { m.moves[i] };
        if i < a.n_slots as usize {
            ensure!(valid_slot(&s), "move slot {i} {s:?}");
        }
        ensure!(a.vslots[i].id <= 165, "virtual slot {i}");
    }
    ensure!(a.vols < 1 << v::COUNT, "volatile bits {:#x}", a.vols);
    let small = |x: i8| (-8..=8).contains(&x);
    ensure!(small(a.confusion_time) && small(a.lockedmove_time) && small(a.bide_time) && small(a.disable_time), "volatile counters");
    ensure!(a.flinch_dur <= 1 && a.pt_dur <= 2 && a.fpt_dur <= 2 && a.ptl_dur <= 5 && a.ptl_total <= 5, "volatile durations");
    let foe_n = b.sides[1 - p].n;
    ensure!(a.pt_source < foe_n && a.fpt_counterpart < foe_n && a.ptl_locked < foe_n, "trap partner slots");
    for (what, id) in [
        ("partialtrappinglock.move", a.ptl_move),
        ("lockedmove.move", a.lockedmove_move),
        ("twoturnmove.move", a.twoturn_move),
        ("disable.move", a.disable_move),
        ("lastMove", a.last_move),
        ("moveThisTurn", a.move_this_turn),
    ] {
        ensure!(id <= 165, "{what} {id}");
    }
    ensure!(a.disable_slot <= 3, "disable slot {}", a.disable_slot);
    ensure!(a.virt <= VIRT_ALL, "virtual slot bits {:#x}", a.virt);
    // A broken Substitute can leave a large negative value behind; only a live one is read.
    ensure!(a.sub_hp.unsigned_abs() < 1 << 30, "substitute hp {}", a.sub_hp);
    ensure!(!a.has(v::SUBSTITUTE) || (1..=1024).contains(&a.sub_hp), "substitute hp {}", a.sub_hp);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The O(1) attainability check equals a brute force over every IV and EV.
    #[test]
    fn attainable_stat_matches_brute_force() {
        for base in (1..=255u8).step_by(7).chain([255]) {
            for level in 1..=100u8 {
                for hp in [false, true] {
                    let mut seen = std::collections::BTreeSet::new();
                    for dv in 0..=15u8 {
                        for ev in 0..=255u8 {
                            seen.insert(crate::battle::calc_stat(base, dv * 2, ev, level, hp));
                        }
                    }
                    let (lo, hi) = (*seen.first().unwrap(), *seen.last().unwrap());
                    for stat in lo.saturating_sub(2)..=hi + 2 {
                        assert_eq!(attainable_stat(base, level, stat, hp), seen.contains(&stat), "base {base} level {level} stat {stat}");
                    }
                }
            }
        }
    }
}
