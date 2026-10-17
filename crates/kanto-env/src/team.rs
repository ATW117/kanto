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
    /// Optional nickname (`Pokemon#name` in Showdown's protocol); not part of [`Mon`].
    #[serde(default)]
    pub nickname: Option<String>,
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
    /// DVs [atk, def, spe, spc] (the HP DV is derived) and stat experience [hp, atk, def, spe, spc]
    /// the stats were computed from (needed by backends that rebuild Showdown sets).
    pub dvs: [u8; 4],
    pub exp: [u16; 5],
}

/// A validated team (1-6 Pokémon). Fixed size, no heap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Team {
    pub size: u8,
    pub mons: [Mon; MAX_PARTY],
}

const EMPTY_MON: Mon = Mon { species: 0, moves: [0; 4], level: 0, stats: [0; 5], dvs: [0; 4], exp: [0; 5] };

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
        Mon { species, moves: m, level, stats, dvs, exp }
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

    /// Validate and resolve a JSON team, including the legality checks (see
    /// [`Team::from_spec_opts`]).
    pub fn from_spec(spec: &TeamSpec) -> Result<Team, String> {
        Team::from_spec_opts(spec, true)
    }

    /// Resolve a JSON team. Always checked: 1-6 Pokémon, known species and moves, 1-4 distinct
    /// moves per Pokémon (no Struggle), level 1-100, DVs 0-15 (out-of-range DVs are an error,
    /// never clamped). With `validate` (the default everywhere), also the Gen 1 legality rules a
    /// team validator enforces: Species Clause (no duplicate species), every move in the
    /// species' Gen 1 learnset, and an HP DV, if given, consistent with the other DVs.
    pub fn from_spec_opts(spec: &TeamSpec, validate: bool) -> Result<Team, String> {
        let n = spec.pokemon.len();
        if n == 0 || n > MAX_PARTY {
            return Err(format!("team '{}' has {n} Pokémon (need 1-6)", spec.name));
        }
        let mut mons: Vec<Mon> = Vec::with_capacity(n);
        for p in &spec.pokemon {
            let species = data::species_id(&p.species)
                .ok_or_else(|| format!("team '{}': unknown species '{}'", spec.name, p.species))?;
            if validate && mons.iter().any(|m| m.species == species) {
                return Err(format!(
                    "team '{}': duplicate species {} (Species Clause; pass validate=False to allow)",
                    spec.name, p.species
                ));
            }
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
                if validate && !data::can_learn(species, id) {
                    return Err(format!(
                        "team '{}': {} cannot learn {m} in Gen 1 (pass validate=False to allow)",
                        spec.name, p.species
                    ));
                }
                moves.push(id);
            }
            let level = p.level.unwrap_or(100);
            if !(1..=100).contains(&level) {
                return Err(format!("team '{}': bad level {level}", spec.name));
            }
            let d = p.dvs.clone().unwrap_or_default();
            let e = p.exp.clone().unwrap_or_default();
            let dv = |what: &str, v: Option<u16>| -> Result<u8, String> {
                match v.unwrap_or(15) {
                    x @ 0..=15 => Ok(x as u8),
                    x => Err(format!("team '{}': {} {what} DV {x} out of range 0-15", spec.name, p.species)),
                }
            };
            let dvs = [dv("atk", d.atk)?, dv("def", d.def)?, dv("spe", d.spe)?, dv("spc", d.spc)?];
            let derived = (dvs[0] & 1) << 3 | (dvs[1] & 1) << 2 | (dvs[2] & 1) << 1 | (dvs[3] & 1);
            let hp = dv("hp", d.hp)?;
            if validate && d.hp.is_some() && hp != derived {
                return Err(format!(
                    "team '{}': {} HP DV {hp} is inconsistent with the other DVs (Gen 1 derives {derived})",
                    spec.name, p.species
                ));
            }
            let ex = |v: Option<u16>| v.unwrap_or(65535);
            mons.push(Mon::new(species, &moves, level, dvs, [ex(e.hp), ex(e.atk), ex(e.def), ex(e.spe), ex(e.spc)]));
        }
        Ok(Team::new(&mons))
    }
}

