# Native simulator: status (Gen 1)

Reference: pokemon-showdown **0.11.11**, vanilla (`Compat::Vanilla`, the default). The
pkmn-patched mode (`Compat::Pkmn`) is checked against the same Showdown with
parity/showdown/patches.js. Numbers below come from `parity fuzz` runs on 2026-10-13; see
docs/SIM_DESIGN.md for what is compared.

## Parity summary (harness v2, after the simulator audit)

Harness v2 compares more than v1, and each step is checked against Showdown:

- the legal choices;
- every PRNG call;
- **every** public event the env's reveal memory consumes, in order;
- the full state snapshot, including the hidden fields the mechanics read:
  - `statusState.source` (self-inflicted sleep), `subFainted`, and the bench's modified stats;
  - the trap partners, `moveThisTurn`, `trapped` / `maybeLocked`, and `maxpp`;
  - the volatile insertion order.

See docs/SIM_DESIGN.md.

| Run (seed) | format | patch | teams | battles | steps compared | diverged |
| --- | --- | --- | --- | ---: | ---: | ---: |
| v1 (1001) | gen1ou | vanilla | mixed, Gen 5 + sodium seeds | 5,000 | 380,937 | 0 |
| v2 (1002) | gen1ou | vanilla | targeted scenarios, Gen 5 + sodium seeds | 2,000 | 138,287 | 0 |
| v3 (1003) | gen1ou | vanilla | thaw | 500 | 7,519 | 0 |
| v4 (1004) | gen1ou | vanilla | random (any moves, DVs, levels) | 2,000 | 102,324 | 0 |
| v5 (1005) | gen1ou | vanilla | stall | 1,000 | 151,739 | 0 |
| v6 (1006) | gen1ou | vanilla | legal learnsets | 1,000 | 104,497 | 0 |
| v7 (1007) | gen1nc | vanilla | random, sodium seeds | 1,000 | 48,411 | 0 |
| v8 (1008) | gen1ou | pkmn | mixed | 1,000 | 73,321 | 0 |
| v9 (1009) | gen1nc | pkmn | targeted | 1,000 | 66,823 | 0 |
| **total** | | | | **14,500** | **1,073,858** | **0 (0.00%)** |

After the performance pass (state layout and hot-path rewrite), with fresh seeds:

| Run (seed) | format | patch | teams | battles | steps compared | diverged |
| --- | --- | --- | --- | ---: | ---: | ---: |
| p1 (11001) | gen1ou | vanilla | mixed, Gen 5 + sodium seeds | 10,000 | 764,759 | 0 |
| p2 (11002) | gen1ou | vanilla | targeted, Gen 5 + sodium seeds | 3,000 | 189,234 | 0 |
| p3 (11003) | gen1ou | vanilla | thaw | 1,000 | 15,559 | 0 |
| p4 (11004) | gen1ou | pkmn | mixed | 2,000 | 150,954 | 0 |
| p5 (11005) | gen1nc | pkmn | targeted | 1,000 | 68,607 | 0 |

The five mutation-testing corpora (5,799 battles) and the fixtures also replay without a
divergence, and every intermediate step of the pass was checked with 2,000 mixed + 1,000
targeted + 300 thaw battles on new seeds.

Coverage in those matched steps:

- Events: `-fail` 104k, `-miss` 89k, `-immune` 81k, `faint` 74k, `-crit` 59k, `cant slp` 25k,
  Transform 22k, `-curestatus` 16k, Bide 13k, `cant partiallytrapped` 11k, confusion 10k,
  Substitute 9.6k, `cant frz` 8.2k, recharge 6.2k, Mimic 5.6k, Disable 4.9k, freeze 1.1k.
- Endings: Endless Battle Clause 324 times, the 1000-turn limit 30 times.

The targeted scenarios hit Showdown's own "moveSlot is null" crash about once in 2,000 battles
(below). Those battles are excluded; none occurred in these runs.

**Divergences the v2 harness found, all fixed.** All three were invisible to v1:

3. Substitute's `-fail` was missing when the user already has a Substitute or too little HP.
   Category `events: showdown fail ours turn`: 51 of 500 battles. The native sim now emits the
   `Fail` event.
4. `setStatus` messages for status moves were missing:
   - `-immune` for a Poison type hit by Toxic or Poison Gas (33 of 500 battles);
   - `-fail` when the status is already set.
   These feed the env's `immune` / `failed` observation bits.
5. `trapped` / `maybeLocked` / `maybeDisabled` lived in the per-switch-in volatile state. Showdown
   keeps them on the Pokémon: `clearVolatile` does not reset them, so a fainted active keeps
   `trapped`. Category `state: .sides[].active.trapped`: 128 of 500. They moved to `Mon`. This had
   no effect on legal choices; the stale values are never read.

**Showdown crashes ("moveSlot is null").** Showdown 0.11.11 throws in `Side#commitChoices` when a
sleeping or frozen Pokémon must "Fight" while its `lastSelectedMoveSlot` points past its current
move slots. Example: a Mew picks Transform, its 4th move, against a one-move Jigglypuff, and is
then put to sleep. Showdown gives no ground truth here, and fuzz runs exclude these battles. They
are rare: 1 in about 2,000 targeted battles, 2 in the 40,998 v1 battles. The audit's repro hit the
state in 111 of 200 seeds. The native simulator used to assert and panic here. It now reads move
slot 0, as pkmn/engine does, and records that as `lastEnemySelectedMove`. Regression tests:
`crates/kanto-gen1/tests/robustness.rs`, `crates/kanto-backend/tests/native.rs`, and
`tests/python/test_env.py::test_move_slot_null_regression`.

## Protocol parity (`parity fuzz --proto`, 2026-10-14)

