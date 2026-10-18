//! Showdown protocol output for the Gen 1 battle: the text helpers the mechanics use to write
//! protocol lines (identifiers, `getHealth`, `getDetails`), the battle header, the `|request|`
//! JSON, and [`ProtocolBattle`], a battle that produces exactly what a pokemon-showdown 0.11.11
//! `BattleStream` emits (checked line by line by `parity fuzz --proto`).

use crate::state::*;
use kanto_core::proto::{extract_channel, Log, ProtoSink};
use kanto_core::{Choice, IllegalChoice, Outcome, Player, Request};
use kanto_data::gen1::{target as tgt, MOVES, SPECIES};
use std::fmt::Write;

/// Run `$body` with `$l: &mut Log` when the sink records a protocol log (compiled away for the
/// other sinks).
macro_rules! plog {
    ($s:expr, |$l:ident| $body:expr) => {
        if let Some($l) = $s.log() {
            $body;
        }
    };
}
pub(crate) use plog;

/// Showdown's status ids, indexed by `st::*` (`FNT` is shown as `fnt`).
const STATUS: [&str; 8] = ["", "slp", "psn", "brn", "frz", "par", "tox", "fnt"];

/// Showdown's name for a stat index of the boost table (`[atk, def, spa, spd, spe, acc, eva]`).
pub(crate) const BOOST_NAMES: [&str; 7] = ["atk", "def", "spa", "spd", "spe", "accuracy", "evasion"];

fn target_name(t: u8) -> &'static str {
    match t {
        tgt::NORMAL => "normal",
        tgt::SELF => "self",
        tgt::ANY => "any",
        tgt::RANDOM_NORMAL => "randomNormal",
        tgt::ALL_ADJACENT_FOES => "allAdjacentFoes",
        tgt::ALL_ADJACENT => "allAdjacent",
        _ => "scripted",
    }
}

/// Showdown's move name for a (pseudo) move id.
pub(crate) fn move_name(id: u8) -> &'static str {
    match id {
        pm::NOMOVE_HACK => "Fissure",
        0 | pm::NOMOVE | pm::CANNOTMOVE => "",
        n => MOVES[n as usize].name,
    }
}

