# Like-for-like speed: kanto vs Showdown on identical battles

1. `gen_games.py` plays random-choice Gen 1 OU battles with kanto and writes them as Showdown
   input logs (seed, packed teams, every choice).
2. `showdown_bench.js` replays every log through Showdown's own `BattleStream` (battle log and
   both players' requests), after a JIT warm-up; median of 3 passes.
3. `protocol_bench` (crates/kanto-backend/examples) replays the same logs with kanto, two ways,
   median of 5 passes:
   - `kanto-protocol`: `ProtocolBattle`, which writes the same battle log and requests (byte for
     byte: docs/SHOWDOWN_INTEROP.md), parsing each input log inside the timed loop as Showdown
     does;
   - `kanto-engine`: the bare simulator, no text (what `VecEnv` uses).

Both engines report the same total turn count, a check that they replayed the same battles.

```bash
python bench/protocol/gen_games.py TEAMS.jsonl 2000 games.jsonl 0
cargo build --release -p kanto-backend --example protocol_bench
taskset -c 9 ./target/release/examples/protocol_bench games.jsonl 5
PS_DIR=/path/to/pokemon-showdown taskset -c 9 node bench/protocol/showdown_bench.js games.jsonl 3
```

Results, 2026-10-17, NVIDIA DGX Spark, one Cortex-X925 core (3.9 GHz), Showdown 0.11.11 on
Node 22, 2,000 battles per pool:

| Engine | Ladder pool (159,891 turns) | kanto pool (180,045 turns) |
| --- | ---: | ---: |
| Pokémon Showdown `BattleStream` (log + requests) | 7.69 k turns/s | 7.69 k turns/s |
| kanto `ProtocolBattle` (same log + requests) | 185 k turns/s (**24.1x**) | 189 k turns/s (**24.5x**) |
| kanto engine (no text) | 6.80 M turns/s (884x) | 7.04 M turns/s (916x) |

The engine figure replays fixed choices; random self-play, which also enumerates legal choices,
runs 4.6-5.0 M turns/s (`examples/bench.rs`).
