//! A drop-in for pokemon-showdown 0.11.11's `BattleStream` / `BattleTextStream` (sim/battle-stream.ts)
//! for `[Gen 1] OU`, backed by the native simulator.
//!
//! Input is Showdown's battle-stream protocol (`>start`, `>player`, `>p1 move 1`, ...); output is
//! the same messages Showdown pushes: `update` (new battle-log lines), `sideupdate` (requests
//! and `|error|` lines for one player), `end` (the summary JSON) and `requesteddata`. Choice
//! parsing and its error messages are a port of `Side#choose` and friends (sim/side.ts), the
//! command handling of `BattleStream#_writeLine`, and the batching of `Battle#sendUpdates`, so a
//! stream of commands gives byte-identical output (the `|t:|` timestamps aside).
//!
//! What is not supported (an error, not a silent difference): formats other than `gen1ou` (and
//! its `@@@!Sleep Clause Mod` / `!Freeze Clause Mod` variants), teams the native simulator
//! cannot reproduce (abilities other than No Ability, items, non-Gen 1 species/moves, levels above
//! 100, more than 6 Pokémon or 4 moves), random teams (no `team`), `>eval`, `>editbattle`,
//! `>show-openteamsheets` before turn 1, and the `replay`/`spectate` output modes.

use super::json::{js_number, Obj, Value};
use super::sets::{self, js_parse_int, to_id, SetInfo};
use kanto_core::proto::{Log, ProtoSink, SetExtra};
use kanto_core::{Choice, Prng, Request};
use kanto_data::gen1::{MOVES, TYPE_NAMES};
use kanto_gen1::state::{st, v, Battle, Compat, Rules};
use kanto_gen1::MonSpec;

/// A Showdown `BattleStream` (object mode: one message per [`BattleStream::write`] output).
pub struct BattleStream {
    battle: Option<Box<SdBattle>>,
    out: Vec<String>,
    /// `pushEnd` happened: later pushes are dropped (as on an ended Showdown stream).
    at_eof: bool,
    pub keep_alive: bool,
    clock: fn() -> u64,
}

/// Unix time in seconds (`Math.floor(Date.now() / 1000)`).
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Default for BattleStream {
    fn default() -> Self {
        BattleStream::new()
    }
}

impl BattleStream {
    pub fn new() -> BattleStream {
        BattleStream { battle: None, out: Vec::new(), at_eof: false, keep_alive: false, clock: now_secs }
    }

    /// Use `clock` for `|t:|` timestamps (tests).
    pub fn with_clock(clock: fn() -> u64) -> BattleStream {
        BattleStream { clock, ..BattleStream::new() }
    }

    /// Whether the stream ended (the battle sent `end`).
    pub fn ended(&self) -> bool {
        self.at_eof
    }

    /// `BattleStream#_write(chunk)`: run every `>`-line of `chunk`, then `sendUpdates`. Returns the
    /// messages pushed meanwhile. An `Err` is what Showdown would throw (`noCatch` mode, as
    /// `simulate-battle` runs): an unknown command, a command before `>start`, an unsupported team.
    pub fn write(&mut self, chunk: &str) -> Result<Vec<String>, String> {
        let r = self.write_lines(chunk);
        if r.is_ok() {
            if let Some(b) = self.battle.as_mut() {
                b.send_updates(&mut Out { msgs: &mut self.out, at_eof: &mut self.at_eof, keep_alive: self.keep_alive });
            }
        }
        // On an error the messages stay queued for [`BattleStream::take_output`].
        r.map(|_| std::mem::take(&mut self.out))
    }

    fn write_lines(&mut self, chunk: &str) -> Result<(), String> {
        for line in chunk.split('\n') {
            if let Some(rest) = line.strip_prefix('>') {
                let (ty, msg) = split_first(rest, ' ');
                self.write_line(ty, msg)?;
            }
        }
        Ok(())
    }

    fn write_line(&mut self, ty: &str, message: &str) -> Result<(), String> {
        let clock = self.clock;
        if ty == "start" {
            let opts = Value::parse(message).map_err(|e| format!("SyntaxError: {e}"))?;
            let opts = opts.as_obj().cloned().unwrap_or_default();
            let mut out = Out { msgs: &mut self.out, at_eof: &mut self.at_eof, keep_alive: self.keep_alive };
            self.battle = Some(Box::new(SdBattle::new(&opts, clock, &mut out)?));
            return Ok(());
        }
        const KNOWN: [&str; 21] = [
            "player", "p1", "p2", "p3", "p4", "forcewin", "forcetie", "forcelose", "reseed", "tiebreak", "chat-inputlogonly", "chat", "eval",
            "editbattle", "requestlog", "requestexport", "requestteam", "show-openteamsheets", "version", "version-origin", "start",
        ];
        if !KNOWN.contains(&ty) {
            return Err(format!("Error: Unrecognized command \">{ty} {message}\""));
        }
        if matches!(ty, "version" | "version-origin") {
            return Ok(());
        }
        let keep = self.keep_alive;
        let Some(b) = self.battle.as_mut() else {
            return Err(format!("TypeError: Cannot read properties of null (command >{ty} before >start)"));
        };
        let mut out = Out { msgs: &mut self.out, at_eof: &mut self.at_eof, keep_alive: keep };
        b.log.timestamp = clock();
        match ty {
            "player" => {
                let (slot, text) = split_first(message, ' ');
                let opts = Value::parse(text).map_err(|e| format!("SyntaxError: {e}"))?;
                b.set_player(slot, opts, &mut out)?;
            }
            "p1" | "p2" => {
                let p = if ty == "p1" { 0 } else { 1 };
                if b.sides[p].is_none() {
                    return Err(format!("TypeError: player {ty} has not joined"));
                }
                if message == "undo" {
                    b.undo_choice(p, &mut out);
                } else {
                    b.choose(p, message, &mut out)?;
                }
            }
            "p3" | "p4" => return Err(format!("TypeError: no side {ty} in a singles battle")),
            "forcewin" | "forcetie" => {
                b.win(if ty == "forcewin" { Some(message) } else { None });
                if !message.is_empty() {
                    b.input_log.push(format!(">forcewin {message}"));
                } else {
                    b.input_log.push(">forcetie".into());
                }
            }
            "forcelose" => {
                b.lose(message);
                b.input_log.push(format!(">forcelose {message}"));
            }
            "reseed" => {
                b.reset_rng(message)?;
                let seed = b.rng().seed_string();
                b.input_log.push(format!(">reseed {seed}"));
            }
            "tiebreak" => b.tiebreak(),
            "chat-inputlogonly" => b.input_log.push(format!(">chat {message}")),
            "chat" => {
                b.input_log.push(format!(">chat {message}"));
                b.log.add(format!("|chat|{message}"));
            }
            "requestlog" => out.push(format!("requesteddata\n{}", b.input_log.join("\n"))),
            "requestexport" => out.push(format!("requesteddata\n{}\n{}", b.prng_seed, b.input_log.join("\n"))),
            "requestteam" => {
                let m = message.trim();
                let n = js_parse_int(m.get(1..).unwrap_or("")).map(|v| v - 1);
                let side = match n {
                    Some(k) if k >= 0 => k as usize,
                    _ => return Err(format!("Error: Team requested for slot {m}, but that slot does not exist.")),
                };
                let team = match b.sides.get(side).and_then(|s| s.as_ref()) {
                    Some(sd) => sets::pack(&sd.team),
                    None => return Err(format!("TypeError: no side {m}")),
                };
                out.push(format!("requesteddata\n{team}"));
            }
            "show-openteamsheets" => {
                if b.turn() != 0 {
                    // `if (this.turn !== 0) return;`
                } else {
                    return Err("show-openteamsheets before the battle started is not supported".into());
                }
            }
            "version" | "version-origin" => {}
            "eval" | "editbattle" => return Err(format!(">{ty} is not supported by kanto's BattleStream")),
            _ => return Err(format!("Error: Unrecognized command \">{ty} {message}\"")),
        }
        Ok(())
    }

