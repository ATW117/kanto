//! Showdown team formats: the teambuilder's export text and the packed one-line format, read
//! into [`TeamSpec`]s and written from [`Team`]s; plus the `[Gen 1] OU` team-validator rules.
//!
//! Conversions (Gen 1, as Showdown stores Gen 1 sets):
//! - IVs are `2 * DV` (Showdown masks them with `& 30`), so `IV 31` and `IV 30` are both DV 15.
//!   Special is the `SpA` IV/EV (Showdown copies it to `SpD` in Gen 1).
//! - EVs are 0..=255 and enter the stat formula as `floor(EV / 4)`, exactly like
//!   `ceil(sqrt(stat exp))`, so EV `e` maps to stat exp `e * e` (255 -> 65535) and back without
//!   changing any stat.
//! - Defaults follow the path a real server takes: a packed set with an empty EV field gets
//!   252 in every stat (the validator's Gen 1-2 default), an export without an `EVs:` line is
//!   treated the same way (Showdown's sim-level `Teams.import` would give 0 EVs, which its
//!   validator then rejects as "exactly 0 EVs"); missing IVs are 31.
//! - The HP IV: Gen 1 derives the HP DV from the other DVs. If the set gives an HP IV it must agree
//!   (as Showdown's validator demands) unless validation is off; if it does not give one, kanto
//!   derives it (Showdown would assume 31 and reject the set unless the derived DV is 15).
//!
//! Items, abilities, natures, gender, shininess and happiness have no Gen 1 effect; they are
//! accepted and ignored, as Showdown's Gen 1 validator ignores them.

use crate::data;
use crate::team::{PokemonSet, StatSpread, Team, TeamSpec};

/// Whether `text` looks like a Showdown team (export or packed) rather than JSON.
pub fn looks_like_showdown(text: &str) -> bool {
    let t = text.trim_start();
    !(t.is_empty() || t.starts_with('{') || t.starts_with('['))
}

/// Parse Showdown teams: packed teams (one per non-empty line, `|`-separated, sets joined by
/// `]`; an optional `format]name|` prefix as in Showdown's team backups is skipped) or export
/// text (sets separated by blank lines; several teams separated by `=== [format] name ===`
/// headers).
pub fn parse_showdown_teams(text: &str) -> Result<Vec<TeamSpec>, String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return Err("empty team".into());
    }
    let packed = lines.iter().all(|l| l.contains('|') && !l.starts_with("- ") && !l.starts_with("==="));
    if packed {
        return lines
            .iter()
            .enumerate()
            .map(|(i, l)| parse_packed(l).map_err(|e| format!("line {}: {e}", i + 1)))
            .collect();
    }
    let mut teams: Vec<TeamSpec> = Vec::new();
    let mut cur: Option<TeamSpec> = None;
    let mut set_lines: Vec<&str> = Vec::new();
    let flush_set = |cur: &mut Option<TeamSpec>, set_lines: &mut Vec<&str>| -> Result<(), String> {
        if set_lines.is_empty() {
            return Ok(());
        }
        let set = parse_export_set(set_lines)?;
        cur.get_or_insert_with(|| TeamSpec {
            name: String::new(),
            source: "showdown-export".into(),
            pokemon: Vec::new(),
        })
        .pokemon
        .push(set);
        set_lines.clear();
        Ok(())
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line == "---" {
            flush_set(&mut cur, &mut set_lines)?;
        } else if let Some(h) = line.strip_prefix("===") {
            flush_set(&mut cur, &mut set_lines)?;
            if let Some(t) = cur.take() {
                teams.push(t);
            }
            let name = h.trim_end_matches('=').trim();
            let name = match name.strip_prefix('[') {
                Some(rest) => rest.split_once(']').map(|x| x.1.trim()).unwrap_or(name),
                None => name,
            };
            cur = Some(TeamSpec { name: name.to_string(), source: "showdown-export".into(), pokemon: Vec::new() });
        } else {
            set_lines.push(line);
        }
    }
    flush_set(&mut cur, &mut set_lines)?;
    if let Some(t) = cur.take() {
        teams.push(t);
    }
    teams.retain(|t| !t.pokemon.is_empty());
    if teams.is_empty() {
        return Err("no Pokémon found in the Showdown team".into());
    }
    Ok(teams)
}

