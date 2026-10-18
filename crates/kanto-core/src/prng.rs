//! Pokémon Showdown's PRNG (`sim/prng.ts`, pokemon-showdown 0.11.11), bit-exact.
//!
//! Showdown picks the generator from the seed string, for every generation:
//! - `"a,b,c,d"` (four 16-bit words, big-endian) or `"gen5,xxxx..."`: the Gen 5 64-bit LCG
//!   `s = s * 0x5D588B656C078965 + 0x269EC3`, returning the upper 32 bits.
//! - `"sodium,<hex>"`: ChaCha20 keystream (libsodium `randombytes_buf_deterministic`): each call
//!   encrypts 36 zero bytes with key = seed and nonce = "LibsodiumDRG"; bytes 0..32 become the next
//!   seed and bytes 32..36 (big-endian) are the output. This is Showdown's default when no seed is
//!   given.
//!
//! The derived helpers reproduce `PRNG#random`, `randomChance`, `sample` and `shuffle` exactly
//! (`Math.floor(r * n / 2**32)` == `(r * n) >> 32` for the integer ranges used).
//!
//! Every helper takes a static `label` naming the call site; with the `trace` feature the calls
//! are recorded (see [`crate::trace`]) so the parity harness can align them with Showdown's.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prng {
    /// Gen 5 LCG state (the full 64-bit seed).
    Gen5(u64),
    /// ChaCha20 key (32 bytes).
    Sodium([u8; 32]),
}

const LCG_A: u64 = 0x5D58_8B65_6C07_8965;
const LCG_C: u64 = 0x0026_9EC3;

impl Prng {
    /// A Gen 5 PRNG from Showdown's four 16-bit seed words (`"a,b,c,d"`).
    pub fn gen5(words: [u16; 4]) -> Prng {
        Prng::Gen5((words[0] as u64) << 48 | (words[1] as u64) << 32 | (words[2] as u64) << 16 | words[3] as u64)
    }

    /// A Gen 5 PRNG from a 64-bit seed; equivalent to [`Prng::gen5`] on its big-endian 16-bit words.
    pub fn from_u64(seed: u64) -> Prng {
        Prng::Gen5(seed)
    }

    /// A sodium PRNG from the hex part of a `"sodium,<hex>"` seed (padded with zeros to 32 bytes).
    pub fn sodium_hex(hex: &str) -> Option<Prng> {
        let mut key = [0u8; 32];
        let hex = hex.as_bytes();
        if hex.len() > 64 || hex.len() % 2 != 0 {
            return None;
        }
        for (i, pair) in hex.chunks(2).enumerate() {
            let s = std::str::from_utf8(pair).ok()?;
            key[i] = u8::from_str_radix(s, 16).ok()?;
        }
        Some(Prng::Sodium(key))
    }

    /// Parse a Showdown seed string.
    pub fn parse(seed: &str) -> Option<Prng> {
        if let Some(hex) = seed.strip_prefix("sodium,") {
            return Prng::sodium_hex(hex);
        }
        if let Some(h) = seed.strip_prefix("gen5,") {
            if h.len() < 16 {
                return None;
            }
            let mut w = [0u16; 4];
            for (i, wi) in w.iter_mut().enumerate() {
                *wi = u16::from_str_radix(&h[4 * i..4 * i + 4], 16).ok()?;
            }
            return Some(Prng::gen5(w));
        }
        let parts: Vec<&str> = seed.split(',').collect();
        if parts.len() != 4 {
            return None;
        }
        let mut w = [0u16; 4];
        for (i, p) in parts.iter().enumerate() {
            w[i] = p.trim().parse().ok()?;
        }
        Some(Prng::gen5(w))
    }

    /// The seed in Showdown's `PRNG#getSeed()` format.
    pub fn seed_string(&self) -> String {
        match self {
            Prng::Gen5(s) => format!("{},{},{},{}", s >> 48, (s >> 32) & 0xFFFF, (s >> 16) & 0xFFFF, s & 0xFFFF),
            Prng::Sodium(k) => {
                let mut out = String::from("sodium,");
                for b in k {
                    out.push_str(&format!("{b:02x}"));
                }
                out
            }
        }
    }

    /// Next raw 32-bit output (`RNG#next`).
    #[inline(always)]
    pub fn next_u32(&mut self) -> u32 {
        match self {
            Prng::Gen5(s) => {
                *s = s.wrapping_mul(LCG_A).wrapping_add(LCG_C);
                (*s >> 32) as u32
            }
            // Out of line: inlining the ChaCha20 block into every call site bloats the hot
            // paths of the (common) Gen 5 case.
            Prng::Sodium(key) => sodium_next(key),
        }
    }

