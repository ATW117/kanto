//! Showdown's `Battle#speedSort` and `comparePriority`, including the RNG consumed by ties.
//!
//! Showdown sorts event handlers and actions with a selection sort; every group of items that
//! compare equal is shuffled with `PRNG#shuffle`, which consumes `len - 1` PRNG calls. Whether
//! anything ties therefore changes the RNG stream even when the order of the tied items does not
//! matter, so the native simulator runs the same algorithm on the same keys.

use crate::prng::Prng;

/// Sort key of an action or event handler (`comparePriority`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Priority {
    /// `order` (low first); Showdown's "unset" value is 2^32.
    pub order: u64,
    /// `priority` (high first).
    pub priority: i32,
    /// `speed` (high first).
    pub speed: i64,
    /// `subOrder` (low first).
    pub sub_order: i32,
    /// `effectOrder` (low first).
    pub effect_order: i32,
}

/// Showdown's default `order` for handlers/actions without one.
pub const ORDER_UNSET: u64 = 4_294_967_296;

impl Priority {
    pub const fn new(order: u64, priority: i32, speed: i64, sub_order: i32) -> Priority {
        Priority { order, priority, speed, sub_order, effect_order: 0 }
    }
}

/// `comparePriority(a, b)`: negative if `a` goes first, positive if `b` goes first, 0 on a tie.
#[inline]
pub fn compare(a: &Priority, b: &Priority) -> i64 {
    let d = a.order as i64 - b.order as i64;
    if d != 0 {
        return d;
    }
    let d = b.priority as i64 - a.priority as i64;
    if d != 0 {
        return d;
    }
    let d = b.speed - a.speed;
    if d != 0 {
        return d;
    }
    let d = a.sub_order as i64 - b.sub_order as i64;
    if d != 0 {
        return d;
    }
    a.effect_order as i64 - b.effect_order as i64
}

/// Shuffle a group of `len` tied items starting at `start` (`len - 1` PRNG calls).
#[inline(always)]
fn shuffle_ties<T>(rng: &mut Prng, list: &mut [T], start: usize, len: usize, label: &'static str) {
    if len > 1 {
        rng.shuffle(list, start, start + len, label);
    }
}

/// `Battle#speedSort(list, comparator)`: sorts `list` in place, shuffling ties with `rng`.
#[inline]
pub fn speed_sort<T: Copy>(rng: &mut Prng, list: &mut [T], cmp: impl Fn(&T, &T) -> i64, label: &'static str) {
    let n = list.len();
    if n < 2 {
        return;
    }
    if n == 2 {
        // The selection sort below, unrolled for two items (the common case: same PRNG calls).
        let delta = cmp(&list[0], &list[1]);
        if delta > 0 {
            list.swap(0, 1);
        }
        shuffle_ties(rng, list, 0, if delta == 0 { 2 } else { 1 }, label);
        return;
    }
    speed_sort_n(rng, list, cmp, label)
}

fn speed_sort_n<T: Copy>(rng: &mut Prng, list: &mut [T], cmp: impl Fn(&T, &T) -> i64, label: &'static str) {
    let n = list.len();
    let mut sorted = 0;
    let mut next = [0usize; 16];
    while sorted + 1 < n {
        let mut len = 1;
        next[0] = sorted;
        for i in sorted + 1..n {
            let delta = cmp(&list[next[0]], &list[i]);
            if delta < 0 {
                continue;
            }
            if delta > 0 {
                len = 1;
                next[0] = i;
            } else {
                next[len] = i;
                len += 1;
            }
        }
        for i in 0..len {
            let index = next[i];
            if index != sorted + i {
                list.swap(sorted + i, index);
            }
        }
        shuffle_ties(rng, list, sorted, len, label);
        sorted += len;
    }
}

/// `order` value of [`key`] for Showdown's unset order (2^32).
pub const KEY_ORDER_UNSET: u16 = u16::MAX;