/// Parse teams from a string: JSON (a single team object, an array of teams, or JSON Lines) or a
/// Showdown team (export text or packed, see [`crate::showdown_team`]).
pub fn parse_teams(text: &str) -> Result<Vec<TeamSpec>, String> {
    if crate::showdown_team::looks_like_showdown(text) {
        return crate::showdown_team::parse_showdown_teams(text);
    }
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

/// Load all teams from a `.json`/`.jsonl` file, a Showdown team file (`.txt` export or `.packed`,
/// one team per line), or a directory of them (non-recursive), with the
/// legality checks of [`Team::from_spec`].
pub fn load_teams(path: &Path) -> Result<Vec<Team>, String> {
    load_teams_opts(path, true)
}

/// Named subsets of a pool directory: `"<dir>:train"` / `"<dir>:eval"` load
/// `<dir>/split/{train,eval}.jsonl`, a deterministic 90/10 split by a stable hash of each team
/// (see `data/teams/gen1ou/split.py`).
pub const SPLITS: [&str; 2] = ["train", "eval"];

/// Resolve a `"<dir>:<split>"` spec to the split file, if `spec` has that form.
pub fn split_path(spec: &str) -> Option<std::path::PathBuf> {
    let (dir, split) = spec.rsplit_once(':')?;
    if !SPLITS.contains(&split) || dir.is_empty() {
        return None;
    }
    Some(Path::new(dir).join("split").join(format!("{split}.jsonl")))
}

/// [`load_teams`]; `validate = false` skips the legality checks (see [`Team::from_spec_opts`]).
pub fn load_teams_opts(path: &Path, validate: bool) -> Result<Vec<Team>, String> {
    let mut files = Vec::new();
    if path.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))? {
            let p = entry.map_err(|e| e.to_string())?.path();
            if matches!(p.extension().and_then(|e| e.to_str()), Some("json" | "jsonl" | "txt" | "packed")) {
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
            teams.push(Team::from_spec_opts(&spec, validate).map_err(|e| format!("{}: {e}", f.display()))?);
        }
    }
    if teams.is_empty() {
        return Err(format!("no teams found in '{}'", path.display()));
    }
    Ok(teams)
}

/// A few hand-written Gen 1 OU teams, used as a fallback/fixture until the curated pool lands.
pub const BUILTIN_TEAMS_JSON: &str = include_str!("../../../tests/fixtures/teams/gen1ou_builtin.json");