/// JSON string literal (Showdown names contain no characters beyond `"`/`\` that JSON escapes,
/// but escape control characters too, as `JSON.stringify` does).
fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl Battle {
    /// `Pokemon#name` of team member `slot` of `p`.
    pub(crate) fn mon_name<'a>(&self, l: &'a Log, p: Player, slot: usize) -> &'a str {
        let sp = SPECIES[self.sides[p].mons[slot.min(MAX_TEAM - 1)].species as usize].name;
        l.name(p, slot, sp)
    }

    /// `pokemon.toString()` for `p`'s active Pokémon: `p1a: Name`.
    pub(crate) fn ident(&self, l: &Log, p: Player) -> String {
        format!("p{}a: {}", (p & 1) + 1, self.mon_name(l, p, self.sides[p].active_slot()))
    }

    /// `pokemon.fullname`: `p1: Name` (no position letter).
    pub(crate) fn fullname(&self, l: &Log, p: Player, slot: usize) -> String {
        format!("p{}: {}", (p & 1) + 1, self.mon_name(l, p, slot))
    }

    /// `pokemon.details`: species name, `, L<level>` below 100 (Gen 1 has no gender).
    pub(crate) fn details(&self, l: &Log, p: Player, slot: usize) -> String {
        let m = &self.sides[p].mons[slot.min(MAX_TEAM - 1)];
        let name = SPECIES[m.species as usize].name;
        let mut d = if m.level == 100 { name.to_string() } else { format!("{name}, L{}", m.level) };
        if let Some(x) = l.extra(p, slot) {
            d.push_str(&x.details_suffix);
        }
        d
    }

    /// `pokemon.getHealth()`: (secret `hp/maxhp[ status]`, shared `pct/100[ status]`) under HP
    /// Percentage Mod.
    pub(crate) fn health(&self, p: Player, slot: usize) -> (String, String) {
        let m = &self.sides[p].mons[slot.min(MAX_TEAM - 1)];
        if m.hp == 0 {
            return ("0 fnt".into(), "0 fnt".into());
        }
        let max = m.stats[0] as u32;
        let hp = m.hp as u32;
        let mut pct = (100 * hp).div_ceil(max);
        if pct == 100 && hp < max {
            pct = 99;
        }
        let mut secret = format!("{hp}/{max}");
        let mut shared = format!("{pct}/100");
        if m.status != st::NONE {
            let s = STATUS[(m.status as usize).min(7)];
            secret.push(' ');
            secret.push_str(s);
            shared.push(' ');
            shared.push_str(s);
        }
        (secret, shared)
    }

    /// `this.add(kind, pokemon, pokemon.getHealth, ...rest)` for `p`'s active: a split line.
    /// `rest` is appended verbatim (it starts with `|` when non-empty).
    pub(crate) fn add_health(&self, l: &mut Log, kind: &str, p: Player, rest: &str) {
        let id = self.ident(l, p);
        let (secret, shared) = self.health(p, self.sides[p].active_slot());
        l.add_split(p, format!("|{kind}|{id}|{secret}{rest}"), Some(format!("|{kind}|{id}|{shared}{rest}")));
    }

    /// `|switch|IDENT|DETAILS|HP` (`getDetails` is a split part).
    pub(crate) fn add_switch(&self, l: &mut Log, p: Player) {
        let slot = self.sides[p].active_slot();
        let id = self.ident(l, p);
        let det = self.details(l, p, slot);
        let (secret, shared) = self.health(p, slot);
        l.add_split(p, format!("|switch|{id}|{det}|{secret}"), Some(format!("|switch|{id}|{det}|{shared}")));
    }

    /// The battle header Showdown writes before the start action (constructor, `setPlayer`,
    /// `start`): `|t:|`, `|gametype|`, `|player|`, `|gen|`, `|tier|`, `|rule|`s.
    pub fn write_header(&self, l: &mut Log) {
        l.add(format!("|t:|{}", l.timestamp));
        l.add("|gametype|singles".into());
        for p in 0..2 {
            l.add(format!("|player|p{}|{}||", p + 1, l.player_names[p]));
        }
        self.write_format_header(l, None);
    }

    /// The part of the header `Battle#start` writes once both players joined: `|gen|`, `|tier|`,
    /// `|rated|` (`rated` = its message, if the battle is rated), the `|rule|` lines and the
    /// custom-rules `|raw|` box.
    pub fn write_format_header(&self, l: &mut Log, rated: Option<&str>) {
        l.add("|gen|1".into());
        l.add("|tier|[Gen 1] OU".into());
        if let Some(r) = rated {
            l.add(format!("|rated|{r}"));
        }
        l.add("|rule|Desync Clause Mod: Desyncs resolve to link battle behavior from the acting player's perspective.".into());
        l.add("|rule|HP Percentage Mod: HP is shown in percentages".into());
        l.add("|rule|Endless Battle Clause: Forcing endless battles is banned".into());
        if self.rules.sleep_clause {
            l.add("|rule|Sleep Clause Mod: Limit one foe put to sleep".into());
        }
        if self.rules.freeze_clause {
            l.add("|rule|Freeze Clause Mod: Limit one foe frozen".into());
        }
        l.add("|rule|Species Clause: Limit one of each Pokémon".into());
        l.add("|rule|OHKO Clause: OHKO moves are banned".into());
        l.add("|rule|Evasion Moves Clause: Evasion moves are banned".into());
        let custom: Vec<&str> = [(!self.rules.sleep_clause, "!Sleep Clause Mod"), (!self.rules.freeze_clause, "!Freeze Clause Mod")]
            .iter()
            .filter(|x| x.0)
            .map(|x| x.1)
            .collect();
        if !custom.is_empty() {
            let plural = if custom.len() == 1 { "" } else { "s" };
            l.add(format!(
                "|raw|<div class=\"infobox\"><details class=\"readmore\" open><summary><strong>{} custom rule{plural}:</strong></summary> {}</details></div>",
                custom.len(),
                custom.join(", ")
            ));
        }
    }

    /// The `|request|` JSON Showdown sends `p` for the current request (`Side#emitRequest`), or
    /// `None` when there is no request (battle not started or over). `noCancel` follows Cancel
    /// Mod: set when only one side has a real (move/switch) request.
    pub fn request_json(&self, l: &Log, p: Player) -> Option<String> {
        if !self.started || self.ended {
            return None;
        }
        let sd = &self.sides[p];
        let real = |s: &Side| matches!(s.request, Request::Move | Request::Switch);
        let multiple = real(&self.sides[0]) && real(&self.sides[1]);
        let mut out = String::with_capacity(1024);
        match sd.request {
            Request::Move => {
                out.push_str("{\"active\":[");
                self.move_request_data(&mut out, p);
                out.push_str("],\"side\":");
                self.side_request_data(&mut out, l, p);
                if !multiple {
                    out.push_str(",\"noCancel\":true");
                }
                out.push('}');
            }
            Request::Switch => {
                out.push_str("{\"forceSwitch\":[true],\"side\":");
                self.side_request_data(&mut out, l, p);
                if !multiple {
                    out.push_str(",\"noCancel\":true");
                }
                out.push('}');
            }
            Request::Wait | Request::None => {
                out.push_str("{\"wait\":true,\"side\":");
                self.side_request_data(&mut out, l, p);
                out.push('}');
            }
        }
        Some(out)
    }

    /// `Pokemon#getMoveRequestData` for `p`'s active Pokémon.
    fn move_request_data(&self, out: &mut String, p: Player) {
        let sd = &self.sides[p];
        let (moves, n, _trapped) = self.request_moves(p);
        let hard = self.locked_move(p).is_some();
        out.push_str("{\"moves\":[");
        for (i, m) in moves[..n].iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            match m.id {
                crate::battle::req::FIGHT => out.push_str("{\"move\":\"Fight\",\"id\":\"fight\"}"),
                crate::battle::req::RECHARGE => out.push_str("{\"move\":\"Recharge\",\"id\":\"recharge\"}"),
                pm::STRUGGLE if n == 1 && !self.is_locked_request(p) => {
                    out.push_str("{\"move\":\"Struggle\",\"id\":\"struggle\",\"target\":\"randomNormal\",\"disabled\":false}")
                }
                id if self.is_locked_request(p) => {
                    // getMoves(lockedMove): {move, id} only.
                    let _ = write!(out, "{{\"move\":");
                    json_str(out, move_name(id));
                    let _ = write!(out, ",\"id\":\"{}\"}}", move_id_str(id));
                }
                id => {
                    let pp = sd.slot(i).pp();
                    let maxpp = sd.slot_maxpp(i);
                    let _ = write!(out, "{{\"move\":");
                    json_str(out, move_name(id));
                    let _ = write!(
                        out,
                        ",\"id\":\"{}\",\"pp\":{pp},\"maxpp\":{maxpp},\"target\":\"{}\",\"disabled\":{}}}",
                        move_id_str(id),
                        target_name(MOVES[id as usize].target),
                        m.disabled
                    );
                }
            }
        }
        out.push(']');
        let mon = sd.mon();
        let can_switch = self.can_switch(p) > 0;
        if hard {
            out.push_str(",\"trapped\":true");
        } else {
            // request_side_effects already folded maybeDisabled into maybeLocked.
            if mon.maybe_disabled() {
                out.push_str(",\"maybeDisabled\":true");
            }
            if mon.maybe_locked() || mon.maybe_disabled() {
                out.push_str(",\"maybeLocked\":true");
            }
            if can_switch && mon.trapped() {
                out.push_str(",\"trapped\":true");
            }
        }
        out.push('}');
    }

    /// Whether the request's move list is a locked move (`getMoves(lockedMove)`): a hard lock
    /// (recharge, Thrash, two-turn moves, Rage) or a semi-lock (Wrap & co., Bide) the player knows
    /// about.
    fn is_locked_request(&self, p: Player) -> bool {
        self.locked_move(p).is_some() || (!self.sides[p].mon().maybe_locked() && self.semi_locked_move(p, true).is_some())
    }

    /// `Side#getRequestData`.
    fn side_request_data(&self, out: &mut String, l: &Log, p: Player) {
        let sd = &self.sides[p];
        out.push_str("{\"name\":");
        json_str(out, &l.player_names[p & 1]);
        let _ = write!(out, ",\"id\":\"p{}\",\"pokemon\":[", (p & 1) + 1);
        for k in 0..sd.n as usize {
            let slot = sd.order[k] as usize;
            let m = &sd.mons[slot.min(MAX_TEAM - 1)];
            if k > 0 {
                out.push(',');
            }
            out.push_str("{\"ident\":");
            json_str(out, &self.fullname(l, p, slot));
            out.push_str(",\"details\":");
            json_str(out, &self.details(l, p, slot));
            out.push_str(",\"condition\":");
            json_str(out, &self.health(p, slot).0);
            let _ = write!(
                out,
                ",\"active\":{},\"stats\":{{\"atk\":{},\"def\":{},\"spa\":{},\"spd\":{},\"spe\":{}}},\"moves\":[",
                k == 0,
                m.stats[1],
                m.stats[2],
                m.stats[3],
                m.stats[4],
                m.stats[5]
            );
            let active = k == 0 && sd.active.species != 0;
            let n_moves = if active { sd.active.n_slots } else { m.n_moves } as usize;
            for i in 0..n_moves.min(4) {
                if i > 0 {
                    out.push(',');
                }
                let id = if active { sd.slot(i).id } else { m.moves[i].id };
                let _ = write!(out, "\"{}\"", move_id_str(id));
            }
            let x = l.extra(p, slot);
            out.push_str("],\"baseAbility\":");
            json_str(out, x.map_or("", |x| x.ability.as_str()));
            out.push_str(",\"item\":");
            json_str(out, x.map_or("", |x| x.item.as_str()));
            out.push_str(",\"pokeball\":");
            json_str(out, x.map(|x| x.pokeball.as_str()).filter(|b| !b.is_empty()).unwrap_or("pokeball"));
            out.push('}');
        }
        out.push_str("]}");
    }
}