    /// Everything pushed so far that [`BattleStream::write`] has not returned (only non-empty
    /// after an error).
    pub fn take_output(&mut self) -> Vec<String> {
        std::mem::take(&mut self.out)
    }

    /// The native battle, once both players joined.
    pub fn battle(&self) -> Option<&Battle> {
        self.battle.as_ref().and_then(|b| b.b.as_ref())
    }

    /// Showdown's `Battle#log` so far.
    pub fn log(&self) -> Option<&[String]> {
        self.battle.as_ref().map(|b| b.log.lines.as_slice())
    }

    /// `Battle#inputLog`.
    pub fn input_log(&self) -> Option<&[String]> {
        self.battle.as_ref().map(|b| b.input_log.as_slice())
    }
}

/// `BattleTextStream`: text in, text out. Input is split into lines (a trailing partial line waits
/// for the next write, and is dropped at the end, as Showdown does); every output message is
/// written followed by a blank line.
pub struct BattleTextStream {
    pub stream: BattleStream,
    current: String,
}

impl Default for BattleTextStream {
    fn default() -> Self {
        BattleTextStream::new()
    }
}

impl BattleTextStream {
    pub fn new() -> BattleTextStream {
        BattleTextStream { stream: BattleStream::new(), current: String::new() }
    }

    pub fn with_clock(clock: fn() -> u64) -> BattleTextStream {
        BattleTextStream { stream: BattleStream::with_clock(clock), current: String::new() }
    }

    /// `BattleTextStream#_write`: returns the text output this write produced.
    pub fn write(&mut self, text: &str) -> Result<String, StreamError> {
        self.current.push_str(text);
        let Some(i) = self.current.rfind('\n') else { return Ok(String::new()) };
        let chunk: String = self.current[..i].to_string();
        self.current = self.current[i + 1..].to_string();
        let r = self.stream.write(&chunk);
        let msgs = match r {
            Ok(m) => m,
            Err(message) => {
                // What was pushed before the throw is still written out.
                let mut output = String::new();
                for m in self.stream.take_output() {
                    format_message(&mut output, &m);
                }
                return Err(StreamError { output, message });
            }
        };
        let mut s = String::new();
        for m in msgs {
            format_message(&mut s, &m);
        }
        Ok(s)
    }
}

/// What Showdown would throw (`simulate-battle` dies with it), and the text output of the same
/// write before the throw.
#[derive(Clone, Debug)]
pub struct StreamError {
    pub output: String,
    pub message: String,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StreamError {}

impl BattleTextStream {
    /// Like [`BattleTextStream::write`] but returns the messages unformatted (object mode).
    pub fn write_messages(&mut self, text: &str) -> Result<Vec<String>, String> {
        self.current.push_str(text);
        let Some(i) = self.current.rfind('\n') else { return Ok(Vec::new()) };
        let chunk: String = self.current[..i].to_string();
        self.current = self.current[i + 1..].to_string();
        self.stream.write(&chunk)
    }
}

fn format_message(out: &mut String, m: &str) {
    out.push_str(m);
    if !m.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
}

/// `Utils.splitFirst(str, delimiter)` with one split.
fn split_first(s: &str, d: char) -> (&str, &str) {
    match s.find(d) {
        Some(i) => (&s[..i], &s[i + d.len_utf8()..]),
        None => (s, ""),
    }
}

/// The output side of the stream: pushes are dropped after `pushEnd`.
struct Out<'a> {
    msgs: &'a mut Vec<String>,
    at_eof: &'a mut bool,
    keep_alive: bool,
}

impl Out<'_> {
    fn push(&mut self, m: String) {
        if !*self.at_eof {
            self.msgs.push(m);
        }
    }

    fn end(&mut self) {
        if !self.keep_alive {
            *self.at_eof = true;
        }
    }
}

/// One player.
struct SdSide {
    name: String,
    avatar: String,
    /// `side.team`: the set objects, as `new Pokemon(set)` left them.
    team: Vec<Value>,
    specs: Vec<MonSpec>,
    infos: Vec<SetInfo>,
}

/// A chosen action (`side.choice.actions[]`).
#[derive(Clone, Debug)]
enum Act {
    /// `moveid` for `getChoice`, and the native choice it maps to.
    Move { moveid: String, native: Choice },
    Switch { pos: usize },
    Pass,
}

#[derive(Clone, Debug, Default)]
struct SideChoice {
    cant_undo: bool,
    error: String,
    actions: Vec<Act>,
    forced_switches_left: u32,
    forced_passes_left: u32,
    switch_ins: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReqState {
    None,
    Move,
    Switch,
}

impl ReqState {
    fn name(self) -> &'static str {
        match self {
            ReqState::None => "",
            ReqState::Move => "move",
            ReqState::Switch => "switch",
        }
    }
}

struct SdBattle {
    rules: Rules,
    rated: Option<Value>,
    prng_seed: String,
    /// The PRNG before the native battle exists (`>reseed` may replace it).
    pending_rng: Prng,
    log: Log,
    sent_log_pos: usize,
    sent_requests: bool,
    sent_end: bool,
    input_log: Vec<String>,
    sides: [Option<SdSide>; 2],
    b: Option<Battle>,
    events: Vec<kanto_core::Event>,
    started: bool,
    ended: bool,
    winner: String,
    active_request: [Option<Value>; 2],
    choice: [SideChoice; 2],
}

/// Map a Showdown format id to native rules (`gen1ou` plus the clause-removing custom rules the
/// native simulator models).
fn rules_of(formatid: &str) -> Result<Rules, String> {
    let (base, custom) = match formatid.split_once("@@@") {
        Some((b, c)) => (b, c),
        None => (formatid, ""),
    };
    if to_id(base) != "gen1ou" {
        return Err(format!("format \"{formatid}\" is not supported (gen1ou only)"));
    }
    let mut r = Rules::OU;
    let custom: Vec<String> = if custom.is_empty() { vec![] } else { custom.split(',').map(|c| c.trim().to_string()).collect() };
    match custom.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => {}
        ["!Sleep Clause Mod"] => r.sleep_clause = false,
        ["!Freeze Clause Mod"] => r.freeze_clause = false,
        ["!Sleep Clause Mod", "!Freeze Clause Mod"] => {
            r.sleep_clause = false;
            r.freeze_clause = false;
        }
        _ => return Err(format!("custom rules \"{}\" are not supported", custom.join(","))),
    }
    Ok(r)
}

