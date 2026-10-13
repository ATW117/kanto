//! Batched environment: N independent envs stepped in parallel with rayon, writing into
//! caller-provided (numpy) buffers.

use crate::backend::{Backend, NUM_ACTIONS};
use crate::env::{Env, EnvConfig, StepResult};
use crate::obs::OBS_DIM;
use crate::rng::derive_seed;
use crate::team::TeamSource;
use rayon::prelude::*;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Minimum envs per rayon task (amortises scheduling overhead).
const MIN_CHUNK: usize = 32;

pub struct VecEnv<B: Backend> {
    pub envs: Vec<Env<B>>,
    pub source: TeamSource,
    pub cfg: EnvConfig,
    pool: rayon::ThreadPool,
    /// Panics caught during the last `step`: (env index, message). See [`VecEnv::step`].
    pub errors: Vec<(usize, String)>,
    /// Total panics caught since construction.
    pub total_errors: u64,
}

/// A panic payload as text.
fn panic_message(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "panic".into())
}

/// Error from [`VecEnv::step`].
#[derive(Debug)]
pub enum StepError {
    /// An action is not legal; no env was modified.
    Illegal(IllegalAction),
    /// An env kept panicking even when reset with new teams (the team source or the simulator
    /// is broken beyond a single game).
    Unrecoverable { env: usize, message: String },
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepError::Illegal(e) => e.fmt(f),
            StepError::Unrecoverable { env, message } => write!(f, "env {env} cannot be reset after a simulator panic: {message}"),
        }
    }
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
    /// [N, 2] `opp_action[i, p]`: player `1 - p`'s choice this step in player `p`'s frame
    /// (`env::opp_action` encoding).
    pub opp_action: &'a mut [i16],
    /// [N] the simulator panicked in this env this step (outcome `ERROR`; the env was reset).
    pub error: &'a mut [bool],
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
        VecEnv { envs, source, cfg, pool, errors: Vec::new(), total_errors: 0 }
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
    ///
    /// Each env is stepped under `catch_unwind`: a simulator panic in one env never poisons the
    /// others. That env's game ends with outcome `ERROR` (`done` and `truncated` set, reward 0,
    /// `error` set, the message recorded in [`VecEnv::errors`]) and the env is reset with new
    /// teams. Only an env that still panics after several resets makes `step` fail.
    pub fn step(&mut self, actions: &[i16], out: StepOut) -> Result<(), StepError> {
        let n = self.envs.len();
        assert_eq!(actions.len(), 2 * n);
        let opp = self.cfg.opponent;
        for (i, env) in self.envs.iter().enumerate() {
            for p in 0..if opp.is_some() { 1 } else { 2 } {
                let a = actions[2 * i + p] as i64;
                if !env.is_legal(p, a) {
                    return Err(StepError::Illegal(IllegalAction { env: i, player: p, action: a }));
                }
            }
        }
        let source = &self.source;
        let cfg = &self.cfg;
        let errors = std::sync::Mutex::new(Vec::new());
        let fatal = std::sync::Mutex::new(None);
        self.pool.install(|| {
            self.envs
                .par_iter_mut()
                .enumerate()
                .zip(actions.par_chunks(2))
                .zip(out.obs.par_chunks_mut(2 * OBS_DIM))
                .zip(out.mask.par_chunks_mut(2 * NUM_ACTIONS))
                .zip(out.reward.par_chunks_mut(2))
                .zip(out.done.par_iter_mut())
                .zip(out.truncated.par_iter_mut())
                .zip(out.outcome.par_iter_mut())
                .zip(out.turns.par_iter_mut())
                .zip(out.opp_action.par_chunks_mut(2))
                .zip(out.error.par_iter_mut())
                .with_min_len(MIN_CHUNK)
                .for_each(|(((((((((((i, env), a), obs), mask), reward), done), trunc), oc), turns), opp_action), err)| {
                    let stepped = catch_unwind(AssertUnwindSafe(|| {
                        let a1 = a[0] as usize;
                        let a2 = match opp {
                            Some(kind) => env.opponent_action(kind),
                            None => a[1] as usize,
                        };
                        let r: StepResult = env.step(a1, a2, cfg, source);
                        write_obs(env, cfg, obs, mask);
                        r
                    }));
                    let r = match stepped {
                        Ok(r) => {
                            *err = false;
                            r
                        }
                        Err(e) => {
                            *err = true;
                            errors.lock().unwrap().push((i, panic_message(&*e)));
                            // Abandon the game and start a new one; retry with fresh teams if
                            // the reset itself panics.
                            let mut last = String::new();
                            let recovered = (0..4).any(|_| {
                                let r = catch_unwind(AssertUnwindSafe(|| {
                                    env.reset(source);
                                    write_obs(env, cfg, obs, mask);
                                }));
                                if let Err(e) = &r {
                                    last = panic_message(&**e);
                                }
                                r.is_ok()
                            });
                            if !recovered {
                                fatal.lock().unwrap().get_or_insert((i, last));
                            }
                            StepResult {
                                reward: 0.0,
                                done: true,
                                outcome: crate::env::outcome::ERROR,
                                turns: 0,
                                opp_action: [crate::env::opp_action::NONE; 2],
                            }
                        }
                    };
                    reward[0] = r.reward;
                    reward[1] = -r.reward;
                    *done = r.done;
                    *oc = r.outcome;
                    *trunc = matches!(r.outcome, crate::env::outcome::TRUNCATED | crate::env::outcome::ERROR);
                    *turns = if r.done { r.turns } else { 0 };
                    opp_action.copy_from_slice(&r.opp_action);
                });
        });
        let mut errors = errors.into_inner().unwrap();
        errors.sort_by_key(|e| e.0);
        self.total_errors += errors.len() as u64;
        self.errors = errors;
        if let Some((env, message)) = fatal.into_inner().unwrap() {
            return Err(StepError::Unrecoverable { env, message });
        }
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

/// Observation and mask for both players. With a built-in opponent, player 2's observation and
/// mask rows are all zeros/false (the Rust opponent picks its own actions from the env's
/// internal mask), so nothing about its options leaks to the caller.
fn write_obs<B: Backend>(env: &Env<B>, cfg: &EnvConfig, obs: &mut [f32], mask: &mut [bool]) {
    env.observe(0, cfg, &mut obs[..OBS_DIM]);
    mask[..NUM_ACTIONS].copy_from_slice(&env.mask[0]);
    if cfg.opponent.is_none() {
        env.observe(1, cfg, &mut obs[OBS_DIM..]);
        mask[NUM_ACTIONS..].copy_from_slice(&env.mask[1]);
    } else {
        obs[OBS_DIM..].fill(0.0);
        mask[NUM_ACTIONS..].fill(false);
    }
}
