//! A single two-player environment: battle + reveal memories + action tables + RNG.

use crate::backend::{Action, Backend, Outcome, MAX_CHOICES, NUM_ACTIONS};
use crate::memory::{Memory, SideTrack, TurnEvents};
use crate::opponent::{self, Opponent};
use crate::rng::Rng;
use crate::team::TeamSource;

/// Default turn cap: battles reaching it end as a truncated tie.
pub const MAX_TURNS: u32 = 1000;

/// Info outcome codes, per env per step.
pub mod outcome {
    pub const ONGOING: i8 = 0;
    pub const P1_WIN: i8 = 1;
    pub const P2_WIN: i8 = 2;
    pub const TIE: i8 = 3;
    pub const TRUNCATED: i8 = 4;
    /// The simulator panicked while stepping this env; the game was abandoned (no winner) and
    /// the env was reset. Also counts as truncated.
    pub const ERROR: i8 = 5;
}

/// Observer-frame encoding of the opponent's action (`StepResult::opp_action`,
/// `info["opp_action_obs_frame"]`; docs/OBSERVATION.md).
pub mod opp_action {
    /// The opponent had no decision (it passed while waiting).
    pub const NONE: i16 = -1;
    /// A move choice that names no specific move (recharge, Gen 1 "Fight" while asleep, frozen
    /// or partially trapped, locked-in turns); `1..=165` is the chosen move's id.
    pub const UNNAMED_MOVE: i16 = 0;
    /// A switch to the opponent Pokémon in appearance-order slot `j` (the observation's
    /// `opp_mon` block `j`) is `SWITCH_BASE + j`.
    pub const SWITCH_BASE: i16 = 166;
    /// Number of classes (`0..CLASSES`; `NONE` is outside).
    pub const CLASSES: i16 = SWITCH_BASE + 6;
}

#[derive(Clone, Debug)]
pub struct EnvConfig {
    pub max_turns: u32,
    /// Built-in policy for player 2 (index 1); `None` = the caller controls both players.
    pub opponent: Option<Opponent>,
    /// Debug only: expose own-side hidden counters in the privileged observation block.
    pub reveal_hidden: bool,
}

impl Default for EnvConfig {
    fn default() -> Self {
        EnvConfig { max_turns: MAX_TURNS, opponent: None, reveal_hidden: false }
    }
}

/// Everything about one env that changes during play. `Copy`: clone/restore is a memcpy.
pub struct Env<B: Backend> {
    pub battle: B::Battle,
    pub mem: [Memory; 2],
    pub rng: Rng,
    /// action index -> raw backend choice, per player (valid where `mask` is set).
    pub table: [[B::Choice; NUM_ACTIONS]; 2],
    pub mask: [[bool; NUM_ACTIONS]; 2],
    /// Updates played in the current game.
    pub steps: u32,
    /// Test hook: the next `step` panics (exercises VecEnv's per-env panic isolation).
    pub inject_panic: bool,
}

impl<B: Backend> Clone for Env<B> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<B: Backend> Copy for Env<B> {}

/// Result of stepping one env.
#[derive(Clone, Copy, Debug, Default)]
pub struct StepResult {
    /// Reward for player 1 (index 0); player 2 gets the negation.
    pub reward: f32,
    pub done: bool,
    pub outcome: i8,
    /// Turn count of the finished game (when `done`).
    pub turns: u32,
    /// `opp_action[p]`: what player `1 - p` chose this step, in player `p`'s frame
    /// (see [`opp_action`]).
    pub opp_action: [i16; 2],
}

#[derive(Debug)]
pub enum StepError {
    IllegalAction { player: usize, action: i64 },
}

impl<B: Backend> Env<B> {
    pub fn new(seed: u64, source: &TeamSource) -> Self {
        let mut rng = Rng::new(seed);
        let p1 = source.sample(&mut rng);
        let p2 = source.sample(&mut rng);
        let battle = B::new_battle(&p1, &p2, rng.next_u64());
        let mut env = Env {
            battle,
            mem: [Memory::default(); 2],
            rng,
            table: [[B::Choice::default(); NUM_ACTIONS]; 2],
            mask: [[false; NUM_ACTIONS]; 2],
            steps: 0,
            inject_panic: false,
        };
        env.start();
        env
    }

