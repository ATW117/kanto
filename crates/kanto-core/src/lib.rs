//! Generation-independent core of the native Showdown-exact simulator.
//!
//! - [`prng`]: Showdown's PRNG (Gen 5 LCG and sodium/ChaCha20), bit-exact.
//! - [`sort`]: `speedSort`/`comparePriority`, including RNG consumed by ties.
//! - [`trace`]: optional PRNG call recording for the parity harness.
//! - [`Choice`], [`Player`], [`Outcome`], [`Event`]/[`Sink`], and the [`Generation`] trait that each
//!   per-generation crate (`kanto-gen1`, ...) implements.

pub mod prng;
pub mod proto;
pub mod sort;
pub mod trace;

pub use prng::Prng;
pub use proto::{Log, ProtoSink};

/// Player index: 0 = p1, 1 = p2.
pub type Player = usize;

/// A player's choice for one request, in Showdown's terms.
///
/// `Move(k)` is `"move k"` (1-based index into the *request's* move list, which is the move slots
/// except for locked/forced turns where it has a single entry); `Switch(k)` is `"switch k"` (1-based
/// *current* team position, as Showdown reorders the team on every switch); `Pass` is no choice
/// (waiting for the opponent).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Choice {
    #[default]
    Pass,
    Move(u8),
    Switch(u8),
}

impl Choice {
    /// Compact 1-byte encoding: `kind << 4 | data` (kind 0 pass, 1 move, 2 switch).
    #[inline]
    pub fn to_u8(self) -> u8 {
        match self {
            Choice::Pass => 0,
            Choice::Move(k) => 0x10 | k,
            Choice::Switch(k) => 0x20 | k,
        }
    }

    #[inline]
    pub fn from_u8(b: u8) -> Choice {
        match b >> 4 {
            1 => Choice::Move(b & 0xF),
            2 => Choice::Switch(b & 0xF),
            _ => Choice::Pass,
        }
    }

    /// Showdown's choice string.
    pub fn to_showdown(self) -> String {
        match self {
            Choice::Pass => "pass".into(),
            Choice::Move(k) => format!("move {k}"),
            Choice::Switch(k) => format!("switch {k}"),
        }
    }

    /// Parse `"move k"`, `"switch k"`, `"pass"` or `""` (pass).
    pub fn parse(s: &str) -> Option<Choice> {
        let s = s.trim();
        if s.is_empty() || s == "pass" {
            return Some(Choice::Pass);
        }
        let (kind, data) = s.split_once(' ')?;
        let k: u8 = data.trim().parse().ok()?;
        match kind {
            "move" => Some(Choice::Move(k)),
            "switch" => Some(Choice::Switch(k)),
            _ => None,
        }
    }
}

/// A choice that is not among the player's legal choices ([`Generation::choices`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IllegalChoice {
    pub player: Player,
    pub choice: Choice,
}

impl std::fmt::Display for IllegalChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "illegal choice '{}' for player {}", self.choice.to_showdown(), self.player + 1)
    }
}

impl std::error::Error for IllegalChoice {}

/// Battle result after an update, from p1's perspective.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Outcome {
    #[default]
    Ongoing,
    P1Win,
    P2Win,
    Tie,
}

/// What a player is being asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Request {
    /// No request (battle over or not started).
    #[default]
    None,
    /// A move request (moves and/or switches).
    Move,
    /// A forced switch (after a faint).
    Switch,
    /// Waiting for the other player.
    Wait,
}

/// `|cant|` reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cant {
    Sleep,
    Freeze,
    Paralysis,
    PartiallyTrapped,
    Flinch,
    Disable,
    Recharge,
}