impl SdBattle {
    fn new(opts: &Obj, clock: fn() -> u64, out: &mut Out) -> Result<SdBattle, String> {
        let formatid = opts.get("formatid").cloned().unwrap_or(Value::Null);
        let rules = rules_of(formatid.as_str().unwrap_or(""))?;
        let seed = match opts.get("seed") {
            None | Some(Value::Null) => None,
            Some(v) if !v.truthy() => None,
            Some(Value::Arr(a)) => Some(a.iter().map(|x| if matches!(x, Value::Null) { String::new() } else { x.to_js_string() }).collect::<Vec<_>>().join(",")),
            Some(Value::Str(s)) => Some(s.clone()),
            Some(v) => return Err(format!("Error: PRNG: Seed {} must be a string", v.to_js_string())),
        };
        let seed = seed.unwrap_or_else(generate_seed);
        let rng = parse_seed(&seed)?;
        let rated = opts.get("rated").filter(|r| r.truthy()).cloned();
        let mut log = Log::default();
        log.timestamp = clock();
        log.add(format!("|t:|{}", log.timestamp));
        let mut b = SdBattle {
            rules,
            rated: rated.clone(),
            prng_seed: seed.clone(),
            pending_rng: rng,
            log,
            sent_log_pos: 0,
            sent_requests: true,
            sent_end: false,
            input_log: Vec::new(),
            sides: [None, None],
            b: None,
            events: Vec::new(),
            started: false,
            ended: false,
            winner: String::new(),
            active_request: [None, None],
            choice: [SideChoice::default(), SideChoice::default()],
        };
        let mut io = Obj::new();
        if !matches!(formatid, Value::Null) {
            io.set("formatid", formatid);
        }
        io.set("seed", Value::Str(seed));
        if let Some(r) = rated {
            io.set("rated", r);
        }
        b.input_log.push(format!(">start {}", Value::Obj(io).stringify()));
        b.log.add("|gametype|singles".into());
        for (k, slot) in ["p1", "p2"].iter().enumerate() {
            if let Some(o) = opts.get(slot).filter(|o| o.truthy()) {
                b.set_player(slot, o.clone(), out)?;
                let _ = k;
            }
        }
        if opts.get("p3").is_some_and(Value::truthy) || opts.get("p4").is_some_and(Value::truthy) {
            return Err("p3/p4 are not supported (singles only)".into());
        }
        Ok(b)
    }

    fn rng(&self) -> Prng {
        match &self.b {
            Some(b) => b.rng,
            None => self.pending_rng,
        }
    }

    fn turn(&self) -> u32 {
        self.b.as_ref().map_or(0, |b| b.turn as u32)
    }

    /// `Battle#resetRNG(seed)` (the `>reseed` command; an empty seed means a new random one).
    fn reset_rng(&mut self, seed: &str) -> Result<(), String> {
        let s = if seed.is_empty() { generate_seed() } else { seed.to_string() };
        let rng = parse_seed(&s)?;
        match self.b.as_mut() {
            Some(b) => b.rng = rng,
            None => self.pending_rng = rng,
        }
        self.log.add("|message|The battle's RNG was reset.".into());
        Ok(())
    }

    fn set_player(&mut self, slot: &str, mut options: Value, out: &mut Out) -> Result<(), String> {
        let n = match slot.as_bytes().get(1).map(|c| c.wrapping_sub(b'1')) {
            Some(k @ 0..=1) => k as usize,
            _ => return Err(format!("TypeError: no side {slot} in a singles battle")),
        };
        let o = options.as_obj_mut().ok_or("TypeError: player options must be an object")?;
        let opt_str = |o: &Obj, k: &str| o.get(k).filter(|v| v.truthy()).map(Value::to_js_string);
        let mut did_something = true;
        if self.sides[n].is_none() {
            let team_v = o.get("team").cloned();
            let mut team: Vec<Value> = match team_v {
                Some(Value::Str(s)) => match sets::unpack(&s) {
                    Some(t) => t,
                    None => return Err("Error: random teams are not supported (no valid team given)".into()),
                },
                Some(Value::Arr(a)) => a,
                Some(v) if v.truthy() => return Err("TypeError: team must be a packed string or an array of sets".into()),
                _ => return Err("Error: random teams are not supported (no team given)".into()),
            };
            if team.is_empty() || team.len() > 6 {
                return Err(format!("teams of {} Pokémon are not supported (1-6)", team.len()));
            }
            let mut specs = Vec::new();
            let mut infos = Vec::new();
            for set in team.iter_mut() {
                let (spec, info) = sets::build_set(set).map_err(|e| format!("Error: {e}"))?;
                specs.push(spec);
                infos.push(info);
            }
            let name = opt_str(o, "name").unwrap_or_else(|| format!("Player {}", n + 1));
            let avatar = opt_str(o, "avatar").unwrap_or_default();
            self.sides[n] = Some(SdSide { name, avatar, team, specs, infos });
        } else {
            let sd = self.sides[n].as_mut().unwrap();
            did_something = false;
            if let Some(nm) = opt_str(o, "name") {
                if sd.name != nm {
                    sd.name = nm;
                    did_something = true;
                }
            }
            if let Some(av) = opt_str(o, "avatar") {
                if sd.avatar != av {
                    sd.avatar = av;
                    did_something = true;
                }
            }
            if o.get("team").is_some_and(Value::truthy) {
                return Err(format!("Error: Player {slot} already has a team!"));
            }
            if did_something {
                // (Side#name changes show in later protocol lines.)
                self.log.player_names[n] = sd.name.clone();
            }
        }
        if let Some(t) = o.get("team").cloned() {
            if t.truthy() && !matches!(t, Value::Str(_)) {
                o.set("team", Value::Str(sets::pack(&self.sides[n].as_ref().unwrap().team)));
            }
        }
        if !did_something {
            return Ok(());
        }
        self.input_log.push(format!(">player {slot} {}", options.stringify()));
        let o = options.as_obj().unwrap();
        let sd = self.sides[n].as_ref().unwrap();
        let rating = o.get("rating").filter(|v| v.truthy()).map(Value::to_js_string).unwrap_or_default();
        self.log.add(format!("|player|p{}|{}|{}|{}", n + 1, sd.name, sd.avatar, rating));
        if self.sides.iter().all(Option::is_some) && !self.started {
            self.start(out)?;
        }
        Ok(())
    }

    /// `Battle#start`, then the native battle sends out the leads.
    fn start(&mut self, out: &mut Out) -> Result<(), String> {
        let (s1, s2) = (self.sides[0].as_ref().unwrap(), self.sides[1].as_ref().unwrap());
        let mut b = Battle::new(&s1.specs, &s2.specs, self.pending_rng, self.rules).map_err(|e| format!("Error: {e}"))?;
        b.rules.compat = Compat::Vanilla;
        self.started = true;
        for (p, sd) in [s1, s2].into_iter().enumerate() {
            self.log.player_names[p] = sd.name.clone();
            self.log.nicknames[p] = sd.infos.iter().map(|i| i.name.clone()).collect();
            self.log.extras[p] = sd.infos.iter().map(|i| SetExtra { details_suffix: i.details_suffix.clone(), ability: i.ability.clone(), item: i.item.clone(), pokeball: i.pokeball.clone() }).collect();
        }
        let rated = self.rated.as_ref().map(|r| match r {
            Value::Str(s) if s != "Rated battle" => s.clone(),
            _ => String::new(),
        });
        b.write_format_header(&mut self.log, rated.as_deref());
        let mut sink = ProtoSink { log: std::mem::take(&mut self.log), events: std::mem::take(&mut self.events) };
        b.apply_choices_unchecked(Choice::Pass, Choice::Pass, &mut sink);
        self.log = sink.log;
        self.b = Some(b);
        self.after_step(out);
        Ok(())
    }

    /// After the native battle ran: the new requests (`makeRequest`) or the end of the battle.
    fn after_step(&mut self, _out: &mut Out) {
        let b = self.b.as_ref().unwrap();
        if b.ended {
            self.ended = true;
            self.winner = match b.winner {
                1 | 2 => self.sides[b.winner as usize - 1].as_ref().unwrap().name.clone(),
                _ => String::new(),
            };
            self.active_request = [None, None];
            return;
        }
        for p in 0..2 {
            self.active_request[p] = b.request_json(&self.log, p).map(|j| Value::parse(&j).expect("request JSON"));
        }
        self.sent_requests = false;
        for p in 0..2 {
            self.clear_choice(p);
        }
    }

