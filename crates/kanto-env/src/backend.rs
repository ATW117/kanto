//! The simulator abstraction. Everything above this trait (VecEnv, observation encoder, opponents,
//! Python bindings) is backend-agnostic so Stage 3's Rust simulator can slot in.

use crate::memory::Memory;
use crate::obs::View;
use crate::team::Team;

/// Result of a battle update, from player 1's perspective.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ongoing,
    P1Win,
    P2Win,
    Tie,
}

impl Outcome {
    pub fn is_over(self) -> bool {
        self != Outcome::Ongoing
    }
}

/// What a player can choose, in env terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// No decision this turn (e.g. waiting for the opponent's forced switch).
    Pass,
    /// Use the move in slot 0..4 (also used for forced "continue" turns such as recharge).
    Move(u8),
    /// Switch to the party member with this original team index 0..6.
    Switch(u8),
}

impl Action {
    /// Index in the `NUM_ACTIONS = 10` action space. `Pass` occupies index 0: when a player's
    /// only option is to pass, action 0 is the single legal action.
    #[inline]
    pub fn index(self) -> usize {
        match self {
            Action::Pass => 0,
            Action::Move(k) => k as usize,
            Action::Switch(k) => 4 + k as usize,
        }
    }
}

pub const NUM_ACTIONS: usize = 10;
/// Upper bound on legal choices per player per turn (4 moves + 5 switches, or 1 pass).
pub const MAX_CHOICES: usize = 16;

pub trait Backend: Send + Sync + 'static {
    /// Fixed-size battle state without heap data: cloning is a memcpy.
    type Battle: Copy + Send + Sync;
    /// The backend's raw choice representation.
    type Choice: Copy + Send + Sync + Default;

    /// A new, not-yet-started battle. Both players' only legal choice is `Pass` until the first
    /// update sends out the leads.
    fn new_battle(p1: &Team, p2: &Team, seed: u64) -> Self::Battle;

    /// Fill `out` with `player`'s legal choices; returns how many.
    fn choices(b: &Self::Battle, player: usize, out: &mut [Self::Choice]) -> usize;

    /// The env-level meaning of a raw choice (for masks and action mapping).
    fn action_of(b: &Self::Battle, player: usize, c: Self::Choice) -> Action;

    /// The move id (1..=165) a move choice selects, or 0 when the choice names no specific move
    /// (recharging, Gen 1 "Fight" while asleep/frozen/trapped, locked-in turns). Used for the
    /// observer-frame opponent-action label (`info["opp_action_obs_frame"]`).
    fn choice_move(_b: &Self::Battle, _player: usize, _c: Self::Choice) -> u8 {
        0
    }

    /// Replace the battle's RNG state with one derived from `seed` (search: sampling different
    /// futures from a cloned battle). Default: no-op (the clone replays the same RNG stream).
    fn reseed(_b: &mut Self::Battle, _seed: u64) {}

    /// Whether [`Backend::reseed`] actually replaces the RNG state.
    fn can_reseed() -> bool {
        false
    }

    /// Play one update with both players' choices; updates both players' reveal memories from
    /// the battle log (memory `p` belongs to player `p` and tracks what it has seen).
    fn update(b: &mut Self::Battle, c1: Self::Choice, c2: Self::Choice, mem: &mut [Memory; 2]) -> Outcome;

    /// Serialise a battle for `clone_state` blobs, and the inverse (`None` if malformed).
    fn battle_to_bytes(b: &Self::Battle, out: &mut Vec<u8>);
    fn battle_from_bytes(bytes: &[u8]) -> Option<Self::Battle>;
    /// Serialised battle size in bytes.
    const BATTLE_BYTES: usize;

    /// Current turn number (as shown to players).
    fn turn(b: &Self::Battle) -> u32;

    /// Decode the full (omniscient) state into the backend-agnostic view used by the observation
    /// encoder and the heuristic opponents. Hidden-information filtering happens in `obs`.
    fn view(b: &Self::Battle) -> View;

    /// Encode `player`'s observation into `out` (len `obs::OBS_DIM`). `privileged` additionally
    /// exposes the own side's hidden counters (debugging only; see docs/OBSERVATION.md).
    fn observe(b: &Self::Battle, player: usize, mem: &Memory, privileged: bool, out: &mut [f32]) {
        crate::obs::encode(&Self::view(b), player, mem, privileged, out);
    }

    /// Both players' observations (`out0`, `out1`, each `obs::OBS_DIM` long), equal to two
    /// [`Backend::observe`] calls. The default decodes the view once for both.
    fn observe_both(b: &Self::Battle, mem: &[Memory; 2], privileged: bool, out0: &mut [f32], out1: &mut [f32]) {
        let v = Self::view(b);
        crate::obs::encode(&v, 0, &mem[0], privileged, out0);
        crate::obs::encode(&v, 1, &mem[1], privileged, out1);
    }
}
