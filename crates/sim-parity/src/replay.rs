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
    /// Protocol mode: the `[type, data]` messages Showdown sent for this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proto: Option<Vec<(String, String)>>,
    /// Protocol mode: per update, `extractChannelMessages` for channels -1, 0, 1, 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub views: Option<Vec<[String; 4]>>,
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
    /// Protocol mode: the protocol text (log, a player's view, or a `|request|`) differs.
    Proto,
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
    let mut b = match spec.battle() {
        Ok(b) => b,
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
    let names = spec.names.clone().unwrap_or_else(|| ["p1".to_string(), "p2".to_string()]);
    let mut sink = sim_core::ProtoSink { log: sim_core::Log::new(&names[0], &names[1]), events: Vec::new() };
    let mut sent = 0usize;
    if spec.proto {
        b.write_header(&mut sink.log);
    }
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
        sink.events.clear();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if spec.proto {
                nb.apply_choices_unchecked(c[0], c[1], &mut sink);
            } else {
                nb.apply_choices_unchecked(c[0], c[1], &mut sink.events);
            }
            nb
        }));
        let events = std::mem::take(&mut sink.events);
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
        // A side switches in at most one Pokémon per step: Showdown's is its active after it.
        let slots = |p: usize, _details: &str| st.state["sides"][p]["active"]["slot"].as_u64().map(|s| s as u8);
        let (sd_ev, our_ev) = (log_events(&st.log, &slots), sim_events(&events));
        if sd_ev != our_ev {
            let k = sd_ev.iter().zip(&our_ev).position(|(a, b)| a != b).unwrap_or(sd_ev.len().min(our_ev.len()));
            let cat = format!("events: showdown {} ours {}", sd_ev.get(k).map(|s| s.as_str()).unwrap_or("-"), our_ev.get(k).map(|s| s.as_str()).unwrap_or("-"));
            let d = format!("choices {:?}\nshowdown {sd_ev:?}\nours     {our_ev:?}\nlog:\n{}\n", st.choices, st.log.join("\n"));
            trace::stop();
            return diverge(&b, i, Kind::Events, cat, d, st);
        }
        if spec.proto {
            let update = sink.log.lines[sent..].to_vec();
            sent = sink.log.lines.len();
            if let Some(d) = check_proto(&b, &sink.log, &update, st) {
                trace::stop();
                let cat = d.0.clone();
                return diverge(&b, i, Kind::Proto, cat, format!("choices {:?}\n{}", st.choices, d.1), st);
            }
        }
        let ours = snapshot(&b);
        let mut diffs = Vec::new();
        // Showdown's `winner` is the side's name; the snapshot uses "p1"/"p2".
        let mut sd_state = std::borrow::Cow::Borrowed(&st.state);
        if let Some(w) = st.state["winner"].as_str() {
            if let Some(k) = names.iter().position(|n| n == w) {
                sd_state.to_mut()["winner"] = Value::String(format!("p{}", k + 1));
            }
        }
        diff(&sd_state, &ours, &mut String::new(), &mut diffs);
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

// ---- Protocol check ------------------------------------------------------------------------------

/// A protocol line's template for categories: the message kind plus its `[tags]`.
fn line_kind(l: &str) -> String {
    let parts: Vec<&str> = l.split('|').collect();
    let mut k = parts.get(1).copied().unwrap_or("").to_string();
    if matches!(k.as_str(), "-start" | "-end" | "-activate" | "cant" | "-fail" | "-hint" | "-message") {
        if let Some(x) = parts.get(3) {
            k.push(' ');
            k.push_str(&x.chars().take(24).collect::<String>());
        }
    }
    for p in parts.iter().skip(2) {
        if p.starts_with('[') {
            k.push(' ');
            k.push_str(p.split(']').next().unwrap_or(""));
            k.push(']');
        }
    }
    k
}