/// Showdown stat names in export lines -> index [hp, atk, def, spa, spd, spe].
fn stat_index(name: &str) -> Option<usize> {
    Some(match data::to_id(name).as_str() {
        "hp" => 0,
        "atk" | "attack" => 1,
        "def" | "defense" => 2,
        "spa" | "spatk" | "specialattack" | "spc" | "special" => 3,
        "spd" | "spdef" | "specialdefense" => 4,
        "spe" | "speed" => 5,
        _ => return None,
    })
}

/// Build a [`PokemonSet`] from Showdown IVs/EVs (`[hp, atk, def, spa, spd, spe]`).
fn set_of(
    species: &str,
    nickname: Option<String>,
    moves: Vec<String>,
    level: Option<u8>,
    ivs: [u8; 6],
    hp_iv_given: bool,
    evs: [u16; 6],
) -> PokemonSet {
    let dv = |iv: u8| (iv.min(31) / 2) as u16;
    let exp = |ev: u16| -> u16 {
        let e = ev.min(255) as u32;
        if e >= 255 {
            65535
        } else {
            (e * e) as u16
        }
    };
    PokemonSet {
        species: species.to_string(),
        nickname,
        moves,
        level,
        dvs: Some(StatSpread {
            hp: hp_iv_given.then(|| dv(ivs[0])),
            atk: Some(dv(ivs[1])),
            def: Some(dv(ivs[2])),
            spe: Some(dv(ivs[5])),
            spc: Some(dv(ivs[3])),
        }),
        exp: Some(StatSpread {
            hp: Some(exp(evs[0])),
            atk: Some(exp(evs[1])),
            def: Some(exp(evs[2])),
            spe: Some(exp(evs[5])),
            spc: Some(exp(evs[3])),
        }),
    }
}

fn parse_export_set(lines: &[&str]) -> Result<PokemonSet, String> {
    let first = lines[0];
    let mut head = first.split(" @ ").next().unwrap_or(first).trim().to_string();
    for g in [" (M)", " (F)"] {
        if let Some(h) = head.strip_suffix(g) {
            head = h.to_string();
        }
    }
    let (nickname, species) = match (head.ends_with(')'), head.rfind('(')) {
        (true, Some(i)) => {
            let sp = head[i + 1..head.len() - 1].trim().to_string();
            let nick = head[..i].trim().to_string();
            (Some(nick).filter(|n| !n.is_empty()), sp)
        }
        _ => (None, head.clone()),
    };
    if data::species_id(&species).is_none() {
        return Err(format!("unknown species '{species}' in '{first}'"));
    }
    let mut ivs = [31u8; 6];
    let mut hp_iv_given = false;
    let mut evs = [252u16; 6];
    let mut level = None;
    let mut moves = Vec::new();
    for line in &lines[1..] {
        if let Some(m) = line.strip_prefix('-').or_else(|| line.strip_prefix('~')) {
            moves.push(m.trim().to_string());
        } else if let Some(l) = line.strip_prefix("Level:") {
            level = Some(l.trim().parse::<u8>().map_err(|_| format!("bad level '{}'", l.trim()))?);
        } else if let Some(s) = line.strip_prefix("EVs:") {
            evs = [0; 6];
            for part in s.split('/') {
                let mut it = part.split_whitespace();
                if let (Some(v), Some(name)) = (it.next(), it.next()) {
                    let i = stat_index(name).ok_or_else(|| format!("unknown stat '{name}'"))?;
                    evs[i] = v.parse().map_err(|_| format!("bad EV '{v}'"))?;
                }
            }
        } else if let Some(s) = line.strip_prefix("IVs:") {
            for part in s.split('/') {
                let mut it = part.split_whitespace();
                if let (Some(v), Some(name)) = (it.next(), it.next()) {
                    let i = stat_index(name).ok_or_else(|| format!("unknown stat '{name}'"))?;
                    ivs[i] = v.parse().map_err(|_| format!("bad IV '{v}'"))?;
                    hp_iv_given |= i == 0;
                }
            }
        }
        // Ability/Nature/Shiny/Happiness/Item/Tera lines: no Gen 1 effect.
    }
    Ok(set_of(&species, nickname, moves, level, ivs, hp_iv_given, evs))
}

