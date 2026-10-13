//! Battle specs shared with the Showdown runner (parity/showdown/runner.js), and random spec
//! generation for fuzzing.

use battle_env::rng::Rng;
use serde::{Deserialize, Serialize};
use sim_data::gen1::{MOVES, SPECIES};
use sim_gen1::{MonSpec, Prng, Rules};
use std::collections::BTreeMap;

/// A Showdown set (`PokemonSet` subset) as the runner consumes it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SetJson {
    pub species: String,
    pub moves: Vec<String>,
    pub level: u8,
    pub ivs: BTreeMap<String, u8>,
    pub evs: BTreeMap<String, u8>,
}

const STATS: [&str; 6] = ["hp", "atk", "def", "spa", "spd", "spe"];

impl SetJson {
    pub fn from_mon(m: &MonSpec) -> SetJson {
        SetJson {
            species: SPECIES[m.species as usize].name.to_string(),
            moves: m.moves.iter().filter(|&&x| x != 0).map(|&x| MOVES[x as usize].id.to_string()).collect(),
            level: m.level,
            ivs: STATS.iter().enumerate().map(|(i, s)| (s.to_string(), m.ivs[i])).collect(),
            evs: STATS.iter().enumerate().map(|(i, s)| (s.to_string(), m.evs[i])).collect(),
        }
    }

