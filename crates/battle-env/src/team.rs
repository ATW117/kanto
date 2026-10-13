//! Teams: the JSON schema from DESIGN.md, validation into engine ids, team pools and random
//! team generators.

use crate::data;
use crate::rng::Rng;
use serde::Deserialize;
use std::path::Path;

pub const MAX_PARTY: usize = 6;

#[derive(Clone, Debug, Deserialize, Default)]
pub struct StatSpread {
    pub hp: Option<u16>,
    pub atk: Option<u16>,
    pub def: Option<u16>,
    pub spe: Option<u16>,
    pub spc: Option<u16>,
}

/// One Pokémon as written in the team JSON (Showdown spelling).
#[derive(Clone, Debug, Deserialize)]
pub struct PokemonSet {
    pub species: String,
    pub moves: Vec<String>,
    #[serde(default)]
    pub level: Option<u8>,
    #[serde(default)]
    pub dvs: Option<StatSpread>,
    #[serde(default)]
    pub exp: Option<StatSpread>,
}

/// A team as written in JSON.
#[derive(Clone, Debug, Deserialize)]
pub struct TeamSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub source: String,
    pub pokemon: Vec<PokemonSet>,
}

/// A validated Pokémon resolved to engine ids, with computed stats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mon {
    pub species: u8,
    pub moves: [u8; 4],
    pub level: u8,
    /// [hp, atk, def, spe, spc]
    pub stats: [u16; 5],
}

/// A validated team (1-6 Pokémon). Fixed size, no heap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Team {
    pub size: u8,
    pub mons: [Mon; MAX_PARTY],
}

const EMPTY_MON: Mon = Mon { species: 0, moves: [0; 4], level: 0, stats: [0; 5] };

impl Mon {
    /// Build a Pokémon from ids, computing stats from DVs (atk, def, spe, spc) and stat exp
    /// (hp, atk, def, spe, spc). The HP DV is derived from the other DVs as in Gen 1.
    pub fn new(species: u8, moves: &[u8], level: u8, dvs: [u8; 4], exp: [u16; 5]) -> Mon {
        assert!((1..=data::NUM_SPECIES as u8).contains(&species));
        assert!(!moves.is_empty() && moves.len() <= 4);
        let base = data::SPECIES_BASE[species as usize];
        let hp_dv = (dvs[0] & 1) << 3 | (dvs[1] & 1) << 2 | (dvs[2] & 1) << 1 | (dvs[3] & 1);
        let all_dvs = [hp_dv, dvs[0], dvs[1], dvs[2], dvs[3]];
        let mut stats = [0u16; 5];
        for i in 0..5 {
            stats[i] = data::calc_stat(i == 0, base[i], all_dvs[i], exp[i], level);
        }
        let mut m = [0u8; 4];
        m[..moves.len()].copy_from_slice(moves);
        Mon { species, moves: m, level, stats }
    }

    /// Max-DV, max-stat-exp Pokémon at the given level.
    pub fn standard(species: u8, moves: &[u8], level: u8) -> Mon {
        Mon::new(species, moves, level, [15; 4], [65535; 5])
    }
}

impl Team {
    pub fn new(mons: &[Mon]) -> Team {
        assert!(!mons.is_empty() && mons.len() <= MAX_PARTY);
        let mut t = Team { size: mons.len() as u8, mons: [EMPTY_MON; MAX_PARTY] };
        t.mons[..mons.len()].copy_from_slice(mons);
        t
    }

    pub fn mons(&self) -> &[Mon] {
        &self.mons[..self.size as usize]
    }

