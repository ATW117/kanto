# Observation layout (`OBS_VERSION = 1`)

`obs[i, p]` is player `p`'s view of battle `i`: a `float32[OBS_DIM]` vector with `OBS_DIM = 752`.
The encoder is `crates/battle-env/src/obs.rs`. The offsets below are its constants, and
`pokemon_env.obs_layout()` / `pokemon_env.id_offsets()` export them at runtime. The visibility
rules come from [OBSERVATION_RULES.md](OBSERVATION_RULES.md). Any change to this layout must bump
`OBS_VERSION`.

**Value kinds.**

- **ID** fields hold an integer index stored as float, meant for an embedding:
  - species: `0` = empty slot, `1..151` = dex number, `152` = unknown (unrevealed opponent slot)
  - move: `0` = empty, `1..165` = pkmn move id, `166` = unknown (unrevealed opponent move)
- All other fields are scaled to roughly [0, 1] (boosts to [-1, 1]).
- Names for the ids come from `pokemon_env.species_names()` and `pokemon_env.move_names()`.
- Types use pkmn order: Normal, Fighting, Flying, Poison, Ground, Rock, Bug, Ghost, Fire, Water,
  Grass, Electric, Psychic, Ice, Dragon.
- Status is a one-hot of 8, in the order `[none, psn, tox, brn, par, slp, frz, fnt]`. `tox` is shown
  only while the Pokémon is active and badly poisoned. On Showdown it reverts to `psn` on switch-out.
- Stats are in the order atk, def, spc, spe.

## Top-level blocks

| Block | Offset | Size | Notes |
| --- | --- | --- | --- |
| global | 0 | 12 | |
| own_mon0..5 | 12 + 51·k | 51 each | own team, **original team order** (switch action `4+k` = slot k) |
| opp_mon0..5 | 318 + 49·j | 49 each | opponent team, **order of first appearance** |
| own_active | 612 | 47 | |
| opp_active | 659 | 47 | public fields only |
| own_events | 706 | 18 | events of the last update |
| opp_events | 724 | 18 | |
| privileged | 742 | 6 | zeros unless `reveal_hidden=True` (debug) |
| reserved | 748 | 4 | zeros |

## global (12)