    /// Resolve names the way Showdown's dex does (case/punctuation-insensitive, aliases such as
    /// `hijumpkick` accepted), so a spec Showdown accepts is never reported as a "spec" failure.
    pub fn to_mon(&self) -> Result<MonSpec, String> {
        let species = sim_data::gen1_species_id(&self.species).ok_or_else(|| format!("unknown species {}", self.species))?;
        let mut moves = [0u8; 4];
        for (k, name) in self.moves.iter().enumerate().take(4) {
            moves[k] = sim_data::gen1_move_id(name).ok_or_else(|| format!("unknown move {name}"))?;
        }
        let get = |m: &BTreeMap<String, u8>, d: u8| -> [u8; 6] { std::array::from_fn(|i| *m.get(STATS[i]).unwrap_or(&d)) };
        Ok(MonSpec { species, level: self.level, moves, ivs: get(&self.ivs, 31), evs: get(&self.evs, 0) })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Spec {
    pub id: String,
    /// "gen1ou" or "gen1nc" (no clauses).
    pub format: String,
    /// "vanilla" or "pkmn".
    #[serde(default = "vanilla")]
    pub patch: String,
    pub seed: String,
    pub p1: Vec<SetJson>,
    pub p2: Vec<SetJson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<[String; 2]>>,
    #[serde(default, rename = "choiceSeed")]
    pub choice_seed: u32,
    #[serde(default, rename = "maxSteps", skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
    /// Override the native sim's compatibility mode ("vanilla"/"pkmn") independently of the
    /// Showdown `patch` (to measure where the two references differ). Ignored by the runner.
    #[serde(default, rename = "simCompat", skip_serializing_if = "Option::is_none")]
    pub sim_compat: Option<String>,
}

fn vanilla() -> String {
    "vanilla".into()
}

impl Spec {
    pub fn rules(&self) -> Rules {
        let mut r = match self.format.as_str() {
            "gen1nc" => Rules::NO_CLAUSES,
            _ => Rules::OU,
        };
        let compat = self.sim_compat.as_deref().unwrap_or(&self.patch);
        if compat == "pkmn" {
            r.compat = sim_gen1::Compat::Pkmn;
        }
        r
    }

    pub fn teams(&self) -> Result<(Vec<MonSpec>, Vec<MonSpec>), String> {
        let a = self.p1.iter().map(|s| s.to_mon()).collect::<Result<Vec<_>, _>>()?;
        let b = self.p2.iter().map(|s| s.to_mon()).collect::<Result<Vec<_>, _>>()?;
        Ok((a, b))
    }

    /// The native battle for this spec (not started).
    pub fn battle(&self) -> Result<sim_gen1::Battle, String> {
        let (t1, t2) = self.teams()?;
        let prng = Prng::parse(&self.seed).ok_or_else(|| format!("bad seed '{}'", self.seed))?;
        sim_gen1::Battle::new(&t1, &t2, prng, self.rules()).map_err(|e| e.to_string())
    }
}

/// Seed string for a 64-bit Gen 5 seed.
pub fn seed_string(seed: u64) -> String {
    Prng::from_u64(seed).seed_string()
}

/// How fuzz teams are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeamKind {
    /// Random species, any 1-4 distinct moves (not necessarily learnable), random DVs/stat exp,
    /// mostly level 100.
    Random,
    /// Random species with moves from their Gen 1 learnsets, max DVs/stat exp.
    Legal,
    /// Showdown random-battle sets.
    RandBats,
    /// Stall-prone teams: mostly non-damaging moves, Transform, Ghosts, partial trapping and
    /// Bide, to reach long battles, the Endless Battle Clause and the 1000-turn limit.
    Stall,
    /// A mix of random/legal/randbats (and some stall).
    Mixed,
    /// Scenario teams that force rare Gen 1 mechanics (one of [`SCENARIOS`] per battle, both
    /// sides drawn from it): freeze thaw, Disable vs locked moves, Counter, copy moves,
    /// Substitute, trapping + switching, PP stalling, Toxic + Leech Seed, boosts, the Endless
    /// Battle Clause, Bide, speed ties, OHKO and friends.
    Targeted,
    /// Freeze-and-thaw battles ([`thaw_teams`]): Ice, Fire and stalling moves on both sides.
    Thaw,
}

impl TeamKind {
    pub fn parse(s: &str) -> Option<TeamKind> {
        Some(match s {
            "random" => TeamKind::Random,
            "legal" => TeamKind::Legal,
            "randbats" => TeamKind::RandBats,
            "mixed" => TeamKind::Mixed,
            "stall" => TeamKind::Stall,
            "targeted" => TeamKind::Targeted,
            "thaw" => TeamKind::Thaw,
            _ => return None,
        })
    }
}

/// Targeted scenarios: (name, move pool of one side, move pool of the other). 13 are ported from
/// the simulator audit's generator; `freeze` (both last Pokémon frozen: Endless Battle Clause),
/// `mirror` (Mirror Move against Mirror Move, with residual damage) and `overkill` (level-100
/// attackers against low-level Substitute users: the 997 damage cap shows in the uncapped
/// `lastDamage` of a Substitute hit) were added for mutants the others could not reach.
/// `speedtie`, `endless`, `freeze` and `overkill` build their teams specially.
pub const SCENARIOS: [(&str, &[&str], &[&str]); 16] = [
    ("overkill", &["explosion", "selfdestruct", "hyperbeam", "doubleedge", "bodyslam", "slash", "earthquake", "counter", "bide"], &["substitute", "recover", "softboiled", "splash", "counter", "bide"]),
    ("freeze", &["icebeam", "blizzard", "icepunch"], &["splash", "teleport", "agility", "harden"]),
    ("mirror", &["mirrormove", "toxic", "leechseed", "bodyslam", "wrap", "hyperbeam"], &["mirrormove", "toxic", "leechseed", "psychic", "thrash", "metronome"]),
    ("bide", &["bide", "disable", "substitute", "counter", "metronome"], &["bide", "dig", "fly", "hyperbeam", "seismictoss", "wrap", "splash"]),
    ("boosts", &["amnesia", "swordsdance", "agility", "growth", "sharpen", "barrier", "acidarmor", "minimize", "doubleteam", "thunderwave", "haze", "psychic"], &["glare", "thunderwave", "ember", "screech", "growl", "leer", "stringshot", "sandattack", "smokescreen", "haze", "focusenergy"]),
    ("copy", &["transform", "mimic", "metronome", "mirrormove", "conversion"], &["transform", "mimic", "metronome", "mirrormove", "disable", "splash", "wrap", "hyperbeam"]),
    ("counter", &["counter", "bide", "substitute", "seismictoss", "nightshade", "bodyslam", "metronome", "mirrormove"], &["counter", "bodyslam", "submission", "seismictoss", "sonicboom", "dragonrage", "superfang", "quickattack", "doublekick", "furyattack", "hyperbeam", "psywave", "bide"]),
    ("disable_locked", &["disable", "counter", "substitute", "seismictoss", "mimic"], &["thrash", "petaldance", "wrap", "bind", "firespin", "clamp", "hyperbeam", "fly", "dig", "skyattack", "solarbeam", "rage", "bide", "razorwind", "skullbash"]),
    ("endless", &["transform"], &["transform"]),
    ("ohko_misc", &["fissure", "horndrill", "guillotine", "psywave", "superfang", "dreameater", "hijumpkick", "jumpkick", "explosion", "selfdestruct", "swift", "dig"], &["fly", "dig", "substitute", "minimize", "doubleteam", "rest", "hypnosis", "lovelykiss"]),
    ("pp_stall", &["skyattack", "hyperbeam", "fireblast", "mimic", "metronome", "transform", "disable", "spore", "wrap"], &["skyattack", "hyperbeam", "fireblast", "thunder", "hydropump", "mirrormove", "focusenergy", "haze"]),
    ("speedtie", &["bodyslam", "thunderwave", "quickattack", "counter", "hyperbeam", "wrap", "sleeppowder", "toxic", "confuseray", "substitute", "agility"], &[]),
    ("sub", &["substitute", "wrap", "bind", "leechseed", "toxic", "hyperbeam", "explosion", "megadrain", "doublekick", "pinmissile", "confuseray", "psybeam", "bide"], &["substitute", "wrap", "bind", "leechseed", "toxic", "thunderwave", "sleeppowder", "twineedle", "bonemerang", "furyswipes", "absorb", "dreameater", "hyperbeam", "counter"]),
    ("thaw", &["icebeam", "blizzard", "icepunch", "aurorabeam"], &["fireblast", "ember", "flamethrower", "firepunch", "firespin", "recover", "rest", "substitute"]),
    ("toxic_seed", &["toxic", "leechseed", "rest", "recover", "haze", "substitute", "softboiled"], &["toxic", "leechseed", "rest", "haze", "bodyslam", "psychic", "reflect", "lightscreen"]),
    ("trap_switch", &["wrap", "bind", "firespin", "clamp"], &["wrap", "bind", "clamp", "firespin", "agility", "substitute", "counter", "hyperbeam"]),
];

const FAST: [&str; 10] = ["Jolteon", "Electrode", "Aerodactyl", "Persian", "Starmie", "Alakazam", "Tauros", "Dugtrio", "Mewtwo", "Mew"];
const ANY: [&str; 37] = [
    "Tauros", "Chansey", "Snorlax", "Starmie", "Alakazam", "Exeggutor", "Zapdos", "Rhydon", "Lapras", "Jynx", "Gengar", "Slowbro",
    "Cloyster", "Dragonite", "Ditto", "Mew", "Mewtwo", "Golem", "Victreebel", "Tentacruel", "Dewgong", "Articuno", "Moltres",
    "Arcanine", "Ninetales", "Charizard", "Haunter", "Gastly", "Clefable", "Wigglytuff", "Kangaskhan", "Hitmonlee", "Machamp",
    "Onix", "Abra", "Magikarp", "Caterpie",
];

fn pick<'a>(rng: &mut Rng, xs: &[&'a str]) -> &'a str {
    xs[rng.below(xs.len() as u64) as usize]
}

