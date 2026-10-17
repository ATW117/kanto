//! Client-side observations: the env's observation and action mask for one player, rebuilt from
//! the Showdown messages a server sends that player (docs/CLIENT.md).
//!
//! A [`ClientBattle`] consumes what a Showdown server sends one player: battle-log updates (one
//! `update` message per resolved decision) and `|request|` JSON. It keeps the same backend-agnostic
//! [`View`] and observer [`Memory`] the env keeps, filled from public information only, and encodes
//! them with the env's own encoder ([`obs::encode`]). For a player of a kanto battle, its output
//! equals the env's observation and mask for that player at every step (tested against
//! `ProtoGame`, which produces the protocol and the env observations of the same battle).
//!
//! Opponent Pokémon are indexed by order of first appearance (the encoder only exposes that order,
//! so the observation does not depend on the opponent's hidden team order).
//!
//! Known limits: after the observer's own Transform, its modified stats are the target's, which
//! the protocol does not reveal; they are estimated from the target's species (max DVs/stat exp).

use serde_json::Value;

use crate::backend::NUM_ACTIONS;
use crate::data;
use crate::memory::{Memory, TurnEvents, UNREVEALED};
use crate::obs::{self, status, vol, View, OBS_DIM};
use crate::protocol::cant;

const STRUGGLE: u8 = 165;

/// Gen 1 boost multipliers (`modifyStat` with Showdown's table).
fn boosted(stored: u16, stage: i8) -> u16 {
    let m = if stage >= 0 {
        [1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0][stage as usize]
    } else {
        [100.0, 66.0, 50.0, 40.0, 33.0, 28.0, 25.0][(-stage) as usize] / 100.0
    };
    modify(stored, m)
}

/// `modifyStat`: `max(1, floor(stat * modifier))`.
fn modify(stat: u16, m: f64) -> u16 {
    ((stat as f64 * m).floor() as i64).max(1) as u16
}

/// Showdown boost id -> index in the simulator's `[atk, def, spa, spd, spe, accuracy, evasion]`.
fn boost_index(s: &str) -> Option<usize> {
    Some(match s {
        "atk" => 0,
        "def" => 1,
        "spa" => 2,
        "spd" => 3,
        "spe" => 4,
        "accuracy" => 5,
        "evasion" => 6,
        _ => return None,
    })
}

/// A battle-log event line (as opposed to room, chat, timer and header lines).
fn is_event(line: &str) -> bool {
    let cmd = line.split('|').nth(1).unwrap_or("");
    cmd.starts_with('-')
        || matches!(
            cmd,
            "start" | "switch" | "drag" | "replace" | "detailschange" | "move" | "cant" | "faint" | "turn" | "upkeep" | "win" | "tie"
        )
}

fn set_bit(v: &mut u32, bit: u32, on: bool) {
    if on {
        *v |= bit;
    } else {
        *v &= !bit;
    }
}

/// What the observer has to decide now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    /// No request yet, or the battle is over.
    None,
    /// A move request (moves and/or switches).
    Move,
    /// A forced switch.
    Switch,
    /// Waiting for the opponent.
    Wait,
}

/// Everything public a [`ClientBattle`] knows, in absolute player order (index = player). The
/// input of state reconstruction (`kanto_backend::determinize`).
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub me: usize,
    pub turn: u16,
    /// The view the observation is encoded from (opponent Pokémon in order of appearance,
    /// opponent HP as percentages of 100, the observer's modified stats and both sides' boosts in
    /// the active blocks).
    pub view: View,
    /// Both players' memories (`mem[me]` the observer's, `mem[1 - me]` the opponent's).
    pub mem: [Memory; 2],
    /// Raw status per (player, party index): Toxic stays Toxic until a switch-in.
    pub raw_status: [[u8; 6]; 2],
    /// The paralysis/burn drops each Pokémon keeps on the bench.
    pub bench_par: [[bool; 6]; 2],
    pub bench_brn: [[bool; 6]; 2],
    /// Both actives' boosts in the simulator's order `[atk, def, spa, spd, spe, accuracy, evasion]`.
    pub boosts: [[i8; 7]; 2],
    /// The observer's active: stored and modified stats `[atk, def, spa, spd, spe]`.
    pub own_stored: [u16; 5],
    pub own_modified: [u16; 5],
    /// The observer's request order: position -> original party index.
    pub order: [u8; 6],
    pub n_order: u8,
    pub request: RequestKind,
    /// Hits of the current partial-trapping sequence per player.
    pub bind_hits: [u8; 2],
    /// Number of opponent Pokémon seen (they occupy appearance indices 0..n).
    pub opp_seen: u8,
}

/// Per-Pokémon state the client tracks beyond the [`View`].
#[derive(Clone, Copy, Debug, Default)]
struct MonExtra {
    /// Raw status: Toxic stays `TOX` until a switch-in turns it into regular poison.
    raw_status: u8,
    /// Last switch-out while alive re-applied the paralysis/burn drop (`bench_par`/`bench_brn`).
    bench_par: bool,
    bench_brn: bool,
}

/// The observer's own active Pokémon: Gen 1 stored/modified stats and boosts, as the simulator
/// keeps them (`[atk, def, spa, spd, spe]`; boosts add accuracy, evasion).
#[derive(Clone, Copy, Debug, Default)]
struct OwnActive {
    stored: [u16; 5],
    modified: [u16; 5],
    boosts: [i8; 7],
}

#[derive(Clone, Debug)]
pub struct ClientBattle {
    /// The observing player (0 = p1, 1 = p2), from the first request or `|player|` line.
    me: Option<usize>,
    name: String,
    view: View,
    mem: Memory,
    /// The opponent's memory: what it has learned about the observer's team (public, so exact);
    /// its public trackers equal `mem`'s.
    opp_mem: Memory,
    extra: [[MonExtra; 6]; 2],
    own: OwnActive,
    /// Opponent boosts in the simulator's order (accuracy/evasion included).
    opp_boosts: [i8; 7],
    /// Own Pokémon: ident name (after "p1: ") per original party index.
    own_names: Vec<String>,
    /// Opponent Pokémon: ident name per appearance index.
    opp_names: Vec<String>,
    /// The request in force (applied) and one received but not yet applied: Showdown sends a
    /// request before the update it follows, so it is applied after that update.
    request: Option<Value>,
    pending: Option<Value>,
    /// Request side order: position -> original party index.
    order: Vec<u8>,
    turn: u16,
    started: bool,
    ended: bool,
    /// The move being resolved: (user, move id, whether its stat change already re-applied the
    /// foe's paralysis/burn drop).
    last_move: Option<(usize, u8, bool)>,
    /// The observer transformed: its next full request shows the target's moves.
    transform_reveal: bool,
    /// Hits of the current partial-trapping sequence per side (0 = not binding).
    bind_hits: [u8; 2],
    /// The observer's move that called its charging two-turn move (Metronome), 0 if none.
    charge_src: u8,
}

