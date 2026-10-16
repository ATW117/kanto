# kanto

A Showdown-exact simulator for Gen 1 Pokémon battles, written in Rust, with Python bindings and a
fast batched environment for reinforcement learning and search.

Given the same teams, seed and choices, kanto makes the same PRNG calls, reaches the same state and
offers the same legal choices as [Pokémon Showdown](https://github.com/smogon/pokemon-showdown)
0.11.11 `[Gen 1] OU`, at every decision. It can also write Showdown's text protocol (battle log,
per-player views, `|request|` JSON) line for line, so Showdown bots can play it offline. It runs
about 5 M turns/s on one core, roughly 1,000 times faster than Showdown itself.

```python
import numpy as np, kanto

env = kanto.VecEnv(num_envs=4096, seed=0, teams="data/teams/gen1ou", threads=16)
obs, mask = env.reset()                        # float32[N, 2, 752], bool[N, 2, 10]
actions = np.zeros((4096, 2), dtype=np.int16)  # pick legal actions from `mask`
obs, mask, reward, done, info = env.step(actions)
```

## Parity with Showdown

kanto is a port of Showdown's Gen 1 code (`data/mods/gen1` and the parts of `sim/` that Gen 1
uses), with Showdown's event ordering, speed-tie shuffles and PRNG reproduced explicitly. The
parity harness (`crates/kanto-parity`) checks it differentially against pokemon-showdown 0.11.11
running in Node. Every decision step of every fuzzed battle compares:

- the legal choices of both players;
- every PRNG call (call site, range and value);
- every public battle event, in order;
- the full state snapshot, including hidden fields the mechanics read (sleep source, volatile
  insertion order, trap partners, `maxpp`, modified stats of benched Pokémon);
- with `--proto`: the log lines, the four channel views and both `|request|` JSONs, byte for byte.

Results (details in [docs/SIM_STATUS.md](docs/SIM_STATUS.md)):

| Check | Battles | Steps compared | Divergences |
| --- | ---: | ---: | ---: |
| State parity: mixed, targeted, thaw, random, stall, legal, no-clause and pkmn-patched runs | 31,500 | 2,262,971 | 0 |
| Protocol parity (`fuzz --proto`) | 8,998 | 668,877 | 0 |

Fuzzed teams cover all 165 moves. Targeted scenarios force rare mechanics (freeze thaw, Disable vs
locked moves, Counter, Mimic/Transform, Substitute, partial trapping, Bide, the Endless Battle
Clause, speed ties). Mutation testing (`parity/mutate.py`, full run 2026-10-14) checks the harness itself: of 62
one-place mutants of the simulator, 55 are caught. Of the 7 survivors, 6 are argued equivalent
(no Gen 1 state tells them apart: `acc-no-1in256`, `pt-source-ignored`, `pt-source-hp-ignored`,
`fpt-counterpart-ignored`, `disable-bide-check`, `move-this-turn-ignored`), and
`stamp-renumber-reversed` needs 255 volatile insertions in one switch-in, which no corpus reaches;
a kanto-gen1 unit test catches it. Recorded Showdown battles in
`parity/fixtures/` replay without Node in `cargo test`.

## Speed

Single-threaded engine throughput: uniform-random self-play, `[Gen 1] OU`, the same games for both
native engines, on one aarch64 core (NVIDIA DGX Spark, Cortex-X925). No observations are built.