/// One scenario Pokémon: 1-4 distinct moves from `pool`, random DVs/stat exp, mostly level 100.
fn scenario_mon(rng: &mut Rng, pool: &[&str], species: Option<&str>, level: Option<u8>) -> MonSpec {
    let species = sim_data::gen1_species_id(species.unwrap_or_else(|| pick(rng, &ANY))).expect("scenario species");
    let mut ids: Vec<u8> = pool.iter().map(|m| sim_data::gen1_move_id(m).expect("scenario move")).collect();
    let k = (1 + rng.below(4) as usize).min(ids.len());
    let mut moves = [0u8; 4];
    for slot in moves.iter_mut().take(k) {
        *slot = ids.swap_remove(rng.below(ids.len() as u64) as usize);
    }
    let level = level.unwrap_or_else(|| match rng.below(5) {
        0..=2 => 100,
        3 => 1 + rng.below(100) as u8,
        _ => 50 + rng.below(51) as u8,
    });
    let mut ivs: [u8; 6] = std::array::from_fn(|_| 2 * rng.below(16) as u8);
    ivs[4] = ivs[3];
    let mut evs: [u8; 6] = std::array::from_fn(|_| if rng.below(3) < 2 { 255 } else { rng.below(256) as u8 });
    evs[4] = evs[3];
    MonSpec { species, level, moves, ivs, evs }
}

