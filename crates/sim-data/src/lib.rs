//! Static data tables for the native simulator, generated per generation from Pokémon Showdown's
//! resolved dex by `tools/gen_sim_data.js` (see docs/SIM_DESIGN.md). Only plain data lives here;
//! behaviour (move callbacks, conditions) is hand-written in the per-generation crates.

pub mod gen1;

/// A species' static data. `types[0] == types[1]` for single-typed species.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpeciesData {
    /// Showdown ID (`"mrmime"`).
    pub id: &'static str,
    pub name: &'static str,
    pub types: [u8; 2],
    /// Base stats `[hp, atk, def, spa, spd, spe]` (Gen 1: spa == spd == Special).
    pub base: [u8; 6],
}

/// A move's secondary effect (Gen 1 moves have at most one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Secondary {
    /// Percent chance (0 = always).
    pub chance: u8,
    pub status: u8,
    pub volatile: u8,
    /// `[atk, def, spa, spd, spe, accuracy, evasion]`
    pub boosts: [i8; 7],
}

/// A move's static data, as resolved by Showdown for the generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveData {
    pub num: u8,
    pub id: &'static str,
    pub name: &'static str,
    pub type_: u8,
    /// 0 Physical, 1 Special, 2 Status.
    pub category: u8,
    pub base_power: u8,
    /// 0 means `true` (never misses).
    pub accuracy: u8,
    pub pp: u8,
    pub priority: i8,
    /// Index into `target::*`.
    pub target: u8,
    pub crit_ratio: u8,
    /// 0 = single hit, 2 = two hits, 25 = 2-5 hits.
    pub multihit: u8,
    /// `[numerator, denominator]`, `[0, 0]` if none.
    pub recoil: [u8; 2],
    /// Heals half the damage dealt.
    pub drain: bool,
    pub status: u8,
    pub volatile: u8,
    pub self_volatile: u8,
    /// `[atk, def, spa, spd, spe, accuracy, evasion]`
    pub boosts: [i8; 7],
    pub secondary: Option<Secondary>,
    pub ohko: bool,
    /// Fixed damage: 0 none, 255 = user's level, otherwise the amount.
    pub damage: u8,
    pub selfdestruct: bool,
    /// `ignoreImmunity === true`.
    pub ignore_immunity: bool,
    /// 0 `willCrit: false`, 1 `true`, 2 unset.
    pub will_crit: u8,
    /// Has the `metronome` flag.
    pub metronome: bool,
}

impl MoveData {
    pub const NONE: MoveData = MoveData {
        num: 0,
        id: "",
        name: "",
        type_: 0,
        category: 2,
        base_power: 0,
        accuracy: 0,
        pp: 0,
        priority: 0,
        target: 0,
        crit_ratio: 1,
        multihit: 0,
        recoil: [0, 0],
        drain: false,
        status: 0,
        volatile: 0,
        self_volatile: 0,
        boosts: [0; 7],
        secondary: None,
        ohko: false,
        damage: 0,
        selfdestruct: false,
        ignore_immunity: false,
        will_crit: 2,
        metronome: false,
    };

    #[inline]
    pub fn is_status(&self) -> bool {
        self.category == 2
    }
}

/// Showdown's `toID`: lowercase ASCII alphanumerics only.
pub fn to_id(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}
