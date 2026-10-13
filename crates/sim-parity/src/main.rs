//! `parity`: differential testing of the native Gen 1 simulator against Pokémon Showdown.
//!
//! ```text
//! parity fuzz     [--battles N] [--seed S] [--teams mixed|random|legal|randbats] [--format gen1ou|gen1nc]
//!                 [--patch vanilla|pkmn] [--jobs J] [--out DIR] [--fixtures DIR] [--shrink]
//! parity replay   <failures.jsonl|fixture.json>... [--verbose]
//! parity fixtures [--dir parity/fixtures]
//! parity bench    [--battles N] [--seed S]
//! ```
//! See docs/SIM_DESIGN.md ("Parity harness").

mod bench;
mod replay;
mod runner;
mod snapshot;
mod spec;

use battle_env::rng::Rng;
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
                } else if i + 1 < raw.len() && !raw[i + 1].starts_with("--") && !matches!(k, "verbose" | "shrink") {
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
        "bench" => bench::bench(args.num("battles", 2000) as usize, args.num("seed", 1), &args.get("teams", "randbats")),
        _ => {
            eprintln!("usage: parity fuzz|replay|fixtures|bench ... (see crates/sim-parity/src/main.rs)");
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
    let specs: Vec<Spec> = (0..n).map(|i| random_spec(format!("s{seed}-{i}"), &mut rng, kind, &format, &patch)).collect();
    let started = std::time::Instant::now();
    let mut report = Report { battles: n, ..Default::default() };
    let mut failures: BTreeMap<String, Vec<Failure>> = BTreeMap::new();
    let mut cat_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut move_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut errors = Vec::new();
    let mut done = 0;
    let mut steps_before = 0usize;
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
        match replay(&specs[i], &trace) {
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

/// Replay every fixture: `passing/` must match, `open/` are known divergences.
fn fixtures(args: &Args) {
    let dir = PathBuf::from(args.get("dir", &repo_root().join("parity/fixtures").to_string_lossy()));
    let mut bad = 0;
    for sub in ["passing", "open"] {
        let d = dir.join(sub);
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
        files.sort();
        let (mut ok, mut div) = (0, 0);
        for p in &files {
            for f in load_failures(p) {
                match replay(&f.spec, &f.trace) {
                    Replay::Match { .. } => ok += 1,
                    Replay::Diverged(d) => {
                        div += 1;
                        if sub == "passing" {
                            bad += 1;
                            println!("REGRESSION {}: {}", p.display(), d.category);
                        } else if args.flag("verbose") {
                            println!("open {}: step {} {}", p.file_name().unwrap().to_string_lossy(), d.step, d.category);
                        }
                    }
                }
            }
        }
        println!("{sub}: {ok} match, {div} diverge ({} files)", files.len());
    }
    if bad > 0 {
        std::process::exit(1);
    }
}
