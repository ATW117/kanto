//! `BattleStream` / `simulate_battle`: Showdown's battle-stream protocol in, Showdown's exact
//! output out (`kanto_backend::sd`).

use kanto_backend::sd;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

/// A drop-in for pokemon-showdown's ``BattleStream`` (``sim/battle-stream``), ``[Gen 1] OU``.
///
/// ``write(text)`` takes battle-stream protocol text (``>start {...}``, ``>player p1 {...}``,
/// ``>p1 move 1``, ...), runs every complete line, and returns what Showdown's
/// ``BattleTextStream`` writes for it: ``update``/``sideupdate``/``end``/``requesteddata``
/// messages, each followed by a blank line (``""`` if nothing was sent). A partial trailing line
/// waits for the next write. ``messages=True`` returns the messages as a list instead
/// (``BattleStream`` object mode: ``"update\n|..."``, ``"sideupdate\np1\n|request|{...}"``, ...).
///
/// Errors Showdown would throw (an unknown command, a choice before the player joined) and teams
/// the native simulator cannot reproduce exactly (abilities other than No Ability, items,
/// non-neutral natures, non-Gen 1 species or moves) raise ``RuntimeError``.
#[pyclass(module = "kanto._kanto", name = "BattleStream")]
pub struct PyBattleStream {
    inner: sd::BattleTextStream,
}

#[pymethods]
impl PyBattleStream {
    #[new]
    #[pyo3(signature = (keep_alive=false))]
    fn new(keep_alive: bool) -> Self {
        let mut inner = sd::BattleTextStream::new();
        inner.stream.keep_alive = keep_alive;
        PyBattleStream { inner }
    }

    #[pyo3(signature = (text, messages=false))]
    fn write(&mut self, py: Python<'_>, text: &str, messages: bool) -> PyResult<Py<PyAny>> {
        if messages {
            // Object mode: split complete lines ourselves.
            let r = self.inner.write_messages(text).map_err(PyRuntimeError::new_err)?;
            return Ok(r.into_pyobject(py)?.into_any().unbind());
        }
        let out = self.inner.write(text).map_err(|e| PyRuntimeError::new_err(e.message))?;
        Ok(out.into_pyobject(py)?.into_any().unbind())
    }

    /// Whether the battle ended (Showdown ends the stream after ``end``).
    #[getter]
    fn ended(&self) -> bool {
        self.inner.stream.ended()
    }

    /// Showdown's ``Battle#log`` so far (``None`` before ``>start``).
    #[getter]
    fn log(&self) -> Option<Vec<String>> {
        self.inner.stream.log().map(<[String]>::to_vec)
    }

    /// ``Battle#inputLog`` (the normalised input that replays this battle).
    #[getter]
    fn input_log(&self) -> Option<Vec<String>> {
        self.inner.stream.input_log().map(<[String]>::to_vec)
    }
}

/// ``pokemon-showdown simulate-battle`` on a whole input (one write, as for ``< file``).
#[pyfunction]
pub fn simulate_battle(text: &str) -> PyResult<String> {
    let mut s = sd::BattleTextStream::new();
    s.write(text).map_err(|e| PyRuntimeError::new_err(e.message))
}

/// Showdown's ``extractChannelMessages(data, [channel])[channel]``: the lines of an ``update``
/// message one channel sees (``-1`` omniscient, ``0`` spectator, ``1``/``2`` a player), as
/// ``getPlayerStreams`` splits them.
#[pyfunction]
pub fn extract_channel(data: &str, channel: i32) -> String {
    let lines: Vec<String> = data.split('\n').map(str::to_string).collect();
    kanto_core::proto::extract_channel(&lines, channel).join("\n")
}