fn first_diff(kind: &str, a: &[String], b: &[String]) -> Option<(String, String)> {
    let n = a.len().max(b.len());
    for k in 0..n {
        let (x, y) = (a.get(k), b.get(k));
        if x != y {
            let cat = format!("proto {kind}: showdown[{}] ours[{}]", x.map(|s| line_kind(s)).unwrap_or("-".into()), y.map(|s| line_kind(s)).unwrap_or("-".into()));
            let mut d = format!("{kind}: first difference at line {k}\n");
            for j in k.saturating_sub(4)..(k + 4).min(n) {
                d.push_str(&format!("  {}{j:3} sd:   {}\n        ours: {}\n", if j == k { "*" } else { " " }, a.get(j).map(|s| s.as_str()).unwrap_or("-"), b.get(j).map(|s| s.as_str()).unwrap_or("-")));
            }
            return Some((cat, d));
        }
    }
    None
}

/// Compare this step's protocol output with Showdown's: the raw update (with `|split|` blocks),
/// each channel's view, and each player's `|request|` JSON.
fn check_proto(b: &Battle, log: &sim_core::Log, update: &[String], st: &Step) -> Option<(String, String)> {
    let msgs = st.proto.as_ref()?;
    let norm = |l: &str| if l.starts_with("|t:|") { "|t:|0".to_string() } else { l.to_string() };
    let sd_update: Vec<String> = msgs.iter().filter(|m| m.0 == "update").flat_map(|m| m.1.split('\n').map(norm).collect::<Vec<_>>()).collect();
    let ours: Vec<String> = update.iter().map(|l| norm(l)).collect();
    if let Some(d) = first_diff("log", &sd_update, &ours) {
        return Some(d);
    }
    if let Some(views) = &st.views {
        let names = ["omniscient", "spectator", "p1", "p2"];
        for (c, name) in names.iter().enumerate() {
            let sd: Vec<String> = views.iter().flat_map(|v| v[c].split('\n').filter(|x| !x.is_empty()).map(norm).collect::<Vec<_>>()).collect();
            let o: Vec<String> = sim_core::proto::extract_channel(&ours, c as i32 - 1);
            if let Some(d) = first_diff(&format!("view {name}"), &sd, &o) {
                return Some(d);
            }
        }
    }
    for p in 0..2 {
        let pre = format!("p{}\n|request|", p + 1);
        let sd: Option<&str> = msgs.iter().filter(|m| m.0 == "sideupdate").find_map(|m| m.1.strip_prefix(&pre));
        let o = b.request_json(log, p);
        if sd != o.as_deref() {
            let (a, c) = (sd.unwrap_or("-"), o.as_deref().unwrap_or("-"));
            let k = a.bytes().zip(c.bytes()).position(|(x, y)| x != y).unwrap_or(a.len().min(c.len()));
            let ctx = |s: &str| s.get(k.saturating_sub(60)..(k + 100).min(s.len())).unwrap_or(s).to_string();
            let cat = format!("proto request p{}: {}", p + 1, if sd.is_none() || o.is_none() { "presence" } else { "json" });
            return Some((cat, format!("p{} request differs at byte {k}\n  sd:   ...{}\n  ours: ...{}\nfull sd:   {a}\nfull ours: {c}\n", p + 1, ctx(a), ctx(c))));
        }
    }
    None
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
    /// Protocol corpora: per step, a hash of Showdown's protocol output (the update lines with
    /// `|t:|` normalised, then each player's `|request|` JSON); see [`proto_hash`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proto: Option<Vec<String>>,
}

/// Hash of one step's protocol output: update lines (`|t:|` normalised) and both requests.
/// The per-channel views are a pure function of the update lines ([`sim_core::proto::extract_channel`]).
pub fn proto_hash(update: &[String], requests: [Option<&str>; 2]) -> String {
    let mut h = 0xcbf29ce484222325u64;
    for l in update {
        let l = if l.starts_with("|t:|") { "|t:|0" } else { l.as_str() };
        h = fnv(h, l.as_bytes());
        h = fnv(h, b"\n");
    }
    for r in requests {
        h = fnv(h, b"\x00");
        h = fnv(h, r.unwrap_or("-").as_bytes());
    }
    format!("{h:016x}")
}

