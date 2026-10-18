//! Static Gen 1 data (ids match pkmn's enums) plus name lookups.

mod generated;
pub use generated::*;
pub mod obtainable;

use std::collections::HashMap;
use std::sync::OnceLock;

/// Normalise a name the way Showdown builds IDs: lowercase alphanumerics only.
pub fn to_id(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

fn index(names: &'static [&'static str]) -> HashMap<String, u8> {
    names.iter().enumerate().skip(1).map(|(i, n)| (to_id(n), i as u8)).collect()
}

/// Species id (1..=151) for a Showdown-spelled name. The in-game spellings "Nidoran♀" and
/// "Nidoran♂" are accepted as aliases of Showdown's "Nidoran-F" / "Nidoran-M".
pub fn species_id(name: &str) -> Option<u8> {
    static MAP: OnceLock<HashMap<String, u8>> = OnceLock::new();
    let name = name.replace('♀', "F").replace('♂', "M");
    MAP.get_or_init(|| index(&SPECIES_NAMES)).get(&to_id(&name)).copied()
}

/// Whether `species` can learn `move_id` in Gen 1 (union over its evolution chain).
pub fn can_learn(species: u8, move_id: u8) -> bool {
    LEARNSETS_ALL.get(species as usize).is_some_and(|l| l.contains(&move_id))
}

/// Move id (1..=165) for a Showdown-spelled name.
pub fn move_id(name: &str) -> Option<u8> {
    static MAP: OnceLock<HashMap<String, u8>> = OnceLock::new();
    MAP.get_or_init(|| index(&MOVE_NAMES)).get(&to_id(name)).copied()
}

/// Effectiveness of an attacking type against a (possibly dual) defending typing, x100
/// (0, 25, 50, 100, 200, 400).
#[inline]
pub fn effectiveness(atk: u8, def1: u8, def2: u8) -> u32 {
    let a = TYPE_CHART[atk as usize][def1 as usize] as u32;
    if def1 == def2 {
        a * 10
    } else {
        a * TYPE_CHART[atk as usize][def2 as usize] as u32
    }
}

/// Max PP of a move in the engine's convention (max PP Ups, capped at 61).
#[inline]
pub fn max_pp(move_id: u8) -> u8 {
    let base = MOVES[move_id as usize].pp as u32;
    (base / 5 * 8).min(61) as u8
}

/// Gen 1 stat formula (identical to the engine's `Stats.calc`).
pub fn calc_stat(is_hp: bool, base: u8, dv: u8, exp: u16, level: u8) -> u16 {
    let evs = ((exp as f64).sqrt().ceil() as u32).min(255);
    let core = 2 * (base as u32 + dv as u32) + evs / 4;
    let factor = if is_hp { level as u32 + 10 } else { 5 };
    (core * level as u32 / 100 + factor) as u16
}

/// Whether the move's effect is fixed damage (Seismic Toss, Night Shade, Dragon Rage, ...).
pub fn is_fixed_damage(move_id: u8) -> bool {
    let e = EFFECT_NAMES[MOVES[move_id as usize].effect as usize];
    matches!(e, "SpecialDamage" | "SuperFang")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert_eq!(species_id("Tauros"), Some(128));
        assert_eq!(species_id("Mr. Mime"), species_id("mrmime"));
        assert_eq!(species_id("Nidoran♀"), species_id("Nidoran-F"));
        assert_eq!(species_id("Nidoran♂"), species_id("Nidoran-M"));
        assert_ne!(species_id("Nidoran♀"), species_id("Nidoran♂"));
        assert_eq!(species_id("Nidoran"), None);
        assert!(can_learn(species_id("Ditto").unwrap(), move_id("Transform").unwrap()));
        assert!(!can_learn(species_id("Tauros").unwrap(), move_id("Spore").unwrap()));
        assert_eq!(move_id("Body Slam"), Some(34));
        assert_eq!(MOVE_NAMES[move_id("Thunder Wave").unwrap() as usize], "Thunder Wave");
        assert_eq!(effectiveness(4, 11, 11), 200); // Ground vs Electric
        assert_eq!(effectiveness(11, 4, 4), 0); // Electric vs Ground
    }

    #[test]
    fn stats() {
        // Values from the engine's own tests.
        assert_eq!(calc_stat(true, SPECIES_BASE[130][0], 0, 5120, 38), 127); // Gyarados
        assert_eq!(calc_stat(false, SPECIES_BASE[18][3], 15, 63001, 100), 279); // Pidgeot
    }
}
