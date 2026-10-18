//! `parity stream-fuzz`: byte-for-byte differential test of kanto's Showdown `BattleStream`
//! (`kanto_backend::sd`) against pokemon-showdown's own `BattleTextStream`, the stream behind
//! `pokemon-showdown simulate-battle`.
//!
//! Each case is a random battle-stream input: `>start` (seed formats, rated, custom rules,
//! players inline), `>player` (packed or JSON teams with random nicknames, levels, EV/IV fields,
//! genders, shininess, key order), then choices chosen from the requests kanto sends back — most
//! of them legal (by index, by name, `default`, with targets), the rest from a grammar of invalid
//! ones (bad indices, names, targets, events, `pass`, `shift`, `team`, garbage), plus `undo`,
//! `testfight`, `>chat`, `>requestlog`/`requestexport`/`requestteam`, `>reseed` and endings by
//! `>forcewin`/`>forcetie`/`>forcelose`/`>tiebreak`. Each input is run three ways in both
//! implementations — one write per line (an interactive client), the whole input as one write
//! (`simulate-battle < file`), and random chunk boundaries — and the outputs are compared with
//! only the `|t:|` timestamps normalised.

use crate::runner::Showdown;
use kanto_backend::sd::json::{Obj, Value};
use kanto_backend::sd::{BattleStream, BattleTextStream};
use kanto_data::gen1::{MOVES, SPECIES};
use kanto_env::rng::{derive_seed, Rng};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn pick<'a, T>(rng: &mut Rng, xs: &'a [T]) -> &'a T {
    &xs[rng.below(xs.len() as u64) as usize]
}

