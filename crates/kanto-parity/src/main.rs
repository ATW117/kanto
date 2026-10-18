//! `parity`: differential testing of the native Gen 1 simulator against Pokémon Showdown.
//!
//! ```text
//! parity fuzz     [--battles N] [--seed S] [--teams mixed|random|legal|randbats|stall|targeted|thaw]
//!                 [--format gen1ou|gen1nc] [--patch vanilla|pkmn] [--prng gen5|sodium|mixed] [--jobs J]
//!                 [--out DIR] [--fixtures DIR] [--corpus FILE] [--dump FILE]
//! parity replay   <failures.jsonl|fixture.json>... [--verbose]
//! parity fixtures [--dir parity/fixtures]
//! parity regen    <corpus.jsonl|fixture.json>...   (re-record fixtures with the current runner)
//! parity mutscan  <dump.jsonl>...                   (Node-free replay of `fuzz --dump` traces)
//! parity shrink   <failures.jsonl>...
//! parity bench    [--battles N] [--seed S]
//! parity stream-fuzz [--battles N] [--seed S] [--jobs J] [--out DIR] [--record FILE]  (BattleStream text protocol vs Showdown)
//! parity teams    [--random N] [--seed S] [--level100]  (Showdown team formats + gen1ou validator)
//! parity diff-pkmn [--battles N] [--teams T] [--compat pkmn|vanilla]
//! ```
//! See docs/SIM_DESIGN.md ("Parity harness").

#[allow(dead_code)]
mod bench;
mod replay;
mod runner;
mod snapshot;
mod spec;
mod streamfuzz;
mod teams;

use kanto_env::rng::Rng;
use replay::{replay, Divergence, Replay, Trace};
use serde::{Deserialize, Serialize};
use spec::{random_spec, Spec, TeamKind};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

struct Args {
    pos: Vec<String>,
    kv: BTreeMap<String, String>,
    flags: Vec<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut a = Args { pos: vec![], kv: BTreeMap::new(), flags: vec![] };
        let mut i = 0;
        while i < raw.len() {
            let s = &raw[i];
            if let Some(k) = s.strip_prefix("--") {
                if let Some((k, v)) = k.split_once('=') {
                    a.kv.insert(k.into(), v.into());
                } else if i + 1 < raw.len() && !raw[i + 1].starts_with("--") && !matches!(k, "verbose" | "shrink" | "level100") {
                    a.kv.insert(k.into(), raw[i + 1].clone());
                    i += 1;
                } else {
                    a.flags.push(k.into());
                }
            } else {
                a.pos.push(s.clone());
            }
            i += 1;
        }
        a
    }
    fn get(&self, k: &str, d: &str) -> String {
        self.kv.get(k).cloned().unwrap_or_else(|| d.to_string())
    }
    fn num(&self, k: &str, d: u64) -> u64 {
        self.kv.get(k).map(|v| v.parse().expect("number")).unwrap_or(d)
    }
    fn flag(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k)
    }
}

fn repo_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.join("../..").canonicalize().unwrap_or(here)
}

/// A failing battle as stored in failures.jsonl / fixtures.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Failure {
    spec: Spec,
    trace: Trace,
    divergence: Option<Divergence>,
    #[serde(default)]
    note: String,
}

