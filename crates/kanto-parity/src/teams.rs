//! `parity teams`: Showdown team formats and the `[Gen 1] OU` validator, checked against
//! Showdown (parity/showdown/teams.js).
//!
//! For every team (the shipped pool plus random teams with random DVs, stat exp, levels,
//! nicknames, banned species/moves, illegal moves and duplicates), kanto writes the export text
//! and the packed string. Showdown imports both and builds a `Battle` from each; the stats,
//! levels, names and moves of every Pokémon must equal kanto's. Showdown's own `Teams.pack` of
//! the export is parsed back by kanto and must give the same team. Finally kanto's verdict
//! (`Team::from_spec` + `format_problems`) is compared with `TeamValidator('gen1ou')`.

use kanto_env::data;
use kanto_env::rng::Rng;
use kanto_env::showdown_team::{format_problems, parse_showdown_teams, to_export, to_packed};
use kanto_env::team::{parse_teams, PokemonSet, StatSpread, Team, TeamSpec};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

fn random_spec(rng: &mut Rng, level100: bool) -> TeamSpec {
    let n = 1 + rng.below(6) as usize;
    let mut pokemon = Vec::new();
    let nicks = ["Bessie", "Mr. Sparkles", "Ω-Zero", "Fluffy"];
    for _ in 0..n {
        let species = 1 + rng.below(151) as u8;
        let learn = data::LEARNSETS_ALL[species as usize];
        let k = 1 + rng.below(4) as usize;
        let mut moves: Vec<String> = Vec::new();
        while moves.len() < k {
            // Mostly learnable moves; sometimes any move (an illegal one).
            let id = if rng.below(10) == 0 || learn.is_empty() {
                1 + rng.below(164) as u8
            } else {
                learn[rng.below(learn.len() as u64) as usize]
            };
            let name = data::MOVE_NAMES[id as usize].to_string();
            if !moves.contains(&name) {
                moves.push(name);
            }
        }
        let dv = |rng: &mut Rng| if rng.below(3) == 0 { Some(rng.below(16) as u16) } else { None };
        let ex = |rng: &mut Rng| match rng.below(4) {
            0 => Some(rng.below(65536) as u16),
            1 => Some((rng.below(256) * rng.below(256)) as u16),
            _ => None,
        };
        let level = if !level100 && rng.below(3) == 0 { Some(1 + rng.below(100) as u8) } else { None };
        let zero = rng.below(40) == 0;
        let exp = if zero {
            Some(StatSpread { hp: Some(0), atk: Some(0), def: Some(0), spe: Some(0), spc: Some(0) })
        } else {
            Some(StatSpread { hp: ex(rng), atk: ex(rng), def: ex(rng), spe: ex(rng), spc: ex(rng) })
        };
        pokemon.push(PokemonSet {
            species: data::SPECIES_NAMES[species as usize].to_string(),
            nickname: if rng.below(4) == 0 {
                Some(nicks[rng.below(nicks.len() as u64) as usize].to_string())
            } else {
                None
            },
            moves,
            level,
            dvs: Some(StatSpread { hp: None, atk: dv(rng), def: dv(rng), spe: dv(rng), spc: dv(rng) }),
            exp,
        });
    }
    TeamSpec { name: String::new(), source: "random".into(), pokemon }
}

fn kanto_mons(t: &Team, nicks: &[String]) -> Vec<Value> {
    t.mons()
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let species = data::SPECIES_NAMES[m.species as usize];
            let name = nicks.get(i).filter(|n| !n.is_empty()).map(String::as_str).unwrap_or(species);
            let moves: Vec<String> = m.moves.iter().filter(|&&x| x != 0).map(|&x| data::to_id(data::MOVE_NAMES[x as usize])).collect();
            json!({"name": name, "species": species, "level": m.level, "maxhp": m.stats[0], "atk": m.stats[1],
                   "def": m.stats[2], "spa": m.stats[4], "spd": m.stats[4], "spe": m.stats[3], "moves": moves, "ability": "noability"})
        })
        .collect()
}

