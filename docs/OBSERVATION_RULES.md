# Observation rules: what a Gen 1 singles player can see on Pokémon Showdown

This spec defines what each player may observe in `[Gen 1] OU`, so the encoder (`kanto-env::obs`,
which owns the vector layout in `docs/OBSERVATION.md`) can build per-player observations that
match what a Showdown player sees. It also proposes a feature list and a reveal-tracking design
driven by the pkmn engine's `-Dlog` protocol.

**Sources.** Line numbers refer to `pokemon-showdown` 0.11.11 (npm) and the pkmn engine checkout
used in this project.

- [PS-PROTO] `sim/SIM-PROTOCOL.md`
- [PS-POKE] `sim/pokemon.ts` (`getHealth` at L2065-2100, `transformInto` at L1275-1330)
- [PS-BATTLE] `sim/battle.ts` (`teamsize` at L2670, `calculatePP` at L2371)
- [PS-RULES] `data/rulesets.ts` (`hppercentagemod` at L1352)
- [PS-FMT] `config/formats.ts` L4353: `[Gen 1] OU` = `Standard` + ban `Uber`
- [G1-COND] `data/mods/gen1/conditions.ts`
- [G1-MOVES] `data/mods/gen1/moves.ts`
- [G1-SCRIPTS] `data/mods/gen1/scripts.ts`
- [G2-COND] `data/mods/gen2/conditions.ts`, inherited by gen1
- [PKMN-PROTO] engine `docs/PROTOCOL.md`
- [PKMN-G1] engine `src/lib/gen1/README.md`
- [PKMN-TS] engine `src/pkg/protocol.ts`

The resolved gen1ou rule table is: `standard obtainable ... hppercentagemod cancelmod
endlessbattleclause sleepclausemod freezeclausemod speciesclause nicknameclause ohkoclause
evasionmovesclause -move:dig -move:fly -tag:uber` (from `Dex.formats.getRuleTable`).

---

## 1. Rules

### 1.1 Battle start: no Team Preview

- Team Preview is **not** in the gen1ou rule table. Only later-gen rulesets add `'Team Preview'`
  ([PS-RULES] L16, L41, L162), so there are no `|poke|` / `|teampreview|` messages.
- `|teamsize|p1|N` and `|teamsize|p2|N` are broadcast to both players at `start` ([PS-BATTLE]
  L2670). The docs say this is so that "you don't know which Pokémon your opponent has, but you at
  least know how many there are" ([PS-PROTO] L52). **The opponent's team size is public.** All
  pool teams have 6 Pokémon, but the encoder should still encode it.
- The `|rule|` lines are public. They include `HP Percentage Mod`, Sleep/Freeze Clause Mod, and
  others. The Switch Priority Clause Mod is not broadcast in Gen 1 ([PKMN-G1] "Bugs").
- Both leads are revealed by `|switch|` at turn 0, before either player makes a choice.

### 1.2 Revealing Pokémon (species, level)

- An opponent Pokémon is revealed only when it enters the field: `|switch|POKEMON|DETAILS|HP STATUS`
  or `|drag|`. In Gen 1, Roar and Whirlwind never force a switch ([G1-MOVES] `roar: forceSwitch:
  false`), so in practice only `|switch|` reveals.