/// Public battle events, emitted by the simulator to a [`Sink`] as they happen (a structured
/// subset of Showdown's protocol). `slot` is a Pokémon's original team index (0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Switch { player: Player, slot: u8 },
    /// A move was used; `from` is set for moves called by another move or forced (`[from]`).
    Move { player: Player, slot: u8, mv: u8, from: bool },
    Cant { player: Player, slot: u8, reason: Cant, mv: u8 },
    Miss { player: Player },
    Crit,
    SuperEffective,
    Resisted,
    Immune,
    Fail,
    Faint { player: Player, slot: u8 },
    /// `|-damage|X|...|[from] confusion`: X hurt itself in its confusion.
    ConfusionDamage { player: Player },
    /// `|-end|X|<binding move>|[partiallytrapped]`: X is no longer partially trapped (Gen 1: it
    /// made a Substitute).
    TrapEnd { player: Player },
    /// `|-mustrecharge|X`.
    MustRecharge { player: Player },
    /// `|-status|X|slp`: X fell asleep.
    SleepStart { player: Player },
    DisableStart { player: Player, slot: u8, mv: u8 },
    MimicStart { player: Player, mv: u8 },
    Transform { player: Player },
    Turn { turn: u16 },
}

/// Receives battle events. `()` ignores them (the fast path).
pub trait Sink {
    fn event(&mut self, ev: Event);
    /// The Showdown protocol log to write into, if this sink records one ([`ProtoSink`]). The
    /// default returns `None`, so for the other sinks every protocol line compiles away.
    #[inline(always)]
    fn log(&mut self) -> Option<&mut Log> {
        None
    }
}

impl Sink for () {
    #[inline(always)]
    fn event(&mut self, _ev: Event) {}
}

/// Collects events into a fixed-capacity buffer (no heap).
#[derive(Clone, Copy, Debug)]
pub struct EventBuf {
    pub len: usize,
    pub events: [Event; 64],
}

impl Default for EventBuf {
    fn default() -> Self {
        EventBuf { len: 0, events: [Event::Crit; 64] }
    }
}

impl EventBuf {
    pub fn as_slice(&self) -> &[Event] {
        &self.events[..self.len]
    }
}

impl Sink for EventBuf {
    #[inline]
    fn event(&mut self, ev: Event) {
        if self.len < self.events.len() {
            self.events[self.len] = ev;
            self.len += 1;
        }
    }
}

impl Sink for Vec<Event> {
    fn event(&mut self, ev: Event) {
        self.push(ev);
    }
}

/// The interface every generation's battle implements. Battles are fixed-size `Copy` values with
/// the PRNG inside, so cloning a battle is a memcpy and replays are deterministic.
pub trait Generation: Copy + Send + Sync + 'static {
    /// The generation number.
    const GEN: u8;
    /// Current request for `player`.
    fn request(&self, player: Player) -> Request;
    /// Legal choices for `player` (what Showdown's request offers); returns the count.
    fn choices(&self, player: Player, out: &mut [Choice]) -> usize;
    /// Whether `choice` is one of `player`'s legal choices (the set [`Generation::choices`]
    /// returns).
    fn is_legal(&self, player: Player, choice: Choice) -> bool {
        let mut buf = [Choice::Pass; 16];
        let n = self.choices(player, &mut buf);
        buf[..n].contains(&choice)
    }
    /// Apply both players' choices (use `Choice::Pass` for a player without a request). Both
    /// choices are validated with [`Generation::is_legal`] first; on an illegal choice the battle
    /// is left untouched and the error is returned.
    fn update<S: Sink>(&mut self, c1: Choice, c2: Choice, sink: &mut S) -> Result<Outcome, IllegalChoice> {
        for (player, choice) in [(0, c1), (1, c2)] {
            if !self.is_legal(player, choice) {
                return Err(IllegalChoice { player, choice });
            }
        }
        Ok(self.update_unchecked(c1, c2, sink))
    }
    /// [`Generation::update`] without validation: the hot path for callers that only pass
    /// choices taken from [`Generation::choices`] (the env does, via its action masks). Illegal
    /// choices give unspecified results and may panic.
    fn update_unchecked<S: Sink>(&mut self, c1: Choice, c2: Choice, sink: &mut S) -> Outcome;
    /// Turn number as shown by `|turn|`.
    fn turn(&self) -> u32;
    /// The result so far.
    fn outcome(&self) -> Outcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_roundtrip() {
        for c in [Choice::Pass, Choice::Move(1), Choice::Move(4), Choice::Switch(6)] {
            assert_eq!(Choice::from_u8(c.to_u8()), c);
            assert_eq!(Choice::parse(&c.to_showdown()), Some(c));
        }
    }
}
