//! Raw FFI bindings to the subset of `pkmn.h` (pkmn/engine) needed for Generation I.
//!
//! libpkmn is built by `build.rs` with `-Dshowdown -Dlog`. The C ABI does not expose battle
//! construction: per the engine's documentation, drivers in other languages serialize the
//! `Battle` bytes themselves following `src/lib/gen1/README.md#layout` / `src/data/layout.json`
//! (this is what the engine's own TypeScript driver does). `kanto-env` does that; this crate only
//! provides the byte-level types, layout constants and the extern functions.
#![allow(non_camel_case_types)]

/// Directory containing the built `libpkmn-showdown.a` (for linking test drivers).
pub const LIB_DIR: &str = env!("PKMN_LIB_DIR");
/// Directory containing the installed `pkmn.h`.
pub const INCLUDE_DIR: &str = env!("PKMN_INCLUDE_DIR");

/// `PKMN_GEN1_BATTLE_SIZE`.
pub const GEN1_BATTLE_SIZE: usize = 384;
/// `PKMN_GEN1_BATTLE_OPTIONS_SIZE`.
pub const GEN1_BATTLE_OPTIONS_SIZE: usize = 128;
/// `PKMN_PSRNG_SIZE`.
pub const PSRNG_SIZE: usize = 8;

/// A Generation I battle (`pkmn_gen1_battle`). The engine treats this as a Zig struct with
/// 8-byte alignment (the Showdown RNG is a `u64`), so we align it accordingly.
#[repr(C, align(8))]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct pkmn_gen1_battle {
    pub bytes: [u8; GEN1_BATTLE_SIZE],
}

impl Default for pkmn_gen1_battle {
    fn default() -> Self {
        Self { bytes: [0; GEN1_BATTLE_SIZE] }
    }
}

impl std::fmt::Debug for pkmn_gen1_battle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pkmn_gen1_battle(")?;
        for b in self.bytes {
            write!(f, "{b:02x}")?;
        }
        write!(f, ")")
    }
}

/// Opaque Generation I battle options (`pkmn_gen1_battle_options`). Note: once initialised with
/// log options it contains a pointer into itself, so it must not be moved between
/// `pkmn_gen1_battle_options_set` and `pkmn_gen1_battle_update`.
#[repr(C, align(8))]
pub struct pkmn_gen1_battle_options {
    pub bytes: [u8; GEN1_BATTLE_OPTIONS_SIZE],
}

impl Default for pkmn_gen1_battle_options {
    fn default() -> Self {
        Self { bytes: [0; GEN1_BATTLE_OPTIONS_SIZE] }
    }
}

/// `pkmn_gen1_log_options`.
#[repr(C)]
pub struct pkmn_gen1_log_options {
    pub buf: *mut u8,
    pub len: usize,
}

/// Pokémon Showdown's RNG (`pkmn_psrng`).
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct pkmn_psrng {
    pub bytes: [u8; PSRNG_SIZE],
}

/// `pkmn_options`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct pkmn_options {
    pub showdown: bool,
    pub log: bool,
    pub chance: bool,
    pub calc: bool,
}

pub type pkmn_choice = u8;
pub type pkmn_result = u8;
pub type pkmn_choice_kind = u8;
pub type pkmn_result_kind = u8;
pub type pkmn_player = u8;

pub const PKMN_PLAYER_P1: pkmn_player = 0;
pub const PKMN_PLAYER_P2: pkmn_player = 1;

pub const PKMN_CHOICE_PASS: pkmn_choice_kind = 0;
pub const PKMN_CHOICE_MOVE: pkmn_choice_kind = 1;
pub const PKMN_CHOICE_SWITCH: pkmn_choice_kind = 2;

pub const PKMN_RESULT_NONE: pkmn_result_kind = 0;
pub const PKMN_RESULT_WIN: pkmn_result_kind = 1;
pub const PKMN_RESULT_LOSE: pkmn_result_kind = 2;
pub const PKMN_RESULT_TIE: pkmn_result_kind = 3;
pub const PKMN_RESULT_ERROR: pkmn_result_kind = 4;