    /// Validate and resolve a JSON team.
    pub fn from_spec(spec: &TeamSpec) -> Result<Team, String> {
        let n = spec.pokemon.len();
        if n == 0 || n > MAX_PARTY {
            return Err(format!("team '{}' has {n} Pokémon (need 1-6)", spec.name));
        }
        let mut mons = Vec::with_capacity(n);
        for p in &spec.pokemon {
            let species = data::species_id(&p.species)
                .ok_or_else(|| format!("team '{}': unknown species '{}'", spec.name, p.species))?;
            if p.moves.is_empty() || p.moves.len() > 4 {
                return Err(format!("team '{}': {} has {} moves", spec.name, p.species, p.moves.len()));
            }
            let mut moves = Vec::with_capacity(4);
            for m in &p.moves {
                let id = data::move_id(m).ok_or_else(|| format!("team '{}': unknown move '{m}'", spec.name))?;
                if id == 165 {
                    return Err(format!("team '{}': Struggle is not a legal move", spec.name));
                }
                if moves.contains(&id) {
                    return Err(format!("team '{}': duplicate move '{m}'", spec.name));
                }
                moves.push(id);
            }
            let level = p.level.unwrap_or(100);
            if !(1..=100).contains(&level) {
                return Err(format!("team '{}': bad level {level}", spec.name));
            }
            let dv = |v: Option<u16>| v.unwrap_or(15).min(15) as u8;
            let ex = |v: Option<u16>| v.unwrap_or(65535);
            let d = p.dvs.clone().unwrap_or_default();
            let e = p.exp.clone().unwrap_or_default();
            mons.push(Mon::new(
                species,
                &moves,
                level,
                [dv(d.atk), dv(d.def), dv(d.spe), dv(d.spc)],
                [ex(e.hp), ex(e.atk), ex(e.def), ex(e.spe), ex(e.spc)],
            ));
        }
        Ok(Team::new(&mons))
    }
}

/// Parse teams from a JSON string: a single team object, an array of teams, or JSON Lines.
pub fn parse_teams(text: &str) -> Result<Vec<TeamSpec>, String> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('[') {
        return serde_json::from_str(trimmed).map_err(|e| e.to_string());
    }
    if let Ok(t) = serde_json::from_str::<TeamSpec>(trimmed) {
        return Ok(vec![t]);
    }
    trimmed
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1)))
        .collect()
}

/// Load all teams from a `.json`/`.jsonl` file or a directory of them (non-recursive).
pub fn load_teams(path: &Path) -> Result<Vec<Team>, String> {
    let mut files = Vec::new();
    if path.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))? {
            let p = entry.map_err(|e| e.to_string())?.path();
            if matches!(p.extension().and_then(|e| e.to_str()), Some("json" | "jsonl")) {
                files.push(p);
            }
        }
        files.sort();
    } else if path.is_file() {
        files.push(path.to_path_buf());
    } else {
        return Err(format!("team path '{}' does not exist", path.display()));
    }
    let mut teams = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        for spec in parse_teams(&text).map_err(|e| format!("{}: {e}", f.display()))? {
            teams.push(Team::from_spec(&spec).map_err(|e| format!("{}: {e}", f.display()))?);
        }
    }
    if teams.is_empty() {
        return Err(format!("no teams found in '{}'", path.display()));
    }
    Ok(teams)
}

/// A few hand-written Gen 1 OU teams, used as a fallback/fixture until the curated pool lands.
pub const BUILTIN_TEAMS_JSON: &str = include_str!("../../../tests/fixtures/teams/gen1ou_builtin.json");

pub fn builtin_teams() -> Vec<Team> {
    parse_teams(BUILTIN_TEAMS_JSON)
        .expect("builtin teams parse")
        .iter()
        .map(|t| Team::from_spec(t).expect("builtin teams validate"))
        .collect()
}

/// Where each game's teams come from.
#[derive(Clone, Debug)]
pub enum TeamSource {
    /// Sample uniformly (with replacement) from a fixed pool, independently per side.
    Pool(Vec<Team>),
    /// Six distinct random species, each with up to four random moves from its Gen 1 learnset.
    RandomLegal,
    /// Six distinct species from Showdown's Gen 1 random-battle sets (levels as in randbats).
    RandBats,
}

impl TeamSource {
    /// Parse a source spec: "random", "randbats", "builtin", or a path to a file/directory.
    pub fn from_spec(spec: &str) -> Result<TeamSource, String> {
        match spec {
            "random" => Ok(TeamSource::RandomLegal),
            "randbats" => Ok(TeamSource::RandBats),
            "builtin" => Ok(TeamSource::Pool(builtin_teams())),
            path => Ok(TeamSource::Pool(load_teams(Path::new(path))?)),
        }
    }