- `DETAILS` gives the species, plus `, L##` if the level is not 100 ([PS-PROTO] "Identifying
  Pokémon"). Gen 1 has no gender or shininess. **A switch-in therefore reveals species, level,
  HP (as a percentage) and major status together.**
- Never revealed: DVs, stat exp, exact stats, exact HP, the moveset (until moves are used), and
  slot order in the opponent's team.
- Nicknames are irrelevant (the engine has none, and Showdown has a Nickname Clause).
- Species Clause makes `POKEMON|DETAILS` unique, so a switch-in always identifies which already
  seen Pokémon returned ([PS-PROTO] L260).

### 1.3 HP

- **Own side: exact `CURRENT/MAX`.** Showdown sends the `secret` half of a split message to the
  owner ([PS-POKE] `getHealth`: `secret = hp/maxhp`). The `|request|` JSON also carries it.
- **Opponent: an integer percentage out of 100.** Under `HP Percentage Mod` (`reportPercentages =
  true`, [PS-RULES] L1352), [PS-POKE] L2080-2086 computes:

  ```
  if hp == 0:                 "0 fnt"
  pct = ceil(100 * hp / maxhp)
  if pct == 100 and hp < maxhp: pct = 99
  shared = f"{pct}/100"
  ```

  So any damage at all shows as at most 99, and any HP above 0 shows as at least 1. The encoder
  must use exactly this function. **Never pass the engine's exact opponent HP or max HP to the
  encoder.** Max HP alone would leak DVs and stat exp.
- The opponent's max HP is never shown. Observed percentage changes give damage only as a
  percentage.

### 1.4 Moves: when they are revealed, and PP

- `|move|SOURCE|MOVE|TARGET` reveals that SOURCE used MOVE. It may be followed by `|[miss]`,
  `|[still]` or `|[from] EFFECT` ([PS-PROTO] L244).
- **Slot reveal rule.** A `|move|` line with **no `[from]`** reveals a move in the user's current
  move slots. Lines with `[from]` do not reveal a slot:
  - `[from] Metronome` or `[from] Mirror Move` marks a *called* move ([G1-SCRIPTS] L329: `if
    (sourceEffect) attrs += '|[from] ' + name`). Only Metronome or Mirror Move, which appear as
    their own preceding `|move|` line, is revealed as a slot.
  - `[from] <same move>` marks a continuation of a locked move: Thrash or Petal Dance turns 2-3,
    Wrap/Bind/Clamp/Fire Spin continuation turns, Rage turns after the first, the second turn of a
    two-turn move, and Bide release. In [G1-SCRIPTS] L202-203, `lockedMove` sets `sourceEffect =
    move`. The first use, which has no `[from]`, already revealed the move.
  - The pkmn log encodes this as `|move|` `Reason = 0x01` with a trailing `[from]` move byte
    ([PKMN-PROTO] `|move|`).
- **Other move reveals:**
  - `|cant|X|Disable|MOVE` reveals MOVE in X's set.
  - `|-start|X|Disable|MOVE` reveals that the target X knows MOVE. This holds when **you Disable
    the opponent**: Disable picks a random slot with PP > 0 ([G1-MOVES] L250-262).
  - `|-start|USER|Mimic|MOVE`: Mimic copies a **random move of the target's moveset** ([G1-MOVES]
    L501-517). If USER is your Pokémon, MOVE is revealed as one of the opponent's moves. USER's
    Mimic slot temporarily becomes MOVE (until it switches out). Later plain `|move|USER|MOVE` lines
    must **not** add MOVE to the user's permanent revealed moveset.
  - `|-transform|USER|TARGET`: USER copies TARGET's species, types, stats except HP, boosts and
    **all four moves**, each at 5 PP ([PS-POKE] L1321). If USER is your Pokémon, your own
    `|request|` (and the engine state) now lists the opponent's full moveset and its non-HP stats.
    A human would remember these, so mark all four opponent moves revealed. The exact stats imply
    DVs and stat exp, but the encoder may ignore that. If the opponent transforms, suppress move
    reveals for that Pokémon until it switches out, because its moves came from your Pokémon.
- **PP.** Own PP is exact. Opponent PP is never shown. Track it as `uses` = the number of `|move|`
  lines with *no* `[from]` for that move by that Pokémon. That is one deduction per selection,
  which matches Showdown: PP is deducted after `BeforeMove` succeeds, so `cant` turns cost nothing;
  locked or continuation turns cost nothing; two-turn moves deduct once ([G1-SCRIPTS] L202-210).
  Metronome and Mirror Move deduct from their own slot, so a called move uses no PP of its own.
- **Max PP assumption.** Showdown applies 3 PP Ups by default: `maxpp = pp * 8/5`, and for the
  40-PP moves in Gen ≤ 2, `64 - 3 = 61` ([PS-BATTLE] L2371). Estimate the opponent's remaining PP
  as `maxpp - uses`.
- **Struggle** appears as `|-activate|X|move: Struggle` (pkmn) or `|move|X|Struggle`. It means all
  of X's moves have no PP left (or are disabled). It is not a slot reveal.