/// One packed team. Fields per set: `name|species|item|ability|moves|nature|evs|gender|ivs|
/// shiny|level|happiness,...`; sets joined by `]`.
pub fn parse_packed(line: &str) -> Result<TeamSpec, String> {
    let mut body = line.trim();
    let mut name = String::new();
    // Team backups prefix `gen1ou]Team Name|`: a `]` before the first `|`.
    if let Some((head, rest)) = body.split_once('|') {
        if let Some((_format, n)) = head.split_once(']') {
            name = n.to_string();
            body = rest;
        }
    }
    let mut pokemon = Vec::new();
    for set in body.split(']') {
        if set.is_empty() {
            continue;
        }
        let f: Vec<&str> = set.split('|').collect();
        if f.len() < 11 {
            return Err(format!("packed set '{set}' has {} fields (need at least 11)", f.len()));
        }
        let nick = f[0].to_string();
        let species = if f[1].is_empty() { nick.clone() } else { f[1].to_string() };
        if data::species_id(&species).is_none() {
            return Err(format!("unknown species '{species}'"));
        }
        let nickname = (!nick.is_empty() && data::to_id(&nick) != data::to_id(&species)).then_some(nick);
        let moves: Vec<String> = f[4].split(',').filter(|m| !m.is_empty()).map(str::to_string).collect();
        let mut evs = [252u16; 6];
        if !f[6].is_empty() {
            for (i, v) in f[6].split(',').take(6).enumerate() {
                evs[i] = v.parse().unwrap_or(0);
            }
        }
        let mut ivs = [31u8; 6];
        let mut hp_iv_given = false;
        if !f[8].is_empty() {
            for (i, v) in f[8].split(',').take(6).enumerate() {
                if !v.is_empty() {
                    ivs[i] = v.parse().unwrap_or(0);
                    hp_iv_given |= i == 0;
                }
            }
        }
        let level = if f[10].is_empty() {
            None
        } else {
            Some(f[10].parse::<u8>().map_err(|_| format!("bad level '{}'", f[10]))?)
        };
        pokemon.push(set_of(&species, nickname, moves, level, ivs, hp_iv_given, evs));
    }
    if pokemon.is_empty() {
        return Err("empty packed team".into());
    }
    Ok(TeamSpec { name, source: "showdown-packed".into(), pokemon })
}

/// Showdown EV for a stat exp value (the inverse of the import mapping).
fn ev_of(exp: u16) -> u16 {
    ((exp as f64).sqrt().ceil() as u16).min(255)
}

/// Showdown IVs/EVs `[hp, atk, def, spa, spd, spe]` of a team member.
fn ivs_evs(m: &crate::team::Mon) -> ([u8; 6], [u16; 6]) {
    let d = m.dvs;
    let hp = (d[0] & 1) << 3 | (d[1] & 1) << 2 | (d[2] & 1) << 1 | (d[3] & 1);
    let iv = |dv: u8| if dv == 15 { 31 } else { dv * 2 };
    let e = m.exp;
    (
        [iv(hp), iv(d[0]), iv(d[1]), iv(d[3]), iv(d[3]), iv(d[2])],
        [ev_of(e[0]), ev_of(e[1]), ev_of(e[2]), ev_of(e[4]), ev_of(e[4]), ev_of(e[3])],
    )
}

