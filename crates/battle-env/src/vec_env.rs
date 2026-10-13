//! Batched environment: N independent envs stepped in parallel with rayon, writing into
//! caller-provided (numpy) buffers.

use crate::backend::{Backend, NUM_ACTIONS};
use crate::env::{Env, EnvConfig, StepResult};
use crate::obs::OBS_DIM;
use crate::rng::derive_seed;
use crate::team::TeamSource;
use rayon::prelude::*;

/// Minimum envs per rayon task (amortises scheduling overhead).
const MIN_CHUNK: usize = 32;

pub struct VecEnv<B: Backend> {
    pub envs: Vec<Env<B>>,
    pub source: TeamSource,
    pub cfg: EnvConfig,
    pool: rayon::ThreadPool,
}

/// Output buffers for one step; slices are row-major over envs.
pub struct StepOut<'a> {
    /// [N, 2, OBS_DIM]
    pub obs: &'a mut [f32],
    /// [N, 2, NUM_ACTIONS]
    pub mask: &'a mut [bool],
    /// [N, 2]
    pub reward: &'a mut [f32],
    /// [N]
    pub done: &'a mut [bool],
    /// [N] the game hit the turn cap (a subset of `done`)
    pub truncated: &'a mut [bool],
    /// [N] (`env::outcome` codes)
    pub outcome: &'a mut [i8],
    /// [N] turn count at the end of the finished game (0 if not done)
    pub turns: &'a mut [u32],
}

#[derive(Debug)]
pub struct IllegalAction {
    pub env: usize,
    pub player: usize,
    pub action: i64,
}

impl std::fmt::Display for IllegalAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "illegal action {} for player {} in env {}", self.action, self.player, self.env)
    }
}

impl<B: Backend> VecEnv<B> {
    pub fn new(num_envs: usize, seed: u64, source: TeamSource, cfg: EnvConfig, threads: usize) -> Self {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads.max(1)).build().expect("thread pool");
        let envs = pool.install(|| {
            (0..num_envs).into_par_iter().map(|i| Env::new(derive_seed(seed, i as u64), &source)).collect()
        });
        VecEnv { envs, source, cfg, pool }
    }

    pub fn num_envs(&self) -> usize {
        self.envs.len()
    }

    /// Start fresh games in every env. With `seed`, per-env RNG streams are re-derived from it.
    pub fn reset(&mut self, seed: Option<u64>, obs: &mut [f32], mask: &mut [bool]) {
        let source = &self.source;
        let cfg = &self.cfg;
        self.pool.install(|| {
            self.envs
                .par_iter_mut()
                .zip(obs.par_chunks_mut(2 * OBS_DIM))
                .zip(mask.par_chunks_mut(2 * NUM_ACTIONS))
                .enumerate()
                .with_min_len(MIN_CHUNK)
                .for_each(|(i, ((env, obs), mask))| {
                    match seed {
                        Some(s) => *env = Env::new(derive_seed(s, i as u64), source),
                        None => env.reset(source),
                    }
                    write_obs(env, cfg, obs, mask);
                });
        });
    }

    /// Step all envs. `actions` is [N, 2]; player 2's column is ignored when a built-in opponent
    /// is configured. All actions are validated before any env is touched.
    pub fn step(&mut self, actions: &[i16], out: StepOut) -> Result<(), IllegalAction> {
        let n = self.envs.len();
        assert_eq!(actions.len(), 2 * n);
        let opp = self.cfg.opponent;
        for (i, env) in self.envs.iter().enumerate() {
            for p in 0..if opp.is_some() { 1 } else { 2 } {
                let a = actions[2 * i + p] as i64;
                if !env.is_legal(p, a) {
                    return Err(IllegalAction { env: i, player: p, action: a });
                }
            }
        }
        let source = &self.source;
        let cfg = &self.cfg;
        self.pool.install(|| {
            self.envs
                .par_iter_mut()
                .zip(actions.par_chunks(2))
                .zip(out.obs.par_chunks_mut(2 * OBS_DIM))
                .zip(out.mask.par_chunks_mut(2 * NUM_ACTIONS))
                .zip(out.reward.par_chunks_mut(2))
                .zip(out.done.par_iter_mut())
                .zip(out.truncated.par_iter_mut())
                .zip(out.outcome.par_iter_mut())
                .zip(out.turns.par_iter_mut())
                .with_min_len(MIN_CHUNK)
                .for_each(|((((((((env, a), obs), mask), reward), done), trunc), oc), turns)| {
                    let a1 = a[0] as usize;
                    let a2 = match opp {
                        Some(kind) => env.opponent_action(kind),
                        None => a[1] as usize,
                    };
                    let r: StepResult = env.step(a1, a2, cfg, source);
                    reward[0] = r.reward;
                    reward[1] = -r.reward;
                    *done = r.done;
                    *oc = r.outcome;
                    *trunc = r.outcome == crate::env::outcome::TRUNCATED;
                    *turns = if r.done { r.turns } else { 0 };
                    write_obs(env, cfg, obs, mask);
                });
        });
        Ok(())
    }

    pub fn clone_state(&self, i: usize) -> Vec<u8> {
        self.envs[i].to_bytes()
    }

    pub fn restore_state(&mut self, i: usize, blob: &[u8]) -> Result<(), String> {
        self.envs[i] = Env::from_bytes(blob).ok_or("malformed env state blob")?;
        Ok(())
    }

    /// Write env `i`'s current observation/mask (e.g. after `restore_state`).
    pub fn observe(&self, i: usize, obs: &mut [f32], mask: &mut [bool]) {
        write_obs(&self.envs[i], &self.cfg, obs, mask);
    }
}

/// Observation for both players (player 2's is skipped — zeros — with a built-in opponent) and
/// both masks.
fn write_obs<B: Backend>(env: &Env<B>, cfg: &EnvConfig, obs: &mut [f32], mask: &mut [bool]) {
    env.observe(0, cfg, &mut obs[..OBS_DIM]);
    if cfg.opponent.is_none() {
        env.observe(1, cfg, &mut obs[OBS_DIM..]);
    } else {
        obs[OBS_DIM..].fill(0.0);
    }
    mask[..NUM_ACTIONS].copy_from_slice(&env.mask[0]);
    mask[NUM_ACTIONS..].copy_from_slice(&env.mask[1]);
}