| Off | Field |
| --- | --- |
| 0 | turn / 1000 |
| 1 | own team size / 6 |
| 2 | own Pokémon alive / 6 |
| 3 | opponent team size / 6 (public: Showdown's `\|teamsize\|`) |
| 4 | opponent Pokémon revealed / 6 |
| 5 | revealed opponent Pokémon fainted / 6 |
| 6 | Sleep Clause active against the opponent (an opponent Pokémon is asleep, not from Rest) |
| 7 | Freeze Clause active against the opponent (an opponent Pokémon is frozen) |
| 8, 9 | the same two flags for my own side |
| 10 | my active Pokémon has fainted (forced switch pending) |
| 11 | the opponent's active Pokémon has fainted |

## own_mon (51)

| Off | Field |
| --- | --- |
| 0 | species ID (0 = empty slot) |
| 1 | is active |
| 2 | fainted |
| 3 | exact hp / max hp |
| 4 | max hp / 704 |
| 5 | level / 100 |
| 6-9 | stored stats atk, def, spc, spe / 999 |
| 10-24 | types (multi-hot 15) |
| 25-32 | status one-hot (8) |
| 33 | sleep turns observed so far / 7 (count of `cant slp`; **not** turns remaining) |
| 34 | Rest sleep |
| 35 + 4m | move m ID (0 = empty); for the active Pokémon these are the current moves (Mimic, Transform) |
| 36 + 4m | pp / max pp |
| 37 + 4m | max pp / 64 |
| 38 + 4m | disabled |

## opp_mon (49)

The slot index is the order in which the Pokémon first appeared. If team size < 6, slots at or
beyond the team size are all zeros (species ID 0 = empty). Unrevealed slots below the team size
have species `152` and every other field zero.

| Off | Field |
| --- | --- |
| 0 | species ID (152 = unknown) |
| 1 | revealed |
| 2 | is active |
| 3 | fainted |
| 4 | HP percentage / 100, using Showdown's `ceil(100·hp/maxhp)`, capped at 99 unless at full HP |
| 5 | level / 100 |
| 6 | assumed max hp / 704 (public prior: species base stats at max DVs and stat exp) |
| 7-10 | assumed stats atk, def, spc, spe / 999 (same prior) |
| 11-25 | species types (multi-hot 15) |
| 26-33 | status one-hot (8); no sleep counter |
| 34 | sleep turns observed / 7 |
| 35 | Rest sleep |
| 36 + 3m | move m ID in reveal order (166 = unknown) |
| 37 + 3m | observed uses / max pp |
| 38 + 3m | estimated pp left / 64 (max pp − uses) |
| 48 | revealed moves / 4 |

The opponent's exact HP, max HP, stats, DVs, stat exp and unrevealed moves are never encoded.

## own_active / opp_active (47)

These describe the Pokémon currently on the field. The block is all zeros before the battle starts.

| Off | Field |
| --- | --- |
| 0-5 | boosts atk, def, spc, spe, accuracy, evasion / 6 |
| 6-9 | **own only:** effective stats atk, def, spc, spe / 999, after stages and par/brn. Zero for the opponent. |
| 10-24 | current types (Conversion, Transform) |
| 25-40 | volatiles (see below) |
| 41 | turns confused so far / 5 |
| 42 | turns disabled so far / 8 |
| 43 | turns of partial trapping so far / 5 |
| 44 | turns badly poisoned so far / 15 |
| 45 | transformed-into species ID (0 = not transformed) |
| 46 | move ID copied by Mimic (0 = none) |

The volatiles, in order: confusion, leech_seed, substitute, reflect, light_screen, mist,
focus_energy, must_recharge, partially_trapped (the foe is binding this Pokémon), trapping (this
Pokémon is binding), bide, locked (thrash/rage/charging/fly-dig), rage, transformed, disabled_any,
mimic_active.

All of these are visible to both players on Showdown. The block never contains remaining
durations, Substitute HP or the Toxic counter, for either side; elapsed counters stand in for them.

## own_events / opp_events (18)

These are the public events of the most recent update for that side.

| Off | Field |
| --- | --- |
| 0 | moved |
| 1 | moved first |
| 2 | move ID used, including called moves |
| 3 | called (`[from]`: Metronome/Mirror Move, or a lock continuation) |
| 4 | missed |
| 5 | crit |
| 6 | super effective |
| 7 | resisted |
| 8 | immune |
| 9 | failed |
| 10 | switched |
| 11-17 | `cant` reason one-hot: slp, frz, par, partiallytrapped, flinch, Disable, recharge |

## privileged (6, debug only)

This block is written only with `VecEnv(..., reveal_hidden=True)`. It holds the true hidden
counters of the observing player's own active Pokémon, which a Showdown player cannot see:

| Off | Field |
| --- | --- |
| 0 | sleep turns left / 7 |
| 1 | confusion turns left / 5 |
| 2 | disable turns left / 8 |
| 3 | thrash/bide/binding attacks left / 5 |
| 4 | Substitute hp / max hp |
| 5 | Toxic counter / 15 |

Do not train on these if the policy is meant to play on Showdown.

## Reveal memory

Reveal memory follows OBSERVATION_RULES.md §3, driven by pkmn's `-Dlog` protocol (see
`crates/battle-env/src/{protocol,memory,pkmn1}.rs`):

- **Species.** `|switch|` reveals the species and assigns the next opponent slot.
- **Moves revealed and PP counted.** `|move|` without `[from]` reveals a move and counts one PP
  use. Called moves, lock continuations, moves used while the opponent is transformed, Mimic-copied
  moves and Struggle are skipped.
- **Moves revealed without a PP use:**
  - `|-start|X|Disable|MOVE` and `|cant|X|Disable|MOVE`.
  - `|-start|X|Mimic|MOVE`, which reveals one of X's foe's moves to X's owner.
  - `|-transform|`: when my Pokémon transforms, all four of the target's moves are revealed to me.
- Engine logs carry no `|split|` or `|teamsize|`. HP is quantised by the encoder. The team size is
  taken from the engine state, standing in for `|teamsize|`.

**Known deviations.** pkmn's `choices()` leaks when partial trapping ends (see
OBSERVATION_RULES.md §1.9.5). Only the *own* mask is exposed, never the opponent's. A two-turn move
whose charge is interrupted still counts as one observed use, as the rules prescribe, but the
engine only deducts PP when the attack executes, so in that case the use estimate can exceed the
true consumption by one.
