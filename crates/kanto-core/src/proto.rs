//! Showdown protocol output: the battle log exactly as pokemon-showdown's `Battle#log` holds it,
//! and the per-channel views its `extractChannelMessages` derives from it.
//!
//! The simulator writes protocol lines into a [`Log`] when its [`crate::Sink`] provides one
//! ([`crate::Sink::log`]); the fast path (`()`) provides none, so no text is built. Line
//! semantics follow `sim/battle.ts`:
//!
//! - [`Log::add`] = `Battle#add` without split parts: one public line.
//! - [`Log::add_split`] = `Battle#addSplit` / `add` with a `getHealth`-style part: `|split|pN`,
//!   the secret line (seen by player N and omniscient observers) and the shared line (everyone
//!   else; an empty shared line is dropped for them).
//! - [`Log::add_move`] / [`Log::attr_last_move`] = `addMove` / `attrLastMove` (`[miss]`, `[still]`
//!   edit the last `|move|` line in place).
//! - [`Log::hint`] = `Battle#hint`, with the once-per-battle set.

use crate::Player;

/// Showdown's battle log (`Battle#log`) plus the bookkeeping its helpers keep.
#[derive(Clone, Debug, Default)]
pub struct Log {
    /// Every line so far, as in `Battle#log` (`|split|p1` lines and the empty shared lines
    /// included).
    pub lines: Vec<String>,
    /// `Battle#lastMoveLine`.
    pub last_move_line: Option<usize>,
    /// `Battle#hints` (once-hints; side hints are keyed `"pN|hint"`).
    hints: Vec<String>,
    /// Player names (`Side#name`); used by `|player|`, `|win|` and the request's `side.name`.
    pub player_names: [String; 2],
    /// Optional nicknames per original team slot (`Pokemon#name`); empty = the species' name.
    pub nicknames: [Vec<String>; 2],
    /// Value written into `|t:|` lines (Showdown writes `Math.floor(Date.now() / 1000)`).
    pub timestamp: u64,
}

impl Log {
    pub fn new(p1: &str, p2: &str) -> Log {
        Log { player_names: [p1.to_string(), p2.to_string()], ..Log::default() }
    }

    /// `Battle#add` with only plain parts: `line` is the full line, starting with `|`.
    #[inline]
    pub fn add(&mut self, line: String) {
        self.lines.push(line);
    }

    /// `Battle#addSplit(side, secret, shared)`; `shared = None` pushes the empty line.
    pub fn add_split(&mut self, side: Player, secret: String, shared: Option<String>) {
        self.lines.push(format!("|split|p{}", side + 1));
        self.lines.push(secret);
        self.lines.push(shared.unwrap_or_default());
    }

    /// `Battle#addMove`.
    pub fn add_move(&mut self, line: String) {
        self.last_move_line = Some(self.lines.len());
        self.lines.push(line);
    }

    /// `Battle#attrLastMove(...args)`; `attrs` is the `|`-joined argument list (e.g. `"[miss]"`).
    pub fn attr_last_move(&mut self, attrs: &str) {
        let Some(i) = self.last_move_line else { return };
        let Some(line) = self.lines.get_mut(i) else { return };
        if attrs.split('|').any(|a| a == "[still]") {
            // "If no animation plays, the target should never be known": blank parts[4].
            let mut parts: Vec<&str> = line.split('|').collect();
            if parts.len() > 4 {
                parts[4] = "";
            } else {
                while parts.len() < 4 {
                    parts.push("");
                }
                parts.push("");
            }
            *line = parts.join("|");
        }
        line.push('|');
        line.push_str(attrs);
    }

    /// Append `suffix` to the last line (`this.log[this.log.length - 1] += suffix`, used by Gen 1
    /// Haze to silence the `-end` lines that `removeVolatile` printed).
    pub fn append_last(&mut self, suffix: &str) {
        if let Some(l) = self.lines.last_mut() {
            l.push_str(suffix);
        }
    }