### 1.5 Major status

All major statuses are public. `|-status|X|S` (with `[from] move: Rest` for self-inflicted sleep)
and `|-curestatus|X|S|[msg]` are sent to both sides, and the status also appears in the `HP STATUS`
of every `|switch|`, `|-damage|` and `|-heal|`.

| Status | Visible | Hidden |
| --- | --- | --- |
| `slp` | start (normal, or `[from] move: Rest`), each `cant\|slp`, waking via `-curestatus\|slp\|[msg]` | **Turns remaining.** Normal sleep lasts 1-7 turns (`random(1,8)`, [G1-COND] L62). The wake turn is lost with no message before `-curestatus`. Rest sleep is fixed at 2 ([G1-MOVES] L691), which means one `cant slp`, then a silent wake turn. **Turns slept so far** can be observed (count `cant slp`) and should be encoded. |
| `frz` | start, `cant\|frz` each turn | No natural thaw in Gen 1. Cured only by a Fire move with a burn chance hitting it, or by Haze ([G1-COND] L85-103). |
| `par` | start, `cant\|par` (63/256 full paralysis) | Speed quartered and stored in modified stats; not shown. |
| `brn` | start, end-of-turn `-damage ... brn` | Attack halved; not shown. |
| `psn` | start, residual `-damage` | |
| `tox` | `-status tox`; on switch-in it **silently becomes `psn`** (`-status X psn [silent]`, [G2-COND] L107-110). Haze also turns the user's `tox` into `psn`. | The Toxic counter is not shown, but it can be inferred from residual damage. In Gen 1 the counter also multiplies Leech Seed damage, and survives Rest for PSN/BRN damage ([G1-COND] L20-23). |
| fainted | `\|faint\|X`, and `0 fnt` in HP STATUS | |

Sleep Clause Mod and Freeze Clause Mod are public rules. Whether the opponent already has a
Pokémon asleep or frozen *from your moves* can be derived from history. Rest sleep does not count
for Sleep Clause.

### 1.6 Volatiles (Gen 1)

Volatiles are cleared on switch-out and on faint, with no message. **The encoder must clear the
opponent's (and own) active volatiles on every `|switch|` or `|faint|` for that side.** In Gen 1,
Reflect and Light Screen are user volatiles, sent as `-start` messages, not `-sidestart` side
conditions.