    /// Start a new game with fresh teams (keeps the RNG stream).
    pub fn reset(&mut self, source: &TeamSource) {
        let p1 = source.sample(&mut self.rng);
        let p2 = source.sample(&mut self.rng);
        self.battle = B::new_battle(&p1, &p2, self.rng.next_u64());
        self.mem = [Memory::default(); 2];
        self.steps = 0;
        self.start();
    }

    /// Advance through the initial both-pass update(s) that send out the leads.
    fn start(&mut self) {
        self.refresh();
        let pass = |m: &[bool; NUM_ACTIONS]| m[0] && m[1..].iter().all(|&x| !x);
        while pass(&self.mask[0]) && pass(&self.mask[1]) && self.steps < 4 {
            let (c1, c2) = (self.table[0][0], self.table[1][0]);
            B::update(&mut self.battle, c1, c2, &mut self.mem);
            self.steps += 1;
            self.refresh();
        }
        self.steps = 0;
    }

    /// Recompute legal choices, action tables and masks for both players.
    pub fn refresh(&mut self) {
        assert!(self.try_refresh(), "backend returned no legal choices");
    }

    /// [`Env::refresh`]; `false` if a player has no legal choice or a choice maps outside the
    /// action space (impossible for a live game, but a restored blob may hold a finished one).
    fn try_refresh(&mut self) -> bool {
        let mut buf = [B::Choice::default(); MAX_CHOICES];
        for p in 0..2 {
            self.mask[p] = [false; NUM_ACTIONS];
            let n = B::choices(&self.battle, p, &mut buf);
            if n == 0 {
                return false;
            }
            for &c in &buf[..n] {
                let a = B::action_of(&self.battle, p, c).index();
                if a >= NUM_ACTIONS {
                    return false;
                }
                debug_assert!(!self.mask[p][a], "two choices map to action {a}");
                self.mask[p][a] = true;
                self.table[p][a] = c;
            }
        }
        true
    }

    pub fn is_legal(&self, player: usize, action: i64) -> bool {
        (0..NUM_ACTIONS as i64).contains(&action) && self.mask[player][action as usize]
    }

    /// Pick player 2's action with the built-in opponent.
    pub fn opponent_action(&mut self, kind: Opponent) -> usize {
        let view = (kind == Opponent::MaxDamage).then(|| B::view(&self.battle));
        opponent::pick(kind, view.as_ref(), 1, &self.mask[1], &mut self.rng)
    }

    /// Play one update. Actions must be legal (check with `is_legal` first). Auto-resets when
    /// the game ends.
    pub fn step(&mut self, a1: usize, a2: usize, cfg: &EnvConfig, source: &TeamSource) -> StepResult {
        if std::mem::take(&mut self.inject_panic) {
            panic!("injected panic (Env::inject_panic test hook)");
        }
        let (c1, c2) = (self.table[0][a1], self.table[1][a2]);
        let chosen =[B::action_of(&self.battle, 0, c1), B::action_of(&self.battle, 1, c2)];
        let moves = [B::choice_move(&self.battle, 0, c1), B::choice_move(&self.battle, 1, c2)];
        let o = B::update(&mut self.battle, c1, c2, &mut self.mem);
        self.steps += 1;
        // Labels use the observers' memories after the update (a switch-in is revealed by it).
        let label = |obs: usize| -> i16 {
            let q = 1 - obs;
            match chosen[q] {
                Action::Pass => opp_action::NONE,
                Action::Move(_) => moves[q] as i16,
                Action::Switch(slot) => {
                    let m = &self.mem[obs];
                    let j = if m.is_revealed(slot) { m.slot_of[slot as usize] } else { m.n_revealed.min(5) };
                    opp_action::SWITCH_BASE + j as i16
                }
            }
        };
        let opp = [label(0), label(1)];
        let turn = B::turn(&self.battle);
        let (reward, code) = match o {
            Outcome::P1Win => (1.0, outcome::P1_WIN),
            Outcome::P2Win => (-1.0, outcome::P2_WIN),
            Outcome::Tie => (0.0, outcome::TIE),
            Outcome::Ongoing if turn >= cfg.max_turns => (0.0, outcome::TRUNCATED),
            Outcome::Ongoing => (0.0, outcome::ONGOING),
        };
        let done = code != outcome::ONGOING;
        if done {
            self.reset(source);
        } else {
            self.refresh();
        }
        StepResult { reward, done, outcome: code, turns: turn, opp_action: opp }
    }

