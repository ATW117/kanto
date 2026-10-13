# pokemon-agent: Stage 2 design contract

Goal: a fast, batched, Gymnasium-style **Python RL environment for Gen 1 singles**, backed by
[pkmn/engine](https://github.com/pkmn/engine) (Zig, MIT) in Showdown-compatibility mode. Later, the
same environment API will be backed by our own Rust multi-gen simulator (Stage 3), so agents and
training code must only depend on the API below, never on pkmn internals.

## Layout

```
pokemon-agent/
  third_party/pkmn-engine/   git submodule, pinned commit (Zig source)
  crates/
    pkmn-sys/                Rust FFI to libpkmn (Gen 1, showdown mode); build.rs runs `zig build`
    battle-env/              Rust: Backend trait, VecEnv, observation encoder, team sampler, rayon
    pokemon-env-py/          PyO3 + maturin bindings -> Python module `pokemon_env`
  python/pokemon_env/        thin Python package (Gymnasium wrapper, typing, helpers)
  data/teams/gen1ou/         team pool (JSON, schema below)
  tests/                     Rust + pytest tests
  bench/                     throughput benchmarks
  docs/
```

Toolchains on the DGX Spark (aarch64): Rust in `~/.cargo`, Zig 0.16 at
`~/tools/zig-aarch64-linux-0.16.0`, Python 3.12, `uv` at `~/.local/bin/uv` (use a project `.venv`).

## Backend trait (Rust, `battle-env`)

```rust
pub trait Backend: Send + Sync + 'static {
    type Battle: Copy + Send + Sync;             // fixed-size, no heap: clone = memcpy
    type Choice: Copy + Send + Sync + Default;   // backend's raw choice
    const BATTLE_BYTES: usize;
    fn new_battle(p1: &Team, p2: &Team, seed: u64) -> Self::Battle;            // not yet started
    fn choices(b: &Self::Battle, player: usize, out: &mut [Self::Choice]) -> usize; // legal choices
    fn action_of(b: &Self::Battle, player: usize, c: Self::Choice) -> Action;  // Pass/Move(k)/Switch(k)
    fn update(b: &mut Self::Battle, c1: Self::Choice, c2: Self::Choice,
              mem: &mut [Memory; 2]) -> Outcome;                              // one turn + reveals
    fn turn(b: &Self::Battle) -> u32;
    fn view(b: &Self::Battle) -> View;                                         // omniscient decode
    fn observe(b: &Self::Battle, player: usize, mem: &Memory, privileged: bool, out: &mut [f32]);
    fn battle_to_bytes(b: &Self::Battle, out: &mut Vec<u8>);                   // clone_state blobs
    fn battle_from_bytes(bytes: &[u8]) -> Option<Self::Battle>;
}
```
Deviations from the original sketch (Stage 2 implementation):
- `update` takes both players' `Memory` because reveals are parsed from the update's protocol log,
  which only exists inside `update`.
- `Choice` is an associated type plus `action_of`, so env code maps backend choices to the
  10-action space without knowing pkmn's encoding (pkmn switch data is a *current* party position).
- `view` decodes the battle into a backend-agnostic `obs::View`; `observe` (default method) runs the
  shared encoder on it, so Stage 3 only needs to implement `view`.
- Battles start with both players' only choice being `Pass`; the env auto-plays that update (the
  leads are sent out) on reset.
`Outcome` = ongoing / p1 win / p2 win / tie. Turn cap: 1000 turns -> tie (truncated).

## Python API (`pokemon_env`)

