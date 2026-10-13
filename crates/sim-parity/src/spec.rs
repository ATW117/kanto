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

    pub fn to_mon(&self) -> Result<MonSpec, String> {
        let sid = sim_data::to_id(&self.species);
        let species = (1..=151u8)
            .find(|&i| SPECIES[i as usize].id == sid)
            .ok_or_else(|| format!("unknown species {}", self.species))?;
        let mut moves = [0u8; 4];
        for (k, name) in self.moves.iter().enumerate().take(4) {
            let id = sim_data::to_id(name);
            moves[k] = (1..=165u8).find(|&i| MOVES[i as usize].id == id).ok_or_else(|| format!("unknown move {name}"))?;
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
        if self.patch == "pkmn" {
            r.compat = sim_gen1::Compat::Pkmn;
        }
        r
    }

    pub fn prng(&self) -> Prng {
        Prng::parse(&self.seed).expect("bad seed")
    }

    pub fn teams(&self) -> Result<(Vec<MonSpec>, Vec<MonSpec>), String> {
        let a = self.p1.iter().map(|s| s.to_mon()).collect::<Result<Vec<_>, _>>()?;
        let b = self.p2.iter().map(|s| s.to_mon()).collect::<Result<Vec<_>, _>>()?;
        Ok((a, b))
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
    /// A mix of the above.
    Mixed,
}

impl TeamKind {
    pub fn parse(s: &str) -> Option<TeamKind> {
        Some(match s {
            "random" => TeamKind::Random,
            "legal" => TeamKind::Legal,
            "randbats" => TeamKind::RandBats,
            "mixed" => TeamKind::Mixed,
            _ => return None,
        })
    }
}

fn from_env_team(t: &battle_env::Team) -> Vec<MonSpec> {
    t.mons().iter().map(|m| MonSpec::from_dvs(m.species, m.level, &m.moves[..], m.dvs, m.exp)).collect()
}

pub fn random_team(rng: &mut Rng, kind: TeamKind) -> Vec<MonSpec> {
    let kind = if kind == TeamKind::Mixed {
        [TeamKind::Random, TeamKind::Legal, TeamKind::RandBats][rng.below(3) as usize]
    } else {
        kind
    };
    match kind {
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

/// A random fuzz spec.
pub fn random_spec(id: String, rng: &mut Rng, kind: TeamKind, format: &str, patch: &str) -> Spec {
    let p1 = random_team(rng, kind);
    let p2 = random_team(rng, kind);
    Spec {
        id,
        format: format.into(),
        patch: patch.into(),
        seed: seed_string(rng.next_u64()),
        p1: p1.iter().map(SetJson::from_mon).collect(),
        p2: p2.iter().map(SetJson::from_mon).collect(),
        choices: None,
        choice_seed: rng.next_u64() as u32,
        max_steps: None,
    }
}