    /// `random(n)`: uniform integer in `[0, n)`.
    #[inline(always)]
    pub fn random(&mut self, n: u32, label: &'static str) -> u32 {
        let r = self.next_u32();
        let v = ((r as u64 * n as u64) >> 32) as u32;
        crate::trace::record(label, 0, n, v);
        v
    }

    /// `random(m, n)`: uniform integer in `[m, n)`.
    #[inline(always)]
    pub fn random_range(&mut self, m: u32, n: u32, label: &'static str) -> u32 {
        let r = self.next_u32();
        let v = ((r as u64 * (n - m) as u64) >> 32) as u32 + m;
        crate::trace::record(label, m, n, v);
        v
    }

    /// `randomChance(num, den)`: `random(den) < num`.
    #[inline(always)]
    pub fn chance(&mut self, num: u32, den: u32, label: &'static str) -> bool {
        self.random(den, label) < num
    }

    /// `sample(items)`: index of a uniformly chosen item of a list of length `len`.
    #[inline(always)]
    pub fn sample(&mut self, len: u32, label: &'static str) -> u32 {
        self.random(len, label)
    }

    /// `shuffle(items, start, end)` (Fisher-Yates as Showdown implements it) applied to `items`.
    pub fn shuffle<T>(&mut self, items: &mut [T], mut start: usize, end: usize, label: &'static str) {
        while start + 1 < end {
            let next = self.random_range(start as u32, end as u32, label) as usize;
            if next != start {
                items.swap(start, next);
            }
            start += 1;
        }
    }
}

/// One sodium `RNG#next`: a ChaCha20 block keyed by the seed; the first 32 bytes are the next
/// seed, bytes 32..36 (big-endian) the output.
#[cold]
#[inline(never)]
fn sodium_next(key: &mut [u8; 32]) -> u32 {
    let block = chacha20_block(key, 0, b"LibsodiumDRG");
    key.copy_from_slice(&block[..32]);
    u32::from_be_bytes([block[32], block[33], block[34], block[35]])
}

#[inline(always)]
fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One RFC 7539 ChaCha20 keystream block (32-bit counter, 96-bit nonce).
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let le = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let mut init = [0u32; 16];
    init[0] = 0x6170_7865;
    init[1] = 0x3320_646e;
    init[2] = 0x7962_2d32;
    init[3] = 0x6b20_6574;
    for i in 0..8 {
        init[4 + i] = le(&key[4 * i..]);
    }
    init[12] = counter;
    for i in 0..3 {
        init[13 + i] = le(&nonce[4 * i..]);
    }
    let mut s = init;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&s[i].wrapping_add(init[i]).to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_roundtrip() {
        let p = Prng::parse("1,2,3,4").unwrap();
        assert_eq!(p.seed_string(), "1,2,3,4");
        let s = Prng::parse("sodium,00ff").unwrap();
        assert!(s.seed_string().starts_with("sodium,00ff00"));
    }

    /// Vectors from pokemon-showdown 0.11.11 `PRNG` (5 raw outputs, then random(256),
    /// random(217, 256), random(2, 6), then getSeed()).
    #[test]
    fn showdown_vectors() {
        let cases: [(&str, [u32; 8], &str); 3] = [
            (
                "1,2,3,4",
                [2030470262, 3793892072, 2851743046, 574702432, 3620926519, 147, 254, 3],
                "25495,32001,50713,50540",
            ),
            (
                "65535,0,4660,22136",
                [1763894022, 1538292574, 3897545321, 47068229, 1678850042, 240, 251, 3],
                "22687,17018,1049,53600",
            ),
            (
                "sodium,0123456789abcdef0123456789abcdef",
                [1564598223, 2628916691, 3021397871, 1916167142, 4157841405, 64, 232, 3],
                "sodium,20f55f41ac83804d7a63f683773b041adc115d9c518501d744beaf5da3ce585d",
            ),
        ];
        for (seed, outs, end) in cases {
            let mut p = Prng::parse(seed).unwrap();
            for &o in &outs[..5] {
                assert_eq!(p.next_u32(), o, "{seed}");
            }
            assert_eq!(p.random(256, "t"), outs[5]);
            assert_eq!(p.random_range(217, 256, "t"), outs[6]);
            assert_eq!(p.random_range(2, 6, "t"), outs[7]);
            assert_eq!(p.seed_string(), end);
        }
    }

    #[test]
    fn rfc7539_block() {
        // RFC 7539 section 2.3.2 test vector.
        let mut key = [0u8; 32];
        for (i, k) in key.iter_mut().enumerate() {
            *k = i as u8;
        }
        let nonce = [0, 0, 0, 0x09, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let b = chacha20_block(&key, 1, &nonce);
        assert_eq!(&b[..4], &[0x10, 0xf1, 0xe7, 0xe4]);
        assert_eq!(&b[60..], &[0xa2, 0x50, 0x3c, 0x4e]);
    }
}
