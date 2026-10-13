# Native simulator: design (Stage 3)

Goal: an open, fast, **Showdown-exact** battle simulator in Rust, built for several generations and
starting with Gen 1. It implements `battle_env::Backend`, so it can replace pkmn/engine under the
unchanged Python API: `VecEnv(..., backend="native")`.

"Exact" means: given the same teams, PRNG seed and choices, the native simulator makes the same
PRNG calls (same ranges, same order) as pokemon-showdown 0.11.11. It also reaches the same state
after every decision (HP, status, sleep counters, PP, boosts, modified stats, volatiles and their
counters, Gen 1 move-selection memory, the cached `speed` values Showdown sorts by) and offers the
same legal choices. The parity harness (below) checks this; it is the main deliverable.

## Crates

```
crates/sim-core     PRNG (Gen 5 LCG + sodium/ChaCha20), speedSort/comparePriority with tie RNG,
                    PRNG call tracing (feature "trace"), Choice / Event / Sink, the Generation trait
crates/sim-data     tables generated per gen from Showdown's resolved dex (tools/gen_sim_data.js)
crates/sim-gen1     the Gen 1 battle: state.rs (Copy state), battle.rs (turn loop, queue, requests),
                    events.rs (statuses, volatiles, damage, boosts, handler dispatch),
                    moves.rs (runMove/useMove/tryMoveHit/moveHit/getDamage + move callbacks),
                    ser.rs (validated fixed-size serialisation for clone_state)
crates/sim-backend  battle_env::Backend impls (Native1 = [Gen 1] OU rules, Native1Nc = no clauses),
                    examples/bench.rs (speed vs pkmn)
crates/sim-parity   the `parity` CLI: Showdown fuzzing/replay/shrinking, fixtures, diff vs pkmn
parity/showdown     runner.js (drives Showdown), patches.js (pkmn's Showdown patches, optional)
parity/fixtures     Node-free regression corpora + harness-sensitivity fixtures
```

### State

`sim_gen1::Battle` is a plain `Copy` struct of about 1.4 KB. Clone is a memcpy, and the hot path
does not allocate. The PRNG is stored inside the battle. The layout follows Showdown's objects, so
each field Showdown reads has exactly one home:

- `Mon`: a team member as it persists on the bench, indexed by original team slot. It holds HP,
  status and sleep counters, base move slots, Showdown's cached `speed` sort key, `fainted`,
  `subFainted`, and the modified stats left by the last `clearVolatile`.
- `Active`: everything `clearVolatile` resets. That includes boosts, modified/stored stats,
  types, Transform/Mimic move slots, a volatile bitset with per-volatile data, and an insertion
  stamp per volatile. Showdown iterates volatiles in insertion order, and that order decides how
  tied handlers are shuffled.
- `Side`: `side.pokemon` order, `pokemonLeft`, and Gen 1 move-selection memory:
  `lastSelectedMove`, `lastEnemySelectedMove`, `lastSelectedMoveSlot`, `lastMove`,
  `lastEnemyMove`.
- `Battle`: `lastDamage`, the turn, a fixed-capacity action queue, the faint queue, rules
  (clauses, compat mode), and the winner.

### Mechanics: plain Rust, but Showdown's ordering

There is no event bus. Each Showdown function is ported as a Rust function with the same name and
order (`turn_loop`, `run_action`, `run_move`, `use_move`, `try_move_hit`, `move_hit`,
`get_damage`, ...). Showdown's event system matters for exactness in only one way: wherever it
sorts handlers or actions (`speedSort`), a tie consumes the PRNG. So each place where Gen 1 has
more than one handler builds the handlers actually present, in Showdown's discovery order, with
their (order, priority, speed, subOrder) keys. It then runs the generic `sim_core::sort::speed_sort`,
which is Showdown's selection sort plus Fisher-Yates on ties. The places are:

- `BeforeMove`, `AfterMoveSelf`, `AfterMove`, `DisableMove`, `SetStatus` (Sleep/Freeze Clause Mod
  handlers tie), and `fieldEvent('Residual')`.
