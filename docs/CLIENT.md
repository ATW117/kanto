# Client-side observations

`kanto.ClientBattle` rebuilds the env's observation and action mask for one player from the
messages a Pokémon Showdown server sends that player: battle-log updates and `|request|` JSON. An
agent trained on `VecEnv` can therefore play on a real Showdown server (a ladder, a tournament)
with the exact inputs it was trained on.

```python
import kanto

cb = kanto.ClientBattle("MyBot")          # the player's username (the first request also sets the seat)
for message in battle_room_messages:      # each server message for the battle room, as received
    cb.feed(message)                      # "|request|{...}" or a block of battle-log lines
    if it_is_time_to_act:
        obs, mask = cb.observe()          # float32[752], bool[10], as VecEnv gives that player
        a = policy(obs, mask)
        send(f"/choose {cb.choice(a)}")   # "move 2", "switch 3"
```

## Semantics

- **One update per message.** Showdown sends one `update` message per resolved decision; each
  is one env step. `feed` treats a message containing battle-log lines as one update. The env
  produces an observation at every step, including steps where a player only waits (its
  opponent's forced switch); `observe()` after such an update returns a pass-only mask
  (`[True, False, ...]`), exactly like the env.
- **Room messages.** Only messages with battle events (moves, switches, `|turn|`, `-` lines, ...)
  are updates. Chat, joins, timer notices and a header sent before `|start|` are not; the
  header's `|player|` and `|teamsize|` lines are still read.
- **Requests.** Showdown sends a player's `|request|` before the update it follows. `feed` keeps a
  request pending and applies it after the next update. `flush()` applies a pending request that
  no update follows (e.g. a resent request).
- **Opponent order.** The encoder lists opponent Pokémon in order of first appearance, so the
  client indexes them that way; the opponent's hidden team order never matters.
- **Actions.** Actions 0-3 are the request's move slots; action `4 + k` switches to the player's
  `k`-th Pokémon in its original team order (the order of the first request). `choice(a)` maps
  them to Showdown's positional choice strings.

## What is reconstructed

From the player's own `|request|`: its team, exact stats, HP, move slots, PP and switch options.
From the log: everything public about both sides, with Gen 1 rules applied where Showdown changes
state without a message. Among them:

- the observer's **modified stats** (boosts, the 999/1 boost quirk, paralysis/burn drops, the
  drops re-applied when a move changes a stat, and the drops kept on the bench and on fainting);
- **PP**, including two-turn moves (used on release) and two-turn moves called by Metronome
  (the caller's PP is used on release);
- **silent volatiles**: Rage, Thrash locks (ended by fatigue confusion), charging and Dig/Fly
  invulnerability, recharge (cancelled by sleep and by partial trapping), and what full
  paralysis, Disable and confusion self-hits cancel;
- the **reveal memory** and last-update events, as `kanto_env::reveal` applies them in the env.

## Verification

`crates/kanto-backend/tests/client.rs` plays kanto battles, feeds each player's protocol view
(request first, then the update, as a server sends them) into a `ClientBattle`, and compares the
client's observation, mask and choice strings with the env's (`ProtoGame`) at every step.

| Teams | Battles | Player-steps | Exact steps | Differences |
| --- | ---: | ---: | ---: | --- |
| Metamon Gen 1 OU eval split | 3,000 | 402,358 | 100% | none |
| Metamon Gen 1 OU train split | 3,000 | 394,624 | 99.994% | own Mimic PP (25 steps) |
| kanto's generated pool | 3,000 | 392,070 | 99.9995% | own Mimic slot (2 steps) |

Two pieces of Gen 1 state are hidden from both players, and the env and the client estimate
both from the protocol in the same way (`memory::SideTrack`), so the observation never reveals
them:

- **Partial trapping** (Wrap, Bind, Fire Spin, Clamp) lasts 2-5 hits (3/8, 3/8, 1/8, 1/8), drawn in
  secret, and ends without a message; not even the binder's request shows it. The
  `partially_trapped`/`trapping` flags and the trap-elapsed counter predict it from the hits so
  far (binding after one or two, where continuing is more likely); a player whose request is a
  lone "Fight" knows it is bound.
- **Recharge** after Hyper Beam can be cancelled by a hidden flinch. The flag follows
  `|-mustrecharge|` and the events that end a recharge; for the own side the request shows it.

Moves are counted as revealed by a public rule too: a move used while it is the user's Mimic copy
is not a reveal, even if the user also knows it.

The remaining differences are in the observer's own PP and move slots after its own Mimic copies
a move it also knows: the PP comes off the slot the player chose, which the client is not told.
Neither involves information the player lacks. The test requires every field to match exactly.

Other approximations (not reached by the test corpus):

- After the observer's own **Transform**, its modified stats are the target's, which Showdown
  does not reveal; the client estimates them from the target's species (maximum DVs and stat exp).
- A **Mimic** copy of a move the opponent also knows is indistinguishable from its own move; the
  client counts it as a reveal when the species can learn the move.

**Showdown version.** The client is verified against pokemon-showdown 0.11.11 (kanto's reference).
A live server running a newer Showdown may print some messages differently.