extern "C" {
    pub static PKMN_OPTIONS: pkmn_options;
    pub static PKMN_GEN1_MAX_CHOICES: usize;
    pub static PKMN_GEN1_CHOICES_SIZE: usize;
    pub static PKMN_GEN1_MAX_LOGS: usize;
    pub static PKMN_GEN1_LOGS_SIZE: usize;

    pub fn pkmn_choice_init(kind: pkmn_choice_kind, data: u8) -> pkmn_choice;
    pub fn pkmn_choice_type(choice: pkmn_choice) -> pkmn_choice_kind;
    pub fn pkmn_choice_data(choice: pkmn_choice) -> u8;

    pub fn pkmn_result_type(result: pkmn_result) -> pkmn_result_kind;
    pub fn pkmn_result_p1(result: pkmn_result) -> pkmn_choice_kind;
    pub fn pkmn_result_p2(result: pkmn_result) -> pkmn_choice_kind;
    pub fn pkmn_error(result: pkmn_result) -> bool;

    pub fn pkmn_psrng_init(psrng: *mut pkmn_psrng, seed: u64);
    pub fn pkmn_psrng_next(psrng: *mut pkmn_psrng) -> u32;

    pub fn pkmn_gen1_battle_options_set(
        options: *mut pkmn_gen1_battle_options,
        log: *const pkmn_gen1_log_options,
        chance: *const core::ffi::c_void,
        calc: *const core::ffi::c_void,
    );

    pub fn pkmn_gen1_battle_update(
        battle: *mut pkmn_gen1_battle,
        c1: pkmn_choice,
        c2: pkmn_choice,
        options: *mut pkmn_gen1_battle_options,
    ) -> pkmn_result;

    pub fn pkmn_gen1_battle_choices(
        battle: *const pkmn_gen1_battle,
        player: pkmn_player,
        request: pkmn_choice_kind,
        out: *mut pkmn_choice,
        len: usize,
    ) -> u8;
}

/// Pure-Rust equivalents of the trivial bit-packing helpers (identical to the engine's packed
/// structs; avoids an FFI call in hot loops). Verified against the C functions in tests.
pub mod bits {
    use super::*;

    /// `Choice = packed struct(u8) { type: u2, data: u6 }`.
    #[inline(always)]
    pub const fn choice(kind: pkmn_choice_kind, data: u8) -> pkmn_choice {
        (kind & 0b11) | (data << 2)
    }
    #[inline(always)]
    pub const fn choice_kind(c: pkmn_choice) -> pkmn_choice_kind {
        c & 0b11
    }
    #[inline(always)]
    pub const fn choice_data(c: pkmn_choice) -> u8 {
        c >> 2
    }
    /// `Result = packed struct(u8) { type: u4, p1: u2, p2: u2 }`.
    #[inline(always)]
    pub const fn result_kind(r: pkmn_result) -> pkmn_result_kind {
        r & 0x0f
    }
    #[inline(always)]
    pub const fn result_p1(r: pkmn_result) -> pkmn_choice_kind {
        (r >> 4) & 0b11
    }
    #[inline(always)]
    pub const fn result_p2(r: pkmn_result) -> pkmn_choice_kind {
        (r >> 6) & 0b11
    }
}

/// Byte offsets from `src/data/layout.json` (Gen 1, Showdown mode).
pub mod layout {
    pub const SIDE_SIZE: usize = 184;
    pub const POKEMON_SIZE: usize = 24;

    pub const BATTLE_SIDES: usize = 0;
    pub const BATTLE_TURN: usize = 368;
    pub const BATTLE_LAST_DAMAGE: usize = 370;
    pub const BATTLE_LAST_MOVES: usize = 372;
    /// Showdown mode: 64-bit seed at 376 (native endian).
    pub const BATTLE_RNG: usize = 376;

    pub const SIDE_POKEMON: usize = 0;
    pub const SIDE_ACTIVE: usize = 144;
    pub const SIDE_ORDER: usize = 176;
    pub const SIDE_LAST_SELECTED_MOVE: usize = 182;
    pub const SIDE_LAST_USED_MOVE: usize = 183;