impl Default for ClientBattle {
    fn default() -> Self {
        Self::new("")
    }
}

/// Parse `p1a: Name` / `p2: Name` into (player, name).
fn parse_ident(s: &str) -> Option<(usize, &str)> {
    let (pos, name) = s.split_once(": ")?;
    let p = match pos.get(..2)? {
        "p1" => 0,
        "p2" => 1,
        _ => return None,
    };
    Some((p, name))
}

/// Parse a condition: `"313/313"`, `"45/100 par"`, `"0 fnt"` -> (hp, max_hp, status token).
fn parse_condition(s: &str) -> (u16, u16, &str) {
    let mut it = s.split_whitespace();
    let hp = it.next().unwrap_or("0");
    let st = it.next().unwrap_or("");
    match hp.split_once('/') {
        Some((a, b)) => (a.parse().unwrap_or(0), b.parse().unwrap_or(0), st),
        None => (0, 0, if st.is_empty() { "fnt" } else { st }),
    }
}

fn status_code(tok: &str) -> u8 {
    match tok {
        "slp" => status::SLP,
        "psn" => status::PSN,
        "tox" => status::TOX,
        "brn" => status::BRN,
        "frz" => status::FRZ,
        "par" => status::PAR,
        _ => status::NONE,
    }
}

/// `Alakazam, L74, M` -> (species id, level).
fn parse_details(s: &str) -> (u8, u8) {
    let mut parts = s.split(", ");
    let species = data::species_id(parts.next().unwrap_or("")).unwrap_or(0);
    let mut level = 100;
    for p in parts {
        if let Some(l) = p.strip_prefix('L') {
            level = l.parse().unwrap_or(100);
        }
    }
    (species, level)
}

impl ClientBattle {
    /// A client for the player named `name` (used to find the seat from `|player|` lines; the
    /// first request also sets it).
    pub fn new(name: &str) -> Self {
        let mut view = View::default();
        view.sides[0].active.party_idx = UNREVEALED;
        view.sides[1].active.party_idx = UNREVEALED;
        ClientBattle {
            me: None,
            name: name.to_string(),
            view,
            mem: Memory::default(),
            opp_mem: Memory::default(),
            extra: [[MonExtra::default(); 6]; 2],
            own: OwnActive::default(),
            opp_boosts: [0; 7],
            own_names: Vec::new(),
            opp_names: Vec::new(),
            request: None,
            pending: None,
            order: Vec::new(),
            turn: 0,
            started: false,
            ended: false,
            last_move: None,
            bind_hits: [0; 2],
            transform_reveal: false,
            charge_src: 0,
        }
    }

    pub fn player(&self) -> Option<usize> {
        self.me
    }

    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Feed one server message for this battle (an update or a `|request|`; any mix of lines).
    /// Each message containing battle-log lines is one update, as one env step. A request is
    /// applied after the next update (Showdown sends it first) or by [`Self::flush`].
    pub fn feed(&mut self, chunk: &str) -> Result<(), String> {
        let mut log = Vec::new();
        for line in chunk.lines() {
            if let Some(json) = line.strip_prefix("|request|") {
                if !json.trim().is_empty() {
                    self.set_request(json)?;
                }
            } else if line.starts_with('|') {
                log.push(line);
            }
        }
        // Only battle events make an update (one env step); room messages (chat, timer, the
        // header before `|start|`) do not.
        if log.iter().any(|l| is_event(l)) {
            let turn_before = self.turn;
            self.update(&log);
            self.flush();
            if self.started {
                self.update_trackers(self.turn > turn_before && turn_before > 0);
            }
            // Public trackers are the same in both players' memories.
            self.opp_mem.track = self.mem.track;
            self.opp_mem.sleep_obs = self.mem.sleep_obs;
            self.opp_mem.last = self.mem.last;
        } else {
            self.header(&log);
        }
        Ok(())
    }

    /// Header lines outside an update: the seat and team sizes.
    fn header(&mut self, log: &[&str]) {
        for line in log {
            let parts: Vec<&str> = line.split('|').collect();
            match parts.get(1).copied() {
                Some("player") if parts.len() >= 4 => {
                    if self.me.is_none() && !self.name.is_empty() && parts[3] == self.name {
                        self.me = Some(if parts[2] == "p2" { 1 } else { 0 });
                    }
                }
                Some("teamsize") if parts.len() >= 4 => {
                    let p = if parts[2] == "p2" { 1 } else { 0 };
                    if p != self.me() || self.view.sides[p].size == 0 {
                        self.view.sides[p].size = parts[3].parse().unwrap_or(6);
                    }
                }
                _ => {}
            }
        }
    }

    /// Apply a request received without a following update (e.g. a resent request).
    pub fn flush(&mut self) {
        if let Some(req) = self.pending.take() {
            self.apply_request(req);
        }
    }

