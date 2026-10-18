# Using kanto with Showdown tools

How to move teams, seeds and battles between kanto and Pokémon Showdown, and how to run
Showdown clients (poke-env and the agents built on it) against kanto without a Node server.

## Teams

Every API that takes a team (`VecEnv(teams=...)` files, `ProtocolBattle`, `ProtocolTeams`,
`kanto.server`) accepts:

- kanto's JSON schema (docs/DESIGN.md), one team, a list, or JSON Lines;
- Showdown's **export** (teambuilder) text, including nicknames (`Bessie (Tauros)`), and team
  backups with several `=== [gen1ou] Name ===` sections;
- Showdown's **packed** format (what `/utm` sends), one team per line, with or without the
  `gen1ou]Name|` backup prefix.

Team files may be `.json`, `.jsonl`, `.txt` (export) or `.packed`.

```python
packed = kanto.convert_team(export_text, "packed")    # or "export"; input may be any format
problems = kanto.validate_team(packed, "gen1ou")      # [] = legal
```

Conversions follow how Showdown stores Gen 1 sets: DV = IV // 2 (Special is the SpA IV/EV);
stat exp = EV² (255 = 65535), which gives the same stats. A packed set with an empty EV field
gets 252 everywhere (the validator's Gen 1 default); so does an export without an `EVs:` line
(Showdown's sim-level import would give 0 EVs, which its validator rejects). A missing HP IV is
derived from the other DVs; a given one must agree. Items, abilities, natures and the like are
ignored. kanto writes explicit EVs, IVs and `No Ability`: an unvalidated Showdown battle fills an
empty ability from the modern dex (Tauros would get Intimidate).

`parity teams` checks all of this against Showdown (Node + `PS_DIR` needed):

```bash
cargo run --release -p kanto-parity --bin parity -- teams --random 10000 --level100
```

Result (pokemon-showdown 0.11.11, 1,000 pool + 10,000 random teams): the stats, levels, names,
moves and ability of every Pokémon of a Showdown `Battle` built from kanto's export and packed
text equal kanto's (22,000 of 22,000); kanto reads Showdown's own `Teams.pack` output back to the
same team (10,172 of 10,172; teams with an all-zero-EV Pokémon are skipped because the packed
format writes those EVs as "default").

**Validator.** `validate_team` = kanto's legality checks (Species Clause, Gen 1 learnsets, HP DV)
plus `[Gen 1] OU`'s bans (Ubers: Mewtwo, Mew; Dig and Fly; OHKO Clause; Evasion Moves Clause;
Nickname Clause), the EV sanity checks (exactly 0, 508 or 510 EVs; level 50 with an EV total
divisible by 4) and Showdown's Gen 1 obtainability checks, ported from `TeamValidator`
(`tools/gen_obtainable.js` extracts the data): the "Obtainable Moves" combination bans (e.g.
Kakuna + String Shot + Harden), level-up moves above the set's level (counting pre-evolutions),
event-only moves (one event must give all of them, no Japan-only events, the event's level), the
lowest Gen 1 encounter level and the minimum level to be evolved. Messages are Showdown's.

Against `TeamValidator('gen1ou')` (`parity teams`, 2026-10-18) it agrees on every team: 11,000
teams with random levels (1,000 pool + 10,000 random legal-learnset teams; 7,817 illegal, 3,183
legal) and 11,000 level-100 teams (7,440 / 3,560). Before the obtainability checks it accepted 123
of 4,000 random-level teams that Showdown rejects. (kanto does not store shininess, so it cannot
check that event Pokémon are not shiny; teams it builds are never shiny.)

## Seeds

`ProtocolBattle(..., seed=...)` takes an int (the env's 64-bit Gen 5 seed, words big-endian) or a
Showdown seed string: `"sodium,<64 hex>"` (ChaCha20, what Showdown uses for unseeded battles,
i.e. every battle on a live server), `"gen5,<16 hex>"` or `"a,b,c,d"`. `pb.seed` is the seed as
Showdown writes it. (`VecEnv` seeds are ints.)

## Input logs: replay a kanto battle in Showdown

`pb.input_log()` returns the battle as a Showdown input log:

```
>start {"formatid":"gen1ou","seed":"sodium,…"}
>player p1 {"name":"Alice","team":"Bessie|Tauros||NoAbility|bodyslam,…"}
>player p2 {…}
>p1 move 1
>p2 switch 3
…
```

`pokemon-showdown simulate-battle` or a `BattleStream` replays it into the same battle.
`parity/showdown/inputlog.js` does that for JSON lines of `{id, input}` and prints Showdown's
`Battle#log`, so anyone can check kanto against any Showdown version without the parity harness:

```python
import json, subprocess, kanto
pb = kanto.ProtocolTeams(kanto.TEAMS_EVAL).battle(seed=1)
# ... play it ...
r = subprocess.run(["node", "parity/showdown/inputlog.js"], input=json.dumps({"id": 0, "input": pb.input_log()}),
                   capture_output=True, text=True, env={"PS_DIR": ps_dir}).stdout
norm = lambda log: ["|t:|" if l.startswith("|t:|") else l for l in log]   # |t:| lines hold timestamps
assert norm(json.loads(r)["log"]) == norm(pb.log)
```

Measured (2026-10-16): 3,000 random battles (pool, random and randbats teams; gen1ou and gen1nc;
Gen 5 and sodium seeds; 269,496 decisions) give identical logs in pokemon-showdown 0.11.11 and in
Showdown master 7244fe8 (2026-10-16). `tests/python/test_showdown_interop.py` runs 120 of them
when `PS_DIR` is set.

## A drop-in `BattleStream` / `simulate-battle`

`kanto.BattleStream` (Rust: `kanto_backend::sd::{BattleStream, BattleTextStream}`) speaks
Showdown's battle-stream protocol, the one `sim/battle-stream` and `pokemon-showdown
simulate-battle` use, for `[Gen 1] OU`: write `>start`, `>player` and `>p1`/`>p2` lines, read back
exactly the messages Showdown would send (`update`, `sideupdate` with `|request|` and `|error|`,
`end`, `requesteddata`). `kanto-simulate` is the same as a command-line program.

```bash
cargo build --release -p kanto-backend --bin kanto-simulate
target/release/kanto-simulate < battle.txt        # same stdout as: pokemon-showdown simulate-battle < battle.txt
```

```python
s = kanto.BattleStream()
out = s.write('>start {"formatid":"gen1ou","seed":"1,2,3,4"}\n'
              '>player p1 {"name":"Alice","team":"Tauros|||NoAbility|bodyslam,hyperbeam|||||||"}\n'
              '>player p2 {"name":"Bob","team":"Snorlax|||NoAbility|bodyslam,rest|||||||"}\n')
out = s.write(">p1 move Body Slam\n>p2 move 9\n")   # p2 gets "|error|[Invalid choice] Can't move: Your Snorlax doesn't have a move 9"
msgs = s.write(">p2 default\n", messages=True)       # ["update\n|\n|t:|...", "sideupdate\np1\n|request|{...}", ...]
kanto.simulate_battle(text)                           # a whole input at once
kanto.extract_channel(update, 1)                      # p1's view of an update (getPlayerStreams / extractChannelMessages)
```

What is ported (from pokemon-showdown 0.11.11, `sim/battle-stream.ts`, `sim/side.ts`,
`sim/battle.ts`, `sim/teams.ts`):

- Commands: `>start` (seed as a string or an array, `rated`, players inline), `>player` (packed or
  JSON teams, name/avatar/rating, renames), `>p1`/`>p2` choices and `undo`, `>forcewin`,
  `>forcetie`, `>forcelose`, `>tiebreak`, `>reseed`, `>chat`, `>chat-inputlogonly`, `>requestlog`,
  `>requestexport`, `>requestteam`, `>version`; unknown commands throw as in Showdown.
- Choices: `Side#choose` with its exact error messages: move by index or name, target locations,
  `mega`/`zmove`/`max`/`terastallize` suffixes (rejected the way Showdown rejects them in Gen 1),
  `switch` by slot or name, `pass`/`skip`, `default`/`auto` and an empty `move`, `shift`, `team`,
  comma lists, `testfight`; `[Invalid choice]` and `[Unavailable choice]` errors, request updates
  (`"update":true`), the Gen 1 Struggle notice, and the Cancel Mod `undo` rules.
- Output batching: one `update` (and the pending requests) per write, flushed early past 500 log
  lines, partial input lines held for the next write and dropped at the end, the `end` summary
  (winner, seed, teams as Showdown's `Pokemon` constructor left them, score, input log), and
  nothing after the stream ends.
- Showdown's own crash: for the "moveSlot is null" state (docs/SIM_STATUS.md) the stream fails
  the same way, on the same input.

Checked with `parity stream-fuzz` (2026-10-18): random input streams through Showdown's own
`BattleTextStream` and kanto's, compared byte for byte with only the `|t:|` timestamps
normalised. Each input is written three ways in both — one write per line (an interactive
client), the whole input in one write (`simulate-battle < file`) and random chunk boundaries.
Inputs mix legal choices (by index, by name, `default`, with targets) with invalid ones (bad
indices and names, targets, events, `pass`/`shift`/`team`, garbage), `undo` and `testfight`, chat
and request commands, `>reseed`, and endings by faint, `>forcewin`, `>forcetie`, `>forcelose` and
`>tiebreak`; teams come packed or as JSON with random nicknames, levels, EV/IV fields, genders,
shininess and key order.

| Run (seed) | inputs | input lines | runs | battles ended | differ |
| --- | ---: | ---: | ---: | ---: | ---: |
| 100 | 3,000 | 293,335 | 9,000 | 2,743 | 0 |
| 4 | 1,000 | 98,051 | 3,000 | 911 | 0 |

The runs covered 37,879 `|error|` lines (2,648 distinct messages), 353 request updates, 1,467
Struggle notices, 6,144 forced-switch and 5,882 "Fight" requests, 2,469 wins, 274 ties and 144
tiebreaks. Runs where Showdown throws (a choice before both players joined, the "moveSlot is
null" crash) count as equal only if kanto throws too and the output before the throw is equal.
`pokemon-showdown simulate-battle --skip-build < input` and `kanto-simulate < input` gave
byte-identical stdout and the same exit status on 60 of 60 generated inputs; fed line by line
through a pipe, they also agree when Node keeps up with the input (Node reads whatever has
arrived since its last read as one chunk, so its batching depends on timing, as it does between
two runs of Showdown). `parity/fixtures/stream/corpus.jsonl` (160 runs) replays without Node in
`cargo test`; `tests/python/test_battle_stream.py` compares with Node when `PS_DIR` is set.

Not supported (an error, never a silent difference):

- Formats other than `gen1ou` and its `@@@!Sleep Clause Mod` / `!Freeze Clause Mod` variants.
- Set fields Showdown's simulator would act on but kanto does not model: abilities other than No
  Ability (an empty packed ability becomes the species' modern ability in Showdown, e.g.
  Intimidate, and works in Gen 1 battles there), items, natures other than the neutral ones,
  non-Gen 1 species or moves, unknown move names, levels above 100, more than 6 Pokémon or 4
  moves. Showdown's server validator would reject most of these anyway.
- Random teams (`>player` without a team), `>eval`, `>editbattle`, `>show-openteamsheets` before
  the battle starts, the `--replay`/`--spectate`/`--debug` output modes, doubles/p3/p4.
- Timers, chat rooms, ladders and request ids (`rqid`) are server features, not part of
  `BattleStream`; `kanto.server` covers the parts poke-env needs.

## A local Showdown server: `kanto.server`

`python -m kanto.server` speaks enough of the Showdown *server* protocol for unmodified poke-env
players to log in, challenge or `/search`, and battle; the battles are kanto `ProtocolBattle`s.

```bash
pip install websockets            # or: pip install kanto[server]
python -m kanto.server --port 8000 --teams data/teams/gen1ou:eval
```

```python
from poke_env import AccountConfiguration, RandomPlayer, MaxBasePowerPlayer, ServerConfiguration
cfg = ServerConfiguration("ws://localhost:8000/showdown/websocket", "")
p1 = RandomPlayer(battle_format="gen1ou", server_configuration=cfg,
                  account_configuration=AccountConfiguration("alice", None))
p2 = MaxBasePowerPlayer(battle_format="gen1ou", server_configuration=cfg,
                        account_configuration=AccountConfiguration("bob", None))
await p1.battle_against(p2, n_battles=100)
```

Players that send a team (`team=` in poke-env, i.e. `/utm`) play it, after `validate_team`
(`--no-validate` to skip); players that do not get teams from `--teams`. Choices may name moves
and Pokémon as poke-env sends them (`move thunderbolt`, `switch chansey`), and `default`.

Measured with poke-env 0.16.1 (one Python process for both clients, `/utm` with one 6-Pokémon
pool team; DGX Spark, 2026-10-16):

| Random vs Random | kanto.server | `pokemon-showdown start --no-security` (0.11.11) |
| --- | ---: | ---: |
| 10 concurrent battles | 115-121 battles/s | 56 battles/s |
| one battle at a time | 52-71 battles/s | 7.6 battles/s |

Random vs MaxBasePower, 10 concurrent: 226 vs 118 battles/s. At these rates poke-env's own message
handling, not the simulator, is the bottleneck (kanto.server uses about 12 ms of CPU per battle);
the speedup comes mostly from Showdown's per-turn latency.
`tests/python/test_server.py` plays challenge, `/utm` and ladder battles when poke-env is
installed.

Limits: a development server, not a ladder. No authentication (any name is accepted, no
password), no ratings, chat, spectators or replays; the timer is acknowledged but not enforced;
formats `gen1ou` and `gen1nc` only. Clients that use Showdown's HTTP login endpoint or room
features beyond battles need a real server. Metamon and PokéChamp use their own poke-env forks
and were not tested.
