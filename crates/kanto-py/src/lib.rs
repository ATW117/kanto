//! Python bindings: `kanto._kanto`.
//!
//! The env owns preallocated numpy output arrays and writes into them in place on every
//! `reset`/`step` (zero-copy); the same array objects are returned each call, so callers that keep
//! data across steps must copy it. `step` releases the GIL while stepping in parallel.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

mod client;
mod proto;

use kanto_env::backend::NUM_ACTIONS;
use kanto_env::env::EnvConfig;
use kanto_env::obs::{self, OBS_DIM, OBS_VERSION};
#[cfg(feature = "pkmn")]
use kanto_env::Pkmn1;
use kanto_env::{data, Opponent, StepOut, TeamSource};
use kanto_backend::{Native1, Native1Nc};
use numpy::{PyArray1, PyArray2, PyArray3, PyArrayMethods, PyReadonlyArray2, PyUntypedArrayMethods};
use pyo3::exceptions::{PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

/// The simulator backends compiled into this build.
#[cfg(feature = "pkmn")]
const BACKENDS: &[&str] = &["native", "native-nc", "pkmn"];
#[cfg(not(feature = "pkmn"))]
const BACKENDS: &[&str] = &["native", "native-nc"];

fn backend_error(b: &str) -> String {
    if b == "pkmn" {
        "backend 'pkmn' is not compiled in: rebuild with `maturin develop --release --features pkmn` (needs Zig 0.16)".into()
    } else {
        format!("unknown backend '{b}' (available: {})", BACKENDS.join(", "))
    }
}

/// The env, monomorphised per simulator backend.
enum Inner {
    #[cfg(feature = "pkmn")]
    Pkmn(kanto_env::VecEnv<Pkmn1>),
    Native(kanto_env::VecEnv<Native1>),
    NativeNc(kanto_env::VecEnv<Native1Nc>),
}

/// Dispatch a method call to whichever backend the env uses.
macro_rules! with_inner {
    ($inner:expr, $v:ident => $body:expr) => {
        match $inner {
            #[cfg(feature = "pkmn")]
            Inner::Pkmn($v) => $body,
            Inner::Native($v) => $body,
            Inner::NativeNc($v) => $body,
        }
    };
}

impl Inner {
    fn new(backend: &str, n: usize, seed: u64, source: TeamSource, cfg: EnvConfig, threads: usize) -> Result<Inner, String> {
        Ok(match backend {
            #[cfg(feature = "pkmn")]
            "pkmn" => Inner::Pkmn(kanto_env::VecEnv::new(n, seed, source, cfg, threads)),
            "native" => Inner::Native(kanto_env::VecEnv::new(n, seed, source, cfg, threads)),
            "native-nc" => Inner::NativeNc(kanto_env::VecEnv::new(n, seed, source, cfg, threads)),
            b => return Err(backend_error(b)),
        })
    }
    fn num_envs(&self) -> usize {
        with_inner!(self, v => v.num_envs())
    }
    fn cfg(&self) -> &EnvConfig {
        with_inner!(self, v => &v.cfg)
    }
}

#[pyclass(module = "kanto._kanto")]
struct VecEnv {
    inner: Inner,
    backend: &'static str,
    obs: Py<PyArray3<f32>>,
    mask: Py<PyArray3<bool>>,
    reward: Py<PyArray2<f32>>,
    done: Py<PyArray1<bool>>,
    truncated: Py<PyArray1<bool>>,
    outcome: Py<PyArray1<i8>>,
    turns: Py<PyArray1<u32>>,
    opp_action: Py<PyArray2<i16>>,
    error: Py<PyArray1<bool>>,
}

#[pymethods]
impl VecEnv {
    #[new]
    #[pyo3(signature = (num_envs, seed=0, teams="data/teams/gen1ou", threads=16, opponent=None, max_turns=1000, reveal_hidden=false, backend="native", validate=true))]
    fn new(
        py: Python<'_>,
        num_envs: usize,
        seed: u64,
        teams: &str,
        threads: usize,
        opponent: Option<&str>,
        max_turns: u32,
        reveal_hidden: bool,
        backend: &str,
        validate: bool,
    ) -> PyResult<Self> {
        if num_envs == 0 {
            return Err(PyValueError::new_err("num_envs must be > 0"));
        }
        let source = TeamSource::from_spec_opts(teams, validate).map_err(PyValueError::new_err)?;
        let opponent = opponent.map(Opponent::parse).transpose().map_err(PyValueError::new_err)?;
        let cfg = EnvConfig { max_turns, opponent, reveal_hidden };
        let backend: &'static str = match backend {
            #[cfg(feature = "pkmn")]
            "pkmn" => "pkmn",
            "native" => "native",
            "native-nc" => "native-nc",
            b => return Err(PyValueError::new_err(backend_error(b))),
        };
        let inner = py.detach(|| Inner::new(backend, num_envs, seed, source, cfg, threads)).map_err(PyValueError::new_err)?;
        let n = num_envs;
        Ok(VecEnv {
            inner,
            backend,
            obs: PyArray3::<f32>::zeros(py, [n, 2, OBS_DIM], false).unbind(),
            mask: PyArray3::<bool>::zeros(py, [n, 2, NUM_ACTIONS], false).unbind(),
            reward: PyArray2::<f32>::zeros(py, [n, 2], false).unbind(),
            done: PyArray1::<bool>::zeros(py, n, false).unbind(),
            truncated: PyArray1::<bool>::zeros(py, n, false).unbind(),
            outcome: PyArray1::<i8>::zeros(py, n, false).unbind(),
            turns: PyArray1::<u32>::zeros(py, n, false).unbind(),
            opp_action: PyArray2::<i16>::zeros(py, [n, 2], false).unbind(),
            error: PyArray1::<bool>::zeros(py, n, false).unbind(),
        })
    }

    /// Start new games in all envs. Returns `(obs, mask)` (reused buffers).
    #[pyo3(signature = (seed=None))]
    fn reset<'py>(&mut self, py: Python<'py>, seed: Option<u64>) -> PyResult<(Py<PyArray3<f32>>, Py<PyArray3<bool>>)> {
        let obs = self.obs.bind(py);
        let mask = self.mask.bind(py);
        // SAFETY: the arrays are owned by this object, C-contiguous, and not resized; we hold
        // `&mut self`, so no other Rust code touches them while we write.
        let (o, m) = unsafe { (obs.as_slice_mut()?, mask.as_slice_mut()?) };
        let inner = &mut self.inner;
        py.detach(|| with_inner!(inner, v => v.reset(seed, o, m)));
        Ok((self.obs.clone_ref(py), self.mask.clone_ref(py)))
    }

    /// Step all envs with `actions` (int16[N, 2]). Returns `(obs, mask, reward, done, info)`.
    fn step<'py>(
        &mut self,
        py: Python<'py>,
        actions: PyReadonlyArray2<'py, i16>,
    ) -> PyResult<Bound<'py, pyo3::types::PyTuple>> {
        let n = self.inner.num_envs();
        if actions.shape() != [n, 2] {
            return Err(PyValueError::new_err(format!("actions must have shape ({n}, 2), got {:?}", actions.shape())));
        }
        let acts = actions.as_slice()?;
        let (obs, mask, reward, done, outcome_arr, turns, truncated, opp_action, error) = (
            self.obs.bind(py),
            self.mask.bind(py),
            self.reward.bind(py),
            self.done.bind(py),
            self.outcome.bind(py),
            self.turns.bind(py),
            self.truncated.bind(py),
            self.opp_action.bind(py),
            self.error.bind(py),
        );
        // SAFETY: as in `reset`.
        let out = unsafe {
            StepOut {
                obs: obs.as_slice_mut()?,
                mask: mask.as_slice_mut()?,
                reward: reward.as_slice_mut()?,
                done: done.as_slice_mut()?,
                truncated: truncated.as_slice_mut()?,
                outcome: outcome_arr.as_slice_mut()?,
                turns: turns.as_slice_mut()?,
                opp_action: opp_action.as_slice_mut()?,
                error: error.as_slice_mut()?,
            }
        };
        let inner = &mut self.inner;
        py.detach(|| with_inner!(inner, v => v.step(acts, out).map_err(|e| e.to_string()))).map_err(PyValueError::new_err)?;
        let info = PyDict::new(py);
        info.set_item("outcome", outcome_arr)?;
        info.set_item("turns", turns)?;
        info.set_item("truncated", truncated)?;
        info.set_item("opp_action_obs_frame", opp_action)?;
        info.set_item("error", error)?;
        let errors: Vec<(usize, String)> = with_inner!(&self.inner, v => v.errors.clone());
        info.set_item("errors", errors)?;
        (obs, mask, reward, done, info).into_pyobject(py)
    }

    /// Test hook: make env `i`'s next step panic inside the simulator call (to exercise the
    /// per-env panic isolation).
    fn _inject_panic(&mut self, i: usize) -> PyResult<()> {
        self.check(i)?;
        with_inner!(&mut self.inner, v => v.envs[i].inject_panic = true);
        Ok(())
    }

    /// Simulator panics caught (and turned into `ERROR` outcomes) since construction.
    #[getter]
    fn total_errors(&self) -> u64 {
        with_inner!(&self.inner, v => v.total_errors)
    }

    /// Serialise env `i`'s full state (battle, reveal memories, RNG) to bytes.
    fn clone_state<'py>(&self, py: Python<'py>, i: usize) -> PyResult<Bound<'py, PyBytes>> {
        self.check(i)?;
        Ok(PyBytes::new(py, &with_inner!(&self.inner, v => v.clone_state(i))))
    }

    /// Restore env `i` from `clone_state` bytes and refresh its rows of the obs/mask buffers.
    fn restore_state(&mut self, py: Python<'_>, i: usize, blob: &[u8]) -> PyResult<()> {
        self.check(i)?;
        with_inner!(&mut self.inner, v => v.restore_state(i, blob)).map_err(PyValueError::new_err)?;
        let obs = self.obs.bind(py);
        let mask = self.mask.bind(py);
        let (o, m) = unsafe { (obs.as_slice_mut()?, mask.as_slice_mut()?) };
        let (o, m) = (&mut o[i * 2 * OBS_DIM..(i + 1) * 2 * OBS_DIM], &mut m[i * 2 * NUM_ACTIONS..(i + 1) * 2 * NUM_ACTIONS]);
        with_inner!(&self.inner, v => v.observe(i, o, m));
        Ok(())
    }

    #[getter]
    fn num_envs(&self) -> usize {
        self.inner.num_envs()
    }

    #[getter]
    fn obs_dim(&self) -> usize {
        OBS_DIM
    }

    #[getter]
    fn num_actions(&self) -> usize {
        NUM_ACTIONS
    }

    /// The simulator backend: "native", "native-nc" or "pkmn".
    #[getter]
    fn backend(&self) -> &'static str {
        self.backend
    }

    #[getter]
    fn opponent(&self) -> Option<&'static str> {
        self.inner.cfg().opponent.map(|o| match o {
            Opponent::Random => "random",
            Opponent::MaxDamage => "max_damage",
            Opponent::Lookahead => "lookahead",
        })
    }

    /// Built-in policy actions: int16[N], `kind`'s ("random" | "max_damage" | "lookahead")
    /// legal action for player `seats[i]` (0 or 1) in each env `i`. Uses the envs' own RNGs.
    fn builtin_actions<'py>(&mut self, py: Python<'py>, kind: &str, seats: numpy::PyReadonlyArray1<'py, u8>) -> PyResult<Bound<'py, PyArray1<i16>>> {
        let kind = Opponent::parse(kind).map_err(PyValueError::new_err)?;
        let n = self.inner.num_envs();
        if seats.len() != n {
            return Err(PyValueError::new_err(format!("seats must have length {n}")));
        }
        let seats = seats.as_slice()?.to_vec();
        let mut out = vec![0i16; n];
        let inner = &mut self.inner;
        py.detach(|| with_inner!(inner, v => v.builtin_actions(kind, &seats, &mut out)));
        Ok(PyArray1::from_vec(py, out))
    }

    /// Privileged labels for the hidden-information auxiliary head (training only; never part of
    /// the observation): `(species uint8[N, 2, 6], moves uint8[N, 2, 6, 4])`. Row `[i, p]` is the
    /// opponent team as seen from player `p`: appearance-order slots first (the observation's
    /// `opp_mon` blocks), then the unrevealed Pokémon; true species ids and original move ids,
    /// 0 = empty.
    fn opp_team_targets<'py>(&self, py: Python<'py>) -> PyResult<(Bound<'py, PyArray3<u8>>, Bound<'py, numpy::PyArray4<u8>>)> {
        let n = self.inner.num_envs();
        let mut sp = vec![0u8; n * 12];
        let mut mv = vec![0u8; n * 48];
        let inner = &self.inner;
        py.detach(|| with_inner!(inner, v => v.opp_team_targets(&mut sp, &mut mv)));
        let sp = PyArray1::from_vec(py, sp).reshape([n, 2, 6])?;
        let mv = PyArray1::from_vec(py, mv).reshape([n, 2, 6, 4])?;
        Ok((sp, mv))
    }
}