#[derive(Default, Serialize)]
struct Report {
    battles: usize,
    showdown_errors: usize,
    matched: usize,
    diverged: usize,
    steps_compared: usize,
    steps_before_divergence_mean: f64,
    divergence_rate: f64,
    categories: Vec<(String, usize, String)>,
    /// Move name -> uses in steps that matched Showdown (verification coverage).
    move_coverage: BTreeMap<String, usize>,
    /// How the compared battles ended: "p1"/"p2"/"tie"/"unfinished", plus "turn-limit" and
    /// "endless-clause" counts, and the longest battle (turns).
    endings: BTreeMap<String, usize>,
    max_turn: u64,
    /// Protocol events (`-start` volatiles, `-status`, `cant` reasons) seen in matched steps.
    event_coverage: BTreeMap<String, usize>,
    kinds: BTreeMap<String, usize>,
    moves_at_divergence: Vec<(String, usize)>,
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = Args::parse(&raw);
    let cmd = args.pos.first().cloned().unwrap_or_default();
    match cmd.as_str() {
        "fuzz" => fuzz(&args),
        "replay" => replay_files(&args),
        "fixtures" => fixtures(&args),
        "shrink" => shrink(&args),
        "mutscan" => mutscan(&args),
        "regen" => regen(&args),
        "proto-corpus" => proto_corpus(&args),
        "stream-fuzz" => {
            let repo = repo_root();
            let sd = runner::Showdown::locate(&repo, args.kv.get("ps-dir").map(|s| s.as_str())).unwrap_or_else(|e| panic!("{e}"));
            let out = PathBuf::from(args.get("out", "parity/out/stream-fuzz"));
            let record = args.kv.get("record").map(PathBuf::from);
            let d = streamfuzz::run(args.num("battles", 200) as usize, args.num("seed", 1), args.num("jobs", 4) as usize, &out, &sd, record.as_deref());
            if d > 0 {
                std::process::exit(1);
            }
        }
        "teams" => {
            let ps = args.kv.get("ps").cloned();
            if let Err(e) = teams::run(&repo_root(), args.num("random", 5000) as usize, args.num("seed", 1), args.flag("level100"), ps.as_deref()) {
                eprintln!("parity teams: {e}");
                std::process::exit(1);
            }
        }
        "bench" => bench::bench(args.num("battles", 2000) as usize, args.num("seed", 1), &args.get("teams", "randbats")),
        #[cfg(feature = "pkmn")]
        "diff-pkmn" => {
            let compat = if args.get("compat", "pkmn") == "vanilla" { kanto_gen1::Compat::Vanilla } else { kanto_gen1::Compat::Pkmn };
            bench::diff_pkmn(args.num("battles", 2000) as usize, args.num("seed", 1), &args.get("teams", "randbats"), compat, args.flag("verbose"));
        }
        #[cfg(not(feature = "pkmn"))]
        "diff-pkmn" => {
            eprintln!("diff-pkmn needs the pkmn feature: cargo run --release -p kanto-parity --features pkmn --bin parity -- diff-pkmn");
            std::process::exit(2);
        }
        _ => {
            eprintln!("usage: parity fuzz|replay|fixtures|bench ... (see crates/kanto-parity/src/main.rs)");
            std::process::exit(2);
        }
    }
}

