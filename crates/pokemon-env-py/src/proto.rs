//! `ProtocolBattle` / `ProtocolTeams`: one native Gen 1 battle that speaks the exact Showdown
//! protocol (per-player views, `|request|` JSON) and also gives our env's observations, for
//! playing external agents (poke-env players, Metamon, Foul Play) against ours.

use battle_env::backend::NUM_ACTIONS;
use battle_env::obs::OBS_DIM;
use battle_env::rng::Rng;
use battle_env::team::{parse_teams, Team, TeamSource};
use numpy::{PyArray1, PyArrayMethods};
use pyo3::exceptions::{PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use sim_backend::{try_new_battle, ProtoGame};
use sim_core::Choice;
use sim_gen1::{Rules, StepOutput};

fn rules_of(format: &str) -> PyResult<Rules> {
    match format {
        "gen1ou" => Ok(Rules::OU),
        "gen1nc" => Ok(Rules::NO_CLAUSES),
        f => Err(PyValueError::new_err(format!("unknown format '{f}' (expected 'gen1ou' or 'gen1nc')"))),
    }
}

fn parse_team(text: &str, validate: bool) -> PyResult<Team> {
    let specs = parse_teams(text).map_err(PyValueError::new_err)?;
    let spec = specs.first().ok_or_else(|| PyValueError::new_err("no team in JSON"))?;
    Team::from_spec_opts(spec, validate).map_err(PyValueError::new_err)
}

fn choice_of(s: &str) -> PyResult<Choice> {
    Choice::parse(s).ok_or_else(|| PyValueError::new_err(format!("bad choice '{s}' (expected 'move N', 'switch N' or '')")))
}

/// One step's protocol output as a dict.
fn output_dict<'py>(py: Python<'py>, out: &StepOutput, game: &ProtoGame) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("opp_action_obs_frame", (game.opp_action[0], game.opp_action[1]))?;
    d.set_item("update", out.update.clone())?;
    d.set_item("omniscient", out.view(-1))?;
    d.set_item("spectator", out.view(0))?;
    d.set_item("p1", out.view(1))?;
    d.set_item("p2", out.view(2))?;
    d.set_item("requests", (out.requests[0].clone(), out.requests[1].clone()))?;
    Ok(d)
}

/// A native Gen 1 battle with Showdown protocol output.
///
/// ``ProtocolBattle(p1_team, p2_team, seed=0, names=("p1", "p2"), format="gen1ou",
/// validate=True)``: teams are JSON strings in the docs/DESIGN.md schema; ``seed`` is the
/// 64-bit battle seed (as the env uses). ``start()`` and ``step(c1, c2)`` return a dict:
///
/// - ``update``: the new `Battle#log` lines (with ``|split|`` blocks), as Showdown's
///   ``update`` message;
/// - ``omniscient``, ``spectator``, ``p1``, ``p2``: that update as each channel sees it
///   (``extractChannelMessages``: exact HP for one's own side, percentages for the foe);
/// - ``requests``: ``(p1, p2)`` ``|request|`` JSON strings (``None`` once the battle is over);
/// - ``opp_action_obs_frame``: per player, what its opponent chose this step in its frame (as
///   ``VecEnv``'s ``info["opp_action_obs_frame"]``).
///
/// Choices are Showdown choice strings (``"move 2"``, ``"switch 3"``, ``""`` to pass).
#[pyclass(module = "pokemon_env._pokemon_env")]
pub struct ProtocolBattle {
    game: ProtoGame,
}

impl ProtocolBattle {
    fn build(p1: &Team, p2: &Team, seed: u64, names: (String, String), format: &str, timestamp: u64) -> PyResult<ProtocolBattle> {
        let b = try_new_battle(p1, p2, seed, rules_of(format)?).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let mut game = ProtoGame::new(b, [&names.0, &names.1]);
        game.pb.set_timestamp(timestamp);
        Ok(ProtocolBattle { game })
    }

    fn check_player(p: usize) -> PyResult<usize> {
        if p < 2 {
            Ok(p)
        } else {
            Err(PyIndexError::new_err(format!("player {p} out of range (0 or 1)")))
        }
    }
}

#[pymethods]
impl ProtocolBattle {
    #[new]
    #[pyo3(signature = (p1_team, p2_team, seed=0, names=("p1".to_string(), "p2".to_string()), format="gen1ou", validate=true, timestamp=0))]
    fn new(p1_team: &str, p2_team: &str, seed: u64, names: (String, String), format: &str, validate: bool, timestamp: u64) -> PyResult<Self> {
        let (a, b) = (parse_team(p1_team, validate)?, parse_team(p2_team, validate)?);
        ProtocolBattle::build(&a, &b, seed, names, format, timestamp)
    }