| Volatile | Start / ongoing / end messages | Visible to both? | Hidden part |
| --- | --- | --- | --- |
| Confusion | `-start\|X\|confusion`; `-activate\|X\|confusion` each turn; self-hit `-damage ... [from] confusion`; `-end\|X\|confusion` | yes | Duration 2-5 (`random(2,6)`, [G1-COND] L139). After Thrash or Petal Dance it starts with `[silent]`, but the message still goes to **both** sides, so treat it as visible ([PKMN-G1] calls this a Showdown leak). |
| Leech Seed | `-start\|X\|move: Leech Seed`; `-damage ... [from] Leech Seed` and the seeder's `-heal` | yes | |
| Substitute | `-start\|X\|Substitute` (HP cost visible as `-damage`); `-activate\|X\|Substitute\|[damage]` when hit; `-end\|X\|Substitute` when broken; `-fail\|X\|move: Substitute[\|[weak]]` | yes | **Sub HP is shown to nobody,** not even its owner: no message carries it. Showdown sets it to `floor(maxhp/4)+1` ([G1-MOVES] L827). Encode Substitute as a boolean for both sides. |
| Reflect / Light Screen | `-start\|X\|Reflect` / `-start\|X\|Light Screen`; end only on switch, or on Haze (`-end ... [silent]`) | yes | |
| Mist / Focus Energy | `-start\|X\|Mist`, `-start\|X\|move: Focus Energy` | yes | |
| Partial trapping (Wrap, Bind, Clamp, Fire Spin) | `\|move\|A\|Wrap\|B` (first turn, no `[from]`), then `[from]` continuations; the target gets `cant\|B\|partiallytrapped` | yes | Duration 2-5 (`[2,2,2,3,3,3,4,5]`, [G1-COND] `partialtrappinglock`). Showdown deliberately hides the end with `fakepartiallytrapped` and `maybeLocked` ([G1-COND] L232-275). |
| Recharge | `-mustrecharge\|X` after Hyper Beam; next turn `cant\|X\|recharge` | yes | There is no recharge if Hyper Beam KOs ([G1-COND] L280-284), and none if it breaks a Substitute (the cartridge nullifies the rest of the move; [PKMN-G1]). A missing `-mustrecharge` is therefore informative. A Wrap-type hit on a recharging Pokémon also cancels its recharge ([G1-MOVES] L76, L123, L323). |
| Rage | No `-start` message. Shown by `\|move\|X\|Rage` followed by `[from]` continuations, and `-boost\|X\|atk\|1\|[from] Rage` when hit (pkmn boost reason `0x00`). | yes, inferred | Locked until switch or faint. |
| Bide | `-start\|X\|Bide`; `-activate\|X\|Bide` while storing; `-end\|X\|Bide`, then damage; or `-fail` ([G1-MOVES] L40-59) | yes | Stored damage (it can be inferred). |
| Thrash / Petal Dance lock | first `\|move\|`, then `[from]` continuations, then confusion `[silent]` | yes | Length 2-3. |
| Two-turn charge (Solar Beam, Skull Bash, Sky Attack, Razor Wind) | `-prepare\|X\|MOVE` + `\|move\|...\|[still]` | yes | |
| Disable | `-start\|X\|Disable\|MOVE` (**reveals the move**); `cant\|X\|Disable\|MOVE`; `-end\|X\|Disable` | yes | Duration 1-8 (`random(1,9)`, [G1-MOVES] L265). |
| Mimic | `-start\|X\|Mimic\|MOVE` | yes | |
| Transform | `-transform\|X\|TARGET` | yes | The transformed side learns the target's moves and stats (see 1.4). |
| Conversion | `-start\|X\|typechange\|T1/T2\|[from] move: Conversion\|[of] TARGET` | yes | X's types become TARGET's types. |
| Flinch | `cant\|X\|flinch` | yes, after the fact | |
| Haze | `-activate\|X\|move: Haze`, `-clearallboost`, then the major status of the non-user is cured (`-curestatus`, possibly `[silent]`), the user's `tox` becomes `psn`, and silent `-end`s for Disable, confusion, Mist, Focus Energy, Leech Seed, Reflect, Light Screen and the Toxic counter, on both sides ([G1-MOVES] L369-415) | yes | |

### 1.7 Stat stages

- The messages are `|-boost|X|STAT|N` and `|-unboost|X|STAT|N`, with `[from] EFFECT` for
  non-move sources such as Rage. Haze sends `|-clearallboost|`. All are public. Stages range from
  -6 to +6 for `atk def spa spd spe accuracy evasion`.
- **Special:** Gen 1 has one Special stat, but Showdown models it as `spa` and `spd` and emits
  **two lines**. For example, Amnesia sends `-boost spa 2` and `-boost spd 2`, and the Psychic
  secondary effect sends `-unboost spa 1` and `-unboost spd 1` ([G1-MOVES] amnesia/psychic). The
  pkmn log has separate `spa`/`spd` reasons. Encode a single `spc` stage taken from `spa`.
- **999/1 quirk:** boosting a stat already at 999 prints `-boost X STAT n` followed by `-unboost X
  STAT 1`. Dropping a stat already at 1 prints the mirror image. The net stage changes, the stat
  value does not, and the par/brn reapplication does not run ([G1-SCRIPTS] L1007-1022). Apply the
  stage deltas exactly as logged, and the tracked stage stays consistent with Showdown.
- **Stats are never shown,** only stages and status:
  - In Gen 1, stat changes recompute the modified stat from the stored stat × the stage
    multiplier (`[25,28,33,40,50,66,100,150,200,250,300,350,400]/100`), clamped to 1..999
    ([G1-SCRIPTS] L60-130).
  - The paralysis Speed drop (×1/4) and the burn Attack drop (×1/2) are applied directly to
    modified stats when the status is inflicted ([G1-SCRIPTS] L663-679).
  - These effective values are hidden for the opponent. Your own unboosted stats come in
    `|request|`; your boosted, paralysis- or burn-modified stats are not displayed, though a
    careful player can compute them.