fn fuzz(args: &Args) {
    let n = args.num("battles", 200) as usize;
    let seed = args.num("seed", 1);
    let kind = TeamKind::parse(&args.get("teams", "mixed")).expect("--teams");
    let format = args.get("format", "gen1ou");
    let patch = args.get("patch", "vanilla");
    let jobs = args.num("jobs", 4) as usize;
    let out = PathBuf::from(args.get("out", "parity/out/fuzz"));
    let keep = args.num("keep", 5) as usize;
    std::fs::create_dir_all(&out).expect("create out dir");
    let repo = repo_root();
    let sd = runner::Showdown::locate(&repo, args.kv.get("ps-dir").map(|s| s.as_str())).unwrap_or_else(|e| panic!("{e}"));
    let mut rng = Rng::new(seed);
    let prng = args.get("prng", "gen5");
    let mut specs: Vec<Spec> = (0..n).map(|i| random_spec(format!("s{seed}-{i}"), &mut rng, kind, &format, &patch, &prng)).collect();
    for s in specs.iter_mut() {
        if let Some(ms) = args.kv.get("max-steps") {
            s.max_steps = Some(ms.parse().expect("--max-steps"));
        }
        s.sim_compat = args.kv.get("sim-compat").cloned();
        if args.flag("proto") {
            s.proto = true;
            // Exercise player names that need care (spaces, quotes, non-ASCII) in some battles.
            let k = rng.below(4);
            if k > 0 {
                let pool = [["Red", "Blue"], ["Ash Ketchum", "Gary \"Oak\""], ["Rival_99", "Pokémon Trainer"]];
                let p = pool[k as usize - 1];
                s.names = Some([p[0].to_string(), p[1].to_string()]);
            }
            // Nicknames for some Pokémon (incl. one longer than Showdown's 20-character cap and
            // one equal to the species name, which Showdown ignores).
            let nicks = ["Bessie", "Mr. Sparkles", "A Very Long Nickname Indeed", "Ω-Zero", "Fluffy"];
            for team in [&mut s.p1, &mut s.p2] {
                for m in team.iter_mut() {
                    match rng.below(6) {
                        0 => m.name = Some(nicks[rng.below(nicks.len() as u64) as usize].to_string()),
                        1 => m.name = Some(m.species.clone()),
                        _ => {}
                    }
                }
            }
        }
    }
    let started = std::time::Instant::now();
    let mut report = Report { battles: n, ..Default::default() };
    let mut failures: BTreeMap<String, Vec<Failure>> = BTreeMap::new();
    let mut cat_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut move_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut errors = Vec::new();
    let mut done = 0;
    let mut steps_before = 0usize;
    // Regression corpus: matched battles (truncated) that cover moves not yet covered.
    let corpus_dir = args.kv.get("corpus").map(PathBuf::from);
    let corpus_steps = args.num("corpus-steps", 1000) as usize;
    let corpus_min = args.num("corpus-min", 0) as usize;
    let mut corpus_lines = String::new();
    let mut corpus_moves = std::collections::BTreeSet::new();
    let mut corpus_n = 0;
    // `--dump FILE`: every Showdown trace (spec + trace) as JSON lines, for Node-free replays
    // (`parity mutscan`, parity/mutate.py).
    let mut dump = args.kv.get("dump").map(|p| {
        if let Some(parent) = Path::new(p).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::io::BufWriter::new(std::fs::File::create(p).unwrap_or_else(|e| panic!("{p}: {e}")))
    });
    sd.run(&specs, jobs, |i, line| {
        done += 1;
        let trace: Trace = match serde_json::from_str(&line) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("{}: bad trace: {e}", specs[i].id));
                report.showdown_errors += 1;
                return;
            }
        };
        if let Some(e) = &trace.error {
            report.showdown_errors += 1;
            errors.push(format!("{}: {}", specs[i].id, e.lines().next().unwrap_or("")));
            return;
        }
        let res = replay(&specs[i], &trace);
        if let Some(d) = dump.as_mut() {
            use std::io::Write;
            let f = Failure { spec: specs[i].clone(), trace: trace.clone(), divergence: None, note: String::new() };
            writeln!(d, "{}", serde_json::to_string(&f).unwrap()).unwrap();
        }
        if let Some(end) = &trace.end {
            let w = end.get("winner").and_then(|w| w.as_str()).unwrap_or("");
            let ended = end.get("ended").and_then(|e| e.as_bool()).unwrap_or(false);
            let key = if !ended { "unfinished" } else if w.is_empty() { "tie" } else { w };
            *report.endings.entry(key.to_string()).or_default() += 1;
            let turn = end.get("turn").and_then(|t| t.as_u64()).unwrap_or(0);
            report.max_turn = report.max_turn.max(turn);
            if let Some(last) = trace.steps.last() {
                if last.log.iter().any(|l| l.contains("turn limit")) {
                    *report.endings.entry("turn-limit".into()).or_default() += 1;
                }
                if last.log.iter().any(|l| l.contains("Endless Battle Clause activated")) {
                    *report.endings.entry("endless-clause".into()).or_default() += 1;
                }
            }
        }
        let upto = match &res {
            Replay::Match { steps } => *steps,
            Replay::Diverged(d) => d.step,
        };
        for st in &trace.steps[..upto.min(trace.steps.len())] {
            for l in &st.log {
                let parts: Vec<&str> = l.split('|').collect();
                if parts.len() < 3 {
                    continue;
                }
                match parts[1] {
                    "move" if parts.len() >= 4 => *report.move_coverage.entry(parts[3].to_string()).or_default() += 1,
                    "-start" | "-status" | "cant" | "-activate" if parts.len() >= 4 => {
                        let what = parts[3].trim_start_matches("move: ");
                        *report.event_coverage.entry(format!("{} {}", parts[1], what)).or_default() += 1;
                    }
                    "-crit" | "-miss" | "-immune" | "-fail" | "-hitcount" | "-ohko" | "-transform" | "-mustrecharge" | "-curestatus" | "-end" | "-boost" | "-unboost" | "faint" => {
                        *report.event_coverage.entry(parts[1].to_string()).or_default() += 1
                    }
                    _ => {}
                }
            }
        }
        if let (Some(file), Replay::Match { .. }) = (&corpus_dir, &res) {
            let upto = corpus_steps.min(trace.steps.len());
            let mut new = false;
            for st in &trace.steps[..upto] {
                for l in &st.log {
                    let parts: Vec<&str> = l.split('|').collect();
                    if parts.len() >= 4 && parts[1] == "move" && corpus_moves.insert(parts[3].to_string()) {
                        new = true;
                    }
                }
            }
            if new || corpus_n < corpus_min {
                let c = replay::Compact::from_trace(&specs[i], &trace, upto);
                corpus_lines.push_str(&serde_json::to_string(&c).unwrap());
                corpus_lines.push('\n');
                let _ = file;
                corpus_n += 1;
            }
        }
        match res {
            Replay::Match { steps } => {
                report.matched += 1;
                report.steps_compared += steps;
            }
            Replay::Diverged(d) => {
                report.diverged += 1;
                report.steps_compared += d.step;
                steps_before += d.step;
                *report.kinds.entry(format!("{:?}", d.kind)).or_default() += 1;
                *cat_count.entry(d.category.clone()).or_default() += 1;
                for m in &d.moves {
                    *move_count.entry(m.clone()).or_default() += 1;
                }
                let list = failures.entry(d.category.clone()).or_default();
                if list.len() < keep {
                    let mut t = trace.clone();
                    t.steps.truncate(d.step + 1);
                    list.push(Failure { spec: specs[i].clone(), trace: t, divergence: Some(*d), note: String::new() });
                } else if let Some(worst) = list.iter_mut().max_by_key(|f| f.divergence.as_ref().map(|d| d.step).unwrap_or(0)) {
                    // Keep the earliest-diverging examples.
                    if worst.divergence.as_ref().map(|w| w.step).unwrap_or(0) > d.step {
                        let mut t = trace.clone();
                        t.steps.truncate(d.step + 1);
                        *worst = Failure { spec: specs[i].clone(), trace: t, divergence: Some(*d), note: String::new() };
                    }
                }
            }
        }
        if done % 100 == 0 {
            eprintln!("  {done}/{n} battles, {} diverged, {:.0}s", report.diverged, started.elapsed().as_secs_f64());
        }
    })
    .unwrap_or_else(|e| panic!("{e}"));
    let compared = report.matched + report.diverged;
    report.divergence_rate = if compared > 0 { report.diverged as f64 / compared as f64 } else { 0.0 };
    report.steps_before_divergence_mean = if report.diverged > 0 { steps_before as f64 / report.diverged as f64 } else { 0.0 };
    let mut cats: Vec<(String, usize)> = cat_count.into_iter().collect();
    cats.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    report.categories = cats
        .iter()
        .map(|(c, k)| (c.clone(), *k, failures.get(c).and_then(|f| f.first()).map(|f| f.spec.id.clone()).unwrap_or_default()))
        .collect();
    let mut moves: Vec<(String, usize)> = move_count.into_iter().collect();
    moves.sort_by(|a, b| b.1.cmp(&a.1));
    moves.truncate(30);
    report.moves_at_divergence = moves;
    // Outputs.
    let mut f = String::new();
    for list in failures.values() {
        for x in list {
            f.push_str(&serde_json::to_string(x).unwrap());
            f.push('\n');
        }
    }
    std::fs::write(out.join("failures.jsonl"), f).unwrap();
    std::fs::write(out.join("report.json"), serde_json::to_string_pretty(&report).unwrap()).unwrap();
    std::fs::write(out.join("showdown_errors.txt"), errors.join("\n")).unwrap();
    print_report(&report, started.elapsed().as_secs_f64(), &format, &patch, &args.get("teams", "mixed"));
    if let Some(file) = &corpus_dir {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(file, corpus_lines).unwrap();
        eprintln!("saved {corpus_n} corpus battles covering {} moves to {}", corpus_moves.len(), file.display());
    }
    if let Some(dir) = args.kv.get("fixtures") {
        save_fixtures(Path::new(dir), &failures);
    }
}

