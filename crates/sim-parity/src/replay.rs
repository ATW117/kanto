//! Replay a Showdown trace through the native simulator and report the first divergence.

use crate::snapshot::{diff, snapshot};
use crate::spec::Spec;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sim_core::trace;
use sim_gen1::{Battle, Choice};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Step {
    pub choices: [String; 2],
    pub legal: [Vec<String>; 2],
    #[serde(default)]
    pub requests: [String; 2],
    #[serde(default)]
    pub log: Vec<String>,
    #[serde(default)]
    pub rng: Vec<(String, u32, u32, u32)>,
    pub state: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trace {
    pub id: String,
    #[serde(default)]
    pub seed: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub end: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Kind {
    /// Our simulator panicked.
    Panic,
    /// The legal choice lists differ.
    Legal,
    /// The PRNG call sequence differs.
    Rng,
    /// The state differs (with identical PRNG calls).
    State,
    /// The public event stream the env's reveal memory consumes (moves used with `[from]`,
    /// switches, `cant` reasons) differs from Showdown's protocol log.
    Events,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Divergence {
    pub id: String,
    pub step: usize,
    pub turn: u64,
    pub kind: Kind,
    /// Coarse bucket for the report.
    pub category: String,
    /// Moves used and `cant`s in the divergent step (from Showdown's log).
    pub moves: Vec<String>,
    pub details: String,
}

/// Result of replaying one battle.
pub enum Replay {
    Match { steps: usize },
    Diverged(Box<Divergence>),
}

fn short_label(l: &str) -> String {
    // "BattleActions.getDamage@scripts.js:694 < BattleActions.moveHit@scripts.js:515" -> "getDamage<moveHit"
    l.split(" < ")
        .take(2)
        .map(|f| {
            let f = f.split('@').next().unwrap_or(f);
            f.rsplit('.').next().unwrap_or(f).to_string()
        })
        .collect::<Vec<_>>()
        .join("<")
}

fn normalize_path(p: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for c in p.chars() {
        if c == '[' {
            out.push_str("[]");
            skip = true;
        } else if c == ']' {
            skip = false;
        } else if !skip {
            out.push(c);
        }
    }
    // Keep the volatile/field name but drop per-team-slot detail.
    out
}

fn step_moves(log: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for l in log {
        let parts: Vec<&str> = l.split('|').collect();
        if parts.len() >= 4 && parts[1] == "move" {
            let s = parts[3].to_string();
            if !out.contains(&s) {
                out.push(s);
            }
        } else if parts.len() >= 4 && parts[1] == "cant" {
            let s = format!("cant:{}", parts[3]);
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

fn choice_strings(b: &Battle, p: usize) -> Vec<String> {
    let mut buf = [Choice::Pass; 16];
    let n = b.legal_choices(p, &mut buf);
    buf[..n].iter().map(|c| c.to_showdown()).collect()
}

/// Replay `trace` (produced by the runner for `spec`).
pub fn replay(spec: &Spec, trace: &Trace) -> Replay {
    let (t1, t2) = match spec.teams() {
        Ok(t) => t,
        Err(e) => {
            return Replay::Diverged(Box::new(Divergence {
                id: spec.id.clone(),
                step: 0,
                turn: 0,
                kind: Kind::Panic,
                category: "spec".into(),
                moves: vec![],
                details: e,
            }))
        }
    };
    let mut b = Battle::new(&t1, &t2, spec.prng(), spec.rules());
    trace::start();
    let diverge = |b: &Battle, step: usize, kind: Kind, category: String, details: String, st: &Step| {
        Replay::Diverged(Box::new(Divergence {
            id: spec.id.clone(),
            step,
            turn: b.turn as u64,
            kind,
            category,
            moves: step_moves(&st.log),
            details,
        }))
    };
    for (i, st) in trace.steps.iter().enumerate() {
        if i > 0 {
            // Legal choices before the step.
            for p in 0..2 {
                let ours = choice_strings(&b, p);
                if ours != st.legal[p] {
                    let d = format!("p{} legal: showdown {:?} ours {:?}\nrequest {:?}", p + 1, st.legal[p], ours, st.requests[p]);
                    let cat = format!("legal: {} vs {}", st.legal[p].len(), ours.len());
                    trace::stop();
                    return diverge(&b, i, Kind::Legal, cat, d, st);
                }
            }
        }
        let c = [Choice::parse(&st.choices[0]).unwrap_or_default(), Choice::parse(&st.choices[1]).unwrap_or_default()];
        let mut nb = b;
        let mut events: Vec<sim_core::Event> = Vec::new();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            nb.apply_choices(c[0], c[1], &mut events);
            nb
        }));
        match res {
            Ok(x) => b = x,
            Err(e) => {
                let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                trace::stop();
                let cat = format!("panic: {}", msg.split(':').next().unwrap_or(""));
                return diverge(&b, i, Kind::Panic, cat, format!("panic: {msg}\nchoices {:?}\nlog:\n{}", st.choices, st.log.join("\n")), st);
            }
        }
        let ours_rng = trace::take();
        // PRNG calls.
        let n = st.rng.len().max(ours_rng.len());
        let mut first = None;
        for k in 0..n {
            let a = st.rng.get(k).map(|r| (r.1, r.2, r.3));
            let o = ours_rng.get(k).map(|r| (r.lo, r.hi, r.value));
            if a != o {
                first = Some(k);
                break;
            }
        }
        if let Some(k) = first {
            let sd = st.rng.get(k).map(|r| short_label(&r.0)).unwrap_or_else(|| "-".into());
            let ou = ours_rng.get(k).map(|r| r.label.to_string()).unwrap_or_else(|| "-".into());
            let cat = format!("rng: showdown[{sd}] ours[{ou}]");
            let mut d = format!("choices {:?}\nfirst PRNG mismatch at call #{k}\n", st.choices);
            for j in k.saturating_sub(3)..(k + 6).min(n) {
                let a = st.rng.get(j).map(|r| format!("{}({},{})={}", r.0, r.1, r.2, r.3)).unwrap_or_else(|| "-".into());
                let o = ours_rng.get(j).map(|r| format!("{}({},{})={}", r.label, r.lo, r.hi, r.value)).unwrap_or_else(|| "-".into());
                d.push_str(&format!("  #{j}{} sd: {a}\n       ours: {o}\n", if j == k { "*" } else { " " }));
            }
            d.push_str(&format!("log:\n{}\n", st.log.join("\n")));
            trace::stop();
            return diverge(&b, i, Kind::Rng, cat, d, st);
        }
        let (sd_ev, our_ev) = (log_events(&st.log), sim_events(&events));
        if sd_ev != our_ev {
            let k = sd_ev.iter().zip(&our_ev).position(|(a, b)| a != b).unwrap_or(sd_ev.len().min(our_ev.len()));
            let cat = format!("events: showdown {} ours {}", sd_ev.get(k).map(|s| s.as_str()).unwrap_or("-"), our_ev.get(k).map(|s| s.as_str()).unwrap_or("-"));
            let d = format!("choices {:?}\nshowdown {sd_ev:?}\nours     {our_ev:?}\nlog:\n{}\n", st.choices, st.log.join("\n"));
            trace::stop();
            return diverge(&b, i, Kind::Events, cat, d, st);
        }
        let ours = snapshot(&b);
        let mut diffs = Vec::new();
        diff(&st.state, &ours, &mut String::new(), &mut diffs);
        if !diffs.is_empty() {
            let cat = format!("state: {}", normalize_path(&diffs[0].0));
            let mut d = format!("choices {:?}\n", st.choices);
            for (p, a, o) in diffs.iter().take(16) {
                d.push_str(&format!("  {p}: showdown {a} ours {o}\n"));
            }
            d.push_str(&format!("log:\n{}\n", st.log.join("\n")));
            trace::stop();
            return diverge(&b, i, Kind::State, cat, d, st);
        }
    }
    trace::stop();
    Replay::Match { steps: trace.steps.len() }
}

// ---- Compact regression corpus ------------------------------------------------------------------

/// FNV-1a over bytes.
fn fnv(h: u64, bytes: &[u8]) -> u64 {
    let mut h = h;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

/// Hash of a state snapshot in canonical form (object keys sorted, as serde_json's default map).
pub fn state_hash(v: &Value) -> u64 {
    fnv(0xcbf29ce484222325, serde_json::to_string(v).unwrap().as_bytes())
}


/// Compact choice-list encoding: `m<k>` move, `s<k>` switch, `p` pass, concatenated.
fn encode_legal(l: &[String]) -> String {
    l.iter()
        .map(|c| match Choice::parse(c) {
            Some(Choice::Move(k)) => format!("m{k}"),
            Some(Choice::Switch(k)) => format!("s{k}"),
            _ => "p".into(),
        })
        .collect()
}

/// A battle replayable without Node: the spec (with explicit choices) and, per step, Showdown's
/// legal choices before it and a hash of its state after it, as `"<p1 legal>,<p2 legal>,<hex>"`.
/// The state includes the PRNG seed, so any difference in PRNG use changes the hash.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Compact {
    pub spec: Spec,
    pub steps: Vec<String>,
}

impl Compact {
    pub fn from_trace(spec: &Spec, trace: &Trace, upto: usize) -> Compact {
        let steps: Vec<String> = trace.steps[..upto]
            .iter()
            .map(|st| format!("{},{},{:016x}", encode_legal(&st.legal[0]), encode_legal(&st.legal[1]), state_hash(&st.state)))
            .collect();
        let mut spec = spec.clone();
        spec.choices = Some(trace.steps[1..upto].iter().map(|s| s.choices.clone()).collect());
        Compact { spec, steps }
    }

    /// Replay; `Err((step, what))` at the first mismatch.
    pub fn check(&self) -> Result<(), (usize, String)> {
        let (t1, t2) = self.spec.teams().map_err(|e| (0, e))?;
        let mut b = Battle::new(&t1, &t2, self.spec.prng(), self.spec.rules());
        let choices = self.spec.choices.clone().unwrap_or_default();
        for (i, st) in self.steps.iter().enumerate() {
            let parts: Vec<&str> = st.split(',').collect();
            if parts.len() != 3 {
                return Err((i, format!("bad step '{st}'")));
            }
            if i > 0 {
                for p in 0..2 {
                    let ours = encode_legal(&choice_strings(&b, p));
                    if ours != parts[p] {
                        return Err((i, format!("p{} legal: showdown {} ours {}", p + 1, parts[p], ours)));
                    }
                }
            }
            let c = if i == 0 { [String::new(), String::new()] } else { choices[i - 1].clone() };
            b.apply_choices(Choice::parse(&c[0]).unwrap_or_default(), Choice::parse(&c[1]).unwrap_or_default(), &mut ());
            if format!("{:016x}", state_hash(&snapshot(&b))) != parts[2] {
                return Err((i, "state (or PRNG seed) differs".into()));
            }
        }
        Ok(())
    }
}

// ---- Event stream check ---------------------------------------------------------------------

/// Public events from Showdown's log, normalised: `p1 move bodyslam [from]`, `p2 switch`,
/// `p1 cant slp`.
fn log_events(log: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for l in log {
        // `|split|pN` is followed by the secret (exact-HP) copy and then the public copy.
        if std::mem::take(&mut skip) {
            continue;
        }
        if l.starts_with("|split|") {
            skip = true;
            continue;
        }
        let parts: Vec<&str> = l.split('|').collect();
        if parts.len() < 3 {
            continue;
        }
        let who = &parts[2][..2.min(parts[2].len())];
        match parts[1] {
            "move" if parts.len() >= 4 => {
                let from = parts[4..].iter().any(|x| x.starts_with("[from]"));
                out.push(format!("{who} move {}{}", sim_data::to_id(parts[3]), if from { " [from]" } else { "" }));
            }
            "switch" | "drag" => out.push(format!("{who} switch")),
            "cant" if parts.len() >= 4 => out.push(format!("{who} cant {}", sim_data::to_id(parts[3]))),
            _ => {}
        }
    }
    out
}

fn sim_events(ev: &[sim_core::Event]) -> Vec<String> {
    use sim_core::{Cant, Event};
    let mut out = Vec::new();
    for e in ev {
        match *e {
            Event::Move { player, mv, from, .. } => {
                let id = sim_gen1::state::move_id_str(mv);
                out.push(format!("p{} move {id}{}", player + 1, if from { " [from]" } else { "" }));
            }
            Event::Switch { player, .. } => out.push(format!("p{} switch", player + 1)),
            Event::Cant { player, reason, .. } => {
                let r = match reason {
                    Cant::Sleep => "slp",
                    Cant::Freeze => "frz",
                    Cant::Paralysis => "par",
                    Cant::PartiallyTrapped => "partiallytrapped",
                    Cant::Flinch => "flinch",
                    Cant::Disable => "disable",
                    Cant::Recharge => "recharge",
                };
                out.push(format!("p{} cant {r}", player + 1));
            }
            _ => {}
        }
    }
    out
}
