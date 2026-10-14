//! [`ProtoGame`]: one native Gen 1 battle that produces both the exact Showdown protocol (for
//! external agents: poke-env players, Metamon, Foul Play, a real server in reverse) and the env's
//! own observations and action masks (for our agents), from the same state.
//!
//! The protocol sink does not touch the PRNG or the state, so a `ProtoGame` plays exactly the
//! game a `VecEnv(backend="native"|"native-nc")` env plays for the same battle and choices, and
//! its observations equal the env's (`tests/native.rs::proto_game_matches_env`).

use crate::{map_event, track_flags, Reveal};
use battle_env::backend::{Action, Backend, MAX_CHOICES, NUM_ACTIONS};
use battle_env::memory::Memory;
use battle_env::obs::OBS_DIM;
use battle_env::reveal;
use sim_core::{Choice, Outcome};
use sim_gen1::state::Battle;
use sim_gen1::{ProtocolBattle, StepOutput};

/// A battle with both protocol output and env observations.
#[derive(Clone, Debug)]
pub struct ProtoGame {
    pub pb: ProtocolBattle,
    pub mem: [Memory; 2],
    /// action index -> choice per player (valid where `mask` is set), as in `battle_env::Env`.
    pub table: [[Choice; NUM_ACTIONS]; 2],
    pub mask: [[bool; NUM_ACTIONS]; 2],
}

impl ProtoGame {
    /// A new game (not started: call [`ProtoGame::start`]).
    pub fn new(battle: Battle, names: [&str; 2]) -> ProtoGame {
        ProtoGame { pb: ProtocolBattle::new(battle, names), mem: [Memory::default(); 2], table: [[Choice::Pass; NUM_ACTIONS]; 2], mask: [[false; NUM_ACTIONS]; 2] }
    }

    pub fn battle(&self) -> &Battle {
        &self.pb.battle
    }

    /// Send out the leads: the header, the first update and the first requests.
    pub fn start(&mut self) -> StepOutput {
        let out = self.advance(Choice::Pass, Choice::Pass);
        out.expect("starting a battle cannot fail")
    }

    /// Apply both players' choices (validated; `Choice::Pass` for a player without a request).
    pub fn step(&mut self, c1: Choice, c2: Choice) -> Result<StepOutput, sim_core::IllegalChoice> {
        if !self.pb.battle.started {
            return Ok(self.start());
        }
        self.advance(c1, c2)
    }

    /// Apply env actions (indices into the 10-action space; must be legal per [`Self::mask`]).
    pub fn step_actions(&mut self, a1: usize, a2: usize) -> Result<StepOutput, String> {
        for (p, a) in [(0, a1), (1, a2)] {
            if a >= NUM_ACTIONS || !self.mask[p][a] {
                return Err(format!("illegal action {a} for player {}", p + 1));
            }
        }
        self.step(self.table[0][a1], self.table[1][a2]).map_err(|e| e.to_string())
    }

    fn advance(&mut self, c1: Choice, c2: Choice) -> Result<StepOutput, sim_core::IllegalChoice> {
        let turn_before = self.pb.battle.turn;
        let out = if self.pb.battle.started { self.pb.step(c1, c2)? } else { self.pb.start() };
        let b = &self.pb.battle;
        let events = std::mem::take(&mut self.pb.last_events);
        reveal::apply_events(&Reveal(b), events.iter().filter_map(map_event), &mut self.mem);
        if b.started {
            reveal::update_trackers(&track_flags(b), b.turn > turn_before && turn_before > 0, &mut self.mem);
        }
        self.refresh();
        Ok(out)
    }

    fn refresh(&mut self) {
        let b = &self.pb.battle;
        let mut buf = [Choice::Pass; MAX_CHOICES];
        for p in 0..2 {
            self.mask[p] = [false; NUM_ACTIONS];
            if b.ended {
                continue;
            }
            let n = sim_core::Generation::choices(b, p, &mut buf);
            for &c in &buf[..n] {
                let a = <crate::Native1 as Backend>::action_of(b, p, c).index();
                if a < NUM_ACTIONS {
                    self.mask[p][a] = true;
                    self.table[p][a] = c;
                }
            }
        }
    }

    /// Our env's observation for `player` (`OBS_DIM` floats).
    pub fn observe(&self, player: usize, out: &mut [f32]) {
        assert!(out.len() >= OBS_DIM);
        <crate::Native1 as Backend>::observe(&self.pb.battle, player & 1, &self.mem[player & 1], false, out);
    }

    /// The Showdown choice string of env action `a` for `player` (`None` if illegal).
    pub fn action_to_choice(&self, player: usize, a: usize) -> Option<Choice> {
        (a < NUM_ACTIONS && self.mask[player & 1][a]).then(|| self.table[player & 1][a])
    }

    /// The env action of a Showdown choice (`None` if it is not legal now).
    pub fn choice_to_action(&self, player: usize, c: Choice) -> Option<usize> {
        let p = player & 1;
        (0..NUM_ACTIONS).find(|&a| self.mask[p][a] && self.table[p][a] == c)
    }

    pub fn outcome(&self) -> Outcome {
        self.pb.battle.outcome()
    }

    /// The env action a choice maps to, ignoring legality.
    pub fn action_of(&self, player: usize, c: Choice) -> Action {
        <crate::Native1 as Backend>::action_of(&self.pb.battle, player & 1, c)
    }
}