fn print_report(r: &Report, secs: f64, format: &str, patch: &str, teams: &str) {
    println!("== parity fuzz: {} battles ({format}, {patch}, teams={teams}) in {secs:.1}s", r.battles);
    println!(
        "compared {} battles / {} steps; matched {}; diverged {} ({:.2}%); showdown errors {}",
        r.matched + r.diverged,
        r.steps_compared,
        r.matched,
        r.diverged,
        100.0 * r.divergence_rate,
        r.showdown_errors
    );
    println!("endings: {:?}; longest battle {} turns", r.endings, r.max_turn);
    let mut cov: Vec<(&String, &usize)> = r.move_coverage.iter().collect();
    cov.sort_by(|a, b| a.1.cmp(b.1));
    println!("move coverage: {} distinct moves verified; rarest: {}", cov.len(),
        cov.iter().take(8).map(|(m, k)| format!("{m} {k}")).collect::<Vec<_>>().join(", "));
    if r.diverged > 0 {
        println!("mean steps before divergence: {:.1}; by kind: {:?}", r.steps_before_divergence_mean, r.kinds);
        println!("top categories:");
        for (c, k, ex) in r.categories.iter().take(20) {
            println!("  {k:5}  {c}   (e.g. {ex})");
        }
        println!("moves/cants in divergent steps:");
        let s: Vec<String> = r.moves_at_divergence.iter().take(20).map(|(m, k)| format!("{m} {k}")).collect();
        println!("  {}", s.join(", "));
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(60).collect()
}

/// Save the earliest-diverging failure of each category as a fixture (choices made explicit).
fn save_fixtures(dir: &Path, failures: &BTreeMap<String, Vec<Failure>>) {
    let open = dir.join("open");
    std::fs::create_dir_all(&open).unwrap();
    let mut n = 0;
    for (cat, list) in failures {
        let Some(best) = list.iter().min_by_key(|f| f.divergence.as_ref().map(|d| d.step).unwrap_or(usize::MAX)) else { continue };
        let mut f = best.clone();
        f.spec.choices = Some(f.trace.steps.iter().skip(1).map(|s| s.choices.clone()).collect());
        f.note = format!("category: {cat}");
        let name = format!("{}-{}.json", slug(cat), slug(&f.spec.id));
        std::fs::write(open.join(name), serde_json::to_string_pretty(&f).unwrap()).unwrap();
        n += 1;
    }
    eprintln!("saved {n} fixtures to {}", open.display());
}

fn load_failures(path: &Path) -> Vec<Failure> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if path.extension().is_some_and(|e| e == "json") {
        vec![serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))]
    } else {
        text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).expect("failure line")).collect()
    }
}

