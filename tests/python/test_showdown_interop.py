"""Showdown interop: team formats, the gen1ou validator, string seeds, input logs, copies.

The input-log test replays kanto battles through an unmodified Showdown ``BattleStream``
(parity/showdown/inputlog.js) when ``PS_DIR`` points at a pokemon-showdown package and node is
on PATH; it is skipped otherwise.
"""

import json
import os
import random
import shutil
import subprocess
from pathlib import Path

import pytest

import kanto

ROOT = Path(__file__).resolve().parents[2]

EXPORT = """Bessie (Tauros)
Ability: No Ability
EVs: 252 HP / 252 Atk / 252 Def / 252 SpA / 252 SpD / 252 Spe
- Body Slam
- Hyper Beam
- Blizzard
- Earthquake

Chansey
IVs: 14 HP / 28 Atk
- Soft-Boiled
- Thunder Wave
- Ice Beam
- Seismic Toss

Snorlax
- Body Slam
- Rest
- Self-Destruct
- Reflect
"""


def play_random(pb, rng):
    pb.start()
    while not pb.ended:
        pb.step(*[rng.choice(pb.legal_choices(p)) for p in range(2)])


def test_export_and_packed_teams_are_accepted():
    packed = kanto.convert_team(EXPORT, "packed")
    assert packed.startswith("Bessie|Tauros|")
    # Every representation gives the same battle.
    logs = []
    for team in (EXPORT, packed, kanto.convert_team(packed, "export")):
        pb = kanto.ProtocolBattle(team, team, seed=3)
        play_random(pb, random.Random(1))
        logs.append(pb.log)
    assert logs[0] == logs[1] == logs[2]
    assert any("|switch|p1a: Bessie|Tauros|" in l for l in logs[0])


def test_validate_team_gen1ou():
    assert kanto.validate_team(EXPORT) == []
    bad = "Mewtwo\n- Psychic\n- Fly\n\nChansey\n- Minimize\n- Fissure\n"
    problems = kanto.validate_team(bad)
    assert len(problems) >= 3, problems
    assert any("Uber" in p for p in problems)


def test_string_seeds_and_copy():
    team = kanto.convert_team(EXPORT, "packed")
    sodium = "sodium," + "ab" * 32
    a = kanto.ProtocolBattle(team, team, seed=sodium)
    assert a.seed == sodium
    b = kanto.ProtocolBattle(team, team, seed="1,2,3,4")
    assert b.seed == "1,2,3,4"
    # An int seed is the env's 64-bit Gen 5 seed (words big-endian).
    c = kanto.ProtocolBattle(team, team, seed=(1 << 48) | (2 << 32) | (3 << 16) | 4)
    assert c.seed == "1,2,3,4"
    with pytest.raises(ValueError):
        kanto.ProtocolBattle(team, team, seed="nonsense")
    a.start()
    d = a.copy()
    rng = random.Random(5)
    while not d.ended:
        d.step(*[rng.choice(d.legal_choices(p)) for p in range(2)])
    assert not a.ended and a.turn == 1


def _showdown():
    ps = os.environ.get("PS_DIR")
    node = os.environ.get("NODE") or shutil.which("node")
    if not ps or not node or not Path(ps, "dist/sim").exists():
        pytest.skip("needs PS_DIR (pokemon-showdown package) and node")
    return node, ps


def _norm(log):
    return ["|t:|" if l.startswith("|t:|") else l for l in log]


@pytest.mark.parametrize("fmt", ["gen1ou", "gen1nc"])
def test_input_log_replays_identically_in_showdown(fmt):
    node, ps = _showdown()
    teams = kanto.ProtocolTeams(kanto.TEAMS_EVAL)
    rng = random.Random(11)
    battles, lines = [], []
    for i in range(60):
        if i % 3 == 0:
            team = kanto.convert_team(EXPORT, "packed")
            seed = "sodium," + "".join(rng.choice("0123456789abcdef") for _ in range(64))
            pb = kanto.ProtocolBattle(team, team, seed=seed, names=("Alice", "Bob B."), format=fmt)
        else:
            pb = teams.battle(seed=i, names=("p1", "Ω player"), format=fmt)
        play_random(pb, rng)
        battles.append(pb)
        lines.append(json.dumps({"id": i, "input": pb.input_log()}))
    proc = subprocess.run(
        [node, str(ROOT / "parity/showdown/inputlog.js")],
        input="\n".join(lines) + "\n",
        capture_output=True,
        text=True,
        env={**os.environ, "PS_DIR": ps},
        check=True,
    )
    results = [json.loads(l) for l in proc.stdout.splitlines() if l.strip()]
    assert len(results) == len(battles)
    for r in results:
        assert "error" not in r, r.get("error")
        assert _norm(r["log"]) == _norm(battles[r["id"]].log), f"battle {r['id']} differs"


def test_view_log_and_replay_html():
    pb = kanto.ProtocolTeams(kanto.TEAMS_EVAL).battle(seed=4, names=("Alice", "Bob"))
    rng = random.Random(2)
    per_step = {c: [] for c in ("spectator", "p1", "p2")}
    out = pb.start()
    while True:
        for c in per_step:
            per_step[c] += out[c]
        if pb.ended:
            break
        out = pb.step(*[rng.choice(pb.legal_choices(p)) for p in range(2)])
    assert pb.view_log(0) == per_step["spectator"]
    assert pb.view_log(1) == per_step["p1"] and pb.view_log(2) == per_step["p2"]
    assert not any(l.startswith("|split|") for l in pb.view_log(0))
    page = kanto.replay_html(pb)
    assert "Alice vs. Bob" in page and "|win|" in page and 'class="battle-log-data"' in page
    with pytest.raises(ValueError):
        pb.view_log(3)