fn sd_proto_hash(st: &Step) -> Option<String> {
    let msgs = st.proto.as_ref()?;
    let update: Vec<String> = msgs.iter().filter(|m| m.0 == "update").flat_map(|m| m.1.split('\n').map(String::from).collect::<Vec<_>>()).collect();
    let req = |p: usize| {
        let pre = format!("p{}\n|request|", p + 1);
        msgs.iter().filter(|m| m.0 == "sideupdate").find_map(|m| m.1.strip_prefix(&pre))
    };
    Some(proto_hash(&update, [req(0), req(1)]))
}

impl Compact {
    pub fn from_trace(spec: &Spec, trace: &Trace, upto: usize) -> Compact {
        let steps: Vec<String> = trace.steps[..upto]
            .iter()
            .map(|st| format!("{},{},{:016x}", encode_legal(&st.legal[0]), encode_legal(&st.legal[1]), state_hash(&st.state)))
            .collect();
        let mut spec = spec.clone();
        spec.choices = Some(trace.steps[1..upto].iter().map(|s| s.choices.clone()).collect());
        let proto = trace.steps[..upto].iter().map(sd_proto_hash).collect::<Option<Vec<String>>>();
        Compact { spec, steps, proto }
    }

    /// Replay; `Err((step, what))` at the first mismatch.
    pub fn check(&self) -> Result<(), (usize, String)> {
        let mut b = self.spec.battle().map_err(|e| (0, e))?;
        let choices = self.spec.choices.clone().unwrap_or_default();
        let names = self.spec.names.clone().unwrap_or_else(|| ["p1".to_string(), "p2".to_string()]);
        let mut sink = sim_core::ProtoSink { log: sim_core::Log::new(&names[0], &names[1]), events: Vec::new() };
        if self.proto.is_some() {
            b.write_header(&mut sink.log);
        }
        let mut sent = 0;
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
            let (c1, c2) = (Choice::parse(&c[0]).unwrap_or_default(), Choice::parse(&c[1]).unwrap_or_default());
            if let Some(hashes) = &self.proto {
                b.apply_choices_unchecked(c1, c2, &mut sink);
                let update = &sink.log.lines[sent..];
                let h = proto_hash(update, [b.request_json(&sink.log, 0).as_deref(), b.request_json(&sink.log, 1).as_deref()]);
                sent = sink.log.lines.len();
                if hashes.get(i) != Some(&h) {
                    return Err((i, format!("protocol output differs:\n{}", update.join("\n"))));
                }
            } else {
                b.apply_choices_unchecked(c1, c2, &mut ());
            }
            let mut st = snapshot(&b);
            // Showdown's `winner` is the side's name.
            if let Some(k) = st["winner"].as_str().and_then(|w| ["p1", "p2"].iter().position(|x| *x == w)) {
                st["winner"] = Value::String(names[k].clone());
            }
            if format!("{:016x}", state_hash(&st)) != parts[2] {
                return Err((i, "state (or PRNG seed) differs".into()));
            }
        }
        Ok(())
    }
}

// ---- Event stream check ---------------------------------------------------------------------