    fn request_state(&self, p: usize) -> ReqState {
        match &self.active_request[p] {
            None => ReqState::None,
            Some(r) if r.get("wait").is_some_and(Value::truthy) => ReqState::None,
            Some(r) if r.get("forceSwitch").is_some_and(Value::truthy) => ReqState::Switch,
            Some(_) => ReqState::Move,
        }
    }

    /// `battle.requestState`.
    fn battle_request_state(&self) -> ReqState {
        let b = match &self.b {
            Some(b) if !self.ended => b,
            _ => return ReqState::None,
        };
        if b.sides.iter().any(|s| s.request == Request::Switch) {
            ReqState::Switch
        } else if b.sides.iter().any(|s| s.request == Request::Move) {
            ReqState::Move
        } else {
            ReqState::None
        }
    }

    fn send_updates(&mut self, out: &mut Out) {
        if self.sent_log_pos >= self.log.lines.len() {
            return;
        }
        out.push(format!("update\n{}", self.log.lines[self.sent_log_pos..].join("\n")));
        if !self.sent_requests {
            for p in 0..2 {
                self.emit_request(p, false, out);
            }
            self.sent_requests = true;
        }
        self.sent_log_pos = self.log.lines.len();
        if !self.sent_end && self.ended {
            out.push(format!("end\n{}", self.end_json()));
            self.sent_end = true;
            out.end();
        }
    }

    fn end_json(&self) -> String {
        let (s1, s2) = (self.sides[0].as_ref(), self.sides[1].as_ref());
        let mut o = Obj::new();
        o.set("winner", Value::Str(self.winner.clone()));
        o.set("seed", Value::Str(self.prng_seed.clone()));
        o.set("turns", Value::Num(self.turn() as f64));
        o.set("p1", Value::Str(s1.map(|s| s.name.clone()).unwrap_or_default()));
        o.set("p2", Value::Str(s2.map(|s| s.name.clone()).unwrap_or_default()));
        o.set("p1team", Value::Arr(s1.map(|s| s.team.clone()).unwrap_or_default()));
        o.set("p2team", Value::Arr(s2.map(|s| s.team.clone()).unwrap_or_default()));
        let left = |p: usize| match &self.b {
            Some(b) => b.sides[p].left as f64,
            None => self.sides[p].as_ref().map_or(0.0, |s| s.team.len() as f64),
        };
        o.set("score", Value::Arr(vec![Value::Num(left(0)), Value::Num(left(1))]));
        o.set("inputLog", Value::Arr(self.input_log.iter().map(|l| Value::str(l)).collect()));
        Value::Obj(o).stringify()
    }

    /// `Side#emitRequest(this.activeRequest, updated)`.
    fn emit_request(&mut self, p: usize, updated: bool, out: &mut Out) {
        if updated {
            if let Some(r) = self.active_request[p].as_mut().and_then(Value::as_obj_mut) {
                r.set("update", Value::Bool(true));
            }
        }
        let json = self.active_request[p].as_ref().map_or_else(|| "null".to_string(), Value::stringify);
        out.push(format!("sideupdate\np{}\n|request|{json}", p + 1));
    }

    /// `Side#emitChoiceError`; `update` = the `{pokemon, update}` request patch.
    fn choice_error(&mut self, p: usize, msg: &str, update: Option<fn(&mut Obj) -> bool>, out: &mut Out) -> bool {
        self.choice[p].error = msg.to_string();
        let mut updated = false;
        if let Some(f) = update {
            if let Some(req) = self.active_request[p].as_mut().and_then(|r| r.as_obj_mut()).and_then(|r| r.get_mut("active")).and_then(|a| match a {
                Value::Arr(a) => a.get_mut(0).and_then(Value::as_obj_mut),
                _ => None,
            }) {
                updated = f(req);
            }
        }
        let ty = if updated { "[Unavailable choice]" } else { "[Invalid choice]" };
        out.push(format!("sideupdate\np{}\n|error|{ty} {msg}", p + 1));
        if updated {
            self.emit_request(p, true, out);
        }
        false
    }

    /// `Side#clearChoice`.
    fn clear_choice(&mut self, p: usize) {
        let (mut forced_switches, mut forced_passes) = (0, 0);
        if self.battle_request_state() == ReqState::Switch {
            let b = self.b.as_ref().unwrap();
            let sd = &b.sides[p];
            let can_out = sd.active.switch_flag as u32;
            let can_in = (1..sd.n as usize).filter(|&k| !sd.mons[sd.order[k] as usize].fainted()).count() as u32;
            forced_switches = can_out.min(can_in);
            forced_passes = can_out - forced_switches;
        }
        self.choice[p] = SideChoice { forced_switches_left: forced_switches, forced_passes_left: forced_passes, ..SideChoice::default() };
    }

    fn mon_name(&self, p: usize, pos: usize) -> String {
        let b = self.b.as_ref().unwrap();
        let slot = b.sides[p].order[pos] as usize;
        self.sides[p].as_ref().unwrap().infos[slot].name.clone()
    }

    fn active_name(&self, p: usize) -> String {
        self.mon_name(p, 0)
    }

    /// `Battle#choose(sideid, input)`.
    fn choose(&mut self, p: usize, input: &str, out: &mut Out) -> Result<bool, String> {
        if !self.side_choose(p, input, out)? {
            if self.choice[p].error.is_empty() {
                let msg = format!("Unknown error for choice: {input}. If you're not using a custom client, please report this as a bug.");
                self.choice_error(p, &msg, None, out);
            }
            return Ok(false);
        }
        if !self.is_choice_done(p) {
            let msg = format!("Incomplete choice: {input} - missing other pokemon");
            self.choice_error(p, &msg, None, out);
            return Ok(false);
        }
        if (0..2).all(|q| self.is_choice_done(q)) {
            self.commit_choices(out)?;
        }
        Ok(true)
    }

    fn is_choice_done(&mut self, p: usize) -> bool {
        if self.request_state(p) == ReqState::None {
            return true;
        }
        if self.choice[p].forced_switches_left > 0 {
            return false;
        }
        // (getChoiceIndex: a singles active never needs an automatic pass here.)
        !self.choice[p].actions.is_empty()
    }

