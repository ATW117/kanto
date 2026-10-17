//! `ProtocolBattle` / `ProtocolTeams`: one native Gen 1 battle that speaks the exact Showdown
//! protocol (per-player views, `|request|` JSON) and also gives our env's observations, for
//! playing external Showdown agents against agents that use the env.

use kanto_env::backend::NUM_ACTIONS;
use kanto_env::obs::OBS_DIM;
use kanto_env::rng::Rng;
use kanto_env::showdown_team::to_packed;
use kanto_env::team::{parse_teams, Team, TeamSource};
use numpy::{PyArray1, PyArrayMethods};
use pyo3::exceptions::{PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use kanto_backend::{team_specs, ProtoGame};
use kanto_core::{Choice, Prng};
use kanto_gen1::{Rules, StepOutput};

fn rules_of(format: &str) -> PyResult<Rules> {
    match format {
        "gen1ou" => Ok(Rules::OU),
        "gen1nc" => Ok(Rules::NO_CLAUSES),
        f => Err(PyValueError::new_err(format!("unknown format '{f}' (expected 'gen1ou' or 'gen1nc')"))),
    }
}

/// Showdown's format id for a kanto format (what `>start {"formatid": ...}` takes).
fn showdown_format(format: &str) -> &'static str {
    if format == "gen1nc" {
        "gen1ou@@@!Sleep Clause Mod,!Freeze Clause Mod"
    } else {
        "gen1ou"
    }
}

/// A team from JSON (docs/DESIGN.md schema) or Showdown export/packed text, with its nicknames.
pub(crate) fn parse_team(text: &str, validate: bool) -> PyResult<(Team, Vec<String>)> {
    let specs = parse_teams(text).map_err(PyValueError::new_err)?;
    let spec = specs.first().ok_or_else(|| PyValueError::new_err("no team found"))?;
    let team = Team::from_spec_opts(spec, validate).map_err(PyValueError::new_err)?;
    Ok((team, spec.pokemon.iter().map(|p| p.nickname.clone().unwrap_or_default()).collect()))
}