impl VecEnv {
    fn check(&self, i: usize) -> PyResult<()> {
        if i >= self.inner.num_envs() {
            return Err(PyIndexError::new_err(format!("env index {i} out of range")));
        }
        Ok(())
    }
}

/// Top-level observation blocks as `[(name, offset, size)]`.
#[pyfunction]
fn obs_layout() -> Vec<(String, usize, usize)> {
    obs::layout()
}

/// Offsets of ID (embedding) fields as `[(offset, vocab)]`, vocab in {"species", "move"}.
#[pyfunction]
fn id_offsets() -> Vec<(usize, &'static str)> {
    obs::id_offsets()
}

#[pyfunction]
fn species_names() -> Vec<&'static str> {
    data::SPECIES_NAMES.to_vec()
}

#[pyfunction]
fn move_names() -> Vec<&'static str> {
    data::MOVE_NAMES.to_vec()
}

/// Engine-only benchmark: `(seconds, updates, turns)` for `battles` random games.
#[pyfunction]
#[pyo3(signature = (battles, threads=1, teams="builtin", seed=0, backend="native"))]
fn bench_engine(py: Python<'_>, battles: usize, threads: usize, teams: &str, seed: u64, backend: &str) -> PyResult<(f64, u64, u64)> {
    let source = TeamSource::from_spec(teams).map_err(PyValueError::new_err)?;
    match backend {
        #[cfg(feature = "pkmn")]
        "pkmn" => Ok(py.detach(|| kanto_env::bench::engine_only_with::<Pkmn1>(&source, battles, threads, seed))),
        "native" => Ok(py.detach(|| kanto_env::bench::engine_only_with::<Native1>(&source, battles, threads, seed))),
        "native-nc" => Ok(py.detach(|| kanto_env::bench::engine_only_with::<Native1Nc>(&source, battles, threads, seed))),
        b => Err(PyValueError::new_err(backend_error(b))),
    }
}

