# Gen 1 OU team pool

## Layout

| File | What |
| --- | --- |
| `teams.jsonl` | **The pool.** 9,232 distinct teams, one JSON object per line, in the DESIGN.md team schema. |
| `validate.py` | Schema + legality check (Showdown's own `TeamValidator` for `gen1ou`, run through `validate_ps.js`). |
| `validate_ps.js` | Node helper for `validate.py`. Needs a `pokemon-showdown` npm package. |
| `generate.py` | Samples extra teams from Smogon usage stats (a fallback or augmentation; not used for `teams.jsonl`). |
| `usage/gen1ou-1630-2026-09.json` | Smogon "chaos" usage stats snapshot used by `generate.py`. |
| `usage/gen1_names.json` | Showdown id to display-name map for Gen 1 species and moves (from `Dex.mod('gen1')`). |

**Format choice: one `teams.jsonl`, not one file per team.** About 9k files would be unwieldy. A
loader should read `data/teams/gen1ou/*.jsonl` (one team per non-empty line), and may also accept
`*.json` (a single team object, or a list of them) for hand-written teams. `usage/` is a
subdirectory, so a non-recursive `*.json` glob of this directory does not pick up the stats files.

Example line (formatted here for reading):

```json
{"name":"metamon-teams/hl_05_26/team_004635","source":"metamon-teams/hl_05_26","pokemon":[
  {"species":"Alakazam","moves":["Seismic Toss","Psychic","Recover","Thunder Wave"],"level":100}, ...]}
```

- Names use Showdown display spelling (`Soft-Boiled`, `Self-Destruct`, `Farfetch’d` with U+2019).
- `dvs` and `exp` are written only when they differ from the max (DV 15, stat exp 65535). 105 sets
  in the pool are non-default: 22 Jynx/Chansey-style "min Attack" sets (Atk DV 1, Atk exp 0, which
  lowers confusion self-hit damage) and 83 sets with Atk stat exp 0.
- `dvs.hp` is always derived the Gen 1 way, from the low bits of Atk/Def/Spe/Spc. `validate.py`
  enforces this.
- Every team has 6 Pokémon at level 100, each with 4 moves. Slot 0 is the team's lead as it
  appeared in the source.

## Provenance and licenses

| `source` field | Count | Origin |
| --- | --- | --- |
| `metamon-teams/competitive` | 28 | Metamon "competitive" set: human-made Gen 1 OU sample teams scraped from Smogon forum threads. |
| `metamon-teams/hl_05_26` | 9,204 | Metamon "High Ladder May '26" set: teams reconstructed from high-rated Showdown ladder and tournament replays. The parts a replay does not reveal are filled in from rating-appropriate usage stats. |

- Dataset: Hugging Face `jakegrigsby/metamon-teams` (commit `bff785c8`, files
  `competitive/gen1ou.tar.gz` and `hl_05_26/gen1ou.tar.gz`). These are Showdown teambuilder text
  files, converted here with Showdown's `Teams.import`.
- License: the Metamon project (github.com/UT-Austin-RPL/metamon) is **MIT**, according to the
  GitHub license API. The HF dataset card states no separate license, so we treat the teams as
  released under the project's MIT license. Credit: Grigsby et al., "Metamon" (UT Austin RPL).
  Team sets themselves are game data, not creative works of the dataset authors. The competitive
  sample teams originate from Smogon forum posts.
- Conversion rules: a set with no `EVs:` line gets max stat exp, which is the Showdown gen1/2
  teambuilder default. Without this rule the validator would reject those sets with "0 EVs". EV
  `e` maps to stat exp `65535` if `e >= 252`, otherwise `e*e`. IV `i` maps to DV `floor(i/2)`.
- Dedup: 164 of the 9,396 source teams were dropped as duplicates of an earlier team. Teams were
  compared in canonical form: species, sorted moves, DVs and exp, ignoring slot order. 25 of the
  164 were byte-exact duplicates. The 28 competitive teams come first in the file.
- Pool composition: 121 species. The top 11 (Tauros, Snorlax, Chansey, Exeggutor, Starmie,
  Alakazam, Jynx, Zapdos, Rhydon, Gengar, Cloyster) account for most slots. Because these are real
  ladder teams, there is a long tail of off-meta picks (Diglett, Abra, Meowth, Pikachu...). Filter
  on `source == "metamon-teams/competitive"` or on species if you want a "clean" OU pool.
- Usage stats snapshot: `https://www.smogon.com/stats/2026-09/chaos/gen1ou-1630.json` (Smogon/
  Showdown public usage statistics, September 2026, 1630+ rating, 11,702 battles). Smogon publishes
  these without an explicit license. Only aggregate counts are used.
- Other Metamon gen1ou sets (`gl_05_26` 29k, `modern_replays_v2` 19k, `paper_replays`,
  `paper_variety`) can be converted the same way if more variety is needed. The converter used is
  the scratch script described below.

## Validation

```
python3 validate.py                 # all *.jsonl / *.json here; exit 0 iff all pass
python3 validate.py -v some.jsonl   # print every failing team
python3 validate.py --schema-only   # no Node needed
```

Layer 1 (Python) checks the schema: 1-6 Pokémon, 1-4 distinct moves, distinct species, level 1-100,
DVs 0-15 with a consistent Gen 1 HP DV, and stat exp 0-65535.

Layer 2 runs Showdown's `TeamValidator.get('gen1ou')`. This covers learnsets (including the
"can't be transferred from Gen 2" rule, i.e. Stat Tradeback Clause), the Uber ban (Mewtwo, Mew),
the Species Clause, the OHKO Clause (Fissure, Horn Drill, Guillotine), the Evasion Moves Clause
(Double Team, Minimize), and Showdown's gen1 Standard bans on Dig and Fly. Sleep, Freeze and
Desync Clause Mods are battle-time rules, so a validator cannot check them. The engine/env is
responsible for those if it wants Showdown parity. Point `--ps` (or `PS_PATH`) at a
`pokemon-showdown` npm package. Node comes from `--node`, `$NODE`, `PATH`, or the newest
`~/.nvm/versions/node/*`.