- Critical hits in Gen 1 ignore stat stages, as well as Reflect and Light Screen. This is a
  mechanic, not an observation, but it explains damage variance seen in the logs.

### 1.8 What the log reveals about damage

All of the following are public:

| Message | Meaning |
| --- | --- |
| `\|move\|...\|[miss]` and `\|-miss\|SOURCE[\|TARGET]` | Miss, including Gen 1's 1/256 miss. |
| `\|-crit\|X` | Critical hit. |
| `\|-supereffective\|X`, `\|-resisted\|X` | Type effectiveness. Showdown's Move Effectiveness Mod shows the true multiplier, not the cartridge's dual-type misinformation. |
| `\|-immune\|X` | Immune. |
| `\|-fail\|X[\|reason]` | The move failed. |
| `\|-hitcount\|X\|N` | Multi-hit move hit N times. |
| `\|-damage\|X\|HP STATUS[\|[from] ...]` | Damage taken. The `[from]` reasons include psn, brn, confusion, Leech Seed and recoil. |
| `\|-heal\|X\|...` | HP restored. |
| `\|-activate\|X\|Substitute\|[damage]` | The Substitute absorbed the hit. No damage number is given. |

The only damage quantity is the HP change: exact for your own Pokémon, a percentage for the
opponent's. Move order is visible and reveals the speed relation, apart from speed ties and
Quick Attack/Counter priority.

### 1.9 Gen 1 / Showdown / pkmn quirks and leaks the encoder must handle

1. **The pkmn log is omniscient.** It has no `|split|` ([PKMN-PROTO] Overview), so every HP field
   holds the exact `current/max` for *both* sides. Quantize the opponent's HP with the function
   in 1.3 and drop the opponent's max HP.
2. **The pkmn status byte encodes the remaining sleep turns** in its low 3 bits, and a
   self-inflicted flag in bit 7 ([PKMN-G1] `Status`; [PKMN-TS] `decodeStatus`). Map it to the
   status category only. Learn "Rest sleep" from the `-status ... [from] Rest` message, not from
   bit 7.
3. **A pkmn `PokemonIdent` holds the original party slot (1-6)** ([PKMN-PROTO] `PokemonIdent`).
   Use it only as the tracking key. **Do not lay out opponent slots by original index,** because
   that leaks team order, and Metamon teams put the lead in slot 0. Lay them out in order of first
   appearance.
4. **The pkmn engine does not emit** these Showdown messages: `|player|`, `|teamsize|`,
   `|gametype|`, `|gen|`, `|tier|`, `|rule|`, `|start|`, `|request|`, `|upkeep|`, `|split|`,
   `|-hint|` and `|-message|`.
   - The env must supply team size itself.
   - Own-side request information comes from the engine `Battle` bytes.
   - `[still]` and `[miss]` arrive as separate `LastStill (0x01)` and `LastMiss (0x02)` bytes that
     modify the previous `|move|` in the same buffer ([PKMN-PROTO] `LastStill/LastMiss`).
   - There is no `|-notarget|`, `|-sidestart|` or `|-ohko|` in normal gen1ou play.
   - `-end` for Leech Seed lacks `[from]`/`[of]` (a FIXME in [PKMN-PROTO]).
   - Wrap's "partially trapped" end is not logged, which matches Showdown.
5. **`choices()` leaks** ([PKMN-PROTO] footnote 1). The own-side legal mask is computed from true
   state. In Gen 1 this can reveal that a partial-trapping sequence has ended a turn earlier than
   Showdown would (Showdown shows `maybeLocked` instead). This is minor; accept it and document
   it. **Never put the opponent's mask or the opponent's `choices()` into an observation.**
