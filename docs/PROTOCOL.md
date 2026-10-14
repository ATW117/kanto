# Showdown protocol output

kanto's Gen 1 simulator can write Pokémon Showdown's text protocol: the battle log with `|split|`
blocks, each player's view and the `|request|` JSON, line for line as pokemon-showdown 0.11.11
produces them. A program written for Showdown (a bot, a client-side parser, a replay tool) can
therefore play or watch kanto battles offline, with no server and no websocket. The same battle
also produces the env's observations and action masks, so a Showdown agent and an env-based
agent can play each other.

```
          ProtocolBattle (Rust: kanto-gen1 + kanto-backend::ProtoGame)
          ├─ per-player protocol view + |request| JSON ──► Showdown agent ──► "move 2"
          └─ obs[752] / mask[10] / aux label ──────────► env policy ──────► action ─► "switch 3"
```

## Python API

`kanto.ProtocolBattle` (PyO3; Rust: `kanto_gen1::ProtocolBattle`, `kanto_backend::ProtoGame`):

```python
import kanto
teams = kanto.ProtocolTeams(kanto.TEAMS_EVAL)        # or "random", "randbats", a file, ...
pb = teams.battle(seed=7, names=("Alice", "Bob"), format="gen1ou")
# or: kanto.ProtocolBattle(p1_team_json, p2_team_json, seed=..., names=..., format=...)
out = pb.start()
out["update"]       # new Battle#log lines, with |split| blocks (Showdown's `update` message)
out["omniscient"], out["spectator"], out["p1"], out["p2"]   # extractChannelMessages views
out["requests"]     # (p1 |request| JSON, p2 |request| JSON); None once the battle is over
out["opp_action_obs_frame"]   # VecEnv's auxiliary label, per player
out = pb.step("move 2", "switch 3")   # Showdown choice strings; "" = pass; ValueError if illegal
pb.legal_choices(0); pb.observe(0) -> (obs, mask); pb.action_to_choice(0, a); pb.choice_to_action(0, "move 1")
pb.ended, pb.outcome (1 p1, 2 p2, 3 tie), pb.turn, pb.log, pb.request(p), pb.set_timestamp(t)
pb.opp_team_targets(p) -> (species[6], moves[6, 4])   # hidden-team labels, as VecEnv.opp_team_targets
```

`format` is `"gen1ou"` (Sleep and Freeze Clause Mod) or `"gen1nc"` (no clauses).

A driver loop for an agent that consumes Showdown messages:

```python
out = pb.start()
while not pb.ended:
    choices = []
    for p in (0, 1):
        lines = list(out[f"p{p + 1}"])               # what a server would send player p
        if out["requests"][p]:
            lines.append("|request|" + out["requests"][p])
        choices.append(agents[p].choose(lines))      # e.g. "move 1", "switch 4", or ""
    out = pb.step(*choices)
```

A seat can be played from observations instead: `obs, mask = pb.observe(p)`, pick an action `a`
with `mask[a]`, and pass `pb.action_to_choice(p, a)`.

## What is reproduced

Byte for byte against pokemon-showdown 0.11.11:

- every battle-log line, including:
  - `[from]`/`[of]` tags;
  - `attrLastMove` edits (`[still]` blanks the target, `[miss]`, `[notarget]`);
  - hints, with the once-per-battle set and side-only hints (split, empty shared line);
  - `-message`, `bigerror` and the turn-1000 messages;
  - the header (`|player|`, `|rule|`s, gen1nc's custom-rules `|raw|`);
  - `|t:|` lines (the timestamp is a parameter; parity normalises it);
- `|split|pN` blocks: the owner sees `hp/maxhp`, everyone else `pct/100` (HP Percentage Mod);
- the `|request|` JSON, with its key order, `noCancel` (Cancel Mod), `trapped`/`maybeLocked`,
  the Gen 1 `Fight`/`Recharge`/locked-move entries, and `wait` and `forceSwitch`;
- player names and nicknames (stored in the protocol log, not in the `Copy` battle state).

Not reproduced: the server's room messages (`|init|`, `|title|`, `|j|`) and the `rqid` the server
adds to requests. Drivers that need those synthesise them.

## How it is implemented

The protocol comes from the same code path as the fast simulator. `kanto_core::Sink::log()`
returns `None` for the env's sinks and `Some(&mut Log)` for `kanto_core::ProtoSink`, so every
`this.add(...)` site of Showdown's Gen 1 code has a `plog!(s, |l| ...)` counterpart at the same
point in the ported function, and the fast path builds no strings. Details: docs/SIM_DESIGN.md,
"Protocol output".

The env's fast path is unaffected (no text is built): 4.89 M randbats turns/s, as before. With
protocol text, Python stepping and random choices, one thread plays about 1,250 games/s
(about 110 k steps/s).

## Parity

`cargo run --release -p kanto-parity --bin parity -- fuzz --proto ...` replays fuzzed Showdown
battles and compares, at every decision step:

- the update lines;
- the four channel views;
- both requests.

Result: 0 divergences in 8,998 battles / 668,877 steps (mixed, targeted, thaw, stall, random,
legal, gen1nc and pkmn-patched runs; table in docs/SIM_STATUS.md). The fuzzer also covers:

- player names with spaces, quotes and non-ASCII characters;
- nicknames, including over-long ones and nicknames equal to the species name.

Node-free regression: `parity/fixtures/corpus-proto.jsonl` (143 battles), replayed by `cargo
test`. Rust tests:

- `crates/kanto-gen1/tests/protocol.rs`: a golden Showdown transcript, and checks that the
  protocol sink never changes the game and keeps HP private;
- `crates/kanto-backend/tests/native.rs::proto_game_matches_env`: the same observations, masks and
  auxiliary labels as `VecEnv`.