    pub fn sample(&self, rng: &mut Rng) -> Team {
        match self {
            TeamSource::Pool(pool) => pool[rng.below(pool.len() as u64) as usize],
            TeamSource::RandomLegal => random_legal_team(rng),
            TeamSource::RandBats => randbats_team(rng),
        }
    }
}

fn distinct_species(rng: &mut Rng, candidates: &[u8], n: usize) -> Vec<u8> {
    let mut pool = candidates.to_vec();
    let mut out = Vec::with_capacity(n);
    for _ in 0..n.min(pool.len()) {
        let i = rng.below(pool.len() as u64) as usize;
        out.push(pool.swap_remove(i));
    }
    out
}

fn pick_moves(rng: &mut Rng, candidates: &[u8]) -> Vec<u8> {
    let mut pool = candidates.to_vec();
    pool.sort_unstable();
    pool.dedup();
    let mut out = Vec::with_capacity(4);
    while out.len() < 4 && !pool.is_empty() {
        let i = rng.below(pool.len() as u64) as usize;
        out.push(pool.swap_remove(i));
    }
    out
}

/// Random team over legal Gen 1 species/learnsets (max DVs/stat exp, level 100).
pub fn random_legal_team(rng: &mut Rng) -> Team {
    let all: Vec<u8> = (1..=data::NUM_SPECIES as u8).filter(|&s| !data::LEARNSETS[s as usize].is_empty()).collect();
    let mons: Vec<Mon> = distinct_species(rng, &all, MAX_PARTY)
        .into_iter()
        .map(|s| Mon::standard(s, &pick_moves(rng, data::LEARNSETS[s as usize]), 100))
        .collect();
    Team::new(&mons)
}

/// Random team built from Showdown's Gen 1 random-battle sets.
pub fn randbats_team(rng: &mut Rng) -> Team {
    let idx: Vec<u8> = (0..data::RANDBATS.len() as u8).collect();
    let mons: Vec<Mon> = distinct_species(rng, &idx, MAX_PARTY)
        .into_iter()
        .map(|i| {
            let (species, level, moves) = data::RANDBATS[i as usize];
            Mon::standard(species, &pick_moves(rng, moves), level)
        })
        .collect();
    Team::new(&mons)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_teams_validate() {
        let teams = builtin_teams();
        assert!(teams.len() >= 3);
        for t in &teams {
            assert_eq!(t.size, 6);
        }
    }

    #[test]
    fn tauros_stats() {
        let t = Mon::standard(data::species_id("Tauros").unwrap(), &[34], 100);
        assert_eq!(t.stats, [353, 298, 288, 318, 238]);
    }

    #[test]
    fn random_teams_are_legal() {
        let mut rng = Rng::new(1);
        for _ in 0..200 {
            for t in [random_legal_team(&mut rng), randbats_team(&mut rng)] {
                assert_eq!(t.size, 6);
                let mut species: Vec<u8> = t.mons().iter().map(|m| m.species).collect();
                species.sort();
                species.dedup();
                assert_eq!(species.len(), 6);
                for m in t.mons() {
                    assert!(m.moves[0] != 0);
                    let mut mv: Vec<u8> = m.moves.iter().copied().filter(|&x| x != 0).collect();
                    let n = mv.len();
                    mv.sort();
                    mv.dedup();
                    assert_eq!(mv.len(), n, "duplicate moves");
                }
            }
        }
    }

    #[test]
    fn parses_jsonl_and_rejects_bad_names() {
        let text = r#"{"name":"a","pokemon":[{"species":"Tauros","moves":["Body Slam"]}]}
{"name":"b","pokemon":[{"species":"Mr. Mime","moves":["Psychic","Thunder Wave"],"level":80}]}"#;
        let specs = parse_teams(text).unwrap();
        assert_eq!(specs.len(), 2);
        assert!(Team::from_spec(&specs[1]).is_ok());
        let bad: TeamSpec =
            serde_json::from_str(r#"{"pokemon":[{"species":"Tauros","moves":["Bodyslamm"]}]}"#).unwrap();
        assert!(Team::from_spec(&bad).is_err());
    }
}