#[pymodule]
fn _kanto(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<VecEnv>()?;
    m.add_class::<proto::ProtocolBattle>()?;
    m.add_class::<proto::ProtocolTeams>()?;
    m.add_class::<client::ClientBattle>()?;
    m.add("BACKENDS", BACKENDS.to_vec())?;
    m.add("OBS_DIM", OBS_DIM)?;
    m.add("OBS_VERSION", OBS_VERSION)?;
    m.add("NUM_ACTIONS", NUM_ACTIONS)?;
    m.add("MAX_TURNS", kanto_env::MAX_TURNS)?;
    m.add("SPECIES_UNKNOWN", obs::SPECIES_UNKNOWN)?;
    m.add("MOVE_UNKNOWN", obs::MOVE_UNKNOWN)?;
    m.add("OUTCOME_ERROR", kanto_env::env::outcome::ERROR)?;
    m.add("OPP_ACTION_NONE", kanto_env::env::opp_action::NONE)?;
    m.add("OPP_ACTION_SWITCH_BASE", kanto_env::env::opp_action::SWITCH_BASE)?;
    m.add("OPP_ACTION_CLASSES", kanto_env::env::opp_action::CLASSES)?;
    m.add_function(wrap_pyfunction!(obs_layout, m)?)?;
    m.add_function(wrap_pyfunction!(id_offsets, m)?)?;
    m.add_function(wrap_pyfunction!(species_names, m)?)?;
    m.add_function(wrap_pyfunction!(move_names, m)?)?;
    m.add_function(wrap_pyfunction!(bench_engine, m)?)?;
    Ok(())
}