fn replay_files(args: &Args) {
    let verbose = args.flag("verbose");
    for p in &args.pos[1..] {
        for f in load_failures(Path::new(p)) {
            match replay(&f.spec, &f.trace) {
                Replay::Match { steps } => println!("{}: MATCH ({steps} steps)", f.spec.id),
                Replay::Diverged(d) => {
                    println!("{}: DIVERGED at step {} (turn {}): {}  moves {:?}", d.id, d.step, d.turn, d.category, d.moves);
                    if verbose {
                        println!("{}", d.details);
                    }
                }
            }
        }
    }
}

/// `parity proto-corpus DUMP.jsonl --out FILE [--n N] [--steps S]`: turn traces recorded with
/// `fuzz --proto --dump` into a Node-free protocol corpus (spec, choices, per-step state and
/// protocol-output hashes), replayed by `parity fixtures` and `cargo test`.
fn proto_corpus(args: &Args) {
    let out = args.kv.get("out").expect("--out FILE");
    let n = args.num("n", 200) as usize;
    let max_steps = args.num("steps", 400) as usize;
    let mut lines = String::new();
    let mut k = 0;
    for p in &args.pos[1..] {
        for f in load_failures(Path::new(p)) {
            if k >= n || f.divergence.is_some() || f.trace.error.is_some() {
                continue;
            }
            if !matches!(replay(&f.spec, &f.trace), Replay::Match { .. }) {
                continue;
            }
            let upto = f.trace.steps.len().min(max_steps);
            let c = replay::Compact::from_trace(&f.spec, &f.trace, upto);
            if c.proto.is_none() {
                continue;
            }
            lines.push_str(&serde_json::to_string(&c).unwrap());
            lines.push('\n');
            k += 1;
        }
    }
    std::fs::write(out, lines).unwrap_or_else(|e| panic!("{out}: {e}"));
    println!("wrote {k} battles to {out}");
}