6. **Own-side hidden counters.** A Showdown player does *not* see these for their own Pokémon:
   - remaining sleep turns
   - remaining confusion turns
   - remaining Disable turns
   - remaining Thrash or Wrap turns
   - Substitute HP
   - the Toxic counter (it can only be inferred)

   DESIGN.md says "own side: everything". For Showdown fidelity, expose these as **elapsed /
   observed** counters, not as remaining durations from engine state. Remaining durations are
   future RNG and would make the policy unusable against real Showdown. If they are wanted for
   debugging, gate them behind an explicit `privileged` flag.
7. **Transform** gives the transformer's owner the opponent's full moveset and stats; see 1.4.
8. **Disable and Mimic** reveal a random opponent move; see 1.4.
9. **A missing message is informative:** no `-mustrecharge` after Hyper Beam (KO or Substitute
   break), no `cant slp` on a wake turn, and so on. The encoder needs no special handling, but
   "last turn" event features let the policy use this.
10. **`[silent]` messages still reach both players** (Thrash confusion, Haze cleanups, tox→psn on
    switch). Silent means "no display text", not "hidden".
11. **The opponent's stats are unknown.** Assuming max DVs and stat exp (near-universal in Gen 1
    OU) gives a public prior, `assumed_stats(species)`. It is fine to encode, because it uses only
    public knowledge.
12. **Level is public** via `DETAILS`, and is always 100 in the pool.

---

## 2. Proposed observation features

Notation: `ID` channels carry an integer index stored as float32, for an embedding in the model.
All other channels are scaled to roughly [0, 1] or [-1, 1].

**Token conventions** (the same for both sides):

- `SPECIES_ID`: `0 = EMPTY` (no such slot, team smaller than 6), `1..151 =` dex number,
  `152 = UNKNOWN` (the slot exists but is unrevealed).
- `MOVE_ID`: `0 = EMPTY` (known to be empty; own side only), `1..165 =` pkmn move id,
  `166 = UNKNOWN` (unrevealed opponent move).
- `STATUS` one-hot: `[none, psn, tox, brn, par, slp, frz, fnt]`.
- `TYPE` multi-hot over the 15 Gen 1 types.

### 2.1 Global (both players see the same values)

| Feature | Dim | Notes |
| --- | --- | --- |
| turn / 1000 | 1 | |
| own teamsize/6, own alive/6 | 2 | |
| opp teamsize/6, opp revealed/6, opp fainted/6 | 3 | Team size comes from the env, standing in for `|teamsize|`. |
| sleep-clause active vs opp, freeze-clause active vs opp, and the same pair from the opponent's view | 4 | Derived from history. |

### 2.2 Own side: 6 party slots in **own original slot order**

The order is fixed, matching action `4+k` = switch to party slot k.

| Feature | Dim | Source |
| --- | --- | --- |
| species ID | 1 | engine state |
| is_active, fainted | 2 | |
| hp / maxhp (exact), maxhp / 704 | 2 | |
| stored stats atk, def, spc, spe / 999 | 4 | engine state (Showdown `|request|` stats) |
| types | 15 | |
| status one-hot | 8 | |
| sleep turns observed / 7, rest_sleep flag | 2 | count of own `cant slp`; `[from] move: Rest` |
| per move slot ×4: move ID, pp / maxpp, maxpp / 64, disabled | 16 | engine state; `disabled` is shown in Showdown's request |

### 2.3 Own active (extra block)

| Feature | Dim |
| --- | --- |
| boosts atk, def, spc, spe, acc, eva / 6 | 6 |
| effective stats atk, def, spc, spe / 999 (after stages and par/brn) | 4 |
| current types (after Conversion/Transform) | 15 |
| volatiles bitset: confusion, leech_seed, substitute, reflect, light_screen, mist, focus_energy, must_recharge, partially_trapped (is target), trapping (is user), bide, locked (thrash/rage/charging), rage, transformed, disabled_any, mimic_active | 16 |
| confusion turns elapsed / 5, disable turns elapsed / 8, trap turns elapsed / 5, toxic counter observed / 15 | 4 |
| transformed-into species ID, mimic move ID | 2 |

### 2.4 Opponent side: 6 slots in **order of first appearance**

Unrevealed slots get `species = UNKNOWN` with all other channels zero and `revealed = 0`. When
teamsize < 6, the slots beyond teamsize get `species = EMPTY`.