fn nick_of<'a>(nicknames: &'a [String], i: usize) -> Option<&'a str> {
    nicknames.get(i).map(String::as_str).filter(|n| !n.is_empty())
}

/// The team in Showdown's export (teambuilder) format, with explicit EVs and IVs, so that
/// `Teams.import` on it gives exactly this team's stats.
pub fn to_export(team: &Team, nicknames: &[String]) -> String {
    let mut out = String::new();
    for (i, m) in team.mons().iter().enumerate() {
        let species = data::SPECIES_NAMES[m.species as usize];
        match nick_of(nicknames, i) {
            Some(n) if n != species => out.push_str(&format!("{n} ({species})  \n")),
            _ => out.push_str(&format!("{species}  \n")),
        }
        // Explicit, as in Showdown's own validated export: an unvalidated Showdown battle fills
        // a missing ability from the modern dex (Tauros would get Intimidate).
        out.push_str("Ability: No Ability  \n");
        if m.level != 100 {
            out.push_str(&format!("Level: {}  \n", m.level));
        }
        let (ivs, evs) = ivs_evs(m);
        const NAMES: [&str; 6] = ["HP", "Atk", "Def", "SpA", "SpD", "Spe"];
        let ev: Vec<String> = (0..6).map(|k| format!("{} {}", evs[k], NAMES[k])).collect();
        out.push_str(&format!("EVs: {}  \n", ev.join(" / ")));
        let iv: Vec<String> = (0..6).filter(|&k| ivs[k] != 31).map(|k| format!("{} {}", ivs[k], NAMES[k])).collect();
        if !iv.is_empty() {
            out.push_str(&format!("IVs: {}  \n", iv.join(" / ")));
        }
        for &mv in m.moves.iter().filter(|&&x| x != 0) {
            out.push_str(&format!("- {}  \n", data::MOVE_NAMES[mv as usize]));
        }
        out.push('\n');
    }
    out
}

/// The team in Showdown's packed format (what `/utm` and `>player pN {"team": ...}` take), with
/// explicit EVs, IVs and `No Ability` (an empty ability field unpacks to the species' modern
/// ability, which an unvalidated Gen 1 Showdown battle then uses).
pub fn to_packed(team: &Team, nicknames: &[String]) -> String {
    let sets: Vec<String> = team
        .mons()
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let species = data::SPECIES_NAMES[m.species as usize];
            let name = nick_of(nicknames, i).unwrap_or(species);
            let species_field =
                if data::to_id(name) == data::to_id(species) { String::new() } else { species.to_string() };
            let moves: Vec<String> =
                m.moves.iter().filter(|&&x| x != 0).map(|&x| data::to_id(data::MOVE_NAMES[x as usize])).collect();
            let (ivs, evs) = ivs_evs(m);
            let evs: Vec<String> = evs.iter().map(|e| e.to_string()).collect();
            let ivs: Vec<String> = ivs.iter().map(|&v| if v == 31 { String::new() } else { v.to_string() }).collect();
            let ivs = ivs.join(",");
            let ivs = if ivs == ",,,,," { String::new() } else { ivs };
            let level = if m.level == 100 { String::new() } else { m.level.to_string() };
            format!("{name}|{species_field}||NoAbility|{}||{}||{ivs}||{level}|", moves.join(","), evs.join(","))
        })
        .collect();
    sets.join("]")
}