    /// `Side#choose(input)`.
    fn side_choose(&mut self, p: usize, input: &str, out: &mut Out) -> Result<bool, String> {
        if self.request_state(p) == ReqState::None {
            let msg = if self.ended { "Can't do anything: The game is over" } else { "Can't do anything: It's not your turn" };
            return Ok(self.choice_error(p, msg, None, out));
        }
        if self.choice[p].cant_undo {
            return Ok(self.choice_error(p, "Can't undo: A trapping/disabling effect would cause undo to leak information", None, out));
        }
        self.clear_choice(p);
        let strings: Vec<&str> = if input.starts_with("team ") { vec![input] } else { input.split(',').collect() };
        if strings.len() > 1 {
            let msg = format!("Can't make choices: You sent choices for {} Pokémon, but this is a singles game!", strings.len());
            return Ok(self.choice_error(p, &msg, None, out));
        }
        for cs in strings {
            let (ty, data) = split_first(js_trim(cs), ' ');
            let (mut ty, mut data) = (ty.to_string(), js_trim(data).to_string());
            if ty == "testfight" {
                ty = "move".into();
                data = "testfight".into();
            }
            match ty.as_str() {
                "move" => {
                    let original = data.clone();
                    let mut target_loc: Option<i64> = None;
                    let mut event = "";
                    loop {
                        if ends_with_target(&data) && to_id(&data) != "conversion2" {
                            if target_loc.is_some() {
                                return Ok(self.choice_error(p, &format!("Conflicting arguments for \"move\": {original}"), None, out));
                            }
                            let tail: String = data.chars().rev().take(2).collect::<Vec<_>>().into_iter().rev().collect();
                            target_loc = Some(js_parse_int(&tail).unwrap_or(0));
                            let keep = data.chars().count() - 2;
                            data = js_trim(&data.chars().take(keep).collect::<String>()).to_string();
                            continue;
                        }
                        let mut found = None;
                        for (suffix, ev) in [
                            (" mega", "mega"),
                            (" megax", "megax"),
                            (" megay", "megay"),
                            (" zmove", "zmove"),
                            (" ultra", "ultra"),
                            (" dynamax", "dynamax"),
                            (" gigantamax", "dynamax"),
                            (" max", "dynamax"),
                            (" terastal", "terastallize"),
                            (" terastallize", "terastallize"),
                        ] {
                            if data.ends_with(suffix) {
                                found = Some((suffix.len(), ev));
                                break;
                            }
                        }
                        match found {
                            Some((n, ev)) => {
                                if !event.is_empty() {
                                    return Ok(self.choice_error(p, &format!("Conflicting arguments for \"move\": {original}"), None, out));
                                }
                                event = ev;
                                data.truncate(data.len() - n);
                            }
                            None => break,
                        }
                    }
                    if !self.choose_move(p, Some(&data), target_loc.unwrap_or(0), event, out) {
                        return Ok(false);
                    }
                }
                "switch" => {
                    if !self.choose_switch(p, &data, out) {
                        return Ok(false);
                    }
                }
                "shift" => {
                    if !data.is_empty() {
                        return Ok(self.choice_error(p, &format!("Unrecognized data after \"shift\": {data}"), None, out));
                    }
                    if !self.choose_shift(p, out) {
                        return Ok(false);
                    }
                }
                "team" => {
                    return Ok(self.choice_error(p, "Can't choose for Team Preview: You're not in a Team Preview phase", None, out));
                }
                "pass" | "skip" => {
                    if !data.is_empty() {
                        return Ok(self.choice_error(p, &format!("Unrecognized data after \"pass\": {data}"), None, out));
                    }
                    if !self.choose_pass(p, out) {
                        return Ok(false);
                    }
                }
                "auto" | "default" => {
                    if !self.auto_choose(p, out)? {
                        return Ok(false);
                    }
                }
                _ => {
                    self.choice_error(p, &format!("Unrecognized choice: {cs}"), None, out);
                }
            }
        }
        Ok(self.choice[p].error.is_empty())
    }

    /// `Side#autoChoose`.
    fn auto_choose(&mut self, p: usize, out: &mut Out) -> Result<bool, String> {
        match self.request_state(p) {
            ReqState::Switch => {
                let mut i = 0;
                while !self.is_choice_done(p) {
                    if !self.choose_switch(p, "", out) {
                        return Err(format!("Error: autoChoose switch crashed: {}", self.choice[p].error));
                    }
                    i += 1;
                    if i > 10 {
                        return Err("Error: autoChoose failed: infinite looping".into());
                    }
                }
            }
            ReqState::Move => {
                let mut i = 0;
                while !self.is_choice_done(p) {
                    if !self.choose_move(p, None, 0, "", out) {
                        return Err(format!("Error: autoChoose crashed: {}", self.choice[p].error));
                    }
                    i += 1;
                    if i > 10 {
                        return Err("Error: autoChoose failed: infinite looping".into());
                    }
                }
            }
            ReqState::None => {}
        }
        Ok(true)
    }

    /// `Side#choosePass`.
    fn choose_pass(&mut self, p: usize, out: &mut Out) -> bool {
        if !self.choice[p].actions.is_empty() {
            return false;
        }
        let b = self.b.as_ref().unwrap();
        let fainted = b.sides[p].mon().fainted();
        match self.request_state(p) {
            ReqState::Switch => {
                if b.sides[p].active.switch_flag {
                    if self.choice[p].forced_passes_left == 0 {
                        let msg = format!("Can't pass: You need to switch in a Pokémon to replace {}", self.active_name(p));
                        return self.choice_error(p, &msg, None, out);
                    }
                    self.choice[p].forced_passes_left -= 1;
                }
            }
            ReqState::Move => {
                if !fainted {
                    let msg = format!("Can't pass: Your {} must make a move (or switch)", self.active_name(p));
                    return self.choice_error(p, &msg, None, out);
                }
            }
            ReqState::None => return self.choice_error(p, "Can't pass: Not a move or switch request", None, out),
        }
        self.choice[p].actions.push(Act::Pass);
        true
    }

    /// `Side#chooseShift` (singles: always an error).
    fn choose_shift(&mut self, p: usize, out: &mut Out) -> bool {
        let index = self.choice[p].actions.len();
        if index >= 1 {
            return self.choice_error(p, &format!("Can't shift: You do not have a Pokémon in slot {}", index + 1), None, out);
        }
        if self.request_state(p) != ReqState::Move {
            return self.choice_error(p, "Can't shift: You can only shift during a move phase", None, out);
        }
        self.choice_error(p, "Can't shift: You can only shift to the center in triples", None, out)
    }

    /// `Side#chooseSwitch(slotText)`.
    fn choose_switch(&mut self, p: usize, slot_text: &str, out: &mut Out) -> bool {
        let rs = self.request_state(p);
        if rs != ReqState::Move && rs != ReqState::Switch {
            return self.choice_error(p, &format!("Can't switch: You need a {} response", rs.name()), None, out);
        }
        let index = self.choice[p].actions.len();
        if index >= 1 {
            let msg = if rs == ReqState::Switch {
                "Can't switch: You sent more switches than Pokémon that need to switch"
            } else {
                "Can't switch: You sent more choices than unfainted Pokémon"
            };
            return self.choice_error(p, msg, None, out);
        }
        let b = self.b.as_ref().unwrap();
        let sd = &b.sides[p];
        let n = sd.n as usize;
        let fainted_at = |k: usize| sd.mons[sd.order[k] as usize].fainted();
        let slot: i64;
        if slot_text.is_empty() {
            if rs != ReqState::Switch {
                return self.choice_error(p, "Can't switch: You need to select a Pokémon to switch in", None, out);
            }
            if self.choice[p].forced_switches_left == 0 {
                return self.choose_pass(p, out);
            }
            let mut k = 1;
            while k < n && (self.choice[p].switch_ins.contains(&k) || fainted_at(k)) {
                k += 1;
            }
            slot = k as i64;
        } else {
            slot = js_parse_int(slot_text).map_or(-1, |v| v - 1);
        }
        let mut slot = slot;
        if slot < 0 {
            slot = -1;
            let names = &self.sides[p].as_ref().unwrap().infos;
            for k in 0..n {
                let o = sd.order[k] as usize;
                let species_id = kanto_data::gen1::SPECIES[sd.mons[o].species as usize].id;
                if slot_text.to_lowercase() == names[o].name.to_lowercase() || to_id(slot_text) == species_id {
                    slot = k as i64;
                    break;
                }
            }
            if slot < 0 {
                return self.choice_error(p, &format!("Can't switch: You do not have a Pokémon named \"{slot_text}\" to switch to"), None, out);
            }
        }
        let slot = slot as usize;
        if slot >= n {
            return self.choice_error(p, &format!("Can't switch: You do not have a Pokémon in slot {} to switch to", slot + 1), None, out);
        } else if slot < 1 {
            return self.choice_error(p, "Can't switch: You can't switch to an active Pokémon", None, out);
        } else if self.choice[p].switch_ins.contains(&slot) {
            return self.choice_error(p, &format!("Can't switch: The Pokémon in slot {} can only switch in once", slot + 1), None, out);
        }
        if fainted_at(slot) {
            return self.choice_error(p, "Can't switch: You can't switch to a fainted Pokémon", None, out);
        }
        if rs == ReqState::Move {
            if sd.mon().trapped() {
                fn trap(req: &mut Obj) -> bool {
                    let mut updated = false;
                    if req.delete("maybeTrapped") {
                        updated = true;
                    }
                    if !req.get("trapped").is_some_and(Value::truthy) {
                        req.set("trapped", Value::Bool(true));
                        updated = true;
                    }
                    updated
                }
                return self.choice_error(p, "Can't switch: The active Pokémon is trapped", Some(trap), out);
            }
        } else {
            if self.choice[p].forced_switches_left == 0 {
                return self.choice_error(p, "Player somehow switched too many Pokemon", None, out);
            }
            self.choice[p].forced_switches_left -= 1;
        }
        self.choice[p].switch_ins.push(slot);
        self.choice[p].actions.push(Act::Switch { pos: slot });
        true
    }