- `eachEvent('Update'/'BeforeTurn')`: speed ties between the actives consume a call after every
  action.
- `queue.sort` and `insertChoice` (`runSwitch` insertion ties).

Floating-point expressions Showdown evaluates in JS doubles (accuracy and stat-stage multipliers,
`modifyStat`) are evaluated in `f64` in the same order. Integer truncation follows `dex.trunc`.

Where Showdown's behaviour comes from inherited mods (gen1 → gen2 → gen3 → gen4 → base), the port
follows the resolved code. Examples: `runSwitch` and `applyRecoilDamage` come from the gen4 mod,
and `getActionSpeed` comes from gen3. The fuzzer found the one inherited override missed in the
first version (gen4's flooring recoil). Data-only properties (base stats, move numbers, accuracy,
secondaries, flags, the type chart, Metronome's candidate list) are generated from Showdown's
resolved `Dex.mod('gen1')`, so they cannot drift.

### PRNG and seeding

Showdown 0.11.11 chooses the generator from the seed string. `"a,b,c,d"` selects the Gen 5 64-bit
LCG, which pkmn/engine also uses. `"sodium,<hex>"` selects ChaCha20, Showdown's default for unseeded
battles. Both are implemented bit-exactly, and test vectors come from Node. `random(n)`,
`random(m, n)`, `randomChance`, `sample` and `shuffle` reproduce `PRNG#random` exactly. The env
passes a `u64` seed, which is the Gen 5 seed with words `a..d` big-endian, as in pkmn.

### Vanilla vs patched Showdown (decision)

pkmn/engine matches a *patched* Showdown (docs/TESTING.md in pkmn/engine). Its patches are:

- `eachEvent`/`fieldEvent` do not speed-sort before Gen 3.
- `insertChoice` uses host order.
- Handler priorities break the Sleep/Freeze Clause Mod tie and the Disable/Bide tie.

These remove artificial speed ties. **Ground truth here is vanilla Showdown 0.11.11.**
`Rules.compat = Compat::Pkmn` switches the five affected sites to the patched behaviour.
`parity/showdown/patches.js` applies the same patches to Showdown, so both modes are checked
against a Showdown reference. The difference is large: with `[Gen 1] OU` rules, 85.7% of random
battles diverge between vanilla and patched Showdown, almost all within ~14 decisions. Every
status attempt consumes an extra PRNG frame in vanilla (Sleep vs Freeze Clause tie), and every
action under a speed tie does too.

### Per-generation extension

`sim_core::Generation` is the per-gen interface: `request`, `choices`, `update(c1, c2, sink)`,
`turn`, `outcome`. `sim_core::Event`/`Sink` is a structured event stream; the env builds reveal
memory from it, and `()` compiles it away. A new generation adds:

1. A `sim-genN` crate with its own `Copy` state and mechanics, ported the same way.
2. `sim-data/src/genN.rs`, from `tools/gen_sim_data.js` pointed at `Dex.mod('genN')`.
3. A `Backend` impl in `sim-backend`.

The runner, replay, snapshot comparison and fuzzer are generation-agnostic except for the snapshot
mapping (`sim-parity/src/snapshot.rs`). The PRNG, sorting, choices and tracing are shared.

## Parity harness

```
spec (teams, seed, choiceSeed|choices, format, patch)
   │                         ┌──────────────── parity/showdown/runner.js (Node, N workers) ─────┐
   └──► Showdown Battle ───► │ per decision: legal choices (from the request), chosen choices,   │
                             │ protocol log, every PRNG call [call site, lo, hi, value], state   │
                             └──────────────────────────────────────────────────────────────────┘
                                                   │ JSON lines
   native Battle ◄── same seed/choices ── crates/sim-parity (replay.rs)
     compare per step: legal choices → PRNG call sequence → public events → full state snapshot
     first divergence → category + context (diffs, PRNG calls side by side, log)
```

- **Runner** (`runner.js`): drives `new Battle({formatid, seed})` synchronously (no BattleStream)
  without team validation. `gen1ou` has the clauses; `gen1nc` is `gen1ou@@@!Sleep Clause
  Mod,!Freeze Clause Mod`. Choices are either replayed or drawn uniformly from the request's
  legal choices with a separate mulberry32 stream, so the battle PRNG is untouched. It records,
  per decision step: legal choices, choices, protocol log, labelled PRNG calls (stack-derived
  call sites) and a state snapshot. `PS_PATCH=pkmn` applies pkmn's patches; Showdown freezes
  condition objects, so the patch goes on the dex data at process start.
- **Replay** (`parity replay`, `fuzz`): runs the native sim with the same seed and choices, with
  PRNG tracing on. At each step it compares:
  1. the legal choice lists (= the env's masks);
  2. the PRNG calls (range and value);
  3. the public event stream the env consumes (moves with `[from]`, switches, `cant` reasons);
  4. the full state snapshot (`snapshot.rs` mirrors `runner.js`).

  The first mismatch is reported with a category, such as `rng: showdown[getDamage<moveHit]
  ours[tryMoveHit.accuracy]` or `state: .sides[].active.volatiles.confusion.time`. The report
  includes the diff, the PRNG calls around it side by side, and that step's log.
- **Fuzzer** (`parity fuzz`) generates random specs with these options:
  - Teams: `random` (any 1-4 moves, random DVs/stat exp/levels), `legal` (learnsets), `randbats`,
    `stall` (status moves, Transform, Ghosts, partial trapping, Bide: reaches the Endless Battle
    Clause and the 1000-turn limit), or `mixed`.
  - Seeds: Gen 5 or sodium.
  - Format and patch mode.

  It runs N Showdown workers (≤ 5 here), replays each battle, and writes `report.json`. The report
  has: divergence rate, kinds and top categories; moves/cants at divergent steps; verified-move
  and event coverage; and endings. It also writes `failures.jsonl`, keeping the earliest examples
  per category.
- **Shrinker** (`parity shrink`): for each failure category, it searches battles between only the
  Pokémon involved in the divergent step (full sets, new seeds and choices). It keeps the
  earliest/smallest one with the same category and saves it with explicit choices as a fixture.
- **Fixtures** (`parity fixtures`, also a `cargo test`, no Node needed):
  - `corpus-*.jsonl`: Showdown battles stored compactly (spec + per-step legal choices + state
    hash; the hash covers the PRNG seed). Together they verify all 165 moves.
  - `divergent/`: shrunk vanilla-vs-patched cases the harness must keep detecting with the same
    category. This guards the harness itself.
- **pkmn diff** (`parity diff-pkmn`): steps pkmn/engine and the native sim (patched mode, OU
  rules) in lockstep. Actions are drawn from the intersection of both legal sets, and active
  slots, HP and status are compared after every update. This needs no Node and is fast, but pkmn
  targets an older patched Showdown, so it is a drift detector, not the ground truth.

Run: `PS_DIR=<node_modules/pokemon-showdown> NODE=<node> cargo run --release -p sim-parity --bin
parity -- fuzz --battles 5000 --jobs 5 --teams mixed`.

## Env integration

`VecEnv(..., backend="pkmn" | "native" | "native-nc")`. `native` uses `[Gen 1] OU` rules; pkmn's
`-Dshowdown` build also enforces both clauses, so this is the drop-in replacement.

Semantics match the pkmn backend:

- Battles start unstarted, with `Pass` as the only choice.
- `Move(k)` is the k-th entry of Showdown's request.
- `Switch` maps to the original team slot.

One deliberate difference follows Showdown 0.11.11: a sleeping or frozen Pokémon (or one that is
partially trapped) is offered a single "Fight" action, `Move(0)`. pkmn offers all moves with PP.

Reveal memory is updated from the simulator's event stream, through the same backend-agnostic
`battle_env::reveal` code the pkmn backend now uses. `clone_state` blobs use the validated
serialiser; a malformed blob is rejected, never transmuted.