fn chance(rng: &mut Rng, p: f64) -> bool {
    ((rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64) < p
}

const NAMES: [&str; 8] = ["Alice", "Bob", "Ash Ketchum", "Gary \"Oak\"", "Rival_99", "Pokémon Trainer", "p1", "x"];
const NICKS: [&str; 6] = ["Bessie", "Mr. Sparkles", "A Very Long Nickname Indeed", "Ω-Zero", "Fluffy", "bob"];
const NATURES: [&str; 4] = ["Docile", "Serious", "Quirky", "Hardy"];

/// A random set as a Showdown set object (the JSON form).
fn random_set(rng: &mut Rng, used: &mut Vec<u8>) -> Obj {
    let mut sp;
    loop {
        sp = 1 + rng.below(151) as u8;
        if !used.contains(&sp) {
            break;
        }
    }
    used.push(sp);
    let n_moves = 1 + rng.below(4) as usize;
    let mut moves: Vec<u8> = Vec::new();
    while moves.len() < n_moves {
        let m = 1 + rng.below(164) as u8;
        if !moves.contains(&m) {
            moves.push(m);
        }
    }
    let species = SPECIES[sp as usize];
    let mut o = Obj::new();
    if chance(rng, 0.3) {
        o.set("name", Value::str(pick(rng, &NICKS)));
    } else if chance(rng, 0.3) {
        o.set("name", Value::str(species.name));
    }
    o.set("species", Value::str(if chance(rng, 0.2) { species.id } else { species.name }));
    if chance(rng, 0.5) {
        o.set("item", Value::str(""));
    }
    if chance(rng, 0.7) {
        o.set("ability", Value::str(if chance(rng, 0.5) { "No Ability" } else { "noability" }));
    }
    let mv: Vec<Value> = moves.iter().map(|&m| Value::str(if chance(rng, 0.5) { MOVES[m as usize].name } else { MOVES[m as usize].id })).collect();
    o.set("moves", Value::Arr(mv));
    if chance(rng, 0.3) {
        o.set("nature", Value::str(pick(rng, &NATURES)));
    }
    let stats = ["hp", "atk", "def", "spa", "spd", "spe"];
    if chance(rng, 0.7) {
        let mut e = Obj::new();
        for s in stats {
            if chance(rng, 0.85) {
                e.set(s, Value::Num(rng.below(256) as f64));
            }
        }
        o.set("evs", Value::Obj(e));
    }
    if chance(rng, 0.7) {
        let mut e = Obj::new();
        for s in stats {
            if chance(rng, 0.85) {
                e.set(s, Value::Num(rng.below(32) as f64));
            }
        }
        o.set("ivs", Value::Obj(e));
    }
    if chance(rng, 0.6) {
        let l = if chance(rng, 0.5) { 100 } else { 1 + rng.below(100) };
        o.set("level", Value::Num(l as f64));
    }
    if chance(rng, 0.1) {
        o.set("gender", Value::str(pick(rng, &["M", "F", "N"])));
    }
    if chance(rng, 0.1) {
        o.set("shiny", Value::Bool(true));
    }
    if chance(rng, 0.1) {
        o.set("happiness", Value::Num(rng.below(256) as f64));
    }
    if chance(rng, 0.05) {
        o.set("pokeball", Value::str("Great Ball"));
    }
    // Random key order.
    if chance(rng, 0.3) {
        let n = o.0.len();
        for i in (1..n).rev() {
            let j = rng.below(i as u64 + 1) as usize;
            o.0.swap(i, j);
        }
    }
    o
}

fn random_team(rng: &mut Rng) -> Value {
    let n = 1 + rng.below(6) as usize;
    let mut used = Vec::new();
    let sets: Vec<Value> = (0..n).map(|_| Value::Obj(random_set(rng, &mut used))).collect();
    if chance(rng, 0.5) {
        Value::Arr(sets)
    } else {
        // Packed: Showdown's pack of the JSON form, with No Ability (an empty packed ability
        // would get the species' modern ability, which the native simulator rejects).
        let mut sets = sets;
        for s in sets.iter_mut() {
            s.as_obj_mut().unwrap().set("ability", Value::str("No Ability"));
        }
        Value::Str(kanto_backend::sd::sets::pack(&sets))
    }
}

fn random_seed(rng: &mut Rng) -> Value {
    match rng.below(4) {
        0 => Value::Arr((0..4).map(|_| Value::Num(rng.below(65536) as f64)).collect()),
        1 => Value::Str(format!("{},{},{},{}", rng.below(65536), rng.below(65536), rng.below(65536), rng.below(65536))),
        2 => Value::Str(format!("sodium,{:016x}{:016x}", rng.next_u64(), rng.next_u64())),
        _ => Value::Str(format!("gen5,{:016x}", rng.next_u64())),
    }
}

/// The latest request JSON per side in some output messages.
fn scan_requests(msgs: &[String], reqs: &mut [Option<Value>; 2]) {
    for m in msgs {
        if let Some(rest) = m.strip_prefix("sideupdate\n") {
            let (side, body) = rest.split_once('\n').unwrap_or((rest, ""));
            let p = if side == "p1" { 0 } else { 1 };
            if let Some(j) = body.strip_prefix("|request|") {
                reqs[p] = Value::parse(j).ok().filter(|v| !matches!(v, Value::Null));
            }
        }
    }
}

const INVALID: [&str; 34] = [
    "move 0", "move 5", "move 9", "move -1", "move 1 2", "move 1 -1", "move 1 +1", "move 1 mega", "move 1 zmove", "move 1 max",
    "move 1 dynamax", "move 1 terastallize", "move 1 ultra", "move 1 megax", "move 1 1 1", "move notamove", "move Body Slam mega max",
    "switch 0", "switch 1", "switch 7", "switch notamon", "switch", "pass", "skip", "pass now", "shift", "shift 1", "team 123",
    "team", "fight", "", "  ", "move 1, move 2", "garbage words",
];

/// One random battle-stream input; the stream text and how many choice lines it has.
pub fn gen_input(seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut lines: Vec<String> = Vec::new();
    let mut s = BattleStream::with_clock(|| 0);
    let mut reqs: [Option<Value>; 2] = [None, None];
    let push = |s: &mut BattleStream, reqs: &mut [Option<Value>; 2], lines: &mut Vec<String>, l: String| -> bool {
        lines.push(l.clone());
        match s.write(&l) {
            Ok(m) => {
                scan_requests(&m, reqs);
                true
            }
            Err(_) => false,
        }
    };
    let mut start = Obj::new();
    let fmt = match rng.below(10) {
        0 => "gen1ou@@@!Sleep Clause Mod,!Freeze Clause Mod",
        1 => "gen1ou@@@!Sleep Clause Mod",
        2 => "[Gen 1] OU",
        _ => "gen1ou",
    };
    start.set("formatid", Value::str(fmt));
    start.set("seed", random_seed(&mut rng));
    if chance(&mut rng, 0.1) {
        start.set("rated", pick(&mut rng, &[Value::Bool(true), Value::str("Rated battle"), Value::str("Tournament battle")]).clone());
    }
    let mut players: Vec<(String, Obj)> = Vec::new();
    for p in ["p1", "p2"] {
        let mut o = Obj::new();
        if chance(&mut rng, 0.9) {
            o.set("name", Value::str(pick(&mut rng, &NAMES)));
        }
        if chance(&mut rng, 0.2) {
            o.set("avatar", pick(&mut rng, &[Value::str("red"), Value::Num(2.0)]).clone());
        }
        if chance(&mut rng, 0.2) {
            o.set("rating", pick(&mut rng, &[Value::Num(1500.0), Value::str("1234")]).clone());
        }
        o.set("team", random_team(&mut rng));
        players.push((p.to_string(), o));
    }
    let inline = chance(&mut rng, 0.1);
    if inline {
        for (p, o) in &players {
            start.set(p, Value::Obj(o.clone()));
        }
    }
    if !push(&mut s, &mut reqs, &mut lines, format!(">start {}", Value::Obj(start).stringify())) {
        return lines.join("\n") + "\n";
    }
    if !inline {
        for (p, o) in &players {
            if chance(&mut rng, 0.05) {
                // A choice before the battle starts.
                push(&mut s, &mut reqs, &mut lines, format!(">{p} move 1"));
            }
            if !push(&mut s, &mut reqs, &mut lines, format!(">player {p} {}", Value::Obj(o.clone()).stringify())) {
                return lines.join("\n") + "\n";
            }
        }
        if chance(&mut rng, 0.05) {
            push(&mut s, &mut reqs, &mut lines, ">player p1 {\"name\":\"Renamed\"}".into());
        }
    }
    let end_style = rng.below(10);
    let max_lines = 400 + rng.below(1200) as usize;
    while !s.ended() && lines.len() < max_lines {
        // Occasional non-choice commands.
        if chance(&mut rng, 0.02) {
            let c = match rng.below(6) {
                0 => format!(">chat {}", pick(&mut rng, &["hello", "gl hf", "|weird|"])),
                1 => ">requestlog".to_string(),
                2 => ">requestexport".to_string(),
                3 => format!(">requestteam p{}", 1 + rng.below(2)),
                4 => ">chat-inputlogonly secret".to_string(),
                _ => format!(">reseed {}", random_seed(&mut rng).to_js_string()),
            };
            push(&mut s, &mut reqs, &mut lines, c);
        }
        if s.battle().is_some_and(|b| b.turn > 3) && chance(&mut rng, 0.01) {
            let c = match end_style {
                0 => ">forcewin p1".to_string(),
                1 => ">forcewin p2".to_string(),
                2 => ">forcetie".to_string(),
                3 => ">forcelose p1".to_string(),
                4 => ">tiebreak".to_string(),
                5 => format!(">forcewin {}", pick(&mut rng, &NAMES)),
                _ => String::new(),
            };
            if !c.is_empty() {
                push(&mut s, &mut reqs, &mut lines, c);
                continue;
            }
        }
        // A choice for a random side.
        let p = rng.below(2) as usize;
        let req = reqs[p].clone();
        let choice = match &req {
            Some(r) if !r.get("wait").is_some_and(Value::truthy) => {
                let maybe_locked = r.get("active").and_then(Value::as_arr).and_then(|a| a.first()).is_some_and(|a| a.get("maybeLocked").is_some());
                if maybe_locked && chance(&mut rng, 0.3) {
                    pick(&mut rng, &["testfight", "undo", "move 1", "testfight"]).to_string()
                } else if chance(&mut rng, 0.15) {
                    pick(&mut rng, &INVALID).to_string()
                } else if chance(&mut rng, 0.05) {
                    pick(&mut rng, &["undo", "default", "testfight", "move", "auto"]).to_string()
                } else {
                    legal_choice(&mut rng, r)
                }
            }
            _ => {
                if chance(&mut rng, 0.05) {
                    "move 1".to_string()
                } else {
                    continue;
                }
            }
        };
        if !push(&mut s, &mut reqs, &mut lines, format!(">p{} {choice}", p + 1)) {
            break;
        }
        if chance(&mut rng, 0.05) {
            push(&mut s, &mut reqs, &mut lines, format!(">p{} undo", p + 1));
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    // Sometimes a trailing partial line (dropped by both).
    if chance(&mut rng, 0.05) {
        text.push_str(">p1 move 1");
    }
    text
}

fn legal_choice(rng: &mut Rng, r: &Value) -> String {
    let side = r.get("side").and_then(|s| s.get("pokemon")).and_then(Value::as_arr).cloned().unwrap_or_default();
    let switches: Vec<(usize, String)> = side
        .iter()
        .enumerate()
        .filter(|(i, m)| *i > 0 && !m.get("condition").and_then(Value::as_str).unwrap_or("").ends_with(" fnt"))
        .map(|(i, m)| {
            let ident = m.get("ident").and_then(Value::as_str).unwrap_or("");
            (i + 1, ident.split_once(": ").map(|x| x.1).unwrap_or("").to_string())
        })
        .collect();
    let by_name = chance(rng, 0.2);
    if r.get("forceSwitch").is_some() {
        if switches.is_empty() {
            return "pass".into();
        }
        let (k, name) = pick(rng, &switches).clone();
        return if by_name { format!("switch {name}") } else { format!("switch {k}") };
    }
    let active = r.get("active").and_then(Value::as_arr).and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    let moves = active.get("moves").and_then(Value::as_arr).cloned().unwrap_or_default();
    let usable: Vec<(usize, String, Option<String>)> = moves
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.get("disabled").is_some_and(Value::truthy))
        .map(|(i, m)| (i + 1, m.get("id").and_then(Value::as_str).unwrap_or("").to_string(), m.get("target").and_then(Value::as_str).map(str::to_string)))
        .collect();
    let trapped = active.get("trapped").is_some_and(Value::truthy);
    if !switches.is_empty() && !trapped && chance(rng, 0.3) {
        let (k, name) = pick(rng, &switches).clone();
        return if by_name { format!("switch {name}") } else { format!("switch {k}") };
    }
    if usable.is_empty() {
        return "move 1".into();
    }
    let (k, id, target) = pick(rng, &usable).clone();
    let mut c = if by_name { format!("move {id}") } else { format!("move {k}") };
    if target.as_deref() == Some("normal") && chance(rng, 0.1) {
        c.push_str(" 1");
    }
    c
}

/// Our output for `input` written in `chunks` (lengths in bytes; `None` = one write per line).
fn ours(input: &str, chunks: &[usize]) -> (String, Option<String>) {
    let mut t = BattleTextStream::with_clock(|| 0);
    let mut out = String::new();
    let mut i = 0;
    for &n in chunks {
        let part = &input[i..i + n];
        i += n;
        match t.write(part) {
            Ok(o) => out.push_str(&o),
            Err(e) => return (out, Some(e)),
        }
    }
    (out, None)
}

fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("|t:|") {
            let digits = rest.trim_end_matches('\n');
            if digits.chars().all(|c| c.is_ascii_digit()) {
                out.push_str("|t:|");
                if line.ends_with('\n') {
                    out.push('\n');
                }
                continue;
            }
        }
        out.push_str(line);
    }
    out
}