/// `data/teams/gen1ou/teams.jsonl` and its split, embedded (team sources `"gen1ou"`,
/// `"gen1ou:train"`, `"gen1ou:eval"`).
pub const POOL_GEN1OU: &str = include_str!("../../../data/teams/gen1ou/teams.jsonl");
pub const POOL_GEN1OU_TRAIN: &str = include_str!("../../../data/teams/gen1ou/split/train.jsonl");
pub const POOL_GEN1OU_EVAL: &str = include_str!("../../../data/teams/gen1ou/split/eval.jsonl");

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
    /// Parse a source spec: "random", "randbats", "builtin", a path to a file/directory, or
    /// `"<pool dir>:train"` / `"<pool dir>:eval"` (the pool's held-out split, [`split_path`]).
    /// Pool teams get the legality checks of [`Team::from_spec`].
    pub fn from_spec(spec: &str) -> Result<TeamSource, String> {
        TeamSource::from_spec_opts(spec, true)
    }

    /// [`TeamSource::from_spec`]; `validate = false` skips the legality checks for pool files.
    pub fn from_spec_opts(spec: &str, validate: bool) -> Result<TeamSource, String> {
        match spec {
            "random" => Ok(TeamSource::RandomLegal),
            "randbats" => Ok(TeamSource::RandBats),
            "builtin" => Ok(TeamSource::Pool(builtin_teams())),
            // The shipped pool, embedded so installed wheels (no repository checkout) have it.
            "gen1ou" | "gen1ou:train" | "gen1ou:eval" => {
                let text = match spec {
                    "gen1ou" => POOL_GEN1OU,
                    "gen1ou:train" => POOL_GEN1OU_TRAIN,
                    _ => POOL_GEN1OU_EVAL,
                };
                let teams = parse_teams(text)?.iter().map(|t| Team::from_spec_opts(t, validate)).collect::<Result<Vec<_>, _>>()?;
                Ok(TeamSource::Pool(teams))
            }
            path => {
                let file = split_path(path).unwrap_or_else(|| path.into());
                Ok(TeamSource::Pool(load_teams_opts(&file, validate)?))
            }
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

    fn one(json: &str, validate: bool) -> Result<Team, String> {
        let spec: TeamSpec = serde_json::from_str(json).unwrap();
        Team::from_spec_opts(&spec, validate)
    }

    #[test]
    fn validation_rules() {
        let ok = r#"{"pokemon":[{"species":"Tauros","moves":["Body Slam"]},{"species":"Nidoran♀","moves":["Double Kick"]},{"species":"Nidoran♂","moves":["Horn Attack"]}]}"#;
        let t = one(ok, true).unwrap();
        assert_eq!(t.mons[1].species, data::species_id("Nidoran-F").unwrap());
        assert_eq!(t.mons[2].species, data::species_id("Nidoran-M").unwrap());
        // Species Clause and learnsets: rejected by default, allowed with validate=false.
        let dup = r#"{"pokemon":[{"species":"Tauros","moves":["Body Slam"]},{"species":"Tauros","moves":["Earthquake"]}]}"#;
        let illegal = r#"{"pokemon":[{"species":"Tauros","moves":["Spore"]}]}"#;
        let bad_hp = r#"{"pokemon":[{"species":"Tauros","moves":["Body Slam"],"dvs":{"hp":15,"atk":14}}]}"#;
        for (json, what) in [(dup, "Species Clause"), (illegal, "cannot learn"), (bad_hp, "HP DV")] {
            let e = one(json, true).unwrap_err();
            assert!(e.contains(what), "{e}");
            assert!(one(json, false).is_ok());
        }
        let good_hp = r#"{"pokemon":[{"species":"Tauros","moves":["Body Slam"],"dvs":{"hp":7,"atk":14}}]}"#;
        assert!(one(good_hp, true).is_ok());
        // Out-of-range DVs are an error (never clamped), with or without validation.
        let dv16 = r#"{"pokemon":[{"species":"Tauros","moves":["Body Slam"],"dvs":{"atk":16}}]}"#;
        for v in [true, false] {
            assert!(one(dv16, v).unwrap_err().contains("out of range"));
        }
        assert!(one(r#"{"pokemon":[{"species":"Nidoran","moves":["Tackle"]}]}"#, false).is_err());
    }

    #[test]
    fn pool_validates_and_split_is_a_partition() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/teams/gen1ou");
        if !dir.exists() {
            return;
        }
        let all = load_teams(&dir).expect("the pool passes validation");
        let d = dir.to_str().unwrap();
        let TeamSource::Pool(train) = TeamSource::from_spec(&format!("{d}:train")).unwrap() else { panic!() };
        let TeamSource::Pool(eval) = TeamSource::from_spec(&format!("{d}:eval")).unwrap() else { panic!() };
        assert_eq!(train.len() + eval.len(), all.len());
        let frac = eval.len() as f64 / all.len() as f64;
        assert!((0.08..0.12).contains(&frac), "eval fraction {frac}");
        let train_set: std::collections::HashSet<String> = train.iter().map(|t| format!("{t:?}")).collect();
        assert!(eval.iter().all(|t| !train_set.contains(&format!("{t:?}"))), "split overlaps");
        assert!(TeamSource::from_spec(&format!("{d}:test")).is_err());
        // The embedded copies are the same pools.
        for (name, files) in [("gen1ou", all), ("gen1ou:train", train), ("gen1ou:eval", eval)] {
            let TeamSource::Pool(e) = TeamSource::from_spec(name).unwrap() else { panic!() };
            assert_eq!(e, files, "{name}");
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