/// Replay every fixture: `passing/` must match, `open/` are known divergences.
fn fixtures(args: &Args) {
    let dir = PathBuf::from(args.get("dir", &repo_root().join("parity/fixtures").to_string_lossy()));
    let mut bad = 0;
    for sub in ["passing", "divergent", "open"] {
        let d = dir.join(sub);
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
        files.sort();
        let (mut ok, mut div) = (0, 0);
        for p in &files {
            for f in load_failures(p) {
                match replay(&f.spec, &f.trace) {
                    Replay::Match { .. } => {
                        ok += 1;
                        if sub == "divergent" {
                            bad += 1;
                            println!("HARNESS REGRESSION {}: expected a divergence", p.display());
                        }
                    }
                    Replay::Diverged(d) => {
                        div += 1;
                        if sub == "passing" {
                            bad += 1;
                            println!("REGRESSION {}: {}", p.display(), d.category);
                        } else if sub == "divergent" && f.divergence.as_ref().is_some_and(|e| e.category != d.category) {
                            bad += 1;
                            println!("HARNESS REGRESSION {}: category {} != {}", p.display(), d.category, f.divergence.as_ref().unwrap().category);
                        } else if args.flag("verbose") {
                            println!("{sub} {}: step {} {}", p.file_name().unwrap().to_string_lossy(), d.step, d.category);
                        }
                    }
                }
            }
        }
        println!("{sub}: {ok} match, {div} diverge ({} files)", files.len());
    }
    // BattleStream text-protocol corpora (`stream-fuzz --record`).
    if let Ok(rd) = std::fs::read_dir(dir.join("stream")) {
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect();
        files.sort();
        for p in files {
            let (n, b) = streamfuzz::replay_corpus(&p);
            bad += b;
            println!("stream/{}: {n} runs, {} match", p.file_name().unwrap().to_string_lossy(), n - b);
        }
    }
    // Compact corpora (Node-free regression replays).
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect();
        files.sort();
        for p in files {
            let text = std::fs::read_to_string(&p).unwrap();
            let (mut ok, mut steps) = (0, 0);
            for (k, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
                let c: replay::Compact = serde_json::from_str(line).unwrap_or_else(|e| panic!("{}:{}: {e}", p.display(), k + 1));
                steps += c.steps.len();
                match c.check() {
                    Ok(()) => ok += 1,
                    Err((step, what)) => {
                        bad += 1;
                        println!("REGRESSION {}:{} ({}): step {step}: {what}", p.display(), k + 1, c.spec.id);
                    }
                }
            }
            println!("{}: {ok} battles / {steps} steps match", p.file_name().unwrap().to_string_lossy());
        }
    }
    if bad > 0 {
        std::process::exit(1);
    }
}

