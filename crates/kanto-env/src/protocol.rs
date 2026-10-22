//! Minimal parser for pkmn's binary protocol log (engine `docs/PROTOCOL.md`), Gen 1 only.
//! It walks every message (sizes per the engine's `protocol.zig` writer) and yields only the
//! events that reveal memory and the "last turn" features need.

/// `player` is the acting side (0/1); `slot` the Pokémon's original party index (0..6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// `|switch|`: a Pokémon was sent out.
    Switch {
        player: usize,
        slot: u8,
    },
    /// `|move|`; `from` is set for called moves and lock continuations (`[from]`).
    Move {
        player: usize,
        slot: u8,
        mv: u8,
        from: bool,
    },
    /// `LastMiss`: the previous `|move|` missed.
    LastMiss,
    /// `|cant|` with the engine's reason code; `mv` is set for Disable.
    Cant {
        player: usize,
        slot: u8,
        reason: u8,
        mv: u8,
    },
    /// `|-start|X|Disable|MOVE`: reveals that X knows MOVE.
    DisableStart {
        player: usize,
        slot: u8,
        mv: u8,
    },
    /// `|-start|X|Mimic|MOVE`: X copied MOVE from its foe.
    MimicStart {
        player: usize,
        mv: u8,
    },
    /// `|-transform|X|TARGET`.
    Transform {
        player: usize,
    },
    /// `|faint|X`.
    Faint {
        player: usize,
    },
    /// `|-damage|X|...|[from] confusion`: X hurt itself in its confusion.
    ConfusionDamage {
        player: usize,
    },
    /// `|-end|X|<binding move>|[partiallytrapped]` (Showdown only; pkmn/engine does not send it).
    TrapEnd {
        player: usize,
    },
    /// `|-mustrecharge|X`.
    MustRecharge {
        player: usize,
    },
    /// `|-status|X|slp`.
    SleepStart {
        player: usize,
    },
    /// Damage-outcome messages, attributed by the caller to the preceding `|move|` user.
    Crit,
    SuperEffective,
    Resisted,
    Immune,
    Fail,
    Miss,
    /// `|turn|`.
    Turn,
}

/// `|cant|` reason codes.
pub mod cant {
    pub const SLEEP: u8 = 0;
    pub const FREEZE: u8 = 1;
    pub const PARALYSIS: u8 = 2;
    pub const BOUND: u8 = 3;
    pub const FLINCH: u8 = 4;
    pub const DISABLE: u8 = 5;
    pub const RECHARGE: u8 = 6;
    pub const COUNT: usize = 7;
}

mod arg {
    pub const NONE: u8 = 0;
    pub const LAST_STILL: u8 = 1;
    pub const LAST_MISS: u8 = 2;
    pub const MOVE: u8 = 3;
    pub const SWITCH: u8 = 4;
    pub const CANT: u8 = 5;
    pub const FAINT: u8 = 6;
    pub const TURN: u8 = 7;
    pub const WIN: u8 = 8;
    pub const TIE: u8 = 9;
    pub const DAMAGE: u8 = 10;
    pub const HEAL: u8 = 11;
    pub const STATUS: u8 = 12;
    pub const CURE_STATUS: u8 = 13;
    pub const BOOST: u8 = 14;
    pub const CLEAR_ALL_BOOST: u8 = 15;
    pub const FAIL: u8 = 16;
    pub const MISS: u8 = 17;
    pub const HIT_COUNT: u8 = 18;
    pub const PREPARE: u8 = 19;
    pub const MUST_RECHARGE: u8 = 20;
    pub const ACTIVATE: u8 = 21;
    pub const FIELD_ACTIVATE: u8 = 22;
    pub const START: u8 = 23;
    pub const END: u8 = 24;
    pub const OHKO: u8 = 25;
    pub const CRIT: u8 = 26;
    pub const SUPER_EFFECTIVE: u8 = 27;
    pub const RESISTED: u8 = 28;
    pub const IMMUNE: u8 = 29;
    pub const TRANSFORM: u8 = 30;
    pub const DRAG: u8 = 31;
}

// Reason codes that carry extra payload bytes.
const MOVE_FROM: u8 = 1;
const DAMAGE_CONFUSION: u8 = 3;
const DAMAGE_RECOIL_OF: u8 = 5;
const HEAL_DRAIN: u8 = 2;
const STATUS_FROM: u8 = 2;
const ACTIVATE_SUBSTITUTE_BLOCK: u8 = 15;
const ACTIVATE_MAGNITUDE: u8 = 18;
const ACTIVATE_SPITE: u8 = 19;
const START_TYPE_CHANGE: u8 = 9;
const START_DISABLE: u8 = 10;
const START_MIMIC: u8 = 11;
const START_ENCORE: u8 = 16;