    /// Start the battle (header, leads, first requests).
    fn start<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let out = self.game.start();
        output_dict(py, &out, &self.game)
    }

    /// Apply both players' Showdown choices; raises ``ValueError`` on an illegal choice (the
    /// battle is left unchanged).
    fn step<'py>(&mut self, py: Python<'py>, c1: &str, c2: &str) -> PyResult<Bound<'py, PyDict>> {
        let (a, b) = (choice_of(c1)?, choice_of(c2)?);
        let out = self.game.step(a, b).map_err(|e| PyValueError::new_err(e.to_string()))?;
        output_dict(py, &out, &self.game)
    }

    /// Apply env actions (0..10 per player, legal per ``mask``).
    fn step_actions<'py>(&mut self, py: Python<'py>, a1: usize, a2: usize) -> PyResult<Bound<'py, PyDict>> {
        let out = self.game.step_actions(a1, a2).map_err(PyValueError::new_err)?;
        output_dict(py, &out, &self.game)
    }

    /// Our env's observation and action mask for ``player``: ``(float32[OBS_DIM], bool[10])``.
    fn observe<'py>(&self, py: Python<'py>, player: usize) -> PyResult<(Bound<'py, PyArray1<f32>>, Bound<'py, PyArray1<bool>>)> {
        let p = Self::check_player(player)?;
        let mut obs = vec![0f32; OBS_DIM];
        self.game.observe(p, &mut obs);
        Ok((PyArray1::from_vec(py, obs), PyArray1::from_vec(py, self.game.mask[p].to_vec())))
    }

    /// Hidden-information training labels for ``player`` (as ``VecEnv.opp_team_targets``'s row
    /// ``[i, player]``): ``(species uint8[6], moves uint8[6, 4])``.
    fn opp_team_targets<'py>(&self, py: Python<'py>, player: usize) -> PyResult<(Bound<'py, PyArray1<u8>>, Bound<'py, numpy::PyArray2<u8>>)> {
        let p = Self::check_player(player)?;
        let (mut sp, mut mv) = (vec![0u8; 6], vec![0u8; 24]);
        self.game.opp_team_targets(p, &mut sp, &mut mv);
        Ok((PyArray1::from_vec(py, sp), PyArray1::from_vec(py, mv).reshape([6, 4])?))
    }

    /// Showdown choice string for env action ``action`` of ``player`` (``ValueError`` if illegal).
    fn action_to_choice(&self, player: usize, action: usize) -> PyResult<String> {
        let p = Self::check_player(player)?;
        self.game
            .action_to_choice(p, action)
            .map(|c| if c == Choice::Pass { String::new() } else { c.to_showdown() })
            .ok_or_else(|| PyValueError::new_err(format!("action {action} is not legal for player {p}")))
    }

    /// Env action for a Showdown choice string (``ValueError`` if it is not legal now).
    fn choice_to_action(&self, player: usize, choice: &str) -> PyResult<usize> {
        let p = Self::check_player(player)?;
        let c = choice_of(choice)?;
        self.game.choice_to_action(p, c).ok_or_else(|| PyValueError::new_err(format!("choice '{choice}' is not legal for player {p}")))
    }

    /// Legal Showdown choices for ``player`` (``[""]`` when it can only wait/pass).
    fn legal_choices(&self, player: usize) -> PyResult<Vec<String>> {
        let p = Self::check_player(player)?;
        let mut v: Vec<String> = (0..NUM_ACTIONS)
            .filter(|&a| self.game.mask[p][a])
            .map(|a| self.game.table[p][a])
            .map(|c| if c == Choice::Pass { String::new() } else { c.to_showdown() })
            .collect();
        v.sort();
        Ok(v)
    }

    /// The current ``|request|`` JSON for ``player`` (``None`` once over).
    fn request(&self, player: usize) -> PyResult<Option<String>> {
        let p = Self::check_player(player)?;
        Ok(self.game.pb.battle.request_json(&self.game.pb.sink.log, p))
    }

    /// The whole battle log so far (``Battle#log``).
    #[getter]
    fn log(&self) -> Vec<String> {
        self.game.pb.log().to_vec()
    }

    /// 0 ongoing, 1 p1 won, 2 p2 won, 3 tie.
    #[getter]
    fn outcome(&self) -> u8 {
        match self.game.outcome() {
            sim_core::Outcome::Ongoing => 0,
            sim_core::Outcome::P1Win => 1,
            sim_core::Outcome::P2Win => 2,
            sim_core::Outcome::Tie => 3,
        }
    }

    #[getter]
    fn ended(&self) -> bool {
        self.game.pb.battle.ended
    }

    #[getter]
    fn started(&self) -> bool {
        self.game.pb.battle.started
    }

    #[getter]
    fn turn(&self) -> u32 {
        self.game.pb.battle.turn as u32
    }

    /// Set the value written into ``|t:|`` lines (Showdown writes the Unix time).
    fn set_timestamp(&mut self, t: u64) {
        self.game.pb.set_timestamp(t);
    }
}

/// A team source (``"data/teams/gen1ou:eval"``, ``"random"``, ...) that deals
/// ``ProtocolBattle``s the way ``VecEnv`` deals games: ``battle(seed)`` samples both teams and
/// the battle seed from ``seed``.
#[pyclass(module = "pokemon_env._pokemon_env")]
pub struct ProtocolTeams {
    source: TeamSource,
}

#[pymethods]
impl ProtocolTeams {
    #[new]
    #[pyo3(signature = (teams="data/teams/gen1ou", validate=true))]
    fn new(teams: &str, validate: bool) -> PyResult<Self> {
        Ok(ProtocolTeams { source: TeamSource::from_spec_opts(teams, validate).map_err(PyValueError::new_err)? })
    }

    #[pyo3(signature = (seed, names=("p1".to_string(), "p2".to_string()), format="gen1ou", timestamp=0))]
    fn battle(&self, seed: u64, names: (String, String), format: &str, timestamp: u64) -> PyResult<ProtocolBattle> {
        let mut rng = Rng::new(seed);
        let p1 = self.source.sample(&mut rng);
        let p2 = self.source.sample(&mut rng);
        ProtocolBattle::build(&p1, &p2, rng.next_u64(), names, format, timestamp)
    }

    /// Number of teams in a pool source (``None`` for generated sources).
    fn __len__(&self) -> usize {
        match &self.source {
            TeamSource::Pool(p) => p.len(),
            _ => 0,
        }
    }
}
