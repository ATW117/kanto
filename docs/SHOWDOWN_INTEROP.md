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
plus `[Gen 1] OU`'s bans: Ubers (Mewtwo, Mew), Dig and Fly, OHKO Clause, Evasion Moves Clause,
Nickname Clause and the "exactly 0 EVs" check. Against `TeamValidator('gen1ou')` on 11,000
level-100 teams it agrees on 10,950. It is never stricter than Showdown; the 50 teams it accepts
and Showdown rejects fail Showdown's "Obtainable" checks (event-only moves, move combinations
from different events, e.g. Kakuna with Poison Sting + Harden). With random levels, Showdown's
minimum-level checks ("must be at least level 36 to be evolved") add more such cases. For a
ladder-legal guarantee, run Showdown's validator (data/teams/gen1ou/validate.py does).

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