/// Decode a `PokemonIdent` byte: (player, original party index).
#[inline(always)]
pub fn ident(byte: u8) -> (usize, u8) {
    (((byte >> 3) & 1) as usize, (byte & 0b111).wrapping_sub(1))
}

pub struct Parser<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Parser { buf, pos: 0 }
    }

    #[inline(always)]
    fn at(&self, i: usize) -> u8 {
        // Out-of-range reads yield 0, which terminates parsing at the next header.
        self.buf.get(self.pos + i).copied().unwrap_or(0)
    }
}

impl Iterator for Parser<'_> {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        loop {
            if self.pos >= self.buf.len() {
                return None;
            }
            // Payload length (excluding the header byte) and the event, if any.
            let (len, ev) = match self.buf[self.pos] {
                arg::NONE => return None,
                arg::LAST_STILL | arg::TIE | arg::CLEAR_ALL_BOOST | arg::FIELD_ACTIVATE | arg::OHKO => (0, None),
                arg::LAST_MISS => (0, Some(Event::LastMiss)),
                arg::MOVE => {
                    let (player, slot) = ident(self.at(1));
                    let from = self.at(4) == MOVE_FROM;
                    (4 + from as usize, Some(Event::Move { player, slot, mv: self.at(2), from }))
                }
                arg::SWITCH | arg::DRAG => {
                    let (player, slot) = ident(self.at(1));
                    (8, Some(Event::Switch { player, slot }))
                }
                arg::CANT => {
                    let (player, slot) = ident(self.at(1));
                    let reason = self.at(2);
                    let disable = reason == cant::DISABLE;
                    let mv = if disable { self.at(3) } else { 0 };
                    (2 + disable as usize, Some(Event::Cant { player, slot, reason, mv }))
                }
                arg::FAINT => (1, Some(Event::Faint { player: ident(self.at(1)).0 })),
                arg::MUST_RECHARGE => (1, Some(Event::MustRecharge { player: ident(self.at(1)).0 })),
                arg::WIN => (1, None),
                arg::MISS => (1, Some(Event::Miss)),
                arg::CRIT => (1, Some(Event::Crit)),
                arg::SUPER_EFFECTIVE => (1, Some(Event::SuperEffective)),
                arg::RESISTED => (1, Some(Event::Resisted)),
                arg::TURN => (2, Some(Event::Turn)),
                arg::DAMAGE => {
                    let ev = (self.at(7) == DAMAGE_CONFUSION).then(|| Event::ConfusionDamage { player: ident(self.at(1)).0 });
                    (7 + (self.at(7) == DAMAGE_RECOIL_OF) as usize, ev)
                }
                arg::HEAL => (7 + (self.at(7) == HEAL_DRAIN) as usize, None),
                arg::STATUS => {
                    let ev = (self.at(2) & 0b111 != 0).then(|| Event::SleepStart { player: ident(self.at(1)).0 });
                    (3 + (self.at(3) == STATUS_FROM) as usize, ev)
                }
                arg::CURE_STATUS | arg::BOOST => (3, None),
                arg::FAIL => (2, Some(Event::Fail)),
                arg::IMMUNE => (2, Some(Event::Immune)),
                arg::HIT_COUNT | arg::PREPARE => (2, None),
                arg::ACTIVATE => match self.at(2) {
                    ACTIVATE_SUBSTITUTE_BLOCK | ACTIVATE_MAGNITUDE => (3, None),
                    ACTIVATE_SPITE => (4, None),
                    _ => (2, None),
                },
                arg::START => {
                    let (player, slot) = ident(self.at(1));
                    match self.at(2) {
                        START_DISABLE => (3, Some(Event::DisableStart { player, slot, mv: self.at(3) })),
                        START_MIMIC => (3, Some(Event::MimicStart { player, mv: self.at(3) })),
                        START_ENCORE => (3, None),
                        START_TYPE_CHANGE => (4, None),
                        _ => (2, None),
                    }
                }
                arg::END => (2, None),
                arg::TRANSFORM => {
                    let (player, _) = ident(self.at(1));
                    (2, Some(Event::Transform { player }))
                }
                other => {
                    debug_assert!(false, "unknown protocol ArgType {other} at {}", self.pos);
                    return None;
                }
            };
            self.pos += 1 + len;
            if ev.is_some() {
                return ev;
            }
        }
    }
}

/// Length of the log up to (excluding) its terminating 0x00 header, or `None` if parsing stopped
/// elsewhere. Used by tests to validate the message-size table against real logs.
pub fn log_len(buf: &[u8]) -> Option<usize> {
    let mut p = Parser::new(buf);
    while p.next().is_some() {}
    (p.pos < buf.len() && buf[p.pos] == arg::NONE).then_some(p.pos)
}