/// `--teams thaw`: freeze-and-thaw battles (the audit's thaw corpus). Both sides draw from one
/// pool of Ice moves, Fire moves (which thaw a frozen target) and stalling moves, so the side that
/// freezes is often the one that thaws; teams are 1v1, 2v1 or 1v2 so battles last.
pub fn thaw_teams(rng: &mut Rng) -> (Vec<MonSpec>, Vec<MonSpec>) {
    const ICE: [&str; 3] = ["icebeam", "blizzard", "icepunch"];
    const FIRE: [&str; 4] = ["fireblast", "flamethrower", "firepunch", "ember"];
    const STALL: [&str; 8] = ["splash", "recover", "teleport", "substitute", "rest", "haze", "softboiled", "firespin"];
    const SPECIES: [&str; 15] = [
        "Snorlax", "Starmie", "Tauros", "Chansey", "Dewgong", "Mew", "Lapras", "Articuno", "Slowbro", "Jynx", "Gengar",
        "Rhydon", "Cloyster", "Exeggutor", "Golem",
    ];
    let (n1, n2) = match rng.below(100) {
        0..=47 => (1, 1),
        48..=73 => (2, 1),
        _ => (1, 2),
    };
    // Every Pokémon can freeze (one Ice move) and thaw (one Fire move with a burn chance); the
    // rest is stalling so that frozen Pokémon stay in long enough to be thawed.
    let mon = |rng: &mut Rng| {
        let sp = pick(rng, &SPECIES);
        let level = if rng.below(100) < 61 { 100 } else { 10 + rng.below(40) as u8 };
        let mut m = scenario_mon(rng, &STALL, Some(sp), Some(level));
        let id = |name: &str| sim_data::gen1_move_id(name).expect("thaw move");
        // Two distinct stall moves (healing keeps frozen Pokémon in), then Ice and Fire.
        let mut stall: Vec<&str> = STALL.to_vec();
        let s1 = stall.swap_remove(rng.below(stall.len() as u64) as usize);
        let s2 = stall.swap_remove(rng.below(stall.len() as u64) as usize);
        m.moves = [id(pick(rng, &ICE)), id(pick(rng, &FIRE)), id(s1), id(s2)];
        m.ivs = [30; 6];
        m.evs = [255; 6];
        m
    };
    let p1 = (0..n1).map(|_| mon(rng)).collect();
    let p2 = (0..n2).map(|_| mon(rng)).collect();
    (p1, p2)
}