/// A `comparePriority` sort key packed into a `u64`: ascending integer order is Showdown's order
/// (`order` low first, `priority` high first, `speed` high first, `subOrder` low first) and equal
/// keys are exactly the ties. Covers every key the simulator sorts: orders up to 300 (or
/// [`KEY_ORDER_UNSET`]), priorities in `i8`, speeds in `u16`, sub-orders in `u8`.
#[inline(always)]
pub const fn key(order: u16, priority: i8, speed: u16, sub_order: u8) -> u64 {
    (order as u64) << 48 | ((127 - priority as i16) as u64) << 40 | ((u16::MAX - speed) as u64) << 16 | sub_order as u64
}

/// [`speed_sort`] on `(key, payload)` pairs ([`key`]).
#[inline]
pub fn sort_by_key<T: Copy>(rng: &mut Prng, list: &mut [(u64, T)], label: &'static str) {
    speed_sort(rng, list, |a, b| a.0.cmp(&b.0) as i64, label);
}

/// [`speed_sort`] on `(Priority, payload)` pairs with `comparePriority`.
pub fn sort_by_priority<T: Copy>(rng: &mut Prng, list: &mut [(Priority, T)], label: &'static str) {
    speed_sort(rng, list, |a, b| compare(&a.0, &b.0), label);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tie_no_rng() {
        let mut rng = Prng::Gen5(1);
        let before = rng;
        let mut l = [(Priority::new(ORDER_UNSET, 0, 10, 0), 0), (Priority::new(ORDER_UNSET, 0, 20, 0), 1)];
        sort_by_priority(&mut rng, &mut l, "t");
        assert_eq!(l[0].1, 1);
        assert_eq!(rng, before);
    }

    /// The packed key orders exactly like `comparePriority` (sign of the comparison).
    #[test]
    fn key_matches_compare_priority() {
        let orders = [3u16, 4, 101, 103, 200, 300, KEY_ORDER_UNSET];
        let full = |o: u16| if o == KEY_ORDER_UNSET { ORDER_UNSET } else { o as u64 };
        let mut items = Vec::new();
        for &o in &orders {
            for prio in [-128i8, -1, 0, 1, 2, 7, 100, 127] {
                for speed in [0u16, 1, 2, 300, 999, 65535] {
                    for sub in [0u8, 2, 5, 255] {
                        items.push((key(o, prio, speed, sub), Priority::new(full(o), prio as i32, speed as i64, sub as i32)));
                    }
                }
            }
        }
        for a in &items {
            for b in items.iter().step_by(7) {
                assert_eq!(a.0.cmp(&b.0) as i64, compare(&a.1, &b.1).signum(), "{:?} vs {:?}", a.1, b.1);
            }
        }
    }

    /// The two-item fast path makes the same swaps and PRNG calls as the general selection sort.
    #[test]
    fn two_item_fast_path_matches_general_sort() {
        for seed in 0..200u64 {
            for (x, y) in [(1i64, 2i64), (2, 1), (5, 5)] {
                let (mut r1, mut r2) = (Prng::Gen5(seed), Prng::Gen5(seed));
                let mut a = [(x, 0u8), (y, 1u8)];
                let mut b = a;
                speed_sort(&mut r1, &mut a, |p, q| q.0 - p.0, "t");
                speed_sort_n(&mut r2, &mut b, |p, q| q.0 - p.0, "t");
                assert_eq!((a, r1), (b, r2));
            }
        }
    }

    #[test]
    fn tie_consumes_rng() {
        let mut rng = Prng::Gen5(1);
        let before = rng;
        let mut l = [(Priority::new(ORDER_UNSET, 0, 10, 0), 0), (Priority::new(ORDER_UNSET, 0, 10, 0), 1)];
        sort_by_priority(&mut rng, &mut l, "t");
        assert_ne!(rng, before);
    }
}