/// Problems Showdown's `TeamValidator` would report for a `[Gen 1] OU` team beyond what
/// [`Team::from_spec`] already checks (Species Clause, learnsets, HP DV consistency): the
/// format's bans. Empty = legal under these rules. `format`: `"gen1ou"` (also used for
/// `"gen1nc"`, which shares the team rules).
///
/// Covered: Uber species (Mewtwo, Mew), the `Standard` bans (Dig, Fly), OHKO Clause (Fissure,
/// Guillotine, Horn Drill), Evasion Moves Clause (Double Team, Minimize), Nickname Clause, and
/// the "exactly 0 EVs" check. Not covered: Showdown's event-only and level-up minimum-level
/// checks ("Obtainable"), which reject a few move combinations the learnset union allows.
pub fn format_problems(spec: &TeamSpec, format: &str) -> Result<Vec<String>, String> {
    if !matches!(format, "gen1ou" | "gen1nc") {
        return Err(format!("unknown format '{format}' (expected 'gen1ou' or 'gen1nc')"));
    }
    const UBERS: [&str; 2] = ["mewtwo", "mew"];
    const BANNED_MOVES: [(&str, &str); 7] = [
        ("dig", "Dig is banned."),
        ("fly", "Fly is banned."),
        ("fissure", "OHKO Clause"),
        ("guillotine", "OHKO Clause"),
        ("horndrill", "OHKO Clause"),
        ("doubleteam", "Evasion Moves Clause"),
        ("minimize", "Evasion Moves Clause"),
    ];
    let mut out = Vec::new();
    let mut nicks: Vec<String> = Vec::new();
    for p in &spec.pokemon {
        let sid = data::to_id(&p.species);
        let display = data::species_id(&p.species).map(|s| data::SPECIES_NAMES[s as usize]).unwrap_or(&p.species);
        if UBERS.contains(&sid.as_str()) {
            out.push(format!("{display} is tagged Uber, which is banned."));
        }
        for m in &p.moves {
            let mid = data::to_id(m);
            if let Some((_, why)) = BANNED_MOVES.iter().find(|(id, _)| *id == mid) {
                out.push(format!("{display}'s move {m} is banned ({why})."));
            }
        }
        if let Some(n) = &p.nickname {
            let n = data::to_id(n);
            if nicks.contains(&n) {
                out.push(format!(
                    "Your Pokémon must have different nicknames (Nickname Clause): '{}'.",
                    p.nickname.as_deref().unwrap_or("")
                ));
            }
            nicks.push(n);
        }
        // EV sanity checks (`validateStats`), on the EVs Showdown sees: the packed EVs kanto writes
        // (stat exp -> EV, 65535 when not given), Special counted for both SpA and SpD.
        let e = p.exp.clone().unwrap_or_default();
        let ev = |x: Option<u16>| ev_of(x.unwrap_or(65535)) as u32;
        let total = ev(e.hp) + ev(e.atk) + ev(e.def) + 2 * ev(e.spc) + ev(e.spe);
        let level = p.level.unwrap_or(100);
        let named = match &p.nickname {
            Some(n) if !n.is_empty() => format!("{n} ({display})"),
            _ => display.to_string(),
        };
        if level > 1 && total == 0 {
            out.push(format!("{display} has exactly 0 EVs."));
        } else if total == 508 || total == 510 {
            out.push(format!("{named} has exactly {total} EVs, but this format does not restrict you to 510 EVs."));
        }
        if level == 50 && total % 4 == 0 {
            out.push(format!("{named} is level 50, but this format allows level 100 Pokémon."));
        }
        out.extend(obtainability(p));
    }
    Ok(out)
}