| Engine | gen1ou pool shipped here | gen1ou ladder pool (9,232 teams) |
| --- | ---: | ---: |
| Pokémon Showdown 0.11.11 (JavaScript, Node 22) | 4.3 k turns/s | 5.2 k turns/s |
| **kanto** | **4.98 M turns/s** | **4.59 M turns/s** |
| [pkmn/engine](https://github.com/pkmn/engine) (Zig, `-Dshowdown`, no log) | 6.69 M turns/s | 6.18 M turns/s |

Showdown always builds its text log, so its figure includes protocol output; the kanto and pkmn
figures do not. kanto's protocol mode costs little at the env level: a Python-driven protocol
battle with random choices runs about 1,250 games/s on one thread. pkmn/engine is faster (its
battle state is 384 bytes against kanto's 760) but targets a patched Showdown; kanto matches
vanilla Showdown. The ladder pool is not redistributed (see [Team data](#team-data)). Reproduce:

```bash
cargo run --release -p kanto-backend --features pkmn --example bench -- 20000 data/teams/gen1ou
node bench/showdown_bench.js $PS_DIR data/teams/gen1ou/teams.jsonl 2000
python bench/bench.py --quick             # multi-threaded engine and VecEnv.step throughput
```

At 16 threads, `VecEnv.step` with N = 4096 envs runs about 2.5 M env-steps/s, including
observation encoding and masks.

## Install and build

Requirements: Rust (stable), Python ≥ 3.10, [uv](https://github.com/astral-sh/uv). The optional
pkmn/engine backend also needs Zig 0.16 and the submodule.

```bash
git clone --recurse-submodules git@github.com:ATW117/kanto.git && cd kanto
uv venv .venv && source .venv/bin/activate
uv pip install maturin numpy pytest gymnasium
maturin develop --release                    # builds the Rust extension into the venv
pytest                                       # Python tests
cargo test --release                         # Rust tests (the default members, no Zig needed)
```

With pkmn/engine as an extra backend (`backend="pkmn"`, the `diff-pkmn` check and the speed
comparison):

```bash
git submodule update --init                  # third_party/pkmn-engine (pinned)
maturin develop --release --features pkmn    # ZIG=/path/to/zig if zig 0.16 is not on PATH
cargo test --release --workspace --all-features
```

## Python quickstart

**Batched environment.** `kanto.VecEnv` steps N battles in parallel in Rust and returns numpy
arrays. Both seats are controlled by the caller (self-play), or seat 2 by a built-in opponent
(`opponent="random" | "max_damage" | "lookahead"`). The full contract (rewards, `info`, panic
isolation, `clone_state`/`restore_state` for search) is in [docs/DESIGN.md](docs/DESIGN.md).

```python
env = kanto.VecEnv(num_envs=64, seed=0, teams=kanto.TEAMS_TRAIN, threads=8, opponent="max_damage")
obs, mask = env.reset()
rng = np.random.default_rng(0)
for _ in range(100):
    actions = np.array([[rng.choice(np.flatnonzero(m[0])), 0] for m in mask], dtype=np.int16)
    obs, mask, reward, done, info = env.step(actions)
```

`kanto.gym.Gen1Env` is a single-battle Gymnasium env against a built-in opponent.

**Showdown protocol.** `ProtocolBattle` gives each player exactly what a Showdown server would
send, and accepts Showdown choice strings. See [docs/PROTOCOL.md](docs/PROTOCOL.md).

```python
pb = kanto.ProtocolTeams(kanto.TEAMS_EVAL).battle(seed=1, names=("Alice", "Bob"))
out = pb.start()       # {"update", "omniscient", "spectator", "p1", "p2", "requests", ...}
out = pb.step("move 2", "switch 3")
obs, mask = pb.observe(0)   # the same battle as env observations
```

**Playing on a Showdown server.** `ClientBattle` goes the other way: it rebuilds a player's env
observation and action mask from the messages a Showdown server sends that player, so an agent
trained on `VecEnv` can play on a real server. It matches the env at every step except where the
env shows Gen 1 partial trapping's hidden duration (99.78% of steps exact over 3,000 battles). See
[docs/CLIENT.md](docs/CLIENT.md).

**Observations and actions.** 10 actions: 4 move slots and 6 switch targets (by original team
slot), with a legality mask. The 752-float observation holds only what a Showdown player can see
(the opponent's unrevealed Pokémon and moves are unknown, HP is a percentage).
[docs/OBSERVATION.md](docs/OBSERVATION.md) has the layout;
[docs/OBSERVATION_RULES.md](docs/OBSERVATION_RULES.md) the visibility rules.

## Team data

`teams=` accepts a `.json`/`.jsonl` file or directory of teams, `"data/teams/gen1ou:train"` /
`":eval"` (`kanto.TEAMS_TRAIN` / `TEAMS_EVAL`, a deterministic 90/10 split), `"builtin"`,
`"random"` (random legal learnsets) or `"randbats"` (Showdown's Gen 1 random-battle sets). Teams
are checked for Gen 1 legality (Species Clause, learnsets, DVs) unless `validate=False`.

`data/teams/gen1ou/teams.jsonl` holds 1,000 teams sampled from Smogon's aggregate Gen 1 OU usage
statistics and validated by Showdown's own `TeamValidator`. Real ladder teams can be loaded the
same way; see [data/teams/gen1ou/README.md](data/teams/gen1ou/README.md).

## Running the parity harness

The harness needs Node (≥ 16; tested with 22) and an npm install of pokemon-showdown **0.11.11**, the version
kanto reproduces:

```bash
mkdir -p ~/ps && (cd ~/ps && npm install pokemon-showdown@0.11.11)
export PS_DIR=~/ps/node_modules/pokemon-showdown NODE=$(which node)

cargo run --release -p kanto-parity --bin parity -- fuzz --battles 5000 --jobs 5 --teams mixed
cargo run --release -p kanto-parity --bin parity -- fuzz --battles 2000 --jobs 5 --teams targeted
cargo run --release -p kanto-parity --bin parity -- fuzz --proto --battles 2000 --jobs 5 --teams mixed
cargo run --release -p kanto-parity --bin parity -- fixtures        # Node-free regression corpus
python3 parity/mutate.py --jobs 5                                     # mutation testing (~15 min)
cargo run --release -p kanto-parity --features pkmn --bin parity -- diff-pkmn --battles 5000
```

A divergence is reported with its category (for example `rng: showdown[getDamage<moveHit]
ours[tryMoveHit.accuracy]`), the state diff, the PRNG calls side by side and the step's log;
`parity shrink` minimises it into a fixture. Design: [docs/SIM_DESIGN.md](docs/SIM_DESIGN.md).

## Layout

```
crates/kanto-core/      Showdown's PRNG, speedSort, choices, events, protocol log
crates/kanto-data/      Gen 1 tables generated from Showdown's dex (tools/gen_sim_data.js)
crates/kanto-gen1/      the simulator: Copy battle state, mechanics, protocol output
crates/kanto-env/       Backend trait, VecEnv, observation encoder, reveal memory, opponents, teams
crates/kanto-backend/   the simulator as a kanto-env backend; ProtoGame; examples/bench.rs
crates/kanto-parity/    `parity` CLI: Showdown differential fuzzing, shrinking, fixtures
crates/kanto-py/        PyO3 module kanto._kanto
crates/pkmn-sys/        optional FFI to pkmn/engine (feature `pkmn`)
python/kanto/           Python package (+ gym.py)
parity/                 Showdown runner and patches, fixtures, mutate.py
bench/                  bench.py (Python), showdown_bench.js (Showdown JS)
data/teams/gen1ou/      team pool, split, validator and generator
tests/                  Rust, Python and C tests
```

## Limitations

- Gen 1 singles only, `[Gen 1] OU` with or without Sleep and Freeze Clause Mod. The crates are
  organised per generation (docs/SIM_DESIGN.md), but no other generation exists yet.
- Parity is with pokemon-showdown 0.11.11. Later Showdown releases may change Gen 1 behaviour;
  re-run the harness against them before relying on exactness.
- Showdown 0.11.11 itself crashes ("moveSlot is null") in a rare sleep/freeze + Transform state.
  kanto does not crash there; it does what pkmn/engine does. Those battles have no ground truth.
- No server: kanto does not produce room messages (`|init|`, `|j|`) or request ids.

## Citation

Placeholder: there is no paper or archived release yet, and this entry will change. Until then,
please cite the repository:

```bibtex
@software{kanto,  % placeholder
  author = {ATW117},
  title  = {kanto: a Showdown-exact Gen 1 battle simulator},
  year   = {2026},
  url    = {https://github.com/ATW117/kanto}
}
```

## License

MIT, see [LICENSE](LICENSE). kanto ports code and generates data from Pokémon Showdown and
pkmn/engine, both MIT; [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) lists what derives from
which.

Pokémon and all related names are trademarks of Nintendo, Creatures Inc. and GAME FREAK inc.
kanto is not affiliated with or endorsed by Nintendo, Creatures Inc., GAME FREAK inc. or The
Pokémon Company.