    /// `Side#chooseMove(moveText, targetLoc, event)` for the Gen 1 singles active.
    fn choose_move(&mut self, p: usize, move_text: Option<&str>, target_loc: i64, event: &'static str, out: &mut Out) -> bool {
        let rs = self.request_state(p);
        if rs != ReqState::Move {
            return self.choice_error(p, &format!("Can't move: You need a {} response", rs.name()), None, out);
        }
        if !self.choice[p].actions.is_empty() {
            return self.choice_error(p, "Can't move: You sent more choices than unfainted Pokémon.", None, out);
        }
        let auto = move_text.is_none_or(str::is_empty);
        let name = self.active_name(p);
        // pokemon.getMoveRequestData() (side effects already applied by makeRequest).
        let req = {
            let b = self.b.as_ref().unwrap();
            let j = b.request_json(&self.log, p).expect("move request");
            Value::parse(&j).expect("request JSON")
        };
        let req_moves: Vec<Value> = req.get("active").and_then(Value::as_arr).and_then(|a| a.first()).and_then(|a| a.get("moves")).and_then(Value::as_arr).cloned().unwrap_or_default();
        let rm_id = |i: usize| req_moves[i].get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let rm_target = |i: usize| req_moves[i].get("target").and_then(Value::as_str).map(str::to_string);
        let rm_disabled = |i: usize| req_moves[i].get("disabled").is_some_and(Value::truthy);
        let mut move_slot: Option<usize> = None;
        let mut moveid: String;
        let mut target_type: Option<String> = None;
        let text = if auto { "1".to_string() } else { move_text.unwrap().to_string() };
        if !text.is_empty() && text.bytes().all(|c| c.is_ascii_digit()) {
            let idx = text.parse::<f64>().unwrap_or(f64::INFINITY) - 1.0;
            if idx < 0.0 || idx >= req_moves.len() as f64 {
                return self.choice_error(p, &format!("Can't move: Your {name} doesn't have a move {}", js_number(idx + 1.0)), None, out);
            }
            let i = idx as usize;
            move_slot = Some(i);
            moveid = rm_id(i);
            target_type = rm_target(i);
        } else {
            moveid = to_id(&text);
            if moveid.starts_with("hiddenpower") {
                moveid = "hiddenpower".into();
            }
            for i in 0..req_moves.len() {
                if rm_id(i) != moveid {
                    continue;
                }
                move_slot = Some(i);
                target_type = Some(rm_target(i).unwrap_or_else(|| "normal".into()));
                break;
            }
            if target_type.is_none() && moveid != "testfight" {
                return self.choice_error(p, &format!("Can't move: Your {name} doesn't have a move matching {moveid}"), None, out);
            }
        }
        // pokemon.getMoves(): the usable move slots, or [] if none is usable.
        let b = self.b.as_ref().unwrap();
        let sd = &b.sides[p];
        let slots: Vec<(String, bool)> = (0..(sd.active.n_slots as usize).min(4))
            .map(|i| {
                let s = sd.slot(i);
                (kanto_gen1::state::move_id_str(s.id).to_string(), s.disabled() || s.pp() == 0)
            })
            .collect();
        let moves: Vec<(String, bool)> = if slots.iter().any(|s| !s.1) { slots.clone() } else { Vec::new() };
        if auto {
            for i in 0..req_moves.len() {
                if rm_disabled(i) {
                    continue;
                }
                if i < moves.len() && rm_id(i) == moves[i].0 && moves[i].1 {
                    continue;
                }
                moveid = rm_id(i);
                move_slot = Some(i);
                target_type = rm_target(i);
                break;
            }
        }
        let dex_move = DexMove::get(&moveid);
        if event == "zmove" {
            return self.choice_error(p, &format!("Can't move: {name} can't use {} as a Z-move", dex_move.name), None, out);
        }
        let max_move = if event == "dynamax" { dex_move.max_move() } else { None };
        if event == "dynamax" && max_move.is_none() {
            return self.choice_error(p, &format!("Can't move: {name} can't use {} as a Max Move", dex_move.name), None, out);
        }
        if let Some(m) = max_move {
            target_type = Some(m.1.to_string());
        }
        let mut target_loc = target_loc;
        if auto || moveid == "testfight" {
            target_loc = 0;
        } else if target_type.as_deref().is_some_and(|t| matches!(t, "normal" | "any" | "adjacentAlly" | "adjacentAllyOrSelf" | "adjacentFoe")) {
            if !valid_target_loc(target_loc, target_type.as_deref().unwrap()) {
                return self.choice_error(p, &format!("Can't move: Invalid target for {}", dex_move.name), None, out);
            }
        } else if target_loc != 0 {
            return self.choice_error(p, &format!("Can't move: You can't choose a target for {}", dex_move.name), None, out);
        }
        let mon = sd.mon();
        let locked = b.locked_move(p).or_else(|| b.semi_locked_move(p, false));
        if let Some(l) = locked {
            let lid = if l == kanto_gen1::req::RECHARGE { "recharge".to_string() } else { kanto_gen1::state::move_id_str(l).to_string() };
            if mon.maybe_locked() {
                self.choice[p].cant_undo = true;
            }
            self.choice[p].actions.push(Act::Move { moveid: lid, native: Choice::Move(1) });
            return true;
        } else if matches!(mon.status, st::FRZ | st::SLP) || sd.active.has(v::PARTIALLYTRAPPED) {
            if mon.maybe_locked() {
                self.choice[p].cant_undo = true;
            }
            self.choice[p].actions.push(Act::Move { moveid: "fight".into(), native: Choice::Move(1) });
            return true;
        } else if moves.is_empty() {
            let id = self.ident(p);
            out.push(format!("sideupdate\np{}\n|-activate|{id}|move: Struggle", p + 1));
            if mon.maybe_locked() {
                self.choice[p].cant_undo = true;
            }
            self.choice[p].actions.push(Act::Move { moveid: "struggle".into(), native: Choice::Move(1) });
            return true;
        } else if moveid == "testfight" {
            if !mon.maybe_locked() {
                return self.choice_error(p, &format!("Can't move: {name}'s Fight button is known to be safe"), None, out);
            }
            // updateRequestForPokemon(pokemon, updateDisabledRequest): maybeLocked goes.
            self.b.as_mut().unwrap().sides[p].mon_mut().set_maybe_locked(false);
            if let Some(r) = self.active_request[p].as_mut().and_then(|r| r.as_obj_mut()).and_then(|r| r.get_mut("active")).and_then(|a| match a {
                Value::Arr(a) => a.get_mut(0).and_then(Value::as_obj_mut),
                _ => None,
            }) {
                r.delete("maybeLocked");
            }
            self.emit_request(p, true, out);
            self.choice[p].error = "Hack to avoid sending error messages to the client :D".into();
            return false;
        } else if let Some(m) = max_move {
            // maxMoveDisabled(move): `!pokemon.getMoveData(move.id)?.pp` (the first slot with it).
            let pp_zero = match slots.iter().position(|s| s.0 == dex_move.id) {
                Some(i) => sd.slot(i).pp() == 0,
                None => true,
            };
            if pp_zero {
                return self.choice_error(p, &format!("Can't move: {name}'s {} is disabled", m.0), None, out);
            }
        } else {
            let slot = move_slot.unwrap_or(0);
            if moves.get(slot).is_some_and(|m| m.1) {
                return self.choice_error(p, &format!("Can't move: {name}'s {} is disabled", dex_move.name), None, out);
            }
        }
        match event {
            "mega" => return self.choice_error(p, &format!("Can't move: {name} can't mega evolve"), None, out),
            "megax" => return self.choice_error(p, &format!("Can't move: {name} can't mega evolve X"), None, out),
            "megay" => return self.choice_error(p, &format!("Can't move: {name} can't mega evolve Y"), None, out),
            "ultra" => return self.choice_error(p, &format!("Can't move: {name} can't ultra burst"), None, out),
            "dynamax" => return self.choice_error(p, "Can't move: Dynamaxing doesn't outside of Gen 8.", None, out),
            "terastallize" => return self.choice_error(p, &format!("Can't move: {name} can't Terastallize."), None, out),
            _ => {}
        }
        let Some(slot) = move_slot else {
            return self.choice_error(p, "moveSlot should have been set by this point", None, out);
        };
        if mon.maybe_disabled() {
            self.choice[p].cant_undo = true;
        }
        let _ = target_loc;
        self.choice[p].actions.push(Act::Move { moveid, native: Choice::Move(slot as u8 + 1) });
        true
    }