/// The obtainability checks of Showdown's `[Gen 1] OU` validator (`TeamValidator#validateSet`,
/// `validateMoves`, `checkCanLearn`, `validateSource`/`validateEvent`, ported for Gen 1 sources
/// only): the "Obtainable Moves" combination bans, moves learned by level-up above the set's
/// level, event-only moves (one event must give all of them; no Japan-only events; the event's
/// minimum level), the lowest Gen 1 encounter level and the minimum level to be evolved. (Sets are
/// taken as not shiny: kanto does not store shininess.)
pub fn obtainability(p: &crate::team::PokemonSet) -> Vec<String> {
    use crate::data::obtainable::{COMBO_BANS, EVENTS, OBTAIN};
    let mut out = Vec::new();
    let Some(num) = data::species_id(&p.species) else { return out };
    let sp = &OBTAIN[num as usize];
    let level = p.level.unwrap_or(100);
    let species_name = sp.name;
    // validateSet's `name`: "Nick (Species)" for a nickname.
    let name = match &p.nickname {
        Some(n) if !n.is_empty() && n != species_name => format!("{n} ({species_name})"),
        _ => species_name.to_string(),
    };
    let by_id = |id: &str| OBTAIN.iter().find(|o| o.id == id);
    let move_ids: Vec<String> = p.moves.iter().map(|m| data::to_id(m)).collect();
    let move_name = |id: &str| data::move_id(id).map(|k| data::MOVE_NAMES[k as usize]).unwrap_or("");
    // Complex bans ("pokemon:" = this species, "basepokemon:" = its base species: the same in Gen 1).
    for (rule, source, parts) in COMBO_BANS.iter() {
        let has = |part: &str| match part.split_once(':') {
            Some(("pokemon" | "basepokemon", id)) => id == sp.id,
            Some(("move", id)) => move_ids.iter().any(|m| m == id),
            _ => false,
        };
        if parts.iter().all(|x| has(x)) {
            if *source == "Obtainable Moves" {
                out.push(format!("{name} has the combination of {rule}, which is impossible to obtain legitimately."));
            } else {
                out.push(format!("{name} has the combination of {rule}, which is banned by {source}."));
            }
        }
    }
    // The species and its pre-evolutions, as Showdown's getFullLearnset walks them.
    let mut chain = vec![sp];
    while let Some(prev) = by_id(chain.last().unwrap().prevo).filter(|_| !chain.last().unwrap().prevo.is_empty()) {
        chain.push(prev);
    }
    // checkCanLearn for each move; `restricted` = the event sources every move so far allows
    // (None = unrestricted, Showdown's `sourcesBefore`).
    let mut restricted: Option<Vec<(&str, u8)>> = None;
    let mut restrictive_moves: Vec<&str> = Vec::new();
    let mut baby_only: Option<&str> = None;
    let mut move_problem = None;
    for mid in &move_ids {
        let mname = move_name(mid);
        if !restrictive_moves.contains(&mname) {
            restrictive_moves.push(mname);
        }
        let mut reason: Option<u32> = None;
        let mut events: Vec<(&str, u8)> = Vec::new();
        let mut unrestricted = false;
        let mut baby: Option<&str> = None;
        for (k, s) in chain.iter().enumerate() {
            if k > 0 && events.is_empty() && !unrestricted {
                baby = Some(s.id);
            }
            let Some((_, srcs)) = s.moves.iter().find(|(m, _)| m == mid) else { continue };
            for src in srcs.iter() {
                let kind = src.as_bytes().get(1).copied().unwrap_or(b'?');
                match kind {
                    b'L' => {
                        let n: u32 = src[2..].parse().unwrap_or(0);
                        if level as u32 >= n {
                            unrestricted = true;
                        } else {
                            reason = Some(n);
                        }
                    }
                    b'M' | b'T' => unrestricted = true,
                    b'S' => {
                        let i: u8 = src[2..].parse().unwrap_or(0);
                        events.push((s.id, i));
                    }
                    _ => {}
                }
            }
            if unrestricted && baby.is_none() {
                break;
            }
        }
        if unrestricted {
            if baby.is_some() {
                baby_only = baby;
            }
            continue;
        }
        if events.is_empty() {
            move_problem = Some(match reason {
                Some(n) => format!("{name}'s move {mname} is learned at level {n}."),
                None => format!("{name} can't learn {mname}."),
            });
            break;
        }
        let next = match &restricted {
            None => events.clone(),
            Some(r) => r.iter().copied().filter(|e| events.contains(e)).collect(),
        };
        if next.is_empty() {
            move_problem = Some(format!("{name}'s moves {} are incompatible.", restrictive_moves.join(", ")));
            break;
        }
        restricted = Some(next);
        if baby.is_some() {
            baby_only = baby;
        }
    }
    if let Some(m) = move_problem {
        out.push(m);
        return out;
    }
    if let (Some(baby), Some(r)) = (baby_only, restricted.as_mut()) {
        r.retain(|(s, _)| *s == baby);
        if r.is_empty() {
            let bname = by_id(baby).map_or("", |o| o.name);
            out.push(format!("{name}'s event/egg moves are from an evolution, and are incompatible with its moves from {bname}."));
            return out;
        }
    }
    let event = |s: &str, i: u8| EVENTS.iter().find(|e| e.0 == s && e.1 == i);
    // validateEvent with `fastReturn`: whether the set qualifies for the event.
    let qualifies = |s: &str, i: u8| event(s, i).is_some_and(|e| !e.3 && level >= e.2);
    let event_problems = |s: &str, i: u8, because: &str| -> Vec<String> {
        let mut v = Vec::new();
        if let Some(e) = event(s, i) {
            let n = match &p.nickname {
                Some(n) if !n.is_empty() && n != species_name => format!("{n} ({species_name})"),
                _ => species_name.to_string(),
            };
            if e.3 {
                v.push(format!("{n} has moves from Japan-only events, but this format simulates International Yellow/Crystal which can't trade with Japanese games."));
            }
            if level < e.2 {
                v.push(format!("{n} must be at least level {}{because} from an event.", e.2));
            }
        }
        v
    };
    // Lowest Gen 1 encounter level.
    let mut from_rby = false;
    if sp.encounter > 0 {
        if level < sp.encounter {
            out.push(format!("{name} is not obtainable at levels below {} in Gen 1.", sp.encounter));
        }
        from_rby = true;
    }
    // Minimum level to be evolved.
    let mut under: Option<(&str, u8)> = None;
    if !from_rby {
        for e in &chain {
            if e.prevo.is_empty() {
                break;
            }
            if level < e.evo_level {
                under = Some((e.id, e.evo_level));
                break;
            }
        }
    }
    let sources: Option<Vec<(&str, u8)>> = match (&restricted, under) {
        (Some(r), _) => Some(r.clone()),
        (None, Some((uid, _))) => {
            let mut v = Vec::new();
            for e in &chain {
                if e.prevo.is_empty() {
                    break;
                }
                for ev in EVENTS.iter().filter(|ev| ev.0 == e.id) {
                    if ev.2 > 0 && level >= ev.2 {
                        v.push((ev.0, ev.1));
                    }
                }
                if e.id == uid {
                    break;
                }
            }
            Some(v)
        }
        (None, None) => None,
    };
    if let Some(sources) = sources {
        let legal: Vec<&(&str, u8)> = sources
            .iter()
            .filter(|(s, _)| match under {
                // Underleveled: only events of this species or of the underleveled one count.
                Some((uid, _)) => *s == sp.id || *s == uid,
                None => true,
            })
            .filter(|(s, i)| qualifies(s, *i))
            .collect();
        if legal.is_empty() {
            if let Some((_, lv)) = under {
                out.push(format!("{name} must be at least level {lv} to be evolved."));
                if let Some((s, i)) = sources.first() {
                    out.extend(event_problems(s, *i, " to be underleveled"));
                }
            } else if let Some((s, i)) = sources.first() {
                if sources.len() > 1 {
                    out.push(format!("{name} has an event-exclusive move that it doesn't qualify for (only one of several ways to get the move will be listed):"));
                }
                out.extend(event_problems(s, *i, " because it has a move only available"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = "Bessie (Tauros) @ Leftovers\nAbility: No Ability\nEVs: 252 HP / 252 Atk / 252 Def / 252 SpA / 252 SpD / 252 Spe\n- Body Slam\n- Hyper Beam\n- Blizzard\n- Earthquake\n\nChansey\nIVs: 28 Atk / 14 HP\n- Ice Beam\n- Soft-Boiled\n";

    #[test]
    fn export_round_trips_through_team() {
        let specs = parse_showdown_teams(EXPORT).unwrap();
        assert_eq!(specs.len(), 1);
        let t = Team::from_spec(&specs[0]).unwrap();
        assert_eq!(specs[0].pokemon[0].nickname.as_deref(), Some("Bessie"));
        assert_eq!(t.mons[1].dvs, [14, 15, 15, 15]);
        // Round trip: export -> parse gives the same team (stats included).
        let text = to_export(&t, &["Bessie".into()]);
        let t2 = Team::from_spec(&parse_showdown_teams(&text).unwrap()[0]).unwrap();
        assert_eq!(t, t2);
        let packed = to_packed(&t, &["Bessie".into()]);
        let t3 = Team::from_spec(&parse_showdown_teams(&packed).unwrap()[0]).unwrap();
        assert_eq!(t, t3);
    }

    #[test]
    fn inconsistent_hp_iv_is_rejected_when_validating() {
        let text = "Chansey\nIVs: 28 Atk\n- Ice Beam\n";
        // HP IV absent: derived (7), accepted.
        assert!(Team::from_spec(&parse_showdown_teams(text).unwrap()[0]).is_ok());
        let bad = "Chansey\nIVs: 30 HP / 28 Atk\n- Ice Beam\n";
        assert!(Team::from_spec(&parse_showdown_teams(bad).unwrap()[0]).is_err());
    }

    #[test]
    fn packed_defaults_and_backup_prefix() {
        let t = parse_showdown_teams("gen1ou]My Team|Tauros|||-|bodyslam,hyperbeam|||||||").unwrap();
        assert_eq!(t[0].name, "My Team");
        let team = Team::from_spec(&t[0]).unwrap();
        assert_eq!(
            team.mons[0],
            crate::team::Mon::new(team.mons[0].species, &team.mons[0].moves[..2], 100, [15; 4], [63504; 5])
        );
    }

    #[test]
    fn multiple_export_teams() {
        let text = "=== [gen1ou] A ===\n\nTauros\n- Body Slam\n\n=== [gen1ou] B ===\n\nChansey\n- Ice Beam\n\nSnorlax\n- Rest\n";
        let t = parse_showdown_teams(text).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].name.as_str(), t[1].pokemon.len()), ("A", 2));
    }

    #[test]
    fn gen1ou_bans() {
        let t = parse_showdown_teams("Mewtwo\n- Psychic\n- Fly\n\nChansey\n- Minimize\n").unwrap();
        let p = format_problems(&t[0], "gen1ou").unwrap();
        // Uber, Fly banned, Mewtwo can't learn Fly (the obtainability check), Minimize banned.
        assert_eq!(p.len(), 4, "{p:?}");
    }

    /// Showdown's obtainability checks (messages as `TeamValidator('gen1ou')` writes them).
    #[test]
    fn gen1ou_obtainability() {
        let check = |text: &str| format_problems(&parse_showdown_teams(text).unwrap()[0], "gen1ou").unwrap();
        assert_eq!(check("Moltres\nLevel: 40\n- Fire Spin\n"), ["Moltres is not obtainable at levels below 50 in Gen 1."]);
        assert_eq!(check("Charizard\nLevel: 30\n- Ember\n"), ["Charizard must be at least level 36 to be evolved."]);
        assert_eq!(check("Doduo\nLevel: 20\n- Drill Peck\n"), ["Doduo's move Drill Peck is learned at level 30."]);
        assert_eq!(check("Kakuna\n- String Shot\n- Harden\n"), ["Kakuna has the combination of Kakuna + String Shot + Harden, which is impossible to obtain legitimately."]);
        assert_eq!(
            check("Magikarp\n- Dragon Rage\n"),
            ["Magikarp has moves from Japan-only events, but this format simulates International Yellow/Crystal which can't trade with Japanese games."]
        );
        assert!(check("Psyduck\nLevel: 15\n- Amnesia\n- Scratch\n").is_empty());
        assert_eq!(
            check("Psyduck\nLevel: 14\n- Amnesia\n"),
            ["Psyduck is not obtainable at levels below 15 in Gen 1.", "Psyduck must be at least level 15 because it has a move only available from an event."]
        );
    }
}