/// One step's output: what a pokemon-showdown `BattleStream` emits after the step's choices.
#[derive(Clone, Debug, Default)]
pub struct StepOutput {
    /// The new `Battle#log` lines (the `update` message, with `|split|` blocks).
    pub update: Vec<String>,
    /// The `|request|` JSON for each player (`None` once the battle is over).
    pub requests: [Option<String>; 2],
}

impl StepOutput {
    /// The update as one channel sees it (`extractChannelMessages`): -1 omniscient, 0
    /// spectator, 1/2 the players.
    pub fn view(&self, channel: i32) -> Vec<String> {
        extract_channel(&self.update, channel)
    }
}

/// A Gen 1 battle that also produces the Showdown protocol: the omniscient log with `|split|`
/// blocks, the per-player views (exact HP for one's own Pokémon, percentages for the foe's), and
/// the `|request|` JSON, exactly as pokemon-showdown 0.11.11 emits them for the same seed and
/// choices.
#[derive(Clone, Debug)]
pub struct ProtocolBattle {
    pub battle: Battle,
    pub sink: ProtoSink,
    /// The structured events of the last step (what the env's reveal memory consumes).
    pub last_events: Vec<kanto_core::Event>,
    sent: usize,
}

impl ProtocolBattle {
    /// `names` are the player names (`|player|`, `|win|`); `nicknames` optionally name each team
    /// member (empty = species name). Call [`ProtocolBattle::start`] next.
    pub fn new(battle: Battle, names: [&str; 2]) -> ProtocolBattle {
        let mut sink = ProtoSink { log: Log::new(names[0], names[1]), events: Vec::new() };
        battle.write_header(&mut sink.log);
        ProtocolBattle { battle, sink, last_events: Vec::new(), sent: 0 }
    }