| Feature | Dim | Source |
| --- | --- | --- |
| species ID, revealed, is_active, fainted | 4 | `|switch|`, `|faint|` |
| hp_pct / 100 | 1 | quantized per 1.3 |
| assumed maxhp / 704, assumed stats atk, def, spc, spe / 999 | 5 | public prior: species base stats, max DVs and exp |
| types (species) | 15 | |
| status one-hot | 8 | |
| sleep turns observed / 7, rest_sleep | 2 | |
| per revealed-move slot ×4 (in reveal order): move ID (UNKNOWN if not yet revealed), uses / maxpp_assumed, est_pp_left / 64 | 12 | `|move|` without `[from]`, plus Disable, Mimic and Transform reveals |
| n_moves_revealed / 4 | 1 | |

### 2.5 Opponent active (extra block)

This block has the same shape as 2.3 but carries only public data:

- boosts from `-boost`, `-unboost` and `-clearallboost`
- current types (Conversion `typechange`, Transform)
- the volatiles bitset from messages, including `partially_trapped` and `trapping` from the
  move and `cant` lines
- the elapsed counters
- transformed-into species and mimic move
- **no effective stats**

### 2.6 Last-turn events (per side, public)

| Feature | Dim |
| --- | --- |
| moved, moved_first, move ID used (incl. called moves), was_called (`[from]` Metronome/Mirror Move), missed, crit, supereffective, resisted, immune, failed, switched, cant-reason one-hot (slp, frz, par, partiallytrapped, flinch, Disable, recharge) | ~18 |
| hp delta of the target from this side's move (own: exact / maxhp; opp: percentage points / 100) | 1 |

This block is optional but cheap. It lets the policy read crits, misses and speed order without
recurrence.

The total is roughly 600-700 floats per player. The exact layout and `OBS_VERSION` belong in
`docs/OBSERVATION.md`.

---

## 3. Reveal tracking (`Memory`)

There is one `Memory` per (battle, observing player). It is fixed-size and `Copy` so that
`clone_state` stays a memcpy. It is updated by parsing the pkmn `-Dlog` buffer after every
`update`. Both players' Memories are fed the same buffer; the observer only changes which side
counts as "own" and "opp".

```text
Memory {
  opp_teamsize: u8, n_revealed: u8,
  opp_slot_of: [u8; 6],          // original pkmn slot (1..6) -> reveal index, 0xFF = unseen
  opp: [OppMon; 6],              // in reveal order
  opp_active: u8,                // reveal index, or 0xFF
  opp_vol: VolState,             // boosts[6], volatile bits, types, elapsed counters, transform/mimic
  own_vol: VolState,             // same, own elapsed counters (engine state gives the rest)
  own_sleep_obs: [u8; 6], own_rest: u8 (bitset),
  clause: {slp_by_me: bool, frz_by_me: bool, slp_by_opp: bool, frz_by_opp: bool},
  last: [TurnEvents; 2],         // index 0 = own, 1 = opp
}
OppMon { species: u8, level: u8, hp_pct: u8, status: u8, fainted: bool,
         sleep_obs: u8, rest: bool,
         moves: [u8; 4], uses: [u8; 4], n_moves: u8, flags: u8 /*transformed-suppress*/ }
```

**Parsing.** Before dispatching, fold `LastStill` and `LastMiss` into the preceding `|move|`. In
the table, "side(X)" is own or opp according to the ident's player bit, and `oi` is the reveal
index of X.