    pub fn observe(&self, player: usize, cfg: &EnvConfig, out: &mut [f32]) {
        B::observe(&self.battle, player, &self.mem[player], cfg.reveal_hidden, out);
    }

    /// Serialise this env's full state (battle, memories, RNG, step count) into a blob.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(B::BATTLE_BYTES + 2 * 160 + 16);
        out.extend_from_slice(STATE_MAGIC);
        B::battle_to_bytes(&self.battle, &mut out);
        for m in &self.mem {
            write_memory(m, &mut out);
        }
        out.extend_from_slice(&self.rng.state().to_le_bytes());
        out.extend_from_slice(&self.steps.to_le_bytes());
        out
    }

    /// Restore from `to_bytes` output.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader { b: bytes.strip_prefix(STATE_MAGIC)? };
        let battle = B::battle_from_bytes(r.take(B::BATTLE_BYTES)?)?;
        let mem = [read_memory(&mut r)?, read_memory(&mut r)?];
        let rng = Rng::from_state(u64::from_le_bytes(r.take(8)?.try_into().ok()?));
        let steps = u32::from_le_bytes(r.take(4)?.try_into().ok()?);
        if !r.b.is_empty() {
            return None;
        }
        let mut env = Env {
            battle,
            mem,
            rng,
            table: [[B::Choice::default(); NUM_ACTIONS]; 2],
            mask: [[false; NUM_ACTIONS]; 2],
            steps,
            inject_panic: false,
        };
        env.try_refresh().then_some(env)
    }
}

const STATE_MAGIC: &[u8] = b"PKENV\x01";

struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.b.len() < n {
            return None;
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Some(h)
    }
    fn byte(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn arr<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
}

fn write_memory(m: &Memory, out: &mut Vec<u8>) {
    out.push(m.n_revealed);
    out.extend_from_slice(&m.reveal_order);
    out.extend_from_slice(&m.slot_of);
    m.moves.iter().for_each(|x| out.extend_from_slice(x));
    m.move_uses.iter().for_each(|x| out.extend_from_slice(x));
    out.push(m.opp_transformed as u8);
    m.sleep_obs.iter().for_each(|x| out.extend_from_slice(x));
    for t in &m.track {
        out.extend_from_slice(&[t.confusion_elapsed, t.disable_elapsed, t.trap_elapsed, t.toxic_elapsed, t.mimic_move]);
    }
    for e in &m.last {
        let flags = [
            e.moved,
            e.moved_first,
            e.called,
            e.missed,
            e.crit,
            e.super_effective,
            e.resisted,
            e.immune,
            e.failed,
            e.switched,
        ]
        .iter()
        .enumerate()
        .fold(0u16, |acc, (i, &f)| acc | ((f as u16) << i));
        out.extend_from_slice(&flags.to_le_bytes());
        out.push(e.mv);
        out.push(e.cant);
    }
}

fn read_memory(r: &mut Reader) -> Option<Memory> {
    let mut m = Memory { n_revealed: r.byte()?, reveal_order: r.arr()?, slot_of: r.arr()?, ..Default::default() };
    for k in 0..6 {
        m.moves[k] = r.arr()?;
    }
    for k in 0..6 {
        m.move_uses[k] = r.arr()?;
    }
    m.opp_transformed = r.byte()? != 0;
    for p in 0..2 {
        m.sleep_obs[p] = r.arr()?;
    }
    for p in 0..2 {
        let [a, b, c, d, e] = r.arr::<5>()?;
        m.track[p] =
            SideTrack { confusion_elapsed: a, disable_elapsed: b, trap_elapsed: c, toxic_elapsed: d, mimic_move: e };
    }
    for p in 0..2 {
        let f = u16::from_le_bytes(r.arr()?);
        let bit = |i: u32| (f >> i) & 1 != 0;
        m.last[p] = TurnEvents {
            moved: bit(0),
            moved_first: bit(1),
            called: bit(2),
            missed: bit(3),
            crit: bit(4),
            super_effective: bit(5),
            resisted: bit(6),
            immune: bit(7),
            failed: bit(8),
            switched: bit(9),
            mv: r.byte()?,
            cant: r.byte()?,
        };
    }
    m.is_valid().then_some(m)
}
