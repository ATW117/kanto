//! Batched Gen 1 battle environment backed by pkmn/engine (see docs/DESIGN.md).
// Fixed-size battle arrays are indexed in parallel by slot; index loops read clearest.
#![allow(clippy::needless_range_loop)]

pub mod backend;
pub mod bench;
pub mod data;
pub mod env;
pub mod memory;
pub mod obs;
pub mod opponent;
pub mod pkmn1;
pub mod protocol;
pub mod rng;
pub mod team;
pub mod vec_env;

pub use backend::{Action, Backend, Outcome, NUM_ACTIONS};
pub use env::{Env, EnvConfig, MAX_TURNS};
pub use memory::Memory;
pub use obs::{OBS_DIM, OBS_VERSION};
pub use opponent::Opponent;
pub use pkmn1::Pkmn1;
pub use team::{Team, TeamSource};
pub use vec_env::{StepOut, VecEnv};