/// `parity regen FILE...`: re-record fixtures with the current runner (after the snapshot format
/// changes, the stored state hashes / traces are stale). Each battle's spec, with its explicit
/// choices, is replayed by Showdown; the native sim must still match it (`corpus-*.jsonl`,
/// `passing/`) or still diverge with the recorded category (`divergent/`, `open/`). Files are
/// rewritten in place; nothing is written if any battle fails these checks.
fn regen(args: &Args) {
    let repo = repo_root();
    let sd = runner::Showdown::locate(&repo, args.kv.get("ps-dir").map(|s| s.as_str())).unwrap_or_else(|e| panic!("{e}"));
    let jobs = args.num("jobs", 4) as usize;
    let run = |specs: &[Spec]| -> Vec<Trace> {
        let mut out: Vec<Option<Trace>> = vec![None; specs.len()];
        sd.run(specs, jobs, |i, line| {
            let t: Trace = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{}: bad trace: {e}", specs[i].id));
            if let Some(e) = &t.error {
                panic!("{}: Showdown error: {e}", specs[i].id);
            }
            out[i] = Some(t);
        })
        .unwrap_or_else(|e| panic!("{e}"));
        out.into_iter().map(|t| t.expect("missing trace")).collect()
    };
    let mut bad = 0;
    for path in &args.pos[1..] {
        let path = Path::new(path);
        if path.extension().is_some_and(|e| e == "jsonl") {
            let text = std::fs::read_to_string(path).unwrap();
            let olds: Vec<replay::Compact> = text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect();
            let specs: Vec<Spec> = olds
                .iter()
                .map(|c| {
                    let mut s = c.spec.clone();
                    s.max_steps = Some(c.steps.len() as u32 - 1);
                    s
                })
                .collect();
            let traces = run(&specs);
            let mut lines = String::new();
            for ((c, s), t) in olds.iter().zip(&specs).zip(&traces) {
                if t.steps.len() != c.steps.len() {
                    bad += 1;
                    println!("{}: {}: {} steps recorded, expected {}", path.display(), s.id, t.steps.len(), c.steps.len());
                    continue;
                }
                if let Replay::Diverged(d) = replay(s, t) {
                    bad += 1;
                    println!("{}: {}: diverged at step {}: {}", path.display(), s.id, d.step, d.category);
                    continue;
                }
                let mut spec = c.spec.clone();
                spec.max_steps = c.spec.max_steps;
                let mut new = replay::Compact::from_trace(&spec, t, c.steps.len());
                new.spec.max_steps = c.spec.max_steps;
                lines.push_str(&serde_json::to_string(&new).unwrap());
                lines.push('\n');
            }
            if bad == 0 {
                std::fs::write(path, lines).unwrap();
                println!("{}: {} battles re-recorded", path.display(), olds.len());
            }
        } else {
            let old = load_failures(path).remove(0);
            let mut spec = old.spec.clone();
            let n = spec.choices.as_ref().map(|c| c.len()).unwrap_or(0);
            spec.max_steps = Some(n as u32);
            let trace = run(std::slice::from_ref(&spec)).remove(0);
            let res = replay(&old.spec, &trace);
            let want = old.divergence.as_ref().map(|d| d.category.clone());
            let mut new = old.clone();
            match (res, want) {
                (Replay::Match { .. }, None) => new.trace = trace,
                (Replay::Diverged(d), Some(w)) if d.category == w => {
                    let mut t = trace;
                    t.steps.truncate(d.step + 1);
                    new.trace = t;
                    new.divergence = Some(*d);
                }
                (r, w) => {
                    bad += 1;
                    let got = match r {
                        Replay::Match { .. } => "match".to_string(),
                        Replay::Diverged(d) => d.category,
                    };
                    println!("{}: expected {:?}, got {got}", path.display(), w);
                    continue;
                }
            }
            std::fs::write(path, serde_json::to_string_pretty(&new).unwrap()).unwrap();
            println!("{}: re-recorded", path.display());
        }
    }
    if bad > 0 {
        std::process::exit(1);
    }
}

/// `parity mutscan DUMP...`: replay recorded Showdown traces (`fuzz --dump`) without Node and
/// print one summary line: battles, diverged, the first divergence and the top categories. Used
/// by parity/mutate.py to decide whether a source mutant is caught.
fn mutscan(args: &Args) {
    use std::io::BufRead;
    let mut first: Option<(usize, String)> = None;
    let (mut n, mut div) = (0usize, 0usize);
    let mut cats: BTreeMap<String, usize> = BTreeMap::new();
    for p in &args.pos[1..] {
        let f = std::fs::File::open(p).unwrap_or_else(|e| panic!("{p}: {e}"));
        for line in std::io::BufReader::new(f).lines() {
            let line = line.unwrap();
            if line.trim().is_empty() {
                continue;
            }
            let f: Failure = serde_json::from_str(&line).expect("dump line");
            if let Replay::Diverged(d) = replay(&f.spec, &f.trace) {
                div += 1;
                *cats.entry(d.category.clone()).or_default() += 1;
                if first.is_none() {
                    first = Some((n + 1, format!("{} step {}", d.category, d.step)));
                }
            }
            n += 1;
        }
    }
    let mut c: Vec<_> = cats.into_iter().collect();
    c.sort_by(|a, b| b.1.cmp(&a.1));
    c.truncate(3);
    let first = first.map(|(i, s)| format!("#{i} {s}")).unwrap_or_else(|| "-".into());
    println!("{}", serde_json::json!({"battles": n, "diverged": div, "first": first, "top": c}));
}

