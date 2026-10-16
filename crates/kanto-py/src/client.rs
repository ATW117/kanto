//! `ClientBattle`: the env observation and action mask for one player, rebuilt from the Showdown
//! messages a server sends that player (kanto_env::client; docs/CLIENT.md).

use kanto_env::client;
use kanto_env::obs::OBS_DIM;
use numpy::PyArray1;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

/// One player's view of a Showdown battle, as env observations.
///
/// ``feed(message)`` takes each message the server sends for the battle room (a battle-log
/// update or a ``|request|``). ``observe()`` returns ``(obs float32[OBS_DIM], mask bool[10])``
/// after every update, as the env does at every step (a pass-only mask while waiting), and
/// ``choice(a)`` the Showdown choice string for action ``a``.
#[pyclass(module = "kanto._kanto")]
pub struct ClientBattle {
    inner: client::ClientBattle,
}

#[pymethods]
impl ClientBattle {
    /// ``name``: the player's Showdown username (finds the seat before the first request).
    #[new]
    #[pyo3(signature = (name = ""))]
    fn new(name: &str) -> Self {
        ClientBattle { inner: client::ClientBattle::new(name) }
    }

    /// Feed one server message for the battle (newline-separated protocol lines). A request is
    /// applied after the update that follows it (Showdown sends it first).
    fn feed(&mut self, message: &str) -> PyResult<()> {
        self.inner.feed(message).map_err(PyValueError::new_err)
    }

    /// Apply a request that no update followed (a resent request).
    fn flush(&mut self) {
        self.inner.flush();
    }

    /// ``(obs float32[OBS_DIM], mask bool[NUM_ACTIONS])``.
    fn observe<'py>(&self, py: Python<'py>) -> (Bound<'py, PyArray1<f32>>, Bound<'py, PyArray1<bool>>) {
        let mut obs = vec![0f32; OBS_DIM];
        self.inner.observe(&mut obs);
        (PyArray1::from_vec(py, obs), PyArray1::from_vec(py, self.inner.mask().to_vec()))
    }

    /// The Showdown choice string for env action ``a`` (``None`` if illegal or nothing to choose).
    fn choice(&self, a: usize) -> Option<String> {
        self.inner.choice(a)
    }

    /// The observing player (0 = p1, 1 = p2), once known.
    #[getter]
    fn player(&self) -> Option<usize> {
        self.inner.player()
    }

    #[getter]
    fn ended(&self) -> bool {
        self.inner.ended()
    }

    /// A request is waiting for its update.
    #[getter]
    fn has_pending(&self) -> bool {
        self.inner.has_pending()
    }
}