pub fn run(repo: &Path, n_random: usize, seed: u64, level100: bool, ps_dir: Option<&str>) -> Result<(), String> {
    let sd = crate::runner::Showdown::locate(repo, ps_dir)?;
    let pool = std::fs::read_to_string(repo.join("data/teams/gen1ou/teams.jsonl")).map_err(|e| e.to_string())?;
    let mut specs = parse_teams(&pool)?;
    let n_pool = specs.len();
    let mut rng = Rng::new(seed);
    specs.extend((0..n_random).map(|_| random_spec(&mut rng, level100)));

    struct Item {
        team: Team,
        nicks: Vec<String>,
        kanto_ok: bool,
        kanto_why: String,
    }
    let mut items = Vec::new();
    let mut input = String::new();
    for (i, s) in specs.iter().enumerate() {
        // Unvalidated resolution (the stats); skip specs kanto cannot represent at all.
        let Ok(team) = Team::from_spec_opts(s, false) else { continue };
        let nicks: Vec<String> = s.pokemon.iter().map(|p| p.nickname.clone().unwrap_or_default()).collect();
        let (kanto_ok, kanto_why) = match Team::from_spec(s) {
            Err(e) => (false, e),
            Ok(_) => {
                let p = format_problems(s, "gen1ou")?;
                (p.is_empty(), p.join("; "))
            }
        };
        input.push_str(
            &json!({"id": items.len(), "export": to_export(&team, &nicks), "packed": to_packed(&team, &nicks)})
                .to_string(),
        );
        input.push('\n');
        let _ = i;
        items.push(Item { team, nicks, kanto_ok, kanto_why });
    }
    let mut child = Command::new(&sd.node)
        .arg(repo.join("parity/showdown/teams.js"))
        .env("PS_DIR", &sd.ps_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = BufReader::new(child.stdout.take().unwrap());
    let (mut stat_ok, mut stat_bad, mut repack_ok, mut repack_bad, mut repack_skipped) = (0, 0, 0, 0, 0);
    let mut verdict: BTreeMap<String, usize> = BTreeMap::new();
    let mut disagree: BTreeMap<String, usize> = BTreeMap::new();
    let mut examples: Vec<String> = Vec::new();
    for line in out.lines() {
        let line = line.map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        let it = &items[v["id"].as_u64().unwrap() as usize];
        if let Some(e) = v.get("error") {
            return Err(format!("showdown error on team {}: {e}", v["id"]));
        }
        let want = Value::Array(kanto_mons(&it.team, &it.nicks));
        for k in ["export", "packed"] {
            if v[k] == want {
                stat_ok += 1;
            } else {
                stat_bad += 1;
                if examples.len() < 5 {
                    examples.push(format!("{k}: showdown {} kanto {}", v[k], want));
                }
            }
        }
        // Showdown's pack writes all-zero EVs as an empty field, which unpacks as "default"
        // (252 after validation): the packed format cannot express them. Such teams (which
        // Showdown's validator rejects anyway) are not compared.
        if it.team.mons().iter().any(|m| m.exp.iter().all(|&e| e == 0)) {
            repack_skipped += 1;
        } else {
            match parse_showdown_teams(v["repacked"].as_str().unwrap_or(""))
                .and_then(|t| Team::from_spec_opts(&t[0], false))
            {
                Ok(t) if kanto_mons(&t, &it.nicks) == kanto_mons(&it.team, &it.nicks) => repack_ok += 1,
                r => {
                    repack_bad += 1;
                    if examples.len() < 10 {
                        examples.push(format!("repack {:?}: {r:?}", v["repacked"]));
                    }
                }
            }
        }
        let problems: Vec<String> = v["problems"]
            .as_array()
            .map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default();
        let ps_ok = problems.is_empty();
        *verdict
            .entry(format!(
                "kanto {} / showdown {}",
                if it.kanto_ok { "legal" } else { "illegal" },
                if ps_ok { "legal" } else { "illegal" }
            ))
            .or_default() += 1;
        if it.kanto_ok != ps_ok {
            let key = if ps_ok {
                format!("kanto rejects: {}", it.kanto_why)
            } else {
                format!("showdown rejects: {}", problems.join(" | "))
            };
            *disagree.entry(key).or_default() += 1;
        }
    }
    writer.join().unwrap().map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    println!("teams: {} ({} pool, {} random)", items.len(), n_pool, items.len() - n_pool);
    println!(
        "stats/levels/names/moves of Showdown's Battle from kanto's export+packed: {stat_ok} match, {stat_bad} differ"
    );
    println!("kanto parsing Showdown's Teams.pack(Teams.import(export)): {repack_ok} match, {repack_bad} differ ({repack_skipped} teams with an all-zero-EV Pokémon skipped)");
    for (k, c) in &verdict {
        println!("  {k}: {c}");
    }
    let mut d: Vec<_> = disagree.into_iter().collect();
    d.sort_by(|a, b| b.1.cmp(&a.1));
    println!("validator disagreements (top 25 reasons):");
    for (k, c) in d.iter().take(25) {
        println!("  {c:6}  {k}");
    }
    for e in &examples {
        println!("example: {e}");
    }
    if stat_bad > 0 || repack_bad > 0 {
        return Err("team format mismatch".into());
    }
    Ok(())
}