Last run (pokemon-showdown 0.11.11, Node 22):

```
loaded 9232 teams from 1 file(s)
schema:   9232 pass / 0 fail
showdown: 9232 pass / 0 fail  (pokemon-showdown 0.11.11, format gen1ou)
TOTAL: 9232 pass / 0 fail
```

300 teams from `generate.py --seed 1` also passed 300/300. A negative test was rejected
correctly: Mewtwo, Fissure, Dig, Double Team, Gen 2 tradeback Flamethrower on Chansey, duplicate
species, and an inconsistent HP DV.

## generate.py

```
python3 generate.py -n 500 --seed 0 -o extra.jsonl && python3 validate.py extra.jsonl
```

The generator samples the first species by usage share and each later species by usage times
teammate lift. The lift is P(t|s)/P(t), from chaos `Teammates` counts. No species repeats, and
species under `--min-usage` (default 1%) are dropped. Moves are drawn by the species' move usage
without replacement, after dropping moves seen on fewer than 2% of the species' sets and the
clause-banned moves. Levels are 100 with max DVs and stat exp. To refresh the stats, download a
newer `gen1ou-<cutoff>.json` from `smogon.com/stats/<YYYY-MM>/chaos/` and pass `--stats`.

## Reproducing teams.jsonl

1. Download `competitive/gen1ou.tar.gz` and `hl_05_26/gen1ou.tar.gz` from
   `huggingface.co/datasets/jakegrigsby/metamon-teams` and extract them.
2. For each `*.gen1ou_team` file, run `Teams.import` (Showdown). Canonicalize names via
   `Dex.mod('gen1')`, apply the EV/IV rules above, and derive the HP DV.
3. Concatenate competitive first, then hl_05_26, and drop canonical duplicates.

The converter was a throwaway Node script and was not committed. These steps are the whole logic.
