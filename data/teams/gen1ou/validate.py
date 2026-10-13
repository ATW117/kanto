#!/usr/bin/env python3
"""Validate the Gen 1 OU team pool.

Two layers:
  1. Schema checks (pure Python): the DESIGN.md team schema, DV/stat-exp ranges, the Gen 1 HP DV
     rule, 1-6 Pokemon, 1-4 distinct moves, distinct species.
  2. Legality (Pokemon Showdown's own TeamValidator for `gen1ou`, via validate_ps.js under Node):
     species/learnsets/event legality, Uber ban (Mewtwo, Mew), Species/Sleep/Freeze/OHKO/Evasion
     clauses and the Gen 1 move bans that Showdown applies at team-validation time.

Usage:
  python3 validate.py                       # every *.jsonl / *.json team file in this directory
  python3 validate.py teams.jsonl other.json
  python3 validate.py --ps /path/to/node_modules/pokemon-showdown --node /path/to/node
  python3 validate.py --schema-only         # skip the Showdown layer

Exit code 0 iff every team passes every enabled layer.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import shutil
import subprocess
import sys
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_PS = (
    "/tmp/claude-1000/-home-azimuth-projects-pokemon/5dd1f50b-dc9e-47b6-bc32-c02aff9d36ad/"
    "scratchpad/simsurvey/ps/node_modules/pokemon-showdown"
)
STATS = ("hp", "atk", "def", "spe", "spc")
# schema stat -> Showdown stat ids (Gen 1 Special is stored as spa=spd in Showdown)
PS_STAT = {"hp": ["HP"], "atk": ["Atk"], "def": ["Def"], "spe": ["Spe"], "spc": ["SpA", "SpD"]}


def load_teams(paths: list[str]) -> list[tuple[str, dict]]:
    """Return [(location, team)]. Accepts .jsonl (one team per line) and .json (one team, or a list)."""
    out = []
    for path in paths:
        if path.endswith(".jsonl"):
            with open(path) as f:
                for i, line in enumerate(f, 1):
                    if line.strip():
                        out.append((f"{os.path.basename(path)}:{i}", json.loads(line)))
        else:
            with open(path) as f:
                data = json.load(f)
            for i, team in enumerate(data if isinstance(data, list) else [data]):
                out.append((f"{os.path.basename(path)}[{i}]", team))
    return out


def schema_problems(team: dict) -> list[str]:
    p = []
    if not isinstance(team.get("name"), str):
        p.append("missing/invalid 'name'")
    if not isinstance(team.get("source"), str):
        p.append("missing/invalid 'source'")
    mons = team.get("pokemon")
    if not isinstance(mons, list) or not 1 <= len(mons) <= 6:
        return p + ["'pokemon' must be a list of 1-6 entries"]
    species = []
    for j, m in enumerate(mons):
        tag = f"pokemon[{j}]"
        if not isinstance(m.get("species"), str) or not m["species"]:
            p.append(f"{tag}: missing species")
            continue
        species.append(m["species"])
        moves = m.get("moves")
        if not isinstance(moves, list) or not 1 <= len(moves) <= 4:
            p.append(f"{tag} {m['species']}: needs 1-4 moves")
        elif len(set(moves)) != len(moves):
            p.append(f"{tag} {m['species']}: duplicate moves")
        lvl = m.get("level", 100)
        if not isinstance(lvl, int) or not 1 <= lvl <= 100:
            p.append(f"{tag} {m['species']}: level must be int 1-100")
        dvs = m.get("dvs")
        if dvs is not None:
            if set(dvs) - set(STATS):
                p.append(f"{tag}: unknown dv keys {sorted(set(dvs) - set(STATS))}")
            full = {s: dvs.get(s, 15) for s in STATS}
            if any(not isinstance(v, int) or not 0 <= v <= 15 for v in full.values()):
                p.append(f"{tag}: DVs must be ints 0-15")
            else:
                hp = ((full["atk"] & 1) << 3) | ((full["def"] & 1) << 2) | ((full["spe"] & 1) << 1) | (full["spc"] & 1)
                if "hp" in dvs and dvs["hp"] != hp:
                    p.append(f"{tag}: HP DV {dvs['hp']} inconsistent with other DVs (Gen 1 HP DV = {hp})")
        exp = m.get("exp")
        if exp is not None:
            if set(exp) - set(STATS):
                p.append(f"{tag}: unknown exp keys {sorted(set(exp) - set(STATS))}")
            if any(not isinstance(v, int) or not 0 <= v <= 65535 for v in exp.values()):
                p.append(f"{tag}: stat exp must be ints 0-65535")
    if len(set(species)) != len(species):
        p.append("duplicate species (Species Clause)")
    return p


def to_showdown_export(team: dict) -> str:
    """Showdown teambuilder text. Stat exp -> EVs as floor(sqrt(exp)) capped at 252 (what Showdown
    uses for Gen 1/2 stat computation); DVs -> IVs as 2*DV (+1 for DV 15 so 15 -> 31)."""
    blocks = []
    for m in team["pokemon"]:
        lines = [m["species"]]
        if m.get("level", 100) != 100:
            lines.append(f"Level: {m['level']}")
        exp = {s: (m.get("exp") or {}).get(s, 65535) for s in STATS}
        dvs = {s: (m.get("dvs") or {}).get(s, 15) for s in STATS}
        evs = []
        ivs = []
        for s in STATS:
            ev = min(252, int(exp[s] ** 0.5))
            iv = 31 if dvs[s] == 15 else dvs[s] * 2
            for ps in PS_STAT[s]:
                evs.append(f"{ev} {ps}")
                ivs.append(f"{iv} {ps}")
        lines.append("EVs: " + " / ".join(evs))
        lines.append("IVs: " + " / ".join(ivs))
        lines += [f"- {mv}" for mv in m["moves"]]
        blocks.append("\n".join(lines))
    return "\n\n".join(blocks) + "\n"


def find_node(explicit: str | None) -> str | None:
    if explicit:
        return explicit
    if os.environ.get("NODE"):
        return os.environ["NODE"]
    found = shutil.which("node")
    if found:
        return found
    cands = sorted(glob.glob(os.path.expanduser("~/.nvm/versions/node/*/bin/node")))
    return cands[-1] if cands else None


def showdown_problems(teams, node: str, ps: str, fmt: str, batch: int = 2000) -> dict[str, list[str]]:
    script = os.path.join(HERE, "validate_ps.js")
    result: dict[str, list[str]] = {}
    for k in range(0, len(teams), batch):
        chunk = [{"name": loc, "export": to_showdown_export(t)} for loc, t in teams[k:k + batch]]
        proc = subprocess.run([node, script, ps, fmt], input=json.dumps(chunk), capture_output=True,
                              text=True, check=False)
        if proc.returncode != 0:
            sys.exit(f"validate_ps.js failed:\n{proc.stderr}")
        for r in json.loads(proc.stdout):
            result[r["name"]] = r["problems"]
    return result


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="*")
    ap.add_argument("--ps", default=os.environ.get("PS_PATH", DEFAULT_PS),
                    help="path to the pokemon-showdown npm package (env PS_PATH)")
    ap.add_argument("--node", default=None, help="node binary (env NODE; default: PATH, then ~/.nvm)")
    ap.add_argument("--format", default="gen1ou")
    ap.add_argument("--schema-only", action="store_true")
    ap.add_argument("-v", "--verbose", action="store_true", help="print every failing team")
    args = ap.parse_args()

    files = args.files or sorted(glob.glob(os.path.join(HERE, "*.jsonl")) + glob.glob(os.path.join(HERE, "*.json")))
    teams = load_teams(files)
    print(f"loaded {len(teams)} teams from {len(files)} file(s)")

    failures: dict[str, list[str]] = {}
    for loc, t in teams:
        prob = schema_problems(t)
        if prob:
            failures[loc] = prob
    print(f"schema:   {len(teams) - len(failures)} pass / {len(failures)} fail")

    dup = Counter(json.dumps(t["pokemon"], sort_keys=True) for _, t in teams if "pokemon" in t)
    ndup = sum(c - 1 for c in dup.values() if c > 1)
    if ndup:
        print(f"note: {ndup} exact duplicate team(s) in pool")

    if not args.schema_only:
        node = find_node(args.node)
        if not node or not os.path.isdir(args.ps):
            print("Showdown layer unavailable (need node and --ps); rerun with --schema-only to skip", file=sys.stderr)
            return 2
        ver = json.load(open(os.path.join(args.ps, "package.json")))["version"]
        ok_teams = [(loc, t) for loc, t in teams if loc not in failures]
        sd = showdown_problems(ok_teams, node, args.ps, args.format)
        nfail = sum(1 for v in sd.values() if v)
        print(f"showdown: {len(sd) - nfail} pass / {nfail} fail  (pokemon-showdown {ver}, format {args.format})")
        for loc, prob in sd.items():
            if prob:
                failures[loc] = prob

    if failures:
        reasons = Counter(r for prob in failures.values() for r in prob)
        print("top failure reasons:")
        for r, c in reasons.most_common(15):
            print(f"  {c:6d}  {r}")
        if args.verbose:
            for loc, prob in failures.items():
                print(f"{loc}: {prob}")
    print(f"TOTAL: {len(teams) - len(failures)} pass / {len(failures)} fail")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