    pub const POKEMON_STATS: usize = 0;
    pub const POKEMON_MOVES: usize = 10;
    pub const POKEMON_HP: usize = 18;
    pub const POKEMON_STATUS: usize = 20;
    pub const POKEMON_SPECIES: usize = 21;
    pub const POKEMON_TYPES: usize = 22;
    pub const POKEMON_LEVEL: usize = 23;

    pub const ACTIVE_STATS: usize = 0;
    pub const ACTIVE_SPECIES: usize = 10;
    pub const ACTIVE_TYPES: usize = 11;
    pub const ACTIVE_BOOSTS: usize = 12;
    pub const ACTIVE_VOLATILES: usize = 16;
    pub const ACTIVE_MOVES: usize = 24;

    /// Bit offsets within the 64-bit `Volatiles` field.
    pub mod volatiles {
        pub const BIDE: u32 = 0;
        pub const THRASHING: u32 = 1;
        pub const MULTI_HIT: u32 = 2;
        pub const FLINCH: u32 = 3;
        pub const CHARGING: u32 = 4;
        pub const BINDING: u32 = 5;
        pub const INVULNERABLE: u32 = 6;
        pub const CONFUSION: u32 = 7;
        pub const MIST: u32 = 8;
        pub const FOCUS_ENERGY: u32 = 9;
        pub const SUBSTITUTE: u32 = 10;
        pub const RECHARGING: u32 = 11;
        pub const RAGE: u32 = 12;
        pub const LEECH_SEED: u32 = 13;
        pub const TOXIC: u32 = 14;
        pub const LIGHT_SCREEN: u32 = 15;
        pub const REFLECT: u32 = 16;
        pub const TRANSFORM: u32 = 17;
        pub const CONFUSION_TURNS: u32 = 18; // 3 bits
        pub const ATTACKS: u32 = 21; // 3 bits
        pub const STATE: u32 = 24; // 16 bits
        pub const SUBSTITUTE_HP: u32 = 40; // 8 bits
        pub const TRANSFORM_ID: u32 = 48; // 4 bits
        pub const DISABLE_DURATION: u32 = 52; // 4 bits
        pub const DISABLE_MOVE: u32 = 56; // 3 bits
        pub const TOXIC_COUNTER: u32 = 59; // 5 bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_and_sizes() {
        unsafe {
            assert!(PKMN_OPTIONS.showdown, "libpkmn must be built with -Dshowdown");
            assert!(PKMN_OPTIONS.log, "libpkmn must be built with -Dlog");
            assert!(PKMN_GEN1_CHOICES_SIZE >= PKMN_GEN1_MAX_CHOICES);
            assert!(PKMN_GEN1_LOGS_SIZE >= PKMN_GEN1_MAX_LOGS);
        }
    }

    #[test]
    fn bit_helpers_match_c() {
        for kind in 0..3u8 {
            for data in 0..=6u8 {
                let c = unsafe { pkmn_choice_init(kind, data) };
                assert_eq!(c, bits::choice(kind, data));
                assert_eq!(unsafe { pkmn_choice_type(c) }, kind);
                assert_eq!(unsafe { pkmn_choice_data(c) }, data);
            }
        }
        for r in 0..=255u8 {
            if r == 0b1100 {
                continue; // error sentinel
            }
            assert_eq!(unsafe { pkmn_result_type(r) }, bits::result_kind(r));
            assert_eq!(unsafe { pkmn_result_p1(r) }, bits::result_p1(r));
            assert_eq!(unsafe { pkmn_result_p2(r) }, bits::result_p2(r));
        }
    }

    #[test]
    fn psrng_is_deterministic() {
        let mut a = pkmn_psrng::default();
        let mut b = pkmn_psrng::default();
        unsafe {
            pkmn_psrng_init(&mut a, 42);
            pkmn_psrng_init(&mut b, 42);
            for _ in 0..100 {
                assert_eq!(pkmn_psrng_next(&mut a), pkmn_psrng_next(&mut b));
            }
        }
    }
}