/// Chunkings of `input`: per line, whole, random (byte lengths on char boundaries).
fn chunkings(input: &str, rng: &mut Rng) -> Vec<(&'static str, Vec<usize>)> {
    let lines: Vec<usize> = input.split_inclusive('\n').map(str::len).collect();
    let mut random = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let mut n = 1 + rng.below(300) as usize;
        while i + n < input.len() && !input.is_char_boundary(i + n) {
            n += 1;
        }
        n = n.min(input.len() - i);
        random.push(n);
        i += n;
    }
    vec![("lines", lines), ("whole", vec![input.len()]), ("random", random)]
}

/// Protocol features counted in Showdown's output (how much of the protocol a run exercised).
const COVERAGE: [(&str, &str); 16] = [
    ("errors", "|error|[Invalid choice]"),
    ("unavailable", "|error|[Unavailable choice]"),
    ("request-updates", "\"update\":true"),
    ("struggle-activate", "|move: Struggle\n"),
    ("force-switch", "\"forceSwitch\""),
    ("fight", "\"id\":\"fight\""),
    ("recharge", "\"id\":\"recharge\""),
    ("maybe-locked", "\"maybeLocked\":true"),
    ("trapped", "\"trapped\":true"),
    ("win", "|win|"),
    ("tie", "|tie\n"),
    ("requesteddata", "requesteddata\n"),
    ("end", "\nend\n"),
    ("chat", "|chat|"),
    ("reseed", "RNG was reset"),
    ("tiebreak", "Going to tiebreaker"),
];