/// Both teams of a targeted battle for scenario `s` (index into [`SCENARIOS`]).
pub fn scenario_teams(rng: &mut Rng, s: usize) -> (Vec<MonSpec>, Vec<MonSpec>) {
    let (name, a, b) = SCENARIOS[s];
    let (n1, n2) = (1 + rng.below(4) as usize, 1 + rng.below(4) as usize);
    match name {
        "speedtie" => {
            let sp = pick(rng, &FAST);
            let mut p1: Vec<MonSpec> = (0..n1).map(|_| scenario_mon(rng, a, Some(sp), Some(100))).collect();
            let mut p2: Vec<MonSpec> =
                if rng.below(2) == 0 { p1.clone() } else { (0..n2).map(|_| scenario_mon(rng, a, Some(sp), Some(100))).collect() };
            for m in p1.iter_mut().chain(p2.iter_mut()) {
                m.ivs[5] = 30;
                m.evs[5] = 255;
            }
            (p1, p2)
        }
        "overkill" => {
            let p1 = (0..n1).map(|_| scenario_mon(rng, a, None, Some(100))).collect();
            let p2 = (0..n2)
                .map(|_| {
                    let lvl = 1 + rng.below(15) as u8;
                    scenario_mon(rng, b, None, Some(lvl))
                })
                .collect();
            (p1, p2)
        }
        "freeze" => {
            // The Endless Battle Clause with a frozen side: a frozen Pokémon cannot freeze back, so
            // both sides frozen is (nearly) unreachable; instead one side is a level-100 Ghost with
            // only harmless moves, the other a weak Pokémon with one Ice move. Once the Ghost is
            // frozen and the attacker has spent its PP (Struggle cannot touch a Ghost), the
            // clause ends the battle: "all frozen" against "no PP vs all Ghosts".
            let ghost_species = pick(rng, &["Gengar", "Haunter", "Gastly"]);
            let ghost = scenario_mon(rng, b, Some(ghost_species), Some(100));
            let ice = sim_data::gen1_move_id(pick(rng, a)).unwrap();
            let attacker_species = pick(rng, &["Starmie", "Tauros", "Slowbro", "Exeggutor", "Golem"]);
            let level = 5 + rng.below(25) as u8;
            let mut attacker = scenario_mon(rng, a, Some(attacker_species), Some(level));
            attacker.moves = [ice, 0, 0, 0];
            if rng.below(2) == 0 {
                (vec![ghost], vec![attacker])
            } else {
                (vec![attacker], vec![ghost])
            }
        }
        "endless" => {
            let p1 = (0..n1)
                .map(|_| {
                    let sp = pick(rng, &["Ditto", "Mew", "Gengar"]);
                    scenario_mon(rng, &["transform"], Some(sp), None)
                })
                .collect();
            let p2 = (0..n2)
                .map(|_| {
                    let pool: &[&str] = if rng.below(10) < 7 { &["transform"] } else { &["splash"] };
                    let sp = pick(rng, &["Ditto", "Mew", "Gengar", "Haunter"]);
                    scenario_mon(rng, pool, Some(sp), None)
                })
                .collect();
            (p1, p2)
        }
        _ => {
            let (a, b) = if rng.below(2) == 0 { (b, a) } else { (a, b) };
            let p1 = (0..n1).map(|_| scenario_mon(rng, a, None, None)).collect();
            let p2 = (0..n2).map(|_| scenario_mon(rng, b, None, None)).collect();
            (p1, p2)
        }
    }
}

fn from_env_team(t: &battle_env::Team) -> Vec<MonSpec> {
    t.mons().iter().map(|m| MonSpec::from_dvs(m.species, m.level, &m.moves[..], m.dvs, m.exp)).collect()
}

