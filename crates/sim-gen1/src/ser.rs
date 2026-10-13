//! Safe, fixed-size binary serialisation of [`Battle`] (for `clone_state` blobs). Every field is
//! written explicitly and validated on read, so a malformed blob is rejected instead of being
//! transmuted into invalid enums/bools.

use crate::state::*;
use sim_core::{Prng, Request};

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

bin_struct!(MoveSlot { id, pp, maxpp, disabled });
bin_struct!(Mon {
    species, level, ivs, evs, stats, hp, status, slp_time, slp_start, status_self, moves, n_moves, speed, fainted,
    faint_queued, sub_fainted, modified,
});
bin_struct!(Active {
    species, types, stored, modified, boosts, transformed, n_slots, virt, vslots, vols, vstamp, next_stamp,
    confusion_time, flinch_dur, pt_dur, pt_maybe_locked, pt_source, fpt_dur, fpt_counterpart, ptl_dur, ptl_total,
    ptl_move, ptl_damage, ptl_locked, lockedmove_move, lockedmove_time, lockedmove_acc, twoturn_move, bide_damage,
    bide_time, disable_move, disable_slot, disable_time, sub_hp, rage_acc, residual_counter, last_move,
    move_this_turn, trapped, maybe_locked, maybe_disabled, switch_flag,
});
bin_struct!(Side {
    mons, n, order, active, left, last_selected_move, last_enemy_selected_move, last_selected_move_slot, last_move,
    last_enemy_move, fainted_this_turn, request,
});
bin_struct!(Rules { sleep_clause, freeze_clause, compat });
bin_struct!(Action { kind, order, priority, speed, side, data });
bin_struct!(Queue { len, list });
bin_struct!(Faint { side, slot });
bin_struct!(Battle {
    sides, rng, turn, last_damage, rules, queue, mid_turn, started, ended, winner, faint_queue, n_faint,
});

impl Battle {
    /// Append exactly [`BATTLE_BYTES`] bytes.
    pub fn to_bytes(&self, out: &mut Vec<u8>) {
        let start = out.len();
        self.put(out);
        let n = out.len() - start;
        assert!(n <= BATTLE_BYTES, "battle encoding is {n} bytes");
        out.resize(start + BATTLE_BYTES, 0);
    }

    /// Inverse of [`Battle::to_bytes`]; `None` if malformed.
    pub fn from_bytes(bytes: &[u8]) -> Option<Battle> {
        if bytes.len() != BATTLE_BYTES {
            return None;
        }
        let mut i = bytes;
        let b = Battle::get(&mut i)?;
        if i.iter().any(|&x| x != 0) || b.queue.len as usize > QUEUE_CAP || b.n_faint as usize > b.faint_queue.len() {
            return None;
        }
        for sd in &b.sides {
            if sd.n == 0 || sd.n as usize > MAX_TEAM || sd.order.iter().any(|&o| o as usize >= MAX_TEAM) {
                return None;
            }
        }
        Some(b)
    }
}
