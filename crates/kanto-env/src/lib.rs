//! Batched Gen 1 battle environment: the `Backend` trait, observation encoder, reveal memory,
//! opponents, team pool and `VecEnv` (see docs/DESIGN.md). The native simulator backend lives in
//! kanto-backend; the pkmn/engine backend (`Pkmn1`) needs the `pkmn` feature.
// Fixed-size battle arrays are indexed in parallel by slot; index loops read clearest.
#![allow(clippy::needless_range_loop)]

pub mod backend;
pub mod bench;
pub mod data;
pub mod env;
pub mod memory;
pub mod obs;
pub mod opponent;
#[cfg(feature = "pkmn")]
pub mod pkmn1;
pub mod protocol;
pub mod reveal;
pub mod rng;
pub mod team;
pub mod vec_env;

pub use backend::{Action, Backend, Outcome, NUM_ACTIONS};
pub use env::{Env, EnvConfig, MAX_TURNS};
pub use memory::Memory;
pub use obs::{OBS_DIM, OBS_VERSION};
pub use opponent::Opponent;
#[cfg(feature = "pkmn")]
pub use pkmn1::Pkmn1;
/// The raw libpkmn bindings, for drivers and benchmarks that step pkmn/engine directly.
#[cfg(feature = "pkmn")]
pub use pkmn_sys;
pub use team::{Team, TeamSource};
pub use vec_env::{StepError, StepOut, VecEnv};
