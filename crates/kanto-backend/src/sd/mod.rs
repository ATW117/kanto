//! Showdown `BattleStream` compatibility: the battle-stream protocol (`>start`, `>player`,
//! `>p1 move 1`, ...) in, Showdown's exact output messages out. See [`stream`].
pub mod dex;
pub mod json;
pub mod sets;
pub mod stream;

pub use stream::{BattleStream, BattleTextStream};