The simulator writes Showdown's text protocol when its sink records one (`kanto_core::ProtoSink`;
the env's `()` sink compiles it away, so throughput is unchanged: 4.89 M randbats turns/s). In
proto mode the runner captures `Battle#send`, and at every decision step the harness compares,
on top of the checks above:

- the step's `update` (the new `Battle#log` lines, `|split|` blocks and `|t:|` lines included,
  timestamps normalised);
- the omniscient, spectator, p1 and p2 views (`extractChannelMessages`) computed from it;
- both players' `|request|` JSON, byte for byte (key order, `noCancel`, `trapped`/`maybeLocked`,
  Fight/Recharge/locked-move entries, `wait`/`forceSwitch`).

A quarter of the battles use non-default player names (spaces, quotes, non-ASCII), which appear
in `|player|`, `|win|` and the request.

| Run (seed) | format | patch | teams | battles | steps compared | diverged |
| --- | --- | --- | --- | ---: | ---: | ---: |
| q1 (6001) | gen1ou | vanilla | mixed, Gen 5 + sodium seeds | 3,000 | 227,252 | 0 |
| q2 (6002) | gen1ou | vanilla | targeted, Gen 5 + sodium seeds | 2,000 | 134,470 | 0 |
| q3 (6003) | gen1ou | vanilla | thaw | 500 | 7,679 | 0 |
| q4 (6004) | gen1ou | vanilla | stall (Endless Battle Clause, turn limit, `bigerror`) | 500 | 81,349 | 0 |
| q5 (6005) | gen1ou | vanilla | random | 1,000 | 49,489 | 0 |
| q6 (6006) | gen1ou | vanilla | legal | 1,000 | 103,349 | 0 |
| q7 (6007) | gen1nc | vanilla | random, sodium seeds (custom-rules `|raw|` header) | 498 | 26,858 | 0 |
| q8 (6008) | gen1ou | pkmn | mixed | 500 | 38,431 | 0 |
| **total** | | | | **8,998** | **668,877** | **0** |