/// Public events from Showdown's log, normalised to the [`sim_core::Event`]s the env's reveal
/// memory consumes (battle_env::reveal), in order:
/// `p1 move bodyslam [from]`, `p2 switch 3` (original team slot), `p1 cant disable thunderbolt`,
/// `p1 miss`, `crit`, `supereffective`, `resisted`, `immune`, `fail`, `p2 disable psychic`,
/// `p1 mimic surf`, `p1 transform`, `p2 faint`, `turn`.
fn log_events(log: &[String], slots: &dyn Fn(usize, &str) -> Option<u8>) -> Vec<String> {
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
        if parts.len() < 2 {
            continue;
        }
        if parts[1] == "turn" {
            out.push("turn".into());
            continue;
        }
        if parts.len() < 3 {
            continue;
        }
        let who = &parts[2][..2.min(parts[2].len())];
        let p = if who == "p2" { 1 } else { 0 };
        match parts[1] {
            "move" if parts.len() >= 4 => {
                let from = parts[4..].iter().any(|x| x.starts_with("[from]"));
                out.push(format!("{who} move {}{}", sim_data::to_id(parts[3]), if from { " [from]" } else { "" }));
            }
            "switch" | "drag" if parts.len() >= 4 => {
                // `|switch|p1a: Name|Species, L50|hp`: map the details to the team slot.
                let slot = slots(p, parts[3]).map(|s| s.to_string()).unwrap_or_else(|| "?".into());
                out.push(format!("{who} switch {slot}"));
            }
            "cant" if parts.len() >= 4 => {
                let reason = sim_data::to_id(parts[3]);
                match parts.get(4) {
                    Some(m) if reason == "disable" => out.push(format!("{who} cant {reason} {}", sim_data::to_id(m))),
                    _ => out.push(format!("{who} cant {reason}")),
                }
            }
            "-miss" => out.push(format!("{who} miss")),
            "-crit" => out.push("crit".into()),
            "-supereffective" => out.push("supereffective".into()),
            "-resisted" => out.push("resisted".into()),
            "-immune" => out.push("immune".into()),
            "-fail" => out.push("fail".into()),
            "-start" if parts.len() >= 5 && matches!(parts[3], "Disable" | "Mimic") => {
                out.push(format!("{who} {} {}", sim_data::to_id(parts[3]), sim_data::to_id(parts[4])));
            }
            "-transform" => out.push(format!("{who} transform")),
            "faint" => out.push(format!("{who} faint")),
            _ => {}
        }
    }
    out
}

fn sim_events(ev: &[sim_core::Event]) -> Vec<String> {
    use sim_core::{Cant, Event};
    use sim_gen1::state::move_id_str;
    let mut out = Vec::new();
    for e in ev {
        out.push(match *e {
            Event::Move { player, mv, from, .. } => {
                format!("p{} move {}{}", player + 1, move_id_str(mv), if from { " [from]" } else { "" })
            }
            Event::Switch { player, slot } => format!("p{} switch {slot}", player + 1),
            Event::Cant { player, reason, mv, .. } => {
                let r = match reason {
                    Cant::Sleep => "slp",
                    Cant::Freeze => "frz",
                    Cant::Paralysis => "par",
                    Cant::PartiallyTrapped => "partiallytrapped",
                    Cant::Flinch => "flinch",
                    Cant::Disable => "disable",
                    Cant::Recharge => "recharge",
                };
                if reason == Cant::Disable {
                    format!("p{} cant {r} {}", player + 1, move_id_str(mv))
                } else {
                    format!("p{} cant {r}", player + 1)
                }
            }
            Event::Miss { player } => format!("p{} miss", player + 1),
            Event::Crit => "crit".into(),
            Event::SuperEffective => "supereffective".into(),
            Event::Resisted => "resisted".into(),
            Event::Immune => "immune".into(),
            Event::Fail => "fail".into(),
            Event::DisableStart { player, mv, .. } => format!("p{} disable {}", player + 1, move_id_str(mv)),
            Event::MimicStart { player, mv } => format!("p{} mimic {}", player + 1, move_id_str(mv)),
            Event::Transform { player } => format!("p{} transform", player + 1),
            Event::Faint { player, .. } => format!("p{} faint", player + 1),
            Event::Turn { .. } => "turn".into(),
        });
    }
    out
}