/// A battle seed: an int (the env's 64-bit Gen 5 seed) or a Showdown seed string
/// (`"sodium,<64 hex>"`, `"gen5,<16 hex>"` or `"a,b,c,d"`).
fn prng_of(seed: &Bound<'_, PyAny>) -> PyResult<Prng> {
    if let Ok(n) = seed.extract::<u64>() {
        return Ok(Prng::from_u64(n));
    }
    let s: String = seed.extract().map_err(|_| PyValueError::new_err("seed must be an int or a Showdown seed string"))?;
    Prng::parse(&s).ok_or_else(|| PyValueError::new_err(format!("bad Showdown seed '{s}' (expected 'sodium,<hex>', 'gen5,<hex>' or 'a,b,c,d')")))
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
#[pyclass(module = "kanto._kanto", skip_from_py_object)]
#[derive(Clone)]
pub struct ProtocolBattle {
    game: ProtoGame,
    /// For `input_log`: the initial seed, both packed teams, names, Showdown format id and every
    /// choice made so far.
    seed: String,
    packed: [String; 2],
    names: [String; 2],
    format_id: &'static str,
    choices: Vec<[String; 2]>,
}

impl ProtocolBattle {
    fn build(p1: &Team, p2: &Team, nicks: [Vec<String>; 2], prng: Prng, names: (String, String), format: &str, timestamp: u64) -> PyResult<ProtocolBattle> {
        let seed = prng.seed_string();
        let b = kanto_gen1::Battle::new(&team_specs(p1), &team_specs(p2), prng, rules_of(format)?).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let mut game = ProtoGame::new(b, [&names.0, &names.1]);
        game.pb.set_timestamp(timestamp);
        let packed = [to_packed(p1, &nicks[0]), to_packed(p2, &nicks[1])];
        game.pb.sink.log.nicknames = nicks;
        Ok(ProtocolBattle { game, seed, packed, names: [names.0, names.1], format_id: showdown_format(format), choices: Vec::new() })
    }

    fn record(&mut self, c1: Choice, c2: Choice) {
        let s = |c: Choice| if c == Choice::Pass { String::new() } else { c.to_showdown() };
        self.choices.push([s(c1), s(c2)]);
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
    #[pyo3(signature = (p1_team, p2_team, seed=None, names=("p1".to_string(), "p2".to_string()), format="gen1ou", validate=true, timestamp=0))]
    fn new(p1_team: &str, p2_team: &str, seed: Option<&Bound<'_, PyAny>>, names: (String, String), format: &str, validate: bool, timestamp: u64) -> PyResult<Self> {
        let ((a, na), (b, nb)) = (parse_team(p1_team, validate)?, parse_team(p2_team, validate)?);
        let prng = match seed {
            Some(s) => prng_of(s)?,
            None => Prng::from_u64(0),
        };
        ProtocolBattle::build(&a, &b, [na, nb], prng, names, format, timestamp)
    }

    /// The battle as a Showdown input log (``>start``, ``>player``, then every choice), which
    /// ``pokemon-showdown simulate-battle`` or a ``BattleStream`` replays into the same battle:
    /// an independent check of kanto's output against any Showdown version.
    fn input_log(&self) -> Vec<String> {
        let mut out = vec![format!(">start {}", serde_json::json!({"formatid": self.format_id, "seed": self.seed}))];
        for p in 0..2 {
            out.push(format!(">player p{} {}", p + 1, serde_json::json!({"name": self.names[p], "team": self.packed[p]})));
        }
        for c in &self.choices {
            for p in 0..2 {
                if !c[p].is_empty() {
                    out.push(format!(">p{} {}", p + 1, c[p]));
                }
            }
        }
        out
    }

    /// The whole log as one channel sees it (``extractChannelMessages``): ``-1`` omniscient,
    /// ``0`` spectator (what a Showdown replay holds), ``1``/``2`` the players.
    #[pyo3(signature = (channel=0))]
    fn view_log(&self, channel: i32) -> PyResult<Vec<String>> {
        if !(-1..=2).contains(&channel) {
            return Err(PyValueError::new_err(format!("channel {channel} out of range (-1, 0, 1, 2)")));
        }
        Ok(kanto_core::proto::extract_channel(self.game.pb.log(), channel))
    }

    /// The battle's initial seed as a Showdown seed string.
    #[getter]
    fn seed(&self) -> String {
        self.seed.clone()
    }

    /// ``player``'s team in Showdown's packed format (as ``/utm`` takes it).
    fn packed_team(&self, player: usize) -> PyResult<String> {
        Ok(self.packed[Self::check_player(player)?].clone())
    }

    /// An independent copy of the battle (for search: step the copy, keep the original).
    fn copy(&self) -> Self {
        self.clone()
    }

    fn __copy__(&self) -> Self {
        self.clone()
    }

    fn __deepcopy__(&self, _memo: &Bound<'_, PyAny>) -> Self {
        self.clone()
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
        let started = self.game.pb.battle.started;
        let out = self.game.step(a, b).map_err(|e| PyValueError::new_err(e.to_string()))?;
        if started {
            self.record(a, b);
        }
        output_dict(py, &out, &self.game)
    }

    /// Apply env actions (0..10 per player, legal per ``mask``).
    fn step_actions<'py>(&mut self, py: Python<'py>, a1: usize, a2: usize) -> PyResult<Bound<'py, PyDict>> {
        let (c1, c2) = (self.game.table[0][a1.min(NUM_ACTIONS - 1)], self.game.table[1][a2.min(NUM_ACTIONS - 1)]);
        let started = self.game.pb.battle.started;
        let out = self.game.step_actions(a1, a2).map_err(PyValueError::new_err)?;
        if started {
            self.record(c1, c2);
        }
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
            kanto_core::Outcome::Ongoing => 0,
            kanto_core::Outcome::P1Win => 1,
            kanto_core::Outcome::P2Win => 2,
            kanto_core::Outcome::Tie => 3,
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
#[pyclass(module = "kanto._kanto")]
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
        ProtocolBattle::build(&p1, &p2, [vec![], vec![]], Prng::from_u64(rng.next_u64()), names, format, timestamp)
    }

    /// Number of teams in a pool source (``None`` for generated sources).
    fn __len__(&self) -> usize {
        match &self.source {
            TeamSource::Pool(p) => p.len(),
            _ => 0,
        }
    }
}

/// Convert a team (JSON in the docs/DESIGN.md schema, or Showdown export/packed text) to
/// Showdown's ``"export"`` (teambuilder) or ``"packed"`` format, with explicit EVs/IVs.
#[pyfunction]
#[pyo3(signature = (team, format="export", validate=true))]
pub fn convert_team(team: &str, format: &str, validate: bool) -> PyResult<String> {
    let (t, nicks) = parse_team(team, validate)?;
    match format {
        "export" => Ok(kanto_env::showdown_team::to_export(&t, &nicks)),
        "packed" => Ok(to_packed(&t, &nicks)),
        f => Err(PyValueError::new_err(format!("unknown team format '{f}' (expected 'export' or 'packed')"))),
    }
}

/// Problems Showdown's team validator would report for ``team`` in ``format`` (``"gen1ou"``):
/// kanto's legality checks (Species Clause, learnsets, HP DV) plus the format's bans (Ubers,
/// Dig/Fly, OHKO and Evasion Clauses, Nickname Clause, 0 EVs). Empty list = legal. Showdown's
/// event-only/minimum-level checks are not reproduced (docs/AUDIT_2026-10.md).
#[pyfunction]
#[pyo3(signature = (team, format="gen1ou"))]
pub fn validate_team(team: &str, format: &str) -> PyResult<Vec<String>> {
    let specs = parse_teams(team).map_err(PyValueError::new_err)?;
    let spec = specs.first().ok_or_else(|| PyValueError::new_err("no team found"))?;
    let mut out = Vec::new();
    if let Err(e) = Team::from_spec(spec) {
        out.push(e);
    }
    out.extend(kanto_env::showdown_team::format_problems(spec, format).map_err(PyValueError::new_err)?);
    Ok(out)
}
