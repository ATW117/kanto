#!/usr/bin/env python3
"""Sample statistically plausible Gen 1 OU teams from Smogon usage stats ("chaos" JSON).

This is the fallback / augmentation path; the main pool is teams.jsonl (Metamon). Output follows
the DESIGN.md team schema, one team per line (JSONL). Run validate.py on the output.

Sampling model:
  * species 1 ~ usage;  species k+1 ~ usage * teammate affinity with the species already chosen
    (chaos "Teammates" values are deviations from independence; we use
    weight(s) = usage(s) * prod_{t chosen} max(0.05, P(t|s)/P(t)) with P(t|s) = Teammates[t]/count(s)),
    no repeats (Species Clause);
  * moves ~ the species' move counts, without replacement, 4 moves (fewer if the species has
    fewer known moves);
  * Gen 1 OU legality filters: Uber-tagged species (Mewtwo, Mew) excluded; OHKO-clause moves
    (Fissure, Horn Drill, Guillotine), evasion moves (Double Team, Minimize) and Dig/Fly (banned by
    Showdown's gen1 Standard) are dropped. Sleep and Freeze clauses are battle-time rules, not
    team-building rules, so nothing to do here.
  * level 100, max DVs and stat exp (schema defaults, omitted).

Usage:
  python3 generate.py -n 500 --seed 0 -o generated.jsonl
  python3 generate.py --stats usage/gen1ou-1630-2026-09.json --min-usage 0.01 -n 200
Fetch fresh stats from https://www.smogon.com/stats/<YYYY-MM>/chaos/gen1ou-<cutoff>.json.
"""
from __future__ import annotations

import argparse
import json
import os
import random
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_STATS = os.path.join(HERE, "usage", "gen1ou-1630-2026-09.json")
NAMES = os.path.join(HERE, "usage", "gen1_names.json")

BANNED_SPECIES = {"mewtwo", "mew"}  # Gen 1 Ubers
BANNED_MOVES = {"fissure", "horndrill", "guillotine",  # OHKO Clause
                "doubleteam", "minimize",               # Evasion Moves Clause
                "dig", "fly"}                           # Showdown gen1 Standard bans


def to_id(s: str) -> str:
    return re.sub(r"[^a-z0-9]", "", s.lower())


def weighted_pick(rng: random.Random, items: list[tuple[str, float]]) -> str:
    total = sum(w for _, w in items)
    r = rng.random() * total
    for k, w in items:
        r -= w
        if r <= 0:
            return k
    return items[-1][0]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("-n", type=int, default=200, help="number of distinct teams")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--stats", default=DEFAULT_STATS)
    ap.add_argument("--min-usage", type=float, default=0.01, help="drop species below this usage share")
    ap.add_argument("--min-move-share", type=float, default=0.02,
                    help="drop moves used on < this fraction of the species' sets")
    ap.add_argument("--size", type=int, default=6)
    ap.add_argument("-o", "--out", default="-")
    args = ap.parse_args()

    names = json.load(open(NAMES))
    stats = json.load(open(args.stats))
    info, data = stats["info"], stats["data"]
    label = f"generated:smogon-chaos/{os.path.basename(args.stats)}:seed{args.seed}"

    usage, moves, mates, count = {}, {}, {}, {}
    for sname, d in data.items():
        sid = to_id(sname)
        if sid in BANNED_SPECIES or sid not in names["species"] or d["usage"] < args.min_usage:
            continue
        usage[sid] = d["usage"]
        count[sid] = sum(d["Abilities"].values()) or 1.0  # weighted count
        mv = [(m, w) for m, w in d["Moves"].items()
              if m and m in names["moves"] and m not in BANNED_MOVES and w > 0]
        tot = count[sid]
        moves[sid] = [(m, w) for m, w in mv if w / tot >= args.min_move_share] or mv
        mates[sid] = {to_id(k): v for k, v in d["Teammates"].items()}
    if len(usage) < args.size:
        sys.exit("not enough species above --min-usage")

    rng = random.Random(args.seed)
    seen, out = set(), []
    attempts = 0
    while len(out) < args.n and attempts < args.n * 50:
        attempts += 1
        team: list[str] = []
        while len(team) < args.size:
            cands = []
            for s, u in usage.items():
                if s in team:
                    continue
                w = u
                for t in team:
                    # chaos Teammates[t] for s = weighted count of s-teams that also have t,
                    # so P(t|s) = Teammates/count[s]; lift = P(t|s) / P(t)
                    lift = (mates[s].get(t, 0.0) / count[s]) / max(usage[t], 1e-6)
                    w *= max(0.05, lift)
                cands.append((s, w))
            team.append(weighted_pick(rng, cands))
        mons = []
        for s in team:
            pool = list(moves[s])
            chosen = []
            while pool and len(chosen) < 4:
                m = weighted_pick(rng, pool)
                chosen.append(m)
                pool = [(k, w) for k, w in pool if k != m]
            mons.append({"species": names["species"][s], "moves": [names["moves"][m] for m in chosen], "level": 100})
        key = json.dumps(sorted((m["species"], sorted(m["moves"])) for m in mons))
        if key in seen:
            continue
        seen.add(key)
        out.append({"name": f"generated/{args.seed}/{len(out):05d}", "source": label, "pokemon": mons})

    f = sys.stdout if args.out == "-" else open(args.out, "w")
    for t in out:
        f.write(json.dumps(t, separators=(",", ":")) + "\n")
    if f is not sys.stdout:
        f.close()
    print(f"wrote {len(out)} teams ({info['metagame']} cutoff {info['cutoff']}, {info['number of battles']} battles)",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