    /// Whether a request is waiting for its update.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    fn me(&self) -> usize {
        self.me.unwrap_or(0)
    }

    // ---- Requests ----------------------------------------------------------------------------

    fn set_request(&mut self, json: &str) -> Result<(), String> {
        let req: Value = serde_json::from_str(json).map_err(|e| format!("bad request JSON: {e}"))?;
        let side = &req["side"];
        if let Some(id) = side["id"].as_str() {
            self.me = Some(if id == "p2" { 1 } else { 0 });
        }
        let me = self.me();
        let mons = side["pokemon"].as_array().ok_or("request without side.pokemon")?;
        if self.own_names.is_empty() {
            // The first request lists the team in its original order.
            for (i, m) in mons.iter().enumerate().take(6) {
                let ident = m["ident"].as_str().unwrap_or("");
                let name = parse_ident(ident).map(|(_, n)| n.to_string()).unwrap_or_default();
                self.own_names.push(name);
                let (species, level) = parse_details(m["details"].as_str().unwrap_or(""));
                let (hp, max_hp, st) = parse_condition(m["condition"].as_str().unwrap_or(""));
                let s = &m["stats"];
                let g = |k: &str| s[k].as_u64().unwrap_or(0) as u16;
                let mv = &mut self.view.sides[me].mons[i];
                mv.species = species;
                mv.level = level;
                mv.types = data::SPECIES_TYPES[species as usize];
                mv.hp = hp;
                mv.max_hp = max_hp;
                mv.status = status_code(st);
                mv.stats = [max_hp, g("atk"), g("def"), g("spe"), g("spa")];
                if let Some(ms) = m["moves"].as_array() {
                    for (k, id) in ms.iter().enumerate().take(4) {
                        let mid = data::move_id(id.as_str().unwrap_or("")).unwrap_or(0);
                        mv.moves[k] = mid;
                        mv.pp[k] = data::max_pp(mid);
                    }
                }
                self.extra[me][i].raw_status = mv.status;
            }
            self.view.sides[me].size = self.own_names.len() as u8;
        }
        self.pending = Some(req);
        Ok(())
    }

    fn apply_request(&mut self, req: Value) {
        let me = self.me();
        let Some(mons) = req["side"]["pokemon"].as_array() else { return };
        self.order = mons
            .iter()
            .map(|m| {
                let n = parse_ident(m["ident"].as_str().unwrap_or("")).map(|(_, n)| n).unwrap_or("");
                self.own_names.iter().position(|x| x == n).unwrap_or(0) as u8
            })
            .collect();
        // Own HP from the request (exact; the log has it too).
        for (pos, m) in mons.iter().enumerate() {
            let i = self.order[pos] as usize;
            let (hp, max_hp, _) = parse_condition(m["condition"].as_str().unwrap_or(""));
            let mv = &mut self.view.sides[me].mons[i];
            mv.hp = hp;
            if max_hp > 0 {
                mv.max_hp = max_hp;
            }
        }
        // Being bound shows as a lone "Fight" choice (unless asleep/frozen): the foe binds.
        if let Some(act) = req["active"].as_array().and_then(|a| a.first()) {
            let ms = act["moves"].as_array().map(|v| v.as_slice()).unwrap_or(&[]);
            let a = self.view.sides[me].active.party_idx;
            let raw = if a != UNREVEALED { self.extra[me][a as usize].raw_status } else { status::NONE };
            if ms.len() == 1 && ms[0]["id"].as_str() == Some("fight") && !matches!(raw, status::SLP | status::FRZ) {
                self.view.sides[1 - me].active.volatiles |= vol::BINDING;
            }
        }
        // Active moves and PP (skip pseudo/locked entries without PP).
        if let Some(act) = req["active"].as_array().and_then(|a| a.first()) {
            if let Some(ms) = act["moves"].as_array() {
                let full = ms.iter().all(|m| m.get("pp").is_some());
                if full {
                    let a = &mut self.view.sides[me].active;
                    a.moves = [0; 4];
                    a.pp = [0; 4];
                    a.disabled_slot = 0;
                    for (k, m) in ms.iter().enumerate().take(4) {
                        a.moves[k] = data::move_id(m["id"].as_str().unwrap_or("")).unwrap_or(0);
                        a.pp[k] = m["pp"].as_u64().unwrap_or(0) as u8;
                    }
                    self.sync_base_pp();
                    if self.transform_reveal {
                        self.transform_reveal = false;
                        let fi = self.view.sides[1 - me].active.party_idx;
                        if fi != UNREVEALED {
                            for mv in self.view.sides[me].active.moves {
                                self.mem.see_move(fi, mv, false);
                            }
                        }
                    }
                }
            }
        }
        self.request = Some(req);
    }

    /// Copy the active's PP to its party (base) moves where the slot holds the base move.
    fn sync_base_pp(&mut self) {
        let me = self.me();
        let a = self.view.sides[me].active;
        if a.party_idx == UNREVEALED || a.volatiles & vol::TRANSFORM != 0 {
            return;
        }
        let m = &mut self.view.sides[me].mons[a.party_idx as usize];
        for k in 0..4 {
            if a.moves[k] != 0 && a.moves[k] == m.moves[k] {
                m.pp[k] = a.pp[k];
            }
        }
    }

    // ---- Actions -----------------------------------------------------------------------------

    /// The legal-action mask for the current request (`[true, false, ...]` when only passing).
    pub fn mask(&self) -> [bool; NUM_ACTIONS] {
        let mut mask = [false; NUM_ACTIONS];
        let Some(req) = &self.request else {
            mask[0] = true;
            return mask;
        };
        if self.ended || req["wait"].as_bool() == Some(true) {
            mask[0] = true;
            return mask;
        }
        let force = req["forceSwitch"].as_array().and_then(|a| a.first()).and_then(|v| v.as_bool()).unwrap_or(false);
        let mons = req["side"]["pokemon"].as_array();
        let mut trapped = false;
        if !force {
            if let Some(act) = req["active"].as_array().and_then(|a| a.first()) {
                trapped = act["trapped"].as_bool().unwrap_or(false);
                if let Some(ms) = act["moves"].as_array() {
                    for (k, m) in ms.iter().enumerate().take(4) {
                        if !m["disabled"].as_bool().unwrap_or(false) {
                            mask[k] = true;
                        }
                    }
                }
            }
        }
        if !trapped {
            if let Some(mons) = mons {
                for (pos, m) in mons.iter().enumerate() {
                    let active = m["active"].as_bool().unwrap_or(false);
                    let (hp, _, _) = parse_condition(m["condition"].as_str().unwrap_or(""));
                    if !active && hp > 0 {
                        mask[4 + self.order[pos] as usize] = true;
                    }
                }
            }
        }
        if !mask.iter().any(|&x| x) {
            mask[0] = true;
        }
        mask
    }

    /// The Showdown choice for action `a` (`"move 2"`, `"switch 3"`), or `None` if illegal or
    /// nothing is to be chosen.
    pub fn choice(&self, a: usize) -> Option<String> {
        let req = self.request.as_ref()?;
        if self.ended || req["wait"].as_bool() == Some(true) || a >= NUM_ACTIONS || !self.mask()[a] {
            return None;
        }
        if a < 4 {
            let force = req["forceSwitch"].as_array().is_some();
            if force {
                return None;
            }
            return Some(format!("move {}", a + 1));
        }
        let pos = self.order.iter().position(|&o| o as usize == a - 4)?;
        Some(format!("switch {}", pos + 1))
    }

    // ---- Observation -------------------------------------------------------------------------

    /// The env observation for the observing player.
    pub fn observe(&self, out: &mut [f32]) {
        let v = self.view();
        obs::encode(&v, self.me(), &self.mem, false, &mut out[..OBS_DIM]);
    }

    /// The memory (reveals and public trackers) of the observing player.
    pub fn memory(&self) -> &Memory {
        &self.mem
    }

    /// The public state, for state reconstruction.
    pub fn snapshot(&self) -> Snapshot {
        let me = self.me();
        let mut mem = [Memory::default(); 2];
        mem[me] = self.mem;
        mem[1 - me] = self.opp_mem;
        let mut order = [0u8; 6];
        for (k, &o) in self.order.iter().take(6).enumerate() {
            order[k] = o;
        }
        let request = match &self.request {
            None => RequestKind::None,
            Some(_) if self.ended => RequestKind::None,
            Some(r) if r["wait"].as_bool() == Some(true) => RequestKind::Wait,
            Some(r) if r["forceSwitch"].as_array().and_then(|a| a.first()).and_then(|v| v.as_bool()) == Some(true) => {
                RequestKind::Switch
            }
            Some(_) => RequestKind::Move,
        };
        let mut boosts = [[0i8; 7]; 2];
        boosts[me] = self.own.boosts;
        boosts[1 - me] = self.opp_boosts;
        Snapshot {
            me,
            turn: self.turn,
            view: self.view(),
            mem,
            raw_status: std::array::from_fn(|p| std::array::from_fn(|i| self.extra[p][i].raw_status)),
            bench_par: std::array::from_fn(|p| std::array::from_fn(|i| self.extra[p][i].bench_par)),
            bench_brn: std::array::from_fn(|p| std::array::from_fn(|i| self.extra[p][i].bench_brn)),
            boosts,
            own_stored: self.own.stored,
            own_modified: self.own.modified,
            order,
            n_order: self.order.len().min(6) as u8,
            request,
            bind_hits: self.bind_hits,
            opp_seen: self.opp_names.len() as u8,
        }
    }

    /// The opponent's memory of the observer's team (indexed by the observer's original party
    /// order), as the env keeps it for the other seat.
    pub fn opponent_memory(&self) -> &Memory {
        &self.opp_mem
    }

    /// The observer's base move ids of party member `i`.
    fn own_stored(&self, i: usize) -> [u8; 4] {
        self.view.sides[self.me()].mons[i.min(5)].moves
    }

    /// The reconstructed view the observation is encoded from (opponent Pokémon in order of
    /// appearance; opponent HP as percentages of 100).
    pub fn view(&self) -> View {
        let me = self.me();
        let mut v = self.view;
        v.turn = self.turn;
        for p in 0..2 {
            let a = v.sides[p].active.party_idx;
            for i in 0..v.sides[p].size as usize {
                let raw = self.extra[p][i].raw_status;
                let m = &mut v.sides[p].mons[i];
                let toxic_active = a as usize == i && v.sides[p].active.volatiles & vol::TOXIC != 0;
                m.status = if raw == status::TOX && !toxic_active { status::PSN } else { raw };
            }
        }
        if v.sides[me].active.party_idx != UNREVEALED {
            let o = &self.own;
            let a = &mut v.sides[me].active;
            let i = a.party_idx as usize;
            a.stats = [v.sides[me].mons[i].max_hp, o.modified[0], o.modified[1], o.modified[4], o.modified[2]];
            a.boosts = [o.boosts[0], o.boosts[1], o.boosts[4], o.boosts[2], o.boosts[5], o.boosts[6]];
        }
        let foe = 1 - me;
        if v.sides[foe].active.party_idx != UNREVEALED {
            let b = self.opp_boosts;
            v.sides[foe].active.boosts = [b[0], b[1], b[4], b[2], b[5], b[6]];
        }
        v
    }

    // ---- Battle log --------------------------------------------------------------------------

    /// Party index of an ident: own original index or opponent appearance index (registering a
    /// new opponent Pokémon from `details` when needed).
    fn slot(&mut self, p: usize, name: &str, details: Option<&str>) -> Option<usize> {
        if p == self.me() {
            return self.own_names.iter().position(|n| n == name);
        }
        if let Some(j) = self.opp_names.iter().position(|n| n == name) {
            return Some(j);
        }
        let details = details?;
        let j = self.opp_names.len();
        if j >= 6 {
            return None;
        }
        self.opp_names.push(name.to_string());
        let (species, level) = parse_details(details);
        let m = &mut self.view.sides[p].mons[j];
        m.species = species;
        m.level = level;
        m.types = data::SPECIES_TYPES[species as usize];
        m.max_hp = 100;
        m.hp = 100;
        Some(j)
    }

    fn update(&mut self, log: &[&str]) {
        let me = self.me();
        let mut last = [TurnEvents::default(); 2];
        let mut attacker: Option<usize> = None;
        let mut first_mover: Option<usize> = None;
        let mut skip_zero: Option<(usize, usize)> = None;
        // The move whose main effect is being resolved: (user, move id, boosted something).
        self.last_move = None;
        for (li, line) in log.iter().enumerate() {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() < 2 {
                continue;
            }
            let tags = |k: &str| parts.iter().any(|x| x.starts_with(k));
            match parts[1] {
                "player" if parts.len() >= 4 => {
                    if self.me.is_none() && !self.name.is_empty() && parts[3] == self.name {
                        self.me = Some(if parts[2] == "p2" { 1 } else { 0 });
                    }
                }
                "teamsize" if parts.len() >= 4 => {
                    let p = if parts[2] == "p2" { 1 } else { 0 };
                    if p != me || self.view.sides[p].size == 0 {
                        self.view.sides[p].size = parts[3].parse().unwrap_or(6);
                    }
                }
                "start" => self.started = true,
                "turn" => self.turn = parts.get(2).and_then(|t| t.parse().ok()).unwrap_or(self.turn),
                "win" | "tie" => self.ended = true,
                "switch" | "drag" | "replace" if parts.len() >= 5 => {
                    let Some((p, name)) = parse_ident(parts[2]) else { continue };
                    let Some(i) = self.slot(p, name, Some(parts[3])) else { continue };
                    self.switch_in(p, i, parts[4]);
                    // Partial trapping ends when either Pokémon leaves.
                    self.end_binding(0);
                    self.end_binding(1);
                    if p != me {
                        self.mem.reveal(i as u8);
                        self.mem.opp_transformed = false;
                    } else {
                        self.opp_mem.reveal(i as u8);
                        self.opp_mem.opp_transformed = false;
                    }
                    self.mem.track[p] = Default::default();
                    last[p].switched = true;
                }
                "move" if parts.len() >= 4 => {
                    let Some((p, name)) = parse_ident(parts[2]) else { continue };
                    let Some(i) = self.slot(p, name, None) else { continue };
                    let mv = data::move_id(parts[3]).unwrap_or(0);
                    let from = tags("[from]");
                    attacker = Some(p);
                    let first = *first_mover.get_or_insert(p) == p;
                    let sw = last[p].switched;
                    last[p] = TurnEvents {
                        moved: true,
                        moved_first: first,
                        mv,
                        called: from,
                        switched: sw,
                        ..Default::default()
                    };
                    self.last_move = Some((p, mv, false));
                    let still = parts.contains(&"[still]");
                    // A selection (no `[from]`, a charge turn included) reveals the observer's move.
                    if p == me && !from && !self.opp_mem.opp_transformed && self.own_stored(i).contains(&mv) {
                        self.opp_mem.see_move(i as u8, mv, true);
                    }
                    if p != me {
                        // Moves the user does not own (Struggle, a Mimic copy) are not reveals. A
                        // Mimic copy of a move the user also knows is indistinguishable: count it
                        // when the species can learn the move.
                        let species = self.view.sides[p].mons[i].species;
                        let copied = self.mem.track[p].mimic_move == mv && !data::can_learn(species, mv);
                        if !from && !self.mem.opp_transformed && mv != STRUGGLE && !copied {
                            self.mem.see_move(i as u8, mv, true);
                        }
                    } else if !from && !still {
                        self.use_pp(mv);
                    } else if still && from {
                        // A two-turn move called by Metronome/Mirror Move: the caller's PP is used
                        // when the called move is released.
                        if let Some(src) = parts.iter().find_map(|x| x.strip_prefix("[from] ")).and_then(data::move_id)
                        {
                            self.refund_pp(src);
                            self.charge_src = src;
                        }
                    } else if self.releases_charge(mv, &parts) {
                        // Two-turn moves use their PP when released (the `[from]` line).
                        let src = if self.charge_src != 0 { self.charge_src } else { mv };
                        self.charge_src = 0;
                        self.use_pp(src);
                    }
                    if p == me && still && !from {
                        self.charge_src = 0;
                    }
                    self.on_move(p, mv, from, &parts);
                }
                "cant" if parts.len() >= 4 => {
                    let Some((p, name)) = parse_ident(parts[2]) else { continue };
                    let Some(i) = self.slot(p, name, None) else { continue };
                    let (reason, mv) = match parts[3] {
                        "slp" => (cant::SLEEP, 0),
                        "frz" => (cant::FREEZE, 0),
                        "par" => (cant::PARALYSIS, 0),
                        "partiallytrapped" => (cant::BOUND, 0),
                        "flinch" => (cant::FLINCH, 0),
                        "Disable" => (cant::DISABLE, parts.get(4).and_then(|m| data::move_id(m)).unwrap_or(0)),
                        "recharge" => (cant::RECHARGE, 0),
                        _ => continue,
                    };
                    last[p].cant = reason + 1;
                    // Full paralysis ends Bide, a charge, partial trapping and Thrash (a Dig/Fly
                    // user stays semi-invulnerable); Disable ends a charge.
                    if reason == cant::PARALYSIS {
                        self.end_binding(p);
                        self.view.sides[p].active.volatiles &= !(vol::BIDE | vol::CHARGING | vol::THRASHING);
                    }
                    if reason == cant::DISABLE {
                        self.view.sides[p].active.volatiles &= !vol::CHARGING;
                    }
                    if reason == cant::SLEEP {
                        let s = &mut self.mem.sleep_obs[p][i];
                        *s = s.saturating_add(1);
                    }
                    if reason == cant::DISABLE && p != me && self.mem.track[p].mimic_move != mv {
                        self.mem.see_move(i as u8, mv, false);
                    }
                    if reason == cant::DISABLE && p == me && self.own_stored(i).contains(&mv) {
                        self.opp_mem.see_move(i as u8, mv, false);
                    }
                    if reason == cant::RECHARGE {
                        self.view.sides[p].active.volatiles &= !vol::RECHARGING;
                    }
                }
                "-miss" => {
                    if let Some(a) = attacker {
                        last[a].missed = true;
                    }
                }
                "-crit" | "-supereffective" | "-resisted" | "-immune" | "-fail" => {
                    if let Some(a) = attacker {
                        let e = &mut last[a];
                        match parts[1] {
                            "-crit" => e.crit = true,
                            "-supereffective" => e.super_effective = true,
                            "-resisted" => e.resisted = true,
                            "-immune" => e.immune = true,
                            _ => e.failed = true,
                        }
                    }
                }
                "-damage" | "-heal" | "-sethp" if parts.len() >= 4 => {
                    let Some((p, name)) = parse_ident(parts[2]) else { continue };
                    let Some(i) = self.slot(p, name, None) else { continue };
                    self.set_condition(p, i, parts[3]);
                    // Hitting itself in confusion ends Bide, charges, partial trapping and Thrash.
                    if parts[1] == "-damage" && tags("[from] confusion") {
                        self.end_binding(p);
                        let a = &mut self.view.sides[p].active;
                        a.volatiles &= !(vol::BIDE | vol::CHARGING | vol::INVULNERABLE | vol::THRASHING);
                    }
                }
                "faint" if parts.len() >= 3 => {
                    let Some((p, name)) = parse_ident(parts[2]) else { continue };
                    let Some(i) = self.slot(p, name, None) else { continue };
                    self.view.sides[p].mons[i].hp = 0;
                    self.faint(p, i);
                    self.end_binding(0);
                    self.end_binding(1);
                }
                "-status" if parts.len() >= 4 => {
                    let Some((p, name)) = parse_ident(parts[2]) else { continue };
                    let Some(i) = self.slot(p, name, None) else { continue };
                    let st = status_code(parts[3]);
                    let rest = tags("[from] move: Rest");
                    self.set_status(p, i, st, rest);
                }
                "-curestatus" if parts.len() >= 3 => {
                    let Some((p, name)) = parse_ident(parts[2]) else { continue };
                    let Some(i) = self.slot(p, name, None) else { continue };
                    self.set_status(p, i, status::NONE, false);
                }
                "-boost" | "-unboost" if parts.len() >= 5 => {
                    let Some((p, _)) = parse_ident(parts[2]) else { continue };
                    let Some(bi) = boost_index(parts[3]) else { continue };
                    let n: i8 = parts[4].parse().unwrap_or(0);
                    let delta = if parts[1] == "-boost" { n } else { -n };
                    if skip_zero == Some((p, bi)) {
                        // The second line of a Gen 1 999/1 stat boost: the boost stage was already
                        // applied with the simulator's rule.
                        skip_zero = None;
                        if p != me {
                            self.opp_boosts[bi] = (self.opp_boosts[bi] + delta).clamp(-6, 6);
                        }
                        continue;
                    }
                    // A following opposite line of 1 on the same stat marks the 999/1 case.
                    let zero = log.get(li + 1).is_some_and(|nx| {
                        let q: Vec<&str> = nx.split('|').collect();
                        q.len() >= 5
                            && q[1] == if delta > 0 { "-unboost" } else { "-boost" }
                            && q[2] == parts[2]
                            && q[3] == parts[3]
                            && q[4] == "1"
                    });
                    if zero {
                        skip_zero = Some((p, bi));
                    }
                    if p == me {
                        self.own_boost(bi, delta, zero);
                    } else {
                        self.opp_boosts[bi] = (self.opp_boosts[bi] + delta).clamp(-6, 6);
                    }
                    // A move's successful stat change (main or secondary effect) re-applies the
                    // paralysis/burn drop of its user's foe, once per move (Gen 1).
                    if !zero && !tags("[from]") {
                        if let Some((user, mv, done)) = self.last_move {
                            if !done && mv != 0 {
                                self.last_move = Some((user, mv, true));
                                if user != me {
                                    self.redrop_own();
                                }
                            }
                        }
                    }
                }
                "-clearallboost" => {
                    self.opp_boosts = [0; 7];
                    self.own.boosts = [0; 7];
                    self.own.modified = self.own.stored;
                }
                "-start" if parts.len() >= 4 => {
                    let Some((p, name)) = parse_ident(parts[2]) else { continue };
                    let Some(i) = self.slot(p, name, None) else { continue };
                    let own_idx = self.view.sides[me].active.party_idx;
                    let own_stored = if own_idx != UNREVEALED { self.own_stored(own_idx as usize) } else { [0; 4] };
                    let a = &mut self.view.sides[p].active;
                    match parts[3] {
                        "confusion" => {
                            a.volatiles |= vol::CONFUSION;
                            // Thrash/Petal Dance end with fatigue confusion.
                            if tags("[fatigue]") {
                                a.volatiles &= !vol::THRASHING;
                            }
                        }
                        "move: Leech Seed" => a.volatiles |= vol::LEECH_SEED,
                        "Mist" => a.volatiles |= vol::MIST,
                        "Light Screen" => a.volatiles |= vol::LIGHT_SCREEN,
                        "Reflect" => a.volatiles |= vol::REFLECT,
                        "move: Focus Energy" => a.volatiles |= vol::FOCUS_ENERGY,
                        "Bide" => a.volatiles |= vol::BIDE,
                        "Substitute" => a.volatiles |= vol::SUBSTITUTE,
                        "Disable" => {
                            let mv = parts.get(4).and_then(|m| data::move_id(m)).unwrap_or(0);
                            if let Some(k) = a.moves.iter().position(|&x| x == mv && mv != 0) {
                                a.disabled_slot = k as u8 + 1;
                            } else {
                                a.disabled_slot = 1;
                            }
                            if p != me && self.mem.track[p].mimic_move != mv {
                                self.mem.see_move(i as u8, mv, false);
                            }
                            if p == me && own_stored.contains(&mv) {
                                self.opp_mem.see_move(i as u8, mv, false);
                            }
                        }
                        "Mimic" => {
                            let mv = parts.get(4).and_then(|m| data::move_id(m)).unwrap_or(0);
                            self.mem.track[p].mimic_move = mv;
                            if p == me {
                                let foe = 1 - me;
                                let fi = self.view.sides[foe].active.party_idx;
                                if fi != UNREVEALED && !self.mem.opp_transformed {
                                    self.mem.see_move(fi, mv, false);
                                }
                            } else if own_idx != UNREVEALED && own_stored.contains(&mv) {
                                // The foe's Mimic copied one of the observer's moves.
                                self.opp_mem.see_move(own_idx, mv, false);
                            }
                        }
                        "typechange" => {
                            if let Some(names) = parts.get(4) {
                                let ts: Vec<u8> = names
                                    .split('/')
                                    .filter_map(|t| data::TYPE_NAMES.iter().position(|n| n == &t).map(|x| x as u8))
                                    .collect();
                                if let Some(&t0) = ts.first() {
                                    a.types = [t0, *ts.get(1).unwrap_or(&t0)];
                                }
                            }
                        }
                        _ => {}
                    }
                }
                "-end" if parts.len() >= 4 => {
                    let Some((p, name)) = parse_ident(parts[2]) else { continue };
                    let Some(_) = self.slot(p, name, None) else { continue };
                    let partially = tags("[partiallytrapped]");
                    let a = &mut self.view.sides[p].active;
                    match parts[3] {
                        "confusion" => a.volatiles &= !vol::CONFUSION,
                        "move: Leech Seed" => a.volatiles &= !vol::LEECH_SEED,
                        "Mist" => a.volatiles &= !vol::MIST,
                        "Light Screen" => a.volatiles &= !vol::LIGHT_SCREEN,
                        "Reflect" => a.volatiles &= !vol::REFLECT,
                        "move: Focus Energy" => a.volatiles &= !vol::FOCUS_ENERGY,
                        "Bide" => a.volatiles &= !vol::BIDE,
                        "Substitute" => a.volatiles &= !vol::SUBSTITUTE,
                        "Disable" => a.disabled_slot = 0,
                        "Toxic counter" => a.volatiles &= !vol::TOXIC,
                        _ if partially => {
                            // The binding move ended: the other side stops binding.
                            self.view.sides[1 - p].active.volatiles &= !vol::BINDING;
                        }
                        _ => {}
                    }
                }
                "-mustrecharge" if parts.len() >= 3 => {
                    if let Some((p, _)) = parse_ident(parts[2]) {
                        self.view.sides[p].active.volatiles |= vol::RECHARGING;
                    }
                }
                "-prepare" if parts.len() >= 4 => {
                    if let Some((p, _)) = parse_ident(parts[2]) {
                        let mv = data::move_id(parts[3]).unwrap_or(0);
                        let a = &mut self.view.sides[p].active;
                        a.volatiles |= vol::CHARGING;
                        if matches!(data::MOVE_NAMES[mv as usize], "Dig" | "Fly") {
                            a.volatiles |= vol::INVULNERABLE;
                        }
                    }
                }
                "-transform" if parts.len() >= 4 => {
                    let Some((p, _)) = parse_ident(parts[2]) else { continue };
                    let Some((q, tname)) = parse_ident(parts[3]) else { continue };
                    let Some(t) = self.slot(q, tname, None) else { continue };
                    self.transform(p, q, t);
                }
                _ => {}
            }
        }
        self.mem.last = last;
        self.opp_mem.last = last;
    }

    fn update_trackers(&mut self, turn_advanced: bool) {
        let binding = [
            self.view.sides[0].active.volatiles & vol::BINDING != 0,
            self.view.sides[1].active.volatiles & vol::BINDING != 0,
        ];
        for p in 0..2 {
            let a = self.view.sides[p].active;
            let t = &mut self.mem.track[p];
            let step = |c: &mut u8, on: bool| *c = if on { c.saturating_add(turn_advanced as u8) } else { 0 };
            step(&mut t.confusion_elapsed, a.volatiles & vol::CONFUSION != 0);
            step(&mut t.disable_elapsed, a.disabled_slot != 0);
            step(&mut t.trap_elapsed, binding[0] || binding[1]);
            step(&mut t.toxic_elapsed, a.volatiles & vol::TOXIC != 0);
            for i in 0..6 {
                if self.extra[p][i].raw_status != status::SLP || self.view.sides[p].mons[i].hp == 0 {
                    self.mem.sleep_obs[p][i] = 0;
                }
            }
        }
    }

    fn set_condition(&mut self, p: usize, i: usize, cond: &str) {
        let (hp, max_hp, st) = parse_condition(cond);
        let m = &mut self.view.sides[p].mons[i];
        m.hp = hp;
        if max_hp > 0 {
            m.max_hp = max_hp;
        }
        if st == "fnt" {
            m.hp = 0;
        }
        let _ = st;
    }

    fn set_status(&mut self, p: usize, i: usize, st: u8, rest: bool) {
        let m = &mut self.view.sides[p].mons[i];
        m.status = st;
        m.rest = st == status::SLP && rest;
        self.extra[p][i].raw_status = st;
        let active = self.view.sides[p].active.party_idx as usize == i;
        if active {
            let a = &mut self.view.sides[p].active;
            // Gen 1: sleep induced on a Pokémon that must recharge cancels the recharge.
            if st == status::SLP {
                a.volatiles &= !vol::RECHARGING;
            }
            // The Toxic counter shows only while the status is Toxic.
            set_bit(&mut a.volatiles, vol::TOXIC, st == status::TOX);
        }
        // Paralysis/burn from a move's effect applies the stat drop to the observer's own
        // modified stats.
        if p == self.me() && active {
            if let Some((user, mv, _)) = self.last_move {
                if user != p && mv != 0 {
                    if st == status::PAR {
                        self.own.modified[4] = modify(self.own.modified[4], 0.25);
                    } else if st == status::BRN {
                        self.own.modified[0] = modify(self.own.modified[0], 0.5);
                    }
                }
            }
        }
    }

    fn switch_in(&mut self, p: usize, i: usize, cond: &str) {
        let me = self.me();
        let side = &mut self.view.sides[p];
        let out = side.active.party_idx;
        // Switch-out of a living Pokémon: clearVolatile keeps the paralysis/burn drop flags.
        if out != UNREVEALED && side.mons[out as usize].hp > 0 {
            let raw = self.extra[p][out as usize].raw_status;
            self.extra[p][out as usize].bench_par = raw == status::PAR;
            self.extra[p][out as usize].bench_brn = raw == status::BRN;
        }
        if out != UNREVEALED && p == me {
            self.sync_base_pp();
        }
        // runSwitch: Toxic reverts to regular poison.
        if self.extra[p][i].raw_status == status::TOX {
            self.extra[p][i].raw_status = status::PSN;
            self.view.sides[p].mons[i].status = status::PSN;
        }
        let m = self.view.sides[p].mons[i];
        let side = &mut self.view.sides[p];
        side.active = obs::ActiveView {
            party_idx: i as u8,
            species: m.species,
            types: data::SPECIES_TYPES[m.species as usize],
            moves: m.moves,
            pp: m.pp,
            ..Default::default()
        };
        self.set_condition(p, i, cond);
        if p == me {
            let s = self.view.sides[me].mons[i].stats; // [hp, atk, def, spe, spc]
            let stored = [s[1], s[2], s[4], s[4], s[3]];
            let mut modified = stored;
            if self.extra[me][i].bench_par {
                modified[4] = modify(modified[4], 0.25);
            }
            if self.extra[me][i].bench_brn {
                modified[0] = modify(modified[0], 0.5);
            }
            self.own = OwnActive { stored, modified, boosts: [0; 7] };
        } else {
            self.opp_boosts = [0; 7];
        }
    }

    fn faint(&mut self, p: usize, i: usize) {
        let _ = i;
        let a = &mut self.view.sides[p].active;
        // clearVolatile(false) on the fainted active; it stays in front until replaced.
        let keep = a.party_idx;
        *a = obs::ActiveView { party_idx: keep, ..Default::default() };
        let m = self.view.sides[p].mons[i];
        a.species = m.species;
        a.types = data::SPECIES_TYPES[m.species as usize];
        a.moves = m.moves;
        a.pp = m.pp;
        // clearVolatile keeps the paralysis/burn drops (`bench_par`/`bench_brn`).
        let raw = self.extra[p][i].raw_status;
        self.extra[p][i].bench_par = raw == status::PAR;
        self.extra[p][i].bench_brn = raw == status::BRN;
        // A fainted Pokémon's status is cleared.
        self.extra[p][i].raw_status = status::NONE;
        self.view.sides[p].mons[i].status = status::NONE;
        self.view.sides[p].mons[i].rest = false;
        if p == self.me() {
            self.own.boosts = [0; 7];
            self.own.modified = self.own.stored;
            if raw == status::PAR {
                self.own.modified[4] = modify(self.own.modified[4], 0.25);
            }
            if raw == status::BRN {
                self.own.modified[0] = modify(self.own.modified[0], 0.5);
            }
        } else {
            self.opp_boosts = [0; 7];
        }
    }

    fn refund_pp(&mut self, mv: u8) {
        let me = self.me();
        let a = &mut self.view.sides[me].active;
        if let Some(k) = a.moves.iter().position(|&x| x == mv && mv != 0) {
            a.pp[k] = a.pp[k].saturating_add(1);
        }
        self.sync_base_pp();
    }

    /// A `[from] MOVE` line releasing the observer's own charged two-turn move `mv`.
    fn releases_charge(&self, mv: u8, parts: &[&str]) -> bool {
        let me = self.me();
        let charging = self.view.sides[me].active.volatiles & vol::CHARGING != 0;
        let tag = format!("[from] {}", data::MOVE_NAMES[mv as usize]);
        charging && parts.iter().any(|x| *x == tag)
    }

    fn end_binding(&mut self, p: usize) {
        self.bind_hits[p] = 0;
        self.view.sides[p].active.volatiles &= !vol::BINDING;
    }

    /// Gen 1 `boostBy` for the observer's active (exact: its stats are known).
    fn own_boost(&mut self, i: usize, delta: i8, zero: bool) {
        let o = &mut self.own;
        if i >= 5 {
            o.boosts[i] = (o.boosts[i] + delta).clamp(-6, 6);
            return;
        }
        if zero {
            o.boosts[i] = if delta > 0 { (o.boosts[i] + delta).min(6) - 1 } else { (o.boosts[i] + delta).max(-6) + 1 };
            return;
        }
        o.boosts[i] = (o.boosts[i] + delta).clamp(-6, 6);
        o.modified[i] = boosted(o.stored[i], o.boosts[i]);
        if delta > 0 && o.modified[i] > 999 {
            o.modified[i] = 999;
        }
    }

    /// The foe's boosting status move re-applies the observer's paralysis/burn drop.
    fn redrop_own(&mut self) {
        let me = self.me();
        let a = self.view.sides[me].active.party_idx;
        if a == UNREVEALED {
            return;
        }
        match self.extra[me][a as usize].raw_status {
            status::PAR => self.own.modified[4] = modify(self.own.modified[4], 0.25),
            status::BRN => self.own.modified[0] = modify(self.own.modified[0], 0.5),
            _ => {}
        }
    }

    /// PP use of the observer's active move `mv` (a selection, not a called move).
    fn use_pp(&mut self, mv: u8) {
        let me = self.me();
        let a = &mut self.view.sides[me].active;
        if let Some(k) = a.moves.iter().position(|&x| x == mv && mv != 0) {
            a.pp[k] = a.pp[k].saturating_sub(1);
        }
        self.sync_base_pp();
    }

    /// Volatile bookkeeping a `|move|` line implies (silent volatiles of the user).
    fn on_move(&mut self, p: usize, mv: u8, from: bool, parts: &[&str]) {
        let name = data::MOVE_NAMES[mv as usize];
        let still = parts.contains(&"[still]");
        let missed = parts.contains(&"[miss]");
        // Partial trapping lasts 2-5 hits (3/8, 3/8, 1/8, 1/8) and ends silently; its length is
        // hidden from both players. Predict "still binding" while that is more likely than not:
        // after one or two hits.
        if matches!(name, "Wrap" | "Bind" | "Fire Spin" | "Clamp") {
            // Gen 1: partial trapping negates the target's recharge turn, even on a miss.
            self.view.sides[1 - p].active.volatiles &= !vol::RECHARGING;
            self.bind_hits[p] = if from {
                self.bind_hits[p].saturating_add(1)
            } else if missed {
                0
            } else {
                1
            };
        } else {
            self.bind_hits[p] = 0;
        }
        let binding = matches!(self.bind_hits[p], 1 | 2);
        let a = &mut self.view.sides[p].active;
        // A move executes: a charged two-turn move is released.
        if !still {
            a.volatiles &= !(vol::CHARGING | vol::INVULNERABLE);
        }
        match name {
            "Rage" => a.volatiles |= vol::RAGE,
            "Thrash" | "Petal Dance" => a.volatiles |= vol::THRASHING,
            _ if !from => a.volatiles &= !vol::THRASHING,
            _ => {}
        }
        set_bit(&mut a.volatiles, vol::BINDING, binding);
    }

    fn transform(&mut self, p: usize, q: usize, t: usize) {
        let me = self.me();
        let target = self.view.sides[q].active;
        let tm = self.view.sides[q].mons[t];
        let a = &mut self.view.sides[p].active;
        a.species = target.species;
        a.types = target.types;
        a.volatiles |= vol::TRANSFORM;
        if p == me {
            // The target's modified stats are hidden: estimate from its species.
            let s = obs::assumed_stats(tm.species, tm.level);
            self.own.stored = [s[1], s[2], s[4], s[4], s[3]];
            self.own.modified = self.own.stored;
            self.own.boosts = if q == me { self.own.boosts } else { self.opp_boosts };
            self.opp_mem.opp_transformed = true;
            // The observer sees the target's moveset in its next request.
            self.transform_reveal = true;
        } else {
            self.mem.opp_transformed = true;
            self.opp_boosts = self.own.boosts;
            // The transformer's owner sees the observer's moveset.
            let i = self.view.sides[me].active.party_idx;
            if i != UNREVEALED {
                for mv in self.own_stored(i as usize) {
                    self.opp_mem.see_move(i, mv, false);
                }
            }
        }
    }
}