    /// `pokemon.toString()` of `p`'s active: `p1a: Name`.
    fn ident(&self, p: usize) -> String {
        format!("p{}a: {}", p + 1, self.active_name(p))
    }

    /// `Battle#undoChoice(sideid)`.
    fn undo_choice(&mut self, p: usize, out: &mut Out) {
        if self.request_state(p) == ReqState::None {
            return;
        }
        if self.choice[p].cant_undo {
            self.choice_error(p, "Can't undo: A trapping/disabling effect would cause undo to leak information", None, out);
            return;
        }
        let mut updated = false;
        if self.request_state(p) == ReqState::Move {
            let has_move = self.choice[p].actions.iter().any(|a| matches!(a, Act::Move { .. }));
            if has_move && self.b.as_ref().unwrap().sides[p].mon().maybe_locked() {
                self.b.as_mut().unwrap().sides[p].mon_mut().set_maybe_locked(false);
                if let Some(r) = self.active_request[p].as_mut().and_then(|r| r.as_obj_mut()).and_then(|r| r.get_mut("active")).and_then(|a| match a {
                    Value::Arr(a) => a.get_mut(0).and_then(Value::as_obj_mut),
                    _ => None,
                }) {
                    r.delete("maybeLocked");
                }
                updated = true;
            }
        }
        self.clear_choice(p);
        if updated {
            self.emit_request(p, true, out);
        }
    }

    /// `Side#getChoice`.
    fn get_choice(&self, p: usize) -> String {
        let parts: Vec<String> = self.choice[p]
            .actions
            .iter()
            .map(|a| match a {
                Act::Move { moveid, .. } => format!("move {moveid}"),
                Act::Switch { pos } => format!("switch {}", pos + 1),
                Act::Pass => "pass".into(),
            })
            .collect();
        parts.join(", ")
    }

    /// `Battle#commitChoices` (the native battle runs the turn).
    fn commit_choices(&mut self, out: &mut Out) -> Result<(), String> {
        for p in 0..2 {
            let c = self.get_choice(p);
            if !c.is_empty() {
                self.input_log.push(format!(">p{} {c}", p + 1));
            }
        }
        // Side#commitChoices (Gen 1) throws when `lastSelectedMoveSlot` points past the active's
        // move slots for a 'fight' (asleep/frozen) or locked move: Showdown 0.11.11 crashes there
        // (docs/SIM_STATUS.md, "Showdown crashes"), and so does the stream, for the same input.
        {
            let b = self.b.as_ref().unwrap();
            for p in 0..2 {
                if let Some(Act::Move { moveid, native }) = self.choice[p].actions.first() {
                    let sd = &b.sides[p];
                    let normal = matches!(native, Choice::Move(_)) && moveid != "fight" && moveid != "struggle" && b.locked_move(p).or_else(|| b.semi_locked_move(p, false)).is_none();
                    let fight_asleep = moveid == "fight" && matches!(sd.mon().status, st::FRZ | st::SLP);
                    let locked = moveid != "fight" && moveid != "struggle" && !normal;
                    if (fight_asleep || locked) && sd.last_selected_move_slot as usize >= sd.active.n_slots as usize {
                        return Err("Error: moveSlot is null which shouldn't happen".into());
                    }
                }
            }
        }
        let native = |c: &SideChoice| match c.actions.first() {
            Some(Act::Move { native, .. }) => *native,
            Some(Act::Switch { pos }) => Choice::Switch(*pos as u8 + 1),
            _ => Choice::Pass,
        };
        let (c1, c2) = (native(&self.choice[0]), native(&self.choice[1]));
        let mut sink = ProtoSink { log: std::mem::take(&mut self.log), events: std::mem::take(&mut self.events) };
        let r = self.b.as_mut().unwrap().apply_choices(c1, c2, &mut sink);
        self.log = sink.log;
        self.events.clear();
        r.map_err(|e| format!("internal error: the native simulator rejected {e} (please report)"))?;
        self.after_step(out);
        if self.log.lines.len() - self.sent_log_pos > 500 {
            self.send_updates(out);
        }
        Ok(())
    }

    /// `Battle#win(side)`: `side` is the `>forcewin` argument (`getSide` reads its second
    /// character), `None` a tie.
    fn win(&mut self, side: Option<&str>) -> bool {
        if self.ended {
            return false;
        }
        let w = side.and_then(|s| s.chars().nth(1)).and_then(|c| c.to_digit(10)).map(|d| d as i64 - 1).filter(|&k| (0..2).contains(&k) && self.sides[k as usize].is_some());
        self.winner = w.map(|k| self.sides[k as usize].as_ref().unwrap().name.clone()).unwrap_or_default();
        self.log.add("|".into());
        match w {
            Some(_) => self.log.add(format!("|win|{}", self.winner)),
            None => self.log.add("|tie".into()),
        }
        self.ended = true;
        if let Some(b) = self.b.as_mut() {
            b.ended = true;
            b.winner = match w {
                Some(k) => k as u8 + 1,
                None => 3,
            };
            for sd in b.sides.iter_mut() {
                sd.request = Request::None;
            }
        }
        self.active_request = [None, None];
        true
    }

    /// `Battle#lose(side)` (singles: the foe wins).
    fn lose(&mut self, side: &str) {
        let k = side.chars().nth(1).and_then(|c| c.to_digit(10)).map(|d| d as i64 - 1);
        let Some(k) = k.filter(|&k| (0..2).contains(&k) && self.sides[k as usize].is_some()) else { return };
        if !self.started {
            // side.foe is set by start(): before that win(null) is a tie.
            self.win(None);
            return;
        }
        let foe = format!("p{}", 2 - k);
        self.win(Some(&foe));
    }