| pkmn ArgType | Update |
| --- | --- |
| `switch` (0x04) / `drag` (0x1F) | **Opponent:** if the original slot is unseen, assign the next reveal index and set species and level. Set `hp_pct = q(hp, maxhp)`, status = category(status byte), `opp_active = oi`, then reset `opp_vol` (boosts, volatiles, counters, types back to the species types). **Own:** reset `own_vol`. Both: clear `last[side].trapped`, etc. |
| `move` (0x03) | Set `last[side]` (move, missed from LastMiss, still). If there is no `[from]` and X is the opponent, the opponent is not transformed, and the move is not the current Mimic move: add the move to `opp[oi].moves` if new, and `uses += 1`. If `[from]` is Metronome or Mirror Move, set `was_called`. If `[from]` is the same move, it is a lock continuation: set the locked bit. First use of Wrap/Bind/Clamp/Fire Spin: set `trapping` on X and `partially_trapped` on the foe. Rage: set the rage and locked bits. Thrash/Petal Dance: set locked. |
| `cant` (0x05) | Set `last[side].cant = reason`. `slp`: `sleep_obs += 1`. `Disable`+move: reveal the move (opponent). `partiallytrapped`: set `partially_trapped`. `recharge`: clear `must_recharge`. |
| `faint` (0x06) | Set fainted, `hp_pct = 0`, status `fnt`, and clear that side's active vol. If the foe was trapping or trapped, clear the counterpart bits. |
| `turn` (0x07) | Increment elapsed counters (confusion, disable, trap) where active. Roll `last[]` over: keep the finished turn's events for the obs and clear the accumulators. |
| `-damage` (0x0A) / `-heal` (0x0B) | Update `hp_pct` (opponent, quantized) and status. Record the hp delta in `last[attacker]`. For `[from] confusion` or `Leech Seed`, no extra state. |
| `-status` (0x0C) | Set the status. `slp` with `[from]` Rest sets `rest = 1`, `sleep_obs = 0`. Otherwise for `slp`/`frz`, set the clause flag for the side that inflicted it (the foe of X). `[silent]` psn after a switch is a tox→psn conversion. |
| `-curestatus` (0x0D) | Set status none, `sleep_obs = 0`, `rest = 0`, and update the clause flags. |
| `-boost` / `-unboost` (0x0E) | `boosts[stat] += ±N`, clamped to ±6. Map `spa` to `spc` and ignore `spd`, because the duplicate always accompanies it. The reason `0x00` (`atk [from] Rage`) means atk +N. |
| `-clearallboost` (0x0F) | Zero the boosts on both sides. |
| `-fail` / `-miss` / `-crit` / `-supereffective` / `-resisted` / `-immune` / `-hitcount` (0x10-0x12, 0x1A-0x1D) | Set the event bits on `last[attacker]`. The attacker is the `|move|` user preceding in the buffer. |
| `-prepare` (0x13) | Set the locked or charging bit. |
| `-mustrecharge` (0x14) | Set `must_recharge`. |
| `-activate` (0x15) | `confusion`: no-op, since the volatile is already set. `Bide`: bide bit. `move: Haze`: clear the volatile bits on both sides except partial trap, recharge and locks, convert own `tox` to `psn`, and expect the following `-curestatus`. `Substitute\|[damage]`: event bit. `Struggle`: event bit. |
| `-start` (0x17) | Set the bit for: confusion, Bide, Focus Energy, Leech Seed, Light Screen, Mist, Reflect, Substitute. `typechange`: set types (Conversion). `Disable`+move: set `disabled_any`, `disable_elapsed = 0`; **if X is the opponent, reveal the move**. `Mimic`+move: set `mimic_move`. If X is own, the move is an **opponent move reveal**. |
| `-end` (0x18) | Clear the corresponding bit or counter. `Bide` also clears bide. `Substitute` clears the substitute bit. |
| `-transform` (0x1E) | Set `transformed` and `transform_species`, and copy the target's types and boosts. If the source is own and the target is the opponent, mark all 4 of the target's moves revealed: read them from the engine state of **your own** transformed active, which is what Showdown's request shows. If the source is the opponent, set the opponent's suppress-reveals flag until it switches. |

**Invariants worth testing:**

1. The observation for player p is unchanged if you mutate any hidden opponent field of the
   battle that has not been revealed: unrevealed species, moves, DVs, exact HP within the same
   percentage bucket, or sleep turns remaining.
2. Every opponent move revealed in Memory belongs to the opponent's real set, except during the
   opponent's Transform.
3. The opponent's `uses` never exceed the engine's PP consumption for that slot.
4. `q(hp, maxhp)` equals Showdown's `getHealth().shared` for all `0 ≤ hp ≤ maxhp ≤ 704`. This is
   cheap to test exhaustively.
5. A replayed seed gives identical Memory bytes.
