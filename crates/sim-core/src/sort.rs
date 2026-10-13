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

/// `Battle#speedSort(list, comparator)`: sorts `list` in place, shuffling ties with `rng`.
pub fn speed_sort<T: Copy>(rng: &mut Prng, list: &mut [T], cmp: impl Fn(&T, &T) -> i64, label: &'static str) {
    let n = list.len();
    if n < 2 {
        return;
    }
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
        if len > 1 {
            rng.shuffle(list, sorted, sorted + len, label);
        }
        sorted += len;
    }
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

    #[test]
    fn tie_consumes_rng() {
        let mut rng = Prng::Gen5(1);
        let before = rng;
        let mut l = [(Priority::new(ORDER_UNSET, 0, 10, 0), 0), (Priority::new(ORDER_UNSET, 0, 10, 0), 1)];
        sort_by_priority(&mut rng, &mut l, "t");
        assert_ne!(rng, before);
    }
}
