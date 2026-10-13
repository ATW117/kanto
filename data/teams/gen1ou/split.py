#!/usr/bin/env python3
"""Deterministic train/eval split of the team pool (teams.jsonl -> split/{train,eval}.jsonl).

A team goes to the held-out eval split iff the SHA-256 of its canonical JSON (keys sorted, no
whitespace, UTF-8) is 0 mod 10, so about 10% of teams are held out. The assignment depends only
on the team itself: it is stable under reordering of teams.jsonl and when teams are added or
removed. Lines are written in teams.jsonl order, byte-identical to the source lines.

    python3 split.py           # (re)write split/train.jsonl and split/eval.jsonl
    python3 split.py --check   # exit 1 if the split files are missing or stale

Loaders accept "data/teams/gen1ou:train" and "data/teams/gen1ou:eval" for these files.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
EVAL_MOD = 10


def is_eval(team: dict) -> bool:
    canon = json.dumps(team, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return int.from_bytes(hashlib.sha256(canon.encode("utf-8")).digest()[:8], "big") % EVAL_MOD == 0


def split(lines: list[str]) -> tuple[list[str], list[str]]:
    train, held = [], []
    for line in lines:
        if line.strip():
            (held if is_eval(json.loads(line)) else train).append(line.rstrip("\n") + "\n")
    return train, held


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--pool", type=Path, default=HERE / "teams.jsonl")
    ap.add_argument("--out", type=Path, default=HERE / "split")
    ap.add_argument("--check", action="store_true", help="verify the files instead of writing them")
    args = ap.parse_args()
    train, held = split(args.pool.read_text(encoding="utf-8").splitlines())
    want = {"train.jsonl": "".join(train), "eval.jsonl": "".join(held)}
    if args.check:
        stale = [n for n, text in want.items() if not (args.out / n).exists() or (args.out / n).read_text(encoding="utf-8") != text]
        print(f"train {len(train)} / eval {len(held)}: {'stale ' + ', '.join(stale) if stale else 'up to date'}")
        return 1 if stale else 0
    args.out.mkdir(exist_ok=True)
    for name, text in want.items():
        (args.out / name).write_text(text, encoding="utf-8")
    print(f"wrote {args.out}/train.jsonl ({len(train)} teams) and eval.jsonl ({len(held)} teams)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