    /// `Battle#hint(hint, once, side)`.
    pub fn hint(&mut self, hint: &str, once: bool, side: Option<Player>) {
        let key = match side {
            Some(p) => format!("p{}|{hint}", p + 1),
            None => hint.to_string(),
        };
        if self.hints.contains(&key) {
            return;
        }
        match side {
            Some(p) => self.add_split(p, format!("|-hint|{hint}"), None),
            None => self.add(format!("|-hint|{hint}")),
        }
        if once {
            self.hints.push(key);
        }
    }

    /// `Pokemon#name` of `p`'s original team slot `slot`: its nickname, else the species name.
    pub fn name<'a>(&'a self, p: Player, slot: usize, species_name: &'a str) -> &'a str {
        match self.nicknames[p & 1].get(slot) {
            Some(n) if !n.is_empty() => n,
            _ => species_name,
        }
    }
}

/// `extractChannelMessages(lines.join('\n'), [channel])[channel]`: the lines one channel sees.
/// `channel` is -1 (omniscient: every secret line), 0 (spectator: shared lines only) or N for
/// player N (its own secret lines, everyone else's shared lines). Empty lines are dropped, as
/// Showdown's `/^\|split\|p([1234])\n(.*)\n(.*)|.+/gm` never matches them.
pub fn extract_channel(lines: &[String], channel: i32) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = &lines[i];
        if let Some(n) = l.strip_prefix("|split|p") {
            if n.len() == 1 && matches!(n.as_bytes()[0], b'1'..=b'4') && i + 2 < lines.len() {
                let player = (n.as_bytes()[0] - b'0') as i32;
                let secret = &lines[i + 1];
                let shared = &lines[i + 2];
                let line = if channel == -1 || channel == player { secret } else { shared };
                if !line.is_empty() {
                    out.push(line.clone());
                }
                i += 3;
                continue;
            }
        }
        if !l.is_empty() {
            out.push(l.clone());
        }
        i += 1;
    }
    out
}

/// A [`crate::Sink`] that records both the structured events and the protocol log.
#[derive(Clone, Debug, Default)]
pub struct ProtoSink {
    pub log: Log,
    pub events: Vec<crate::Event>,
}

impl crate::Sink for ProtoSink {
    #[inline]
    fn event(&mut self, ev: crate::Event) {
        self.events.push(ev);
    }

    #[inline]
    fn log(&mut self) -> Option<&mut Log> {
        Some(&mut self.log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn split_views() {
        let mut l = Log::new("a", "b");
        l.add("|start".into());
        l.add_split(0, "|-damage|p1a: X|10/20".into(), Some("|-damage|p1a: X|50/100".into()));
        l.hint("secret hint", false, Some(1));
        l.add("|turn|1".into());
        assert_eq!(extract_channel(&l.lines, -1), v(&["|start", "|-damage|p1a: X|10/20", "|-hint|secret hint", "|turn|1"]));
        assert_eq!(extract_channel(&l.lines, 0), v(&["|start", "|-damage|p1a: X|50/100", "|turn|1"]));
        assert_eq!(extract_channel(&l.lines, 1), v(&["|start", "|-damage|p1a: X|10/20", "|turn|1"]));
        assert_eq!(extract_channel(&l.lines, 2), v(&["|start", "|-damage|p1a: X|50/100", "|-hint|secret hint", "|turn|1"]));
    }

    #[test]
    fn attr_last_move_still_blanks_target() {
        let mut l = Log::default();
        l.add_move("|move|p1a: A|Fly|p2a: B".into());
        l.add("|-prepare|p1a: A|Fly".into());
        l.attr_last_move("[still]");
        assert_eq!(l.lines[0], "|move|p1a: A|Fly||[still]");
        l.attr_last_move("[miss]");
        assert_eq!(l.lines[0], "|move|p1a: A|Fly||[still]|[miss]");
    }

    #[test]
    fn once_hints() {
        let mut l = Log::default();
        l.hint("h", true, None);
        l.hint("h", true, None);
        l.hint("g", false, None);
        l.hint("g", false, None);
        assert_eq!(l.lines, v(&["|-hint|h", "|-hint|g", "|-hint|g"]));
    }
}