pub fn run(n: usize, seed: u64, jobs: usize, out_dir: &std::path::Path, sd: &Showdown, record: Option<&std::path::Path>) -> usize {
    std::fs::create_dir_all(out_dir).unwrap();
    let mut cases = Vec::new();
    let save = std::env::var("STREAM_FUZZ_INPUTS").ok().map(std::path::PathBuf::from);
    for i in 0..n {
        let input = gen_input(derive_seed(seed, i as u64));
        if let Some(d) = &save {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join(format!("input{i}.txt")), &input).unwrap();
        }
        let mut rng = Rng::new(derive_seed(seed ^ 0xC0FFEE, i as u64));
        for (mode, ch) in chunkings(&input, &mut rng) {
            cases.push((i, mode, input.clone(), ch));
        }
    }
    // Node: one process per job, cases dealt round-robin.
    let jobs = jobs.max(1);
    let mut results: Vec<Option<(String, Option<String>)>> = vec![None; cases.len()];
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for j in 0..jobs {
            let mine: Vec<usize> = (j..cases.len()).step_by(jobs).collect();
            let cases = &cases;
            let sd = &sd;
            handles.push(scope.spawn(move || {
                let mut child = Command::new(&sd.node)
                    .arg(sd.runner.with_file_name("stream.js"))
                    .env("PS_DIR", &sd.ps_dir)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("node");
                let mut stdin = child.stdin.take().unwrap();
                let lines: Vec<String> = mine
                    .iter()
                    .map(|&k| {
                        let (_, _, input, ch) = &cases[k];
                        // Chunk lengths in UTF-16 units for JS.
                        let mut units = Vec::new();
                        let mut i = 0;
                        for &b in ch {
                            units.push(input[i..i + b].encode_utf16().count());
                            i += b;
                        }
                        let mut o = Obj::new();
                        o.set("id", Value::Num(k as f64));
                        o.set("input", Value::Str(input.clone()));
                        o.set("chunks", Value::Arr(units.into_iter().map(|u| Value::Num(u as f64)).collect()));
                        Value::Obj(o).stringify()
                    })
                    .collect();
                let writer = std::thread::spawn(move || {
                    for l in lines {
                        writeln!(stdin, "{l}").unwrap();
                    }
                });
                let mut got = Vec::new();
                for line in BufReader::new(child.stdout.take().unwrap()).lines() {
                    let v = Value::parse(&line.unwrap()).unwrap();
                    let id = match v.get("id") {
                        Some(Value::Num(x)) => *x as usize,
                        _ => continue,
                    };
                    let out = v.get("out").and_then(Value::as_str).unwrap_or("").to_string();
                    let err = v.get("error").and_then(Value::as_str).map(str::to_string);
                    got.push((id, (out, err)));
                }
                writer.join().unwrap();
                let _ = child.wait();
                got
            }));
        }
        for h in handles {
            for (id, r) in h.join().unwrap() {
                results[id] = Some(r);
            }
        }
    });
    let mut diverged = 0;
    let mut lines_total = 0usize;
    let mut errors = 0;
    let mut by_mode = std::collections::BTreeMap::new();
    let mut ended = 0;
    let mut err_kinds = std::collections::BTreeMap::new();
    let mut recorded: Vec<String> = Vec::new();
    let mut coverage = std::collections::BTreeMap::new();
    let mut errmsgs = std::collections::BTreeMap::new();
    for (k, (i, mode, input, ch)) in cases.iter().enumerate() {
        let Some((theirs, their_err)) = &results[k] else {
            diverged += 1;
            continue;
        };
        let (mine, my_err) = ours(input, ch);
        if *mode == "lines" {
            for (name, pat) in COVERAGE {
                *coverage.entry(name.to_string()).or_insert(0usize) += theirs.matches(pat).count();
            }
            for l in theirs.lines() {
                if let Some(e) = l.strip_prefix("|error|") {
                    let e: String = e.chars().take_while(|c| !c.is_ascii_digit() && *c != '"').collect();
                    *errmsgs.entry(e).or_insert(0usize) += 1;
                }
            }
            lines_total += input.lines().count();
            if theirs.contains("\nend\n") || theirs.starts_with("end\n") {
                ended += 1;
            }
        }
        let same = normalise(theirs) == normalise(&mine) && their_err.is_some() == my_err.is_some();
        if let (Some(_), true, false) = (record, same, *mode == "whole") {
            let mut o = Obj::new();
            o.set("id", Value::Num(*i as f64));
            o.set("mode", Value::str(mode));
            o.set("input", Value::Str(input.clone()));
            o.set("chunks", Value::Arr(ch.iter().map(|&c| Value::Num(c as f64)).collect()));
            o.set("out_hash", Value::Str(format!("{:016x}", fnv64(&normalise(theirs)))));
            o.set("out_len", Value::Num(normalise(theirs).len() as f64));
            o.set("error", Value::Bool(their_err.is_some()));
            recorded.push(Value::Obj(o).stringify());
        }
        by_mode.entry(*mode).or_insert((0, 0)).0 += 1;
        if let Some(e) = their_err {
            errors += 1;
            let key = format!("showdown: {} | kanto: {}", e.lines().next().unwrap_or(""), my_err.as_deref().unwrap_or("-").lines().next().unwrap_or(""));
            *err_kinds.entry(key).or_insert(0usize) += 1;
        }
        if !same {
            diverged += 1;
            by_mode.get_mut(mode).unwrap().1 += 1;
            let f = out_dir.join(format!("case{i}-{mode}"));
            std::fs::write(f.with_extension("in"), input).unwrap();
            std::fs::write(f.with_extension("showdown"), format!("{theirs}\n--- error: {their_err:?}\n")).unwrap();
            std::fs::write(f.with_extension("kanto"), format!("{mine}\n--- error: {my_err:?}\n")).unwrap();
        }
    }
    println!(
        "stream-fuzz: {n} inputs ({lines_total} input lines, {ended} battles ended), {} runs (lines/whole/random chunks): {diverged} differ{}",
        cases.len(),
        if errors > 0 { format!(" ({errors} runs where Showdown threw)") } else { String::new() }
    );
    if let Some(f) = record {
        std::fs::write(f, recorded.join("\n") + "\n").unwrap();
        println!("  recorded {} runs to {}", recorded.len(), f.display());
    }
    for (m, (c, d)) in by_mode {
        println!("  {m:6}: {c} runs, {d} differ");
    }
    for (k, c) in err_kinds {
        println!("  thrown ({c} runs): {k}");
    }
    println!("  coverage (Showdown output, per-line runs): {}", coverage.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", "));
    println!("  distinct |error| messages: {}", errmsgs.len());
    if std::env::var("STREAM_FUZZ_VERBOSE").is_ok() {
        for (e, c) in &errmsgs {
            println!("    {c:6} {e}");
        }
    }
    diverged
}

/// FNV-1a, 64-bit.
pub fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Replay a recorded stream corpus (`stream-fuzz --record`) without Node: (runs, mismatches).
pub fn replay_corpus(path: &std::path::Path) -> (usize, usize) {
    let text = std::fs::read_to_string(path).unwrap();
    let (mut n, mut bad) = (0, 0);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v = Value::parse(line).expect("corpus line");
        let input = v.get("input").and_then(Value::as_str).unwrap();
        let chunks: Vec<usize> = v.get("chunks").and_then(Value::as_arr).unwrap().iter().map(|c| match c {
            Value::Num(x) => *x as usize,
            _ => 0,
        }).collect();
        let (mine, err) = ours(input, &chunks);
        let want = v.get("out_hash").and_then(Value::as_str).unwrap();
        let want_err = v.get("error").is_some_and(Value::truthy);
        n += 1;
        if format!("{:016x}", fnv64(&normalise(&mine))) != want || err.is_some() != want_err {
            bad += 1;
            println!("STREAM REGRESSION {} case {} ({})", path.display(), v.get("id").map(Value::to_js_string).unwrap_or_default(), v.get("mode").and_then(Value::as_str).unwrap_or(""));
        }
    }
    (n, bad)
}