    /// `Battle#tiebreak`.
    fn tiebreak(&mut self) {
        if self.ended {
            return;
        }
        self.input_log.push(">tiebreak".into());
        self.log.add("|message|Time's up! Going to tiebreaker...".into());
        let names: Vec<String> = (0..2).map(|p| self.sides[p].as_ref().map(|s| s.name.clone()).unwrap_or_default()).collect();
        let b = self.b.as_ref();
        let team = |p: usize| -> Vec<(bool, f64, f64)> {
            match b {
                Some(b) => {
                    let sd = &b.sides[p];
                    (0..sd.n as usize).map(|i| {
                        let m = &sd.mons[sd.order[i] as usize];
                        (m.fainted(), m.hp as f64, m.stats[0] as f64)
                    }).collect()
                }
                None => Vec::new(),
            }
        };
        let teams = [team(0), team(1)];
        let not_fainted: Vec<usize> = teams.iter().map(|t| t.iter().filter(|m| !m.0).count()).collect();
        self.log.add(format!("|-message|{}", (0..2).map(|i| format!("{}: {} Pokemon left", names[i], not_fainted[i])).collect::<Vec<_>>().join("; ")));
        let max_nf = *not_fainted.iter().max().unwrap();
        let mut tied: Vec<usize> = (0..2).filter(|&i| not_fainted[i] == max_nf).collect();
        if tied.len() <= 1 {
            let s = format!("p{}", tied[0] + 1);
            self.win(Some(&s));
            return;
        }
        let pct: Vec<f64> = tied.iter().map(|&i| teams[i].iter().map(|m| m.1 / m.2).fold(0.0, |a, b| a + b) * 100.0 / 6.0).collect();
        self.log.add(format!("|-message|{}", tied.iter().enumerate().map(|(k, &i)| format!("{}: {}% total HP left", names[i], js_round(pct[k]))).collect::<Vec<_>>().join("; ")));
        let max_p = pct.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let keep: Vec<usize> = tied.iter().enumerate().filter(|(k, _)| pct[*k] == max_p).map(|(_, &i)| i).collect();
        tied = keep;
        if tied.len() <= 1 {
            let s = format!("p{}", tied[0] + 1);
            self.win(Some(&s));
            return;
        }
        let tot: Vec<f64> = tied.iter().map(|&i| teams[i].iter().map(|m| m.1).fold(0.0, |a, b| a + b)).collect();
        self.log.add(format!("|-message|{}", tied.iter().enumerate().map(|(k, &i)| format!("{}: {} total HP left", names[i], js_round(tot[k]))).collect::<Vec<_>>().join("; ")));
        let max_t = tot.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let keep: Vec<usize> = tied.iter().enumerate().filter(|(k, _)| tot[*k] == max_t).map(|(_, &i)| i).collect();
        if keep.len() <= 1 {
            let s = format!("p{}", keep[0] + 1);
            self.win(Some(&s));
            return;
        }
        self.win(None);
    }
}

/// `Math.round`.
fn js_round(x: f64) -> String {
    js_number((x + 0.5).floor())
}

/// JS `String#trim` (whitespace and line terminators).
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// `/\s(?:-|\+)?[1-3]$/.test(data)`.
fn ends_with_target(data: &str) -> bool {
    let c: Vec<char> = data.chars().collect();
    let n = c.len();
    if n < 2 || !matches!(c[n - 1], '1'..='3') {
        return false;
    }
    if c[n - 2].is_whitespace() || c[n - 2] == '\u{feff}' {
        return true;
    }
    n >= 3 && matches!(c[n - 2], '-' | '+') && (c[n - 3].is_whitespace() || c[n - 3] == '\u{feff}')
}

/// `Battle#validTargetLoc` for a singles active (`sourceLoc` = -1).
fn valid_target_loc(target_loc: i64, target_type: &str) -> bool {
    if target_loc == 0 {
        return true;
    }
    if target_loc.abs() > 1 {
        return false;
    }
    let source_loc = -1i64;
    let is_self = source_loc == target_loc;
    let is_foe = target_loc > 0;
    let across = -(1 + 1 - target_loc);
    let adjacent = if target_loc > 0 { (across - source_loc).abs() <= 1 } else { (target_loc - source_loc).abs() == 1 };
    match target_type {
        "randomNormal" | "scripted" | "normal" => adjacent,
        "adjacentAlly" => adjacent && !is_foe,
        "adjacentAllyOrSelf" => (adjacent && !is_foe) || is_self,
        "adjacentFoe" => adjacent && is_foe,
        "any" => !is_self,
        _ => false,
    }
}

/// `dex.moves.get(id)` as far as `chooseMove` reads it.
struct DexMove {
    id: String,
    name: String,
    /// Type name and whether it is a status move (for `getMaxMove`); `None` = no such move.
    kind: Option<(&'static str, bool)>,
    struggle: bool,
}

impl DexMove {
    fn get(id: &str) -> DexMove {
        if let Some(k) = MOVES.iter().position(|m| m.id == id && !m.id.is_empty()) {
            let m = &MOVES[k];
            return DexMove { id: id.to_string(), name: m.name.to_string(), kind: Some((TYPE_NAMES[m.type_ as usize], m.category == 2)), struggle: id == "struggle" };
        }
        // Not a Gen 1 move: Showdown's dex has every move, but only the name matters here.
        let name = super::dex::MOVES.binary_search_by(|(k, _)| (*k).cmp(id)).ok().map(|i| super::dex::MOVES[i].1.to_string()).unwrap_or_else(|| id.to_string());
        DexMove { id: id.to_string(), name, kind: None, struggle: false }
    }

    /// `getMaxMove(move)`: (name, target).
    fn max_move(&self) -> Option<(&'static str, &'static str)> {
        if self.struggle {
            return Some(("Struggle", "randomNormal"));
        }
        let (ty, status) = self.kind?;
        if status {
            return Some(("Max Guard", "self"));
        }
        let name = match ty {
            "Flying" => "Max Airstream",
            "Fire" => "Max Flare",
            "Bug" => "Max Flutterby",
            "Water" => "Max Geyser",
            "Ice" => "Max Hailstorm",
            "Fighting" => "Max Knuckle",
            "Electric" => "Max Lightning",
            "Psychic" => "Max Mindstorm",
            "Poison" => "Max Ooze",
            "Grass" => "Max Overgrowth",
            "Ghost" => "Max Phantasm",
            "Ground" => "Max Quake",
            "Rock" => "Max Rockfall",
            "Normal" => "Max Strike",
            "Dragon" => "Max Wyrmwind",
            _ => return None,
        };
        Some((name, "adjacentFoe"))
    }
}

/// `PRNG.generateSeed()`: a random sodium seed (`sodium,` + 32 hex digits).
fn generate_seed() -> String {
    let mut bytes = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes)).is_ok();
    if !ok {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        bytes = t.to_le_bytes();
    }
    let mut s = String::from("sodium,");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// `new PRNG(seed)`.
fn parse_seed(seed: &str) -> Result<Prng, String> {
    if let Some(hex) = seed.strip_prefix("sodium,") {
        let hex: String = hex.split(',').next().unwrap_or("").to_string();
        let padded = format!("{hex:0<64}");
        return Prng::sodium_hex(&padded[..64.min(padded.len())]).ok_or_else(|| format!("Error: Unrecognized RNG seed {seed}"));
    }
    if seed.starts_with("gen5,") {
        return Prng::parse(seed).ok_or_else(|| format!("Error: Unrecognized RNG seed {seed}"));
    }
    if seed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        let parts: Vec<&str> = seed.split(',').collect();
        if parts.len() == 4 {
            let mut w = [0u16; 4];
            for (i, p) in parts.iter().enumerate() {
                let v = sets::js_number_parse(p);
                if !(0.0..=65535.0).contains(&v) || v.fract() != 0.0 {
                    return Err(format!("seed word {p} is not supported (0-65535)"));
                }
                w[i] = v as u16;
            }
            return Ok(Prng::gen5(w));
        }
        return Err(format!("seed {seed} is not supported (four 16-bit words)"));
    }
    Err(format!("Error: Unrecognized RNG seed {seed}"))
}
