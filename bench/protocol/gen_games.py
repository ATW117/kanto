"""Generate random-choice Gen 1 OU battles with kanto and write them as Showdown input logs.

    python bench/protocol/gen_games.py TEAMS.jsonl N OUT.jsonl [SEED]

Each output line is {"id", "input": [Showdown input-log lines], "turns"}. Both engines then
replay exactly these battles (bench/protocol/README.md).
"""
import json
import random
import sys

import kanto

teams_path, n, out_path = sys.argv[1], int(sys.argv[2]), sys.argv[3]
seed = int(sys.argv[4]) if len(sys.argv) > 4 else 0
teams = [l for l in open(teams_path).read().splitlines() if l.strip()]
rng = random.Random(seed)
with open(out_path, "w") as f:
    for i in range(n):
        pb = kanto.ProtocolBattle(rng.choice(teams), rng.choice(teams), seed=rng.getrandbits(63))
        pb.start()
        while not pb.ended:
            ch = []
            for p in (0, 1):
                legal = pb.legal_choices(p)
                ch.append(rng.choice(legal) if legal else "")
            pb.step(*ch)
        f.write(json.dumps({"id": i, "input": pb.input_log(), "turns": pb.turn}) + "\n")