    /// Set the value written into `|t:|` lines.
    pub fn set_timestamp(&mut self, t: u64) {
        self.sink.log.timestamp = t;
        // The header's first line is `|t:|`; keep it consistent when set before starting.
        if !self.battle.started && self.sent == 0 {
            if let Some(l) = self.sink.log.lines.first_mut() {
                *l = format!("|t:|{t}");
            }
        }
    }

    /// Start the battle (send out the leads); returns the first update and requests.
    pub fn start(&mut self) -> StepOutput {
        if !self.battle.started {
            self.battle.apply_choices_unchecked(Choice::Pass, Choice::Pass, &mut self.sink);
        }
        self.flush()
    }

    /// Apply both players' choices (`Choice::Pass` for a player without a request).
    pub fn step(&mut self, c1: Choice, c2: Choice) -> Result<StepOutput, IllegalChoice> {
        self.battle.apply_choices(c1, c2, &mut self.sink)?;
        Ok(self.flush())
    }

    /// Like [`ProtocolBattle::step`] with Showdown choice strings (`"move 2"`, `"switch 3"`,
    /// `""`/`"pass"`).
    pub fn step_str(&mut self, c1: &str, c2: &str) -> Result<StepOutput, String> {
        let p1 = Choice::parse(c1).ok_or_else(|| format!("bad choice '{c1}'"))?;
        let p2 = Choice::parse(c2).ok_or_else(|| format!("bad choice '{c2}'"))?;
        self.step(p1, p2).map_err(|e| e.to_string())
    }

    fn flush(&mut self) -> StepOutput {
        let lines = &self.sink.log.lines;
        let update = lines[self.sent.min(lines.len())..].to_vec();
        self.sent = lines.len();
        self.last_events = std::mem::take(&mut self.sink.events);
        let requests = [self.battle.request_json(&self.sink.log, 0), self.battle.request_json(&self.sink.log, 1)];
        StepOutput { update, requests }
    }

    pub fn outcome(&self) -> Outcome {
        self.battle.outcome()
    }

    /// The whole log so far.
    pub fn log(&self) -> &[String] {
        &self.sink.log.lines
    }
}
