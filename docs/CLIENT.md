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

| Teams | Battles | Player-steps | Exact | Differences |
| --- | ---: | ---: | ---: | --- |
| kanto's generated pool | 300 | 41,340 | 99.68% | partial trapping only |
| Metamon Gen 1 OU eval split | 3,000 | 367,712 | 99.78% | partial trapping only |

Every field matches except three that depend on **partial trapping** (Wrap, Bind, Fire Spin,
Clamp). Its length (2-5 hits with probabilities 3/8, 3/8, 1/8, 1/8) is drawn in secret and the
lock ends without a message; in Gen 1 even the binder's request does not show it. The env's
observation, which is computed from the simulator's state, shows whether the lock continues; no
player can know that. The client predicts it from the number of hits so far (binding after one or
two hits, where continuing is more likely), so the `partially_trapped`/`trapping` flags and the
trap-elapsed counter can differ from the env's in those states. The test requires every other
field to match exactly and the partial-trapping differences to stay below 1% of steps.

Other approximations (not reached by the test corpus):

- After the observer's own **Transform**, its modified stats are the target's, which Showdown
  does not reveal; the client estimates them from the target's species (maximum DVs and stat exp).
- A **Mimic** copy of a move the opponent also knows is indistinguishable from its own move; the
  client counts it as a reveal when the species can learn the move.

**Showdown version.** The client is verified against pokemon-showdown 0.11.11 (kanto's reference).
A live server running a newer Showdown may print some messages differently.