```python
env = pokemon_env.VecEnv(num_envs=N, seed=0, teams="data/teams/gen1ou", threads=16)
obs, mask = env.reset()          # obs: float32[N, 2, OBS_DIM], mask: bool[N, 2, NUM_ACTIONS]
obs, mask, reward, done, info = env.step(actions)   # actions: int16[N, 2]
env.clone_state(i) -> bytes / env.restore_state(i, blob)   # for search
```
Implemented details: `reward` is float32[N, 2]; `info` = {`outcome` int8[N] (0 ongoing, 1 p1 win,
2 p2 win, 3 tie, 4 truncated), `truncated` bool[N], `turns` uint32[N]}; output arrays are reused
in place every call (copy to keep). `teams` may also be `"random"` (legal Gen 1 learnsets),
`"randbats"` (Showdown Gen 1 random-battle sets) or `"builtin"` (fixtures). `reveal_hidden=False`
(debug flag, see Observation). With a built-in opponent, `actions[:, 1]` is ignored and `obs[:, 1]`
is zero. Illegal actions raise `ValueError` before any env is modified.
- Both players are controlled by the caller: the agent plays both sides in self-play, or one side
  with a built-in opponent policy (`opponent="random" | "max_damage"`) handled in Rust.
- `step` releases the GIL, steps all battles in parallel (rayon), auto-resets finished battles
  (`done` marks the transition that ended a game), and writes into preallocated numpy buffers.
- Reward: +1 / -1 / 0 at game end for player 0 (player 1 gets the negation). No shaping in the env.
- `NUM_ACTIONS = 10`: 4 move slots + 6 switch slots (index 4+k switches to party slot k, where k is
  the Pokémon's **original** team slot, stable across switches). Illegal actions are masked; Gen 1
  "no choice" turns (recharge, locked moves, Struggle) expose one legal action (index 0), and so
  does waiting while the opponent makes a forced switch (`Pass` maps to index 0).

## Observation (hidden information matters)

Each player sees only what a Showdown player would see:
- Own side: everything a Showdown player sees (species, exact HP, stats, moves and PP, status,
  boosts, volatiles). Hidden durations are **not** included even for the own side: remaining
  sleep/confusion/Disable/partial-trap turns, Substitute HP and the Toxic counter are replaced by
  elapsed/observed counters. `VecEnv(..., reveal_hidden=True)` (debug only) writes the true values
  into a separate privileged block. Visibility rules: `docs/OBSERVATION_RULES.md`.
- Opponent: only revealed information. A Pokémon is revealed when it is sent out. A move is revealed
  when it is used. Visible details are HP **as a percentage**, status, boosts and visible volatiles.
  Unrevealed slots and moves are encoded as "unknown".
- This needs per-player `Memory` (reveal tracking) updated from the battle log each turn.

Vector layout is owned by `battle-env::obs` and documented in `docs/OBSERVATION.md`. Versioned:
`OBS_VERSION`.

## Team data schema (`data/teams/gen1ou/*.json`)

```json
{ "name": "...", "source": "...", "pokemon": [
  { "species": "Tauros", "moves": ["Body Slam","Hyper Beam","Blizzard","Earthquake"],
    "level": 100, "dvs": {"hp":15,"atk":15,"def":15,"spe":15,"spc":15},
    "exp": {"hp":65535,"atk":65535,"def":65535,"spe":65535,"spc":65535} } ] }
```
Names use Showdown spelling (matched case- and punctuation-insensitively, so "Farfetch’d" works).
`dvs`/`exp` optional (default max; the Gen 1 HP DV is always derived from the other DVs). Teams have
1-6 Pokémon. A team source may be a `.json` file (one team or an array), a `.jsonl` file (one team
per line, as in `teams.jsonl`), or a directory of such files (non-recursive, so `usage/` is
ignored).

## Correctness

- Rust tests: a seeded battle replays identically, clone/restore round-trips, and legal masks match
  `choices()`.
- Cross-check against pkmn's own driver for the same seed and choices (identical log/outcome).
- Spot-check versus Showdown (gen1ou, same teams/seed/choices) where practical; full RNG-level
  parity is Stage 3's harness.

## Performance targets (Spark, 16 threads)

- Engine only: >= 1M battles/s aggregate is plausible (pkmn ~108k/s/core).
- Full env step including observation encoding into numpy: >= 2M env-steps/s at N = 4096.