pub fn random_team(rng: &mut Rng, kind: TeamKind) -> Vec<MonSpec> {
    let kind = if kind == TeamKind::Mixed {
        [TeamKind::Random, TeamKind::Legal, TeamKind::RandBats, TeamKind::Random, TeamKind::Legal, TeamKind::RandBats, TeamKind::Stall]
            [rng.below(7) as usize]
    } else {
        kind
    };
    match kind {
        TeamKind::Stall => {
            use sim_data::gen1::mv;
            const POOL: [u8; 40] = [
                mv::SPLASH, mv::TELEPORT, mv::ROAR, mv::WHIRLWIND, mv::TRANSFORM, mv::GROWL, mv::LEER, mv::HARDEN,
                mv::RECOVER, mv::SOFTBOILED, mv::REST, mv::SUBSTITUTE, mv::REFLECT, mv::LIGHTSCREEN, mv::MIST, mv::HAZE,
                mv::DISABLE, mv::BIDE, mv::COUNTER, mv::WRAP, mv::BIND, mv::FIRESPIN, mv::CLAMP, mv::CONFUSERAY,
                mv::TOXIC, mv::LEECHSEED, mv::DOUBLETEAM, mv::MINIMIZE, mv::AMNESIA, mv::METRONOME, mv::MIRRORMOVE,
                mv::MIMIC, mv::CONVERSION, mv::SEISMICTOSS, mv::NIGHTSHADE, mv::SONICBOOM, mv::DRAGONRAGE,
                mv::PSYWAVE, mv::SUPERFANG, mv::RAGE,
            ];
            const GHOSTS_ETC: [u8; 8] = [92, 93, 94, 132, 137, 143, 113, 151];
            let n = 1 + rng.below(6) as usize;
            (0..n)
                .map(|_| {
                    let species = if rng.below(2) == 0 { GHOSTS_ETC[rng.below(8) as usize] } else { 1 + rng.below(151) as u8 };
                    let nm = 1 + rng.below(4) as usize;
                    let mut moves = Vec::new();
                    while moves.len() < nm {
                        let m = POOL[rng.below(POOL.len() as u64) as usize];
                        if !moves.contains(&m) {
                            moves.push(m);
                        }
                    }
                    let level = if rng.below(3) == 0 { 1 + rng.below(100) as u8 } else { 100 };
                    MonSpec::from_dvs(species, level, &moves, [15; 4], [65535; 5])
                })
                .collect()
        }
        TeamKind::Legal => from_env_team(&battle_env::team::random_legal_team(rng)),
        TeamKind::RandBats => from_env_team(&battle_env::team::randbats_team(rng)),
        _ => {
            let n = 1 + rng.below(6) as usize;
            (0..n)
                .map(|_| {
                    let species = 1 + rng.below(151) as u8;
                    let nm = 1 + rng.below(4) as usize;
                    let mut moves = Vec::new();
                    while moves.len() < nm {
                        let m = 1 + rng.below(164) as u8;
                        if !moves.contains(&m) {
                            moves.push(m);
                        }
                    }
                    let level = if rng.below(4) == 0 { 1 + rng.below(100) as u8 } else { 100 };
                    let dvs = std::array::from_fn(|_| rng.below(16) as u8);
                    let exp = std::array::from_fn(|_| if rng.below(2) == 0 { 65535 } else { rng.below(65536) as u16 });
                    MonSpec::from_dvs(species, level, &moves, dvs, exp)
                })
                .collect()
        }
    }
}

/// A random fuzz spec. `prng`: "gen5" (default), "sodium", or "mixed".
pub fn random_spec(id: String, rng: &mut Rng, kind: TeamKind, format: &str, patch: &str, prng: &str) -> Spec {
    // Targeted battles carry their scenario in the id (e.g. `s1-17-copy`) for the reports.
    let (id, (p1, p2)) = match kind {
        TeamKind::Targeted => {
            let s = rng.below(SCENARIOS.len() as u64) as usize;
            (format!("{id}-{}", SCENARIOS[s].0), scenario_teams(rng, s))
        }
        TeamKind::Thaw => (id, thaw_teams(rng)),
        _ => (id, (random_team(rng, kind), random_team(rng, kind))),
    };
    let sodium = match prng {
        "sodium" => true,
        "mixed" => rng.below(2) == 0,
        _ => false,
    };
    let seed = if sodium {
        let mut hex = String::from("sodium,");
        for _ in 0..16 {
            hex.push_str(&format!("{:02x}", rng.below(256)));
        }
        hex
    } else {
        seed_string(rng.next_u64())
    };
    Spec {
        id,
        format: format.into(),
        patch: patch.into(),
        seed,
        p1: p1.iter().map(SetJson::from_mon).collect(),
        p2: p2.iter().map(SetJson::from_mon).collect(),
        choices: None,
        choice_seed: rng.next_u64() as u32,
        max_steps: None,
        sim_compat: None,
    }
}
