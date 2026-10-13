# Native simulator: status (Gen 1)

Reference: pokemon-showdown **0.11.11**, vanilla (`Compat::Vanilla`, the default). The
pkmn-patched mode (`Compat::Pkmn`) is checked against the same Showdown with
parity/showdown/patches.js. Numbers below come from `parity fuzz` runs on 2026-10-13; see
docs/SIM_DESIGN.md for what is compared.

## Parity summary

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

**Showdown crashes (excluded):** 2 of 40,998 battles. Showdown itself throws "moveSlot is null"
in `Side#commitChoices`, a 0.11.11 bug when a Pokémon whose `lastSelectedMoveSlot` points past its
current move slots must "Fight". The native sim raises the same assertion.

**Divergences found and fixed during development** (the fuzzer's categories led to each):

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

Run `cargo run --release -p sim-backend --example bench -- 20000 <teams>`. Each engine plays
20,000 games single-threaded on this DGX Spark core (aarch64). Both engines use the same seeds and
the same uniform-random action policy. No logging, observations or masks are produced.

| Engine | randbats turns/s | gen1ou pool turns/s |
| --- | ---: | ---: |
| pkmn/engine (libpkmn `-Dshowdown`, no log) | 6.29 M | 6.50 M |
| native, vanilla Showdown | 2.59 M | 2.57 M |
| native, pkmn-patched | 2.64 M | 2.66 M |

Native runs at about 0.4× pkmn/engine and about 570× Showdown (~4.5k turns/s). The battle state
is 1,360 bytes against pkmn's 384. Shrinking the state and avoiding per-event volatile scans are
the obvious next optimisations.

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
| Protocol text log | no (structured `Event` stream only) | event stream checked (e1-e3) | – |
| Team validation, team preview, Gen 1 Stadium, other gens | no | – | – |

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