(q7: 2 of 500 battles hit Showdown's own "moveSlot is null" crash and are excluded.) The harness's
sensitivity was checked by corrupting a recorded log line, a view and a request: each is caught
with the right category. `parity/fixtures/corpus-proto.jsonl` (143 battles, 9,506 steps; per-step
hashes of the update and both requests) replays the protocol without Node in `cargo test`.

## Mutation testing (`parity/mutate.py`)

Mutation testing checks whether the harness catches simulator bugs. Each mutant is a one-place
source edit of kanto-gen1/kanto-core. The script rebuilds `parity` in a scratch copy and replays five
recorded Showdown corpora without Node:

- mixed: 1,000 battles;
- stall: 400;
- random gen1nc: 400;
- targeted: 1,999 (one battle hit Showdown's crash);
- thaw: 2,000.

It also runs the fixtures. The cells give the number of divergent battles per corpus.

- **Before** is the audit's run with harness v1. Its first 30 mutants were replayed on only the
  first three corpora (`-`).
- **After** is harness v2 with the targeted and thaw kinds.

| | before (audit, harness v1) | after (harness v2) |
| --- | --- | --- |
| the audit's mutants caught (47 re-run; `dmg-roll-minus1`, a near-duplicate of `dmg-roll-div256`, dropped) | 37 / 47 | **41 / 47** |
| all mutants (plus 5 new ones for hidden state and events, all caught) | – | **46 / 52** |
| survivors | 10 | 6, all argued equivalent (no Gen 1 state tells them apart) |

**Performance pass (2026-10-13).** Every mutant pattern was moved to the rewritten code (the two
`maxpp` mutants now target `Side::slot_maxpp`, which derives the no-longer-stored value), and 10
mutants of the new fast paths, packed sort keys and packed state were added. Result: **55 / 62**.
The original 52 give the same 46 / 52 with the same 6 equivalent survivors; 9 of the 10 new ones
are caught (BeforeMove/AfterMoveSelf/Residual/lock masks and fast paths, the eachEvent shortcut,
the two-item sort, the packed priority key, the bench burn bit). The survivor,
`stamp-renumber-reversed`, needs 255 volatile insertions within one switch-in, which no recorded
corpus reaches; the kanto-gen1 unit test `volatile_insertion_order_survives_stamp_renumbering`
(insertion order vs a model over 20,000 adds/deletes) fails on it.

**Re-run after the protocol-output change (2026-10-14).** The protocol work had moved the code
under three mutants (`counter-vs-counter-allowed`, `freeze-thaw-wrong`,
`status-immune-event-dropped`), whose patterns no longer matched, so `mutate.py` skipped them.
Their patterns now target the current code. A full run gives **55 / 62** again: all three are
caught with the counts below, and the 7 survivors are the 6 equivalent mutants listed under
"Survivors" plus `stamp-renumber-reversed` (covered by the unit test above).

What changed:

- Newly caught, survivors before:
  - volatile order: `volOrder`, taken from the sim's own iteration;
  - Mirror Move vs Mirror Move AfterMove: the new `mirror` scenario;
  - Endless Battle Clause with a frozen side: the new `freeze` scenario;
  - Mimic maxpp: `maxpp` is now in the snapshot.
- Much stronger:
  - `status_self`: 30 → 280 mixed, 489 thaw (the snapshot field);
  - `sub_fainted`: 3 → 76 mixed;
  - Fire thaw: 147 in the thaw corpus;
  - the 997 damage cap: 5 → 15, through the `overkill` scenario.
- Still rarely caught: Focus Energy crit /4 and confusion 1/256. They are inherently rare: a
  Focus Energy user's crit roll must land in one narrow band, and the confusion secondary roll
  must equal exactly 25 (1 in 256).
- Survivors, all equivalent:
  - `pt-source-hp-ignored`: removes a redundant check;
  - `pt-source-ignored`, `fpt-counterpart-ignored`: the trap pair is always the two actives;
  - `disable-bide-check`: during Bide the executed move is always Bide;
  - `move-this-turn-ignored`: no Gen 1 path gives a Pokémon two move actions in one turn;
  - `acc-no-1in256`: Gen 1 has no accuracy boosts or evasion drops, so the accuracy never exceeds
    255.

Full table, from `python3 parity/mutate.py --compare before.json`:

| mutant | before | after (divergent battles: mixed / stall / random_nc / targeted / thaw / fixtures) | note |
| --- | --- | --- | --- |
| dmg-roll-range | caught (999 / 360 / 396 / - / - / fx 108) | caught (994 / 368 / 399 / 1901 / 2000 / fx 108) |  |
| dmg-roll-div256 | caught (932 / 259 / 352 / - / - / fx 117) | caught (924 / 265 / 341 / 1409 / 1170 / fx 117) |  |
| dmg-997cap | caught (0 / 0 / 0 / 5 / 0 / fx 0) | caught (0 / 0 / 0 / 15 / 0 / fx 0) |  |
| crit-odd-basespe | caught (43 / 7 / 16 / - / - / fx 5) | caught (41 / 7 / 19 / 36 / 23 / fx 5) |  |
| crit-high-x8 | caught (61 / 2 / 19 / - / - / fx 2) | caught (60 / 3 / 17 / 7 / 0 / fx 2) |  |
| crit-focus-energy | caught (2 / 0 / 0 / 8 / 0 / fx 0) | caught (2 / 0 / 0 / 2 / 0 / fx 0) | Focus Energy crit /4 |
| crit-uses-modified | caught (210 / 98 / 34 / - / - / fx 22) | caught (215 / 93 / 36 / 93 / 4 / fx 22) |  |
| acc-no-1in256 | survived (0 / 0 / 0 / - / - / fx 0) | survived (0 / 0 / 0 / 0 / 0 / fx 0) | equivalent: Gen 1 has no accuracy boosts or evasion drops, so the computed accuracy never exceeds 255 |
| acc-skip-roll-at-255 | caught (991 / 391 / 393 / 3253 / 500 / fx 130) | caught (983 / 388 / 386 / 1721 / 1816 / fx 130) |  |
| recoil-round | caught (458 / 147 / 77 / - / - / fx 39) | caught (461 / 149 / 74 / 421 / 25 / fx 39) |  |
| psywave-hi-plus1 | caught (116 / 128 / 25 / - / - / fx 14) | caught (110 / 129 / 22 / 91 / 0 / fx 14) |  |
| counter-any-type | caught (74 / 35 / 13 / - / - / fx 5) | caught (64 / 28 / 12 / 137 / 0 / fx 5) |  |
| counter-vs-counter-allowed | caught (1 / 7 / 0 / 55 / 0 / fx 0) | caught (3 / 5 / 1 / 20 / 0 / fx 1) |  |
| confusion-selfhit-def | caught (15 / 18 / 4 / - / - / fx 2) | caught (10 / 22 / 8 / 2 / 0 / fx 2) |  |
| bide-into-sub-x1 | caught (0 / 0 / 0 / 10 / 0 / fx 0) | caught (2 / 0 / 0 / 3 / 0 / fx 0) |  |
| speedtie-reverse | caught (181 / 160 / 36 / - / - / fx 15) | caught (188 / 158 / 34 / 371 / 111 / fx 15) |  |
| speedtie-no-rng | caught (857 / 318 / 73 / - / - / fx 80) | caught (854 / 320 / 68 / 1398 / 1110 / fx 80) |  |
| vol-order-reverse | survived (0 / 0 / 0 / 0 / 0 / fx 0) | caught (337 / 299 / 93 / 749 / 85 / fx 55) | volatile order |
| sec-skip-roll-if-statused | caught (424 / 3 / 74 / - / - / fx 37) | caught (426 / 3 / 86 / 387 / 668 / fx 37) |  |
| sec-confusion-no-minus1 | caught (1 / 0 / 0 / 1 / 0 / fx 0) | caught (1 / 0 / 1 / 0 / 0 / fx 0) | confusion 1/256 |
| freeze-thaw-wrong | caught (0 / 0 / 0 / 0 / 130 / fx 0) | caught (1 / 0 / 1 / 0 / 147 / fx 0) | Fire thaw |
| status-self-ignored | caught (30 / 4 / 0 / 1 / 0 / fx 1) | caught (280 / 118 / 31 / 190 / 489 / fx 27) | status_self |
| slp-rest-3 | caught (332 / 99 / 25 / - / - / fx 27) | caught (280 / 116 / 31 / 180 / 489 / fx 27) |  |
| slp-range-1-7 | caught (537 / 105 / 114 / - / - / fx 26) | caught (519 / 123 / 129 / 368 / 498 / fx 26) |  |
| slp-wake-ge | caught (373 / 89 / 91 / - / - / fx 0) | caught (341 / 111 / 101 / 274 / 417 / fx 0) |  |
| toxic-leechseed-no-inc | caught (2 / 1 / 0 / 69 / 0 / fx 0) | caught (2 / 5 / 0 / 57 / 0 / fx 0) | Leech Seed Toxic counter |
| bench-par-speed-not-reapplied | caught (502 / 8 / 97 / - / - / fx 46) | caught (486 / 7 / 87 / 315 / 0 / fx 46) |  |
| stat-999-cap-1000 | caught (67 / 55 / 19 / - / - / fx 11) | caught (68 / 43 / 12 / 122 / 0 / fx 11) |  |
| rest-255-bug-removed | caught (3 / 1 / 0 / 3 / 2 / fx 1) | caught (1 / 1 / 0 / 1 / 4 / fx 1) |  |
| pp-no-wrap | caught (12 / 22 / 5 / - / - / fx 2) | caught (15 / 31 / 4 / 29 / 1 / fx 2) |  |
| pp-twoturn-both | caught (382 / 0 / 136 / - / - / fx 50) | caught (397 / 0 / 137 / 353 / 0 / fx 50) |  |
| pp-metronome-refund | caught (2 / 11 / 1 / 172 / 0 / fx 2) | caught (1 / 7 / 0 / 78 / 0 / fx 2) | Metronome/Mirror PP refund |
| pp-transform-5to4 | caught (80 / 122 / 36 / - / - / fx 12) | caught (89 / 126 / 31 / 216 / 0 / fx 12) |  |
| transform-mimic-pp-writeback | caught (63 / 101 / 22 / 462 / 0 / fx 6) | caught (63 / 103 / 28 / 200 / 0 / fx 6) |  |
| mimic-maxpp-ups0 | survived (0 / 0 / 0 / - / - / fx 0) | caught (73 / 119 / 24 / 236 / 0 / fx 16) | maxpp (hidden) |
| transform-maxpp-ups0 | - | caught (89 / 126 / 31 / 216 / 0 / fx 12) | maxpp (hidden), new |
| sub-hp-no-plus1 | caught (290 / 118 / 30 / - / - / fx 24) | caught (284 / 119 / 20 / 557 / 594 / fx 24) |  |
| sub-fainted-unset | caught (3 / 0 / 1 / 22 / 0 / fx 0) | caught (76 / 18 / 5 / 210 / 370 / fx 10) | sub_fainted |
| hyperbeam-recharge-on-ko | caught (201 / 2 / 11 / - / - / fx 18) | caught (212 / 4 / 14 / 339 / 0 / fx 18) |  |
| pt-source-ignored | survived (0 / 0 / 0 / 0 / 0 / fx 0) | survived (0 / 0 / 0 / 0 / 0 / fx 0) | pt_source (accuracy); equivalent: a trapped target's trapper is always the active foe within a turn (switching ends the trap) |
| pt-source-hp-ignored | survived (0 / 0 / 0 / 0 / 0 / fx 0) | survived (0 / 0 / 0 / 0 / 0 / fx 0) | pt_source (end of turn); equivalent: removes a redundant check: foe_has() already requires hp > 0 |
| fpt-counterpart-ignored | survived (0 / 0 / 0 / 0 / 0 / fx 0) | survived (0 / 0 / 0 / 0 / 0 / fx 0) | fpt_counterpart; equivalent: the fake-trap pair is always the two actives, and re-adding updates the counterpart |
| pt-maybelocked-unset | caught (88 / 172 / 35 / 827 / 211 / fx 20) | caught (88 / 170 / 28 / 383 / 354 / fx 20) |  |
| disable-bide-check | survived (0 / 0 / 0 / 0 / 0 / fx 0) | survived (0 / 0 / 0 / 0 / 0 / fx 0) | Disable/Bide; equivalent: while Bide is active the executed move is always lastSelectedMove = Bide |
| disable-picks-0pp-slots | caught (1 / 16 / 0 / 28 / 0 / fx 0) | caught (1 / 15 / 0 / 14 / 0 / fx 0) |  |
| move-this-turn-ignored | survived (0 / 0 / 0 / 0 / 0 / fx 0) | survived (0 / 0 / 0 / 0 / 0 / fx 0) | move_this_turn guard; equivalent: no Gen 1 path runs a second move action for the same Pokémon in a turn |
| mirror-mirror-aftermove | survived (0 / 0 / 0 / 0 / 0 / fx 0) | caught (0 / 0 / 0 / 8 / 0 / fx 0) | Mirror-Mirror AfterMove |
| endless-frz-ignored | survived (0 / 0 / 0 / 0 / 0 / fx 0) | caught (0 / 0 / 0 / 122 / 0 / fx 0) | Endless Clause freeze |
| trapped-cleared-on-faint | - | caught (306 / 101 / 48 / 192 / 0 / fx 37) | trapped (hidden), new |
| sub-fail-event-dropped | - | caught (95 / 69 / 8 / 320 / 358 / fx 0) | event, new |
| status-immune-event-dropped | - | caught (119 / 79 / 14 / 92 / 0 / fx 0) | event, new |
| disable-event-wrong-move | - | caught (65 / 106 / 19 / 250 / 0 / fx 0) | event, new |


## Robustness

The simulator audit found that the public API could panic. All of these are fixed and covered by
tests:

| Issue | Fix | Tests |
| --- | --- | --- |
| A1: "moveSlot is null" assert on a legal state (Showdown crash case) | read slot 0 (pkmn's choice) | Rust (kanto-gen1, kanto-backend) + Python regression |
| A2: illegal choices accepted silently or panicking: switch to self/fainted/missing, 0-PP or disabled moves, out-of-range indices | `apply_choices` / `Generation::update` return `IllegalChoice`, battle untouched; `*_unchecked` for the env hot path | `illegal_choices_are_rejected`, `is_legal_matches_choices` (every choice × many states) |
| A3: `from_bytes` checked only sizes and enums | `Battle::validate`: every id, count, range and cross-field invariant; env blobs also validate the reveal memories and reject games without legal choices | every reachable state round-trips; 40,000 random corruptions of mid-game battle blobs (~7,000 accepted) and 20,000 of env blobs step without a panic (944 panics without validation) |
| A4: `Battle::new` panicked on empty/7-Pokémon teams and bad ids | returns `TeamError` | `new_rejects_malformed_teams` |
| One panic poisoned the whole `VecEnv` | per-env `catch_unwind`: outcome 5 (`error`), `info["error"]`/`["errors"]`, env reset | `panics_are_isolated_per_env` (other envs bit-identical to an unpoisoned run), Python `test_panic_is_isolated_per_env` |

`restore_state` blobs for the `pkmn` backend are still trusted. The engine's state is an opaque
byte layout that we do not validate.

## Parity summary (harness v1)

These are the earlier runs. They compared legal choices, PRNG calls and state; the event stream
check covered only moves, switches and `cant` reasons, and only for e1-e3.

| Run (seed) | format | patch | teams | battles | steps compared | diverged |
| --- | --- | --- | --- | ---: | ---: | ---: |
| f1 (201) | gen1ou | vanilla | mixed, Gen 5 + sodium seeds | 5,000 | 382,254 | 0 |
| f2 (202) | gen1ou | vanilla | random (any moves, DVs, levels) | 5,000 | 248,883 | 0 |
| f3 (203) | gen1ou | vanilla | legal learnsets | 5,000 | 520,877 | 0 |
| f4 (204) | gen1nc | vanilla | mixed, sodium seeds | 4,998 | 382,653 | 0 |
| f5 (205) | gen1ou | vanilla | stall (Endless Battle Clause 81×, 1000-turn limit 59×) | 5,000 | 784,027 | 0 |
| f6 (206) | gen1ou | pkmn | mixed | 5,000 | 370,283 | 0 |
| f7 (207) | gen1nc | pkmn | random | 5,000 | 256,813 | 0 |
| e1-e3 (301-303) | both | vanilla | random / stall / mixed, + event-stream check | 6,000 | 578,819 | 0 |
| **total** | | | | **40,998** | **3,524,609** | **0 (0.00%)** |

Each step compares, against Showdown:

- the legal choices;
- every PRNG call (range and value);
- the full state snapshot, which includes the seed;
- for e1-e3, also the public event stream the env's reveal memory consumes.

The coverage data counts how often each feature occurred in those matched steps:

- Events: `-crit` 165k, `cant slp` 70k, `-start Bide` 41k, `-hitcount` 40k, Haze 30k,
  Substitute 25k, `cant partiallytrapped` 24k, Transform 16k, Leech Seed 15k, Hyper Beam recharge
  15k, Disable 13k, Mimic 12k, freeze 2.4k, `cant flinch` 1.6k.
- Endings: Endless Battle Clause and the 1000-turn limit.

**Showdown crashes (excluded):** 2 of 40,998 battles. These are the "moveSlot is null" crash
described above.

**Divergences found and fixed during development** (the fuzzer's categories led to each; v2's
are listed above):

1. Recoil. Showdown's gen4 mod (inherited by Gen 1) overrides `applyRecoilDamage` with `floor`,
   not `round`. Category `state: .sides[].team[].hp`: 22/40 battles before the fix.
2. Harness: pkmn's Sleep/Freeze Clause Mod priority patch must be applied to the dex *data*, since
   Showdown freezes Condition objects. Category `rng: showdown[speedSort] ours[-]`.

**Vanilla vs pkmn-patched Showdown:** run with Showdown patched and the native sim in vanilla mode
(`--patch pkmn --sim-compat vanilla`), 85.7% of 2,000 gen1ou battles diverge, after a mean of
13.7 decisions. Top causes are the Sleep vs Freeze Clause `SetStatus` tie (≈66%), speed-tie
`eachEvent('Update')` (≈13%), `Residual` (3%) and `insertChoice` (1%). Shrunk examples are in
`parity/fixtures/divergent/`.

**pkmn/engine vs Showdown 0.11.11 (patched):** `parity diff-pkmn` runs the two in lockstep, with
actions from the intersection of both legal sets.

- Divergent games: 1.32% of 5,000 randbats games and 16.4% of 5,000 random-legal-set games.
- The legal sets themselves differ often (54k times in 359k updates). Showdown 0.11.11 offers
  sleeping/frozen Pokémon a single "Fight" option; pkmn offers every move.
- So pkmn (pinned commit) is a drift detector, not an exact oracle for 0.11.11.

## Speed

Run `cargo run --release -p kanto-backend --features pkmn --example bench -- 20000 <teams> [seed] [compare|breakdown]`.
Each engine plays 20,000 games single-threaded on one DGX Spark core (aarch64, `taskset -c 5`,
machine load 1-3 from other users; medians of 5 runs). Both engines use the same seeds and the
same uniform-random action policy. No logging, observations or masks are produced. "Before" is
commit 71bc099 (before the performance pass), measured with the same driver.

| | before | after | pkmn/engine |
| --- | ---: | ---: | ---: |
| randbats turns/s (vanilla) | 2.62 M (0.40×) | 4.88 M (0.76×) | 6.45 M |
| gen1ou pool turns/s (vanilla / pkmn-patched) | 2.57 / 2.66 M | 4.99 / 5.05 M (0.75×) | 6.67 M |
| update only (ns/update, recorded choices) | 259 | 107-112 | 69 |
| `choices()` for both players (ns/update) | 24 | 25 | 16 |
| state size (bytes) | 1,400 | 760 | 384 |
| state copy, in cache (ns) | 32 | 18 | 12 |
| `to_bytes` / `from_bytes` + validate (ns) | 607 / 1,106 | 455 / 693 | (memcpy, unvalidated) |
| `VecEnv.step`, N=4096, 16 threads (env-steps/s) | 2.31 M | 2.53 M | 2.49-2.53 M |

The turns/s figures include the benchmark's own policy (listing choices, mapping them to env
actions, sampling), about 50 ns per update for either engine. At the env level the simulator is
no longer the bottleneck: one `VecEnv.step` costs about 6 µs per env per thread, mostly the
observation encoding, so native and pkmn/engine are within noise of each other.

What helped (cumulative, randbats turns/s): packed `u64` sort keys, 8-byte queue actions, masked
volatile iteration and the two-item `speedSort` (2.61 → 3.40 M); cheaper requests and no `Active`
copies (→ 3.60 M); returning early from events with no handlers, borrowing `MoveData` and a move
flag table (→ 4.45 M); clamped/masked side and active-slot indexing, which removes bounds checks
and their panic paths (→ 4.70 M); smaller fixes to the request list, the Endless Battle Clause
check and the queue (→ 4.88 M). Shrinking the state (1,400 → 760 bytes) roughly halved the copy
cost but did not change turns/s measurably. Build settings did not help: fat LTO, `panic = "abort"`
(not usable anyway: `VecEnv` relies on `catch_unwind`) and `-C target-cpu=native` were all within
noise (±1-2%); `codegen-units = 1` was already set.

`... bench -- N teams seed native` plays only the native games, for
`valgrind --tool=callgrind` (perf is not available on this machine). `... bench -- N teams seed h2h [rounds]`
runs the interleaved comparison below.

### Second performance pass (2026-10-18)

Same driver, new `h2h` mode: each engine's choice sequences are recorded once, then 11 rounds
alternate between replaying all of pkmn/engine's games and all of kanto's (update only), and between
full self-play runs (update + `choices()` + the uniform policy). Both engines therefore see the same
machine load in every round. The machine was shared with GPU jobs and ~16 busy Python processes
(load ~14); runs were pinned with `taskset` to Cortex-X925 cores 7, 15 and 18 and, for reference,
to an idle Cortex-A725 core (12). `before` = integrate 974b7ce. Medians of 11 rounds, [min-max]:

| X925, 10,000 games | before | after | after + PGO | pkmn/engine |
| --- | ---: | ---: | ---: | ---: |
| randbats update (ns) | 107.0-108.0 [106.1-111.3] | 94.6-95.6 [94.2-97.3] | 90.8-91.5 [90.5-92.8] | 70.9-72.6 [70.0-75.7] |
| randbats kanto/pkmn update time | 1.48-1.51 | 1.33-1.34 | 1.27-1.28 | 1 |
| randbats self-play (M turns/s) | 4.89-4.93 | 5.35-5.38 | 5.55-5.58 | 6.33-6.52 |
| randbats kanto/pkmn turns/s | 0.77 | 0.84 | 0.85-0.86 | 1 |
| gen1ou pool update (ns) | 111.0-112.0 | 100.4-100.7 | 95.4-96.1 | 73.7-75.3 |
| gen1ou pool kanto/pkmn update time | 1.49-1.51 | 1.34-1.35 | 1.27-1.29 | 1 |
| gen1ou pool kanto/pkmn turns/s | 0.76 | 0.81-0.82 | 0.84 | 1 |

On the A725 core the ratios are 1.49 -> 1.28 -> 1.23 (randbats update) and 0.79 -> 0.89 -> 0.86
(turns/s; the PGO profile was trained on X925-like code layout). Within one binary the round-to-round
spread is about +-1.5% on the X925 cores and +-0.2% on the idle A725 core; between rebuilds of the
same source, code layout moves the X925 numbers by up to +-2%. `choices()` for both players went
from 25 to 22 ns per update (pkmn: 17).

kanto is still 1.27-1.33x slower per update than pkmn/engine. What was measured and kept (every
step checked with `cargo test`, the fixtures and 11,000 recorded Showdown battles replayed
Node-free; final state: a fresh `parity fuzz` of 10,800 battles / 819,833 steps, protocol runs
included, 0 divergences):

- Endless Battle Clause check: an active Pokémon that is not fainted or frozen and has a non-Transform
  move and a move with PP makes the battle not stuck, so the full scan of both teams is skipped
  (-6% update time).
- Handler gates (`BeforeMove`, `AfterMove`, `AfterMoveSelf`, `DisableMove`, `Residual`) inlined, bodies
  outlined; the volatile insertion order uses an insertion sort instead of `sort_by_key` (-4%).
- `action_tail` (end of `runAction`) has a fast path when nothing fainted and no switch flag is set;
  BeforeTurn is put in front of the queue without the `insertChoice` scan when the first queued action
  sorts after it (-2%).
- Queue actions are packed `u64`s whose high bits are the sort key (-1 to -3%).
- The sodium (ChaCha20) PRNG step is out of line and cold, so the Gen 5 LCG is all that is inlined
  into every PRNG call site (-3% on X925; a Gen 5-only PRNG type would gain another ~1%, not worth
  making `Battle` generic).
- Fast paths for unlocked actives in `legal_choices` and `chooseMove`; branchless reset of the
  `disabled` flags at the end of the turn; the outgoing Pokémon's `clearVolatile` on a switch only
  does its Pokémon-side part (the active state is rebuilt from the incoming Pokémon right after).
- `modifyStat` and the accuracy stage table use integer arithmetic. Unit tests compare them with
  Showdown's floating-point expressions for every `u16` stat and every reachable accuracy value.
  (No measurable speed change: the float code was not hot.)
- PGO (`tools/pgo.sh`, needs `rustup component add llvm-tools`): -4%. It changes code layout and
  inlining only; the parity suite runs unchanged on a PGO build. On the code before this pass PGO
  was within noise.

Measured and dropped: compile-time compat/clause modes (const generics) — hardcoding Vanilla
everywhere changed nothing measurable (<0.5%: the checks are predictable branches); `-C
target-cpu=native`, fat LTO, 64-byte function/block alignment — within +-1.5%; caching legal choices
at request time — `choices()` is 11 ns per player and the cache would grow the `Copy` state that
search clones; a hot/cold split of the state — the 760-byte `Battle` already fits in L1. The fast
path for a move-vs-move turn that skips the generic action queue was not built: after the changes
above the queue operations (sort, insert, shift) are about 7% of update time, and only about half
of the updates in random play are move-vs-move turns, so it could save at most ~4% while
re-implementing the speed-tie and `insertChoice` PRNG order.

Profiles (a `SIGPROF` sampler; `perf` is not permitted on this machine) are flat after the pass: no
function above 12%, the largest being move execution (`useMove` and below, ~30% together), the
switch path (`switchIn`, `runSwitch`, building the active state, ~15%), end of turn (~7%) and
choice commit (~8%). The rest of the gap is the Showdown-shaped control flow kanto keeps on
purpose (a real action queue, `runAction` tails and `eachEvent` speed-tie checks after every action)
rather than any single hot spot.

## Mechanics

| Area | Implemented | Verified vs Showdown | Known divergent |
| --- | --- | --- | --- |
| Battle loop, action queue (`sort`/`insertChoice` incl. tie RNG), `beforeTurn`/`residual`, `eachEvent` speed ties | yes | yes | – |
| Move order: priority (Quick Attack +1, Counter −1), speed, ties | yes | yes | – |
| Damage formula, crits (base-Speed, Focus Energy bug, high-crit moves), 217-255 rolls, 1024 rollover, Reflect/Light Screen, Explosion defense halving | yes | yes | – |
| Accuracy (0-255, 1/256 miss, stages, Thrash/Rage accuracy bug, sleep-vs-recharge bypass, self-target +1) | yes | yes | – |
| PP (Gen 1 slot-based deduction, 6-bit underflow, two-turn/locked rules, Metronome/Mirror Move refunds, Transform PP) | yes | yes | – |
| Switching, forced switches after faints, Gen 1 "faint ends the turn" | yes | yes | – |
| Sleep (1-7, Rest = 2, wake turn), freeze (thaw by fire), paralysis (63/256, speed drop), burn (attack drop), poison, Toxic counter (incl. Leech Seed / Rest carry-over) | yes | yes | – |
| Stat stages, 999 cap / 1 floor rules, paralysis/burn re-application bug, Haze | yes | yes | – |
| Volatiles: confusion, flinch, partial trapping (lock, fake trap, maybeLocked), recharge, Thrash/Petal Dance, two-turn moves, Dig/Fly invulnerability, Bide, Disable, Focus Energy, Mist, Light Screen, Reflect, Leech Seed, Substitute (Gen 1 rules), Rage, Toxic counter, Minimize, Defense Curl | yes | yes | – |
| Counter (Desync Clause Mod semantics), Mimic, Mirror Move, Metronome, Transform, Conversion, Psywave, Super Fang, fixed-damage moves, OHKO, multi-hit, drain, recoil, crash damage | yes | yes | – |
| Gen 1 move-selection memory (`lastSelectedMove`, "Fight", `cannotmove`, `nomove` thaw bug) | yes | yes | – |
| Requests/legal choices (locked moves, Recharge, Fight, Struggle, maybeLocked, trapped) | yes | yes | – |
| Sleep Clause Mod, Freeze Clause Mod (and their tie RNG in vanilla) | yes | yes | – |
| Endless Battle Clause (Gen 1 no-progress tie), 1000-turn limit | yes | yes | – |
| PRNG: Gen 5 LCG and sodium (ChaCha20) seeds | yes | yes | – |
| `Compat::Pkmn` (pkmn's patched Showdown) | yes | yes (f6, f7) | – |
| Protocol text log (`Battle#log` with `|split|`, per-player views, `|request|` JSON) | yes (`ProtocolBattle`, `Sink::log`) | yes: every line, view and request (`parity fuzz --proto`) | – |
| Team validation, team preview, Gen 1 Stadium, other gens | no (the env validates teams: Species Clause, learnsets) | – | – |

## Moves

Every Gen 1 move is implemented. "Verified uses" counts how often the move was used (`|move|`
lines, including called moves) in decision steps that matched Showdown exactly, summed over the
runs above. No move is known to diverge.

| # | Move | Verified uses |
| ---: | --- | ---: |
| 1 | Pound | 6898 |
| 2 | Karate Chop | 5044 |
| 3 | Double Slap | 6012 |
| 4 | Comet Punch | 4839 |
| 5 | Mega Punch | 12072 |
| 6 | Pay Day | 8115 |
| 7 | Fire Punch | 4756 |
| 8 | Ice Punch | 5027 |
| 9 | Thunder Punch | 4806 |
| 10 | Scratch | 8232 |
| 11 | Vise Grip | 5545 |
| 12 | Guillotine | 4803 |
| 13 | Razor Wind | 14009 |
| 14 | Swords Dance | 17350 |
| 15 | Cut | 10745 |
| 16 | Gust | 5603 |
| 17 | Wing Attack | 6133 |
| 18 | Whirlwind | 38953 |
| 19 | Fly | 15089 |
| 20 | Bind | 57851 |
| 21 | Slam | 6535 |
| 22 | Vine Whip | 5888 |
| 23 | Stomp | 6724 |
| 24 | Double Kick | 6245 |
| 25 | Mega Kick | 11016 |
| 26 | Jump Kick | 5044 |
| 27 | Rolling Kick | 5091 |
| 28 | Sand Attack | 9025 |
| 29 | Headbutt | 6135 |
| 30 | Horn Attack | 6312 |
| 31 | Fury Attack | 7642 |
| 32 | Horn Drill | 7253 |
| 33 | Tackle | 16603 |
| 34 | Body Slam | 60018 |
| 35 | Wrap | 62473 |
| 36 | Take Down | 27879 |
| 37 | Thrash | 9084 |
| 38 | Double-Edge | 34746 |
| 39 | Tail Whip | 10708 |
| 40 | Poison Sting | 10393 |
| 41 | Twineedle | 5187 |
| 42 | Pin Missile | 5428 |
| 43 | Leer | 48031 |
| 44 | Bite | 8725 |
| 45 | Growl | 48591 |
| 46 | Roar | 36203 |
| 47 | Sing | 8581 |
| 48 | Supersonic | 9190 |
| 49 | Sonic Boom | 32488 |
| 50 | Disable | 37343 |
| 51 | Acid | 7431 |
| 52 | Ember | 6691 |
| 53 | Flamethrower | 5947 |
| 54 | Mist | 40331 |
| 55 | Water Gun | 14832 |
| 56 | Hydro Pump | 7468 |
| 57 | Surf | 23203 |
| 58 | Ice Beam | 14549 |
| 59 | Blizzard | 35674 |
| 60 | Psybeam | 5651 |
| 61 | Bubble Beam | 14907 |
| 62 | Aurora Beam | 5468 |
| 63 | Hyper Beam | 30549 |
| 64 | Peck | 7095 |
| 65 | Drill Peck | 7544 |
| 66 | Submission | 15465 |
| 67 | Low Kick | 5901 |
| 68 | Counter | 43243 |
| 69 | Seismic Toss | 38867 |
| 70 | Strength | 13547 |
| 71 | Absorb | 6925 |
| 72 | Mega Drain | 18739 |
| 73 | Leech Seed | 28476 |
| 74 | Growth | 7746 |
| 75 | Razor Leaf | 9623 |
| 76 | Solar Beam | 15353 |
| 77 | Poison Powder | 8673 |
| 78 | Stun Spore | 12121 |
| 79 | Sleep Powder | 19854 |
| 80 | Petal Dance | 9551 |
| 81 | String Shot | 11294 |
| 82 | Dragon Rage | 29041 |
| 83 | Fire Spin | 50299 |
| 84 | Thunder Shock | 6831 |
| 85 | Thunderbolt | 32195 |
| 86 | Thunder Wave | 23681 |
| 87 | Thunder | 14413 |
| 88 | Rock Throw | 5497 |
| 89 | Earthquake | 20813 |
| 90 | Fissure | 9017 |
| 91 | Dig | 20783 |
| 92 | Toxic | 51698 |
| 93 | Confusion | 6489 |
| 94 | Psychic | 23506 |
| 95 | Hypnosis | 15438 |
| 96 | Meditate | 6520 |
| 97 | Agility | 22226 |
| 98 | Quick Attack | 10322 |
| 99 | Rage | 407540 |
| 100 | Teleport | 40298 |
| 101 | Night Shade | 25814 |
| 102 | Mimic | 16970 |
| 103 | Screech | 8850 |
| 104 | Double Team | 70259 |
| 105 | Recover | 58026 |
| 106 | Harden | 47706 |
| 107 | Minimize | 43873 |
| 108 | Smokescreen | 8033 |
| 109 | Confuse Ray | 34460 |
| 110 | Withdraw | 7659 |
| 111 | Defense Curl | 7804 |
| 112 | Barrier | 7364 |
| 113 | Light Screen | 40330 |
| 114 | Haze | 40345 |
| 115 | Reflect | 55081 |
| 116 | Focus Energy | 9135 |
| 117 | Bide | 47753 |
| 118 | Metronome | 23512 |
| 119 | Mirror Move | 33427 |
| 120 | Self-Destruct | 3034 |
| 121 | Egg Bomb | 5241 |
| 122 | Lick | 6806 |
| 123 | Smog | 5847 |
| 124 | Sludge | 6762 |
| 125 | Bone Club | 5065 |
| 126 | Fire Blast | 18773 |
| 127 | Waterfall | 4769 |
| 128 | Clamp | 38745 |
| 129 | Swift | 16399 |
| 130 | Skull Bash | 36415 |
| 131 | Spike Cannon | 5444 |
| 132 | Constrict | 6095 |
| 133 | Amnesia | 41509 |
| 134 | Kinesis | 6128 |
| 135 | Soft-Boiled | 39756 |
| 136 | High Jump Kick | 5388 |
| 137 | Glare | 7357 |
| 138 | Dream Eater | 6722 |
| 139 | Poison Gas | 6116 |
| 140 | Barrage | 4759 |
| 141 | Leech Life | 6392 |
| 142 | Lovely Kiss | 6784 |
| 143 | Sky Attack | 13197 |
| 144 | Transform | 20394 |
| 145 | Bubble | 7129 |
| 146 | Dizzy Punch | 4786 |
| 147 | Spore | 7984 |
| 148 | Flash | 11485 |
| 149 | Psywave | 32487 |
| 150 | Splash | 40622 |
| 151 | Acid Armor | 7090 |
| 152 | Crabhammer | 5316 |
| 153 | Explosion | 6145 |
| 154 | Fury Swipes | 6900 |
| 155 | Bonemerang | 4655 |
| 156 | Rest | 75168 |
| 157 | Rock Slide | 13731 |
| 158 | Hyper Fang | 5115 |
| 159 | Sharpen | 5183 |
| 160 | Conversion | 35874 |
| 161 | Tri Attack | 7644 |
| 162 | Super Fang | 26430 |
| 163 | Slash | 12880 |
| 164 | Substitute | 51927 |
| 165 | Struggle | 155251 |
