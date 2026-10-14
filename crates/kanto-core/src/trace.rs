//! Optional PRNG call recording (feature `trace`).
//!
//! When the feature is enabled and recording is switched on for the current thread with
//! [`start`], every PRNG helper call appends a [`RngCall`]; [`take`] returns and clears them.
//! Without the feature, [`record`] compiles to nothing.

/// One PRNG call: the call-site label, the range `[lo, hi)` (`random(n)` is `[0, n)`) and the
/// returned value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RngCall {
    pub label: &'static str,
    pub lo: u32,
    pub hi: u32,
    pub value: u32,
}

#[cfg(feature = "trace")]
mod imp {
    use super::RngCall;
    use std::cell::RefCell;

    thread_local! {
        static BUF: RefCell<Option<Vec<RngCall>>> = const { RefCell::new(None) };
    }

    #[inline]
    pub fn record(label: &'static str, lo: u32, hi: u32, value: u32) {
        BUF.with(|b| {
            if let Some(v) = b.borrow_mut().as_mut() {
                v.push(RngCall { label, lo, hi, value });
            }
        });
    }

    pub fn start() {
        BUF.with(|b| *b.borrow_mut() = Some(Vec::new()));
    }

    pub fn take() -> Vec<RngCall> {
        BUF.with(|b| b.borrow_mut().as_mut().map(std::mem::take).unwrap_or_default())
    }

    pub fn stop() {
        BUF.with(|b| *b.borrow_mut() = None);
    }
}

#[cfg(not(feature = "trace"))]
mod imp {
    use super::RngCall;

    #[inline(always)]
    pub fn record(_label: &'static str, _lo: u32, _hi: u32, _value: u32) {}
    pub fn start() {}
    pub fn take() -> Vec<RngCall> {
        Vec::new()
    }
    pub fn stop() {}
}

pub use imp::{record, start, stop, take};

/// Whether tracing is compiled in.
pub const ENABLED: bool = cfg!(feature = "trace");