/// Minimise failing battles: for each failure, search random battles between only the Pokémon
/// involved in the divergent step (their full sets) and keep the earliest-diverging one with the
/// same category; save it (choices made explicit, trace truncated) as a fixture.
fn shrink(args: &Args) {
    let attempts = args.num("attempts", 200) as usize;
    let jobs = args.num("jobs", 4) as usize;
    let out = PathBuf::from(args.get("out", &repo_root().join("parity/fixtures/open").to_string_lossy()));
    std::fs::create_dir_all(&out).unwrap();
    let repo = repo_root();
    let sd = runner::Showdown::locate(&repo, args.kv.get("ps-dir").map(|s| s.as_str())).unwrap_or_else(|e| panic!("{e}"));
    let mut seen = std::collections::BTreeSet::new();
    let mut rng = Rng::new(args.num("seed", 1));
    for path in &args.pos[1..] {
        for f in load_failures(Path::new(path)) {
            let Some(d) = f.divergence.clone() else { continue };
            if !seen.insert(d.category.clone()) {
                continue;
            }
            // Pokémon involved: actives before and after the divergent step.
            let mut involved: [Vec<usize>; 2] = [vec![], vec![]];
            for st in [d.step.saturating_sub(1), d.step] {
                let Some(step) = f.trace.steps.get(st) else { continue };
                for p in 0..2 {
                    if let Some(slot) = step.state["sides"][p]["active"]["slot"].as_u64() {
                        if !involved[p].contains(&(slot as usize)) {
                            involved[p].push(slot as usize);
                        }
                    }
                }
            }
            let teams = [&f.spec.p1, &f.spec.p2];
            let reduced: Vec<Vec<spec::SetJson>> = (0..2).map(|p| involved[p].iter().map(|&i| teams[p][i].clone()).collect()).collect();
            let specs: Vec<Spec> = (0..attempts)
                .map(|k| {
                    let mut s = f.spec.clone();
                    s.id = format!("{}-shrink{k}", f.spec.id);
                    s.p1 = reduced[0].clone();
                    s.p2 = reduced[1].clone();
                    s.seed = spec::seed_string(rng.next_u64());
                    s.choice_seed = rng.next_u64() as u32;
                    s.choices = None;
                    s
                })
                .collect();
            let mut best: Failure = f.clone();
            let mut best_step = d.step;
            let mut best_size = f.spec.p1.len() + f.spec.p2.len();
            sd.run(&specs, jobs, |i, line| {
                let Ok(trace) = serde_json::from_str::<Trace>(&line) else { return };
                if trace.error.is_some() {
                    return;
                }
                if let Replay::Diverged(nd) = replay(&specs[i], &trace) {
                    let size = specs[i].p1.len() + specs[i].p2.len();
                    if nd.category == d.category && (nd.step, size) < (best_step, best_size) {
                        best_step = nd.step;
                        best_size = size;
                        let mut t = trace.clone();
                        t.steps.truncate(nd.step + 1);
                        best = Failure { spec: specs[i].clone(), trace: t, divergence: Some(*nd), note: String::new() };
                    }
                }
            })
            .unwrap_or_else(|e| panic!("{e}"));
            best.trace.steps.truncate(best_step + 1);
            best.spec.choices = Some(best.trace.steps.iter().skip(1).map(|s| s.choices.clone()).collect());
            best.note = format!("category: {}; shrunk from {} (step {}) to {}+{} Pokémon, step {}", d.category, f.spec.id, d.step, best.spec.p1.len(), best.spec.p2.len(), best_step);
            println!("{}", best.note);
            let name = format!("{}-{}.json", slug(&d.category), slug(&best.spec.id));
            std::fs::write(out.join(name), serde_json::to_string_pretty(&best).unwrap()).unwrap();
        }
    }
}
