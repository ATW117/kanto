//! Small, fast, deterministic RNG (SplitMix64) used for env-side randomness: team sampling, battle
//! seeds and built-in opponents. Battle randomness itself is the engine's Showdown PRNG, seeded
//! from this.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rng {
    state: u64,
}

#[inline]
pub fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Per-env seed derived from the master seed and env index; independent of thread count.
pub fn derive_seed(master: u64, index: u64) -> u64 {
    splitmix64(splitmix64(master) ^ splitmix64(index.wrapping_add(0x5EED)))
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng { state: splitmix64(seed) }
    }

    pub fn state(&self) -> u64 {
        self.state
    }

    pub fn from_state(state: u64) -> Rng {
        Rng { state }
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform integer in [0, n) (Lemire's multiply-shift; bias is negligible for small n).
    #[inline]
    pub fn below(&mut self, n: u64) -> u64 {
        debug_assert!(n > 0);
        ((self.next_u64() as u128 * n as u128) >> 64) as u64
    }
}
