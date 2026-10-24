"""kanto.BattleStream: Showdown's battle-stream protocol (what ``pokemon-showdown simulate-battle``
reads and writes), served by the native simulator.

The Node comparison runs when ``PS_DIR`` points at a pokemon-showdown package and node is on PATH;
the larger randomised check is ``cargo run --release -p kanto-parity --bin parity -- stream-fuzz``.
"""

import json
import os
import random
import re
import shutil
import subprocess
from pathlib import Path

import pytest

import kanto

ROOT = Path(__file__).resolve().parents[2]

P1 = "Tauros|||NoAbility|bodyslam,hyperbeam,blizzard,earthquake|||||||]Chansey|||NoAbility|softboiled,thunderwave,icebeam,seismictoss|||||||"
P2 = [
    {"name": "Bessie", "species": "Snorlax", "ability": "No Ability", "moves": ["Body Slam", "Rest", "Amnesia", "Blizzard"]},
    {"species": "Starmie", "moves": ["surf", "recover", "thunderwave"], "level": 90, "evs": {"hp": 252}},
]


def norm(text: str) -> str:
    return re.sub(r"\|t:\|\d+", "|t:|", text)


def header(seed="1,2,3,4"):
    return (
        f'>start {{"formatid":"gen1ou","seed":"{seed}"}}\n'
        f'>player p1 {json.dumps({"name": "Alice", "team": P1})}\n'
        f'>player p2 {json.dumps({"name": "Bob", "team": P2})}\n'
    )


def requests(msgs):
    out = {}
    for m in msgs:
        if m.startswith("sideupdate\n"):
            side, body = m.split("\n", 2)[1:]
            if body.startswith("|request|"):
                out[side] = json.loads(body[len("|request|"):])
    return out


def legal(req):
    if req is None or req.get("wait"):
        return []
    side = req["side"]["pokemon"]
    switches = [f"switch {i + 1}" for i, p in enumerate(side) if i > 0 and not p["condition"].endswith(" fnt")]
    if req.get("forceSwitch"):
        return switches or ["pass"]
    act = req["active"][0]
    moves = [f"move {i + 1}" for i, m in enumerate(act["moves"]) if not m.get("disabled")]
    return moves + ([] if act.get("trapped") else switches)


def play(stream, rng, max_steps=2000):
    """Play to the end with random legal choices; returns every message and the input lines."""
    lines = header().splitlines(keepends=True)
    msgs = stream.write("".join(lines), messages=True)
    reqs = requests(msgs)
    for _ in range(max_steps):
        if stream.ended:
            break
        progressed = False
        for side in ("p1", "p2"):
            opts = legal(reqs.get(side))
            if not opts:
                continue
            line = f">{side} {rng.choice(opts)}\n"
            lines.append(line)
            new = stream.write(line, messages=True)
            msgs += new
            reqs.update(requests(new))
            progressed = True
            if stream.ended:
                break
        assert progressed, "no player has a choice but the battle has not ended"
    return msgs, "".join(lines)


def test_plays_to_the_end():
    s = kanto.BattleStream()
    msgs, _ = play(s, random.Random(1))
    assert s.ended
    end = [m for m in msgs if m.startswith("end\n")]
    assert len(end) == 1
    summary = json.loads(end[0][4:])
    assert summary["seed"] == "1,2,3,4" and summary["p1"] == "Alice" and summary["p2"] == "Bob"
    assert summary["inputLog"][0] == '>start {"formatid":"gen1ou","seed":"1,2,3,4"}'
    # JSON teams are packed in the input log, after Showdown filled in their defaults.
    assert summary["inputLog"][2].startswith('>player p2 {"name":"Bob","team":"Bessie|Snorlax||NoAbility|BodySlam,Rest,Amnesia,Blizzard')
    assert summary["p2team"][1]["level"] == 90 and summary["p2team"][1]["ivs"]["spe"] == 30


def test_invalid_choices_and_undo():
    s = kanto.BattleStream()
    s.write(header())
    out = s.write(">p1 move 9\n>p1 switch 1\n>p2 move Earthquake\n>p1 move 1 mega\n")
    assert out == (
        "sideupdate\np1\n|error|[Invalid choice] Can't move: Your Tauros doesn't have a move 9\n\n"
        "sideupdate\np1\n|error|[Invalid choice] Can't switch: You can't switch to an active Pokémon\n\n"
        "sideupdate\np2\n|error|[Invalid choice] Can't move: Your Bessie doesn't have a move matching earthquake\n\n"
        "sideupdate\np1\n|error|[Invalid choice] Can't move: Tauros can't mega evolve\n\n"
    )
    # A choice can be undone and changed until the other player has chosen (Cancel Mod).
    assert s.write(">p1 move 1\n>p1 undo\n>p1 move blizzard\n") == ""
    out = s.write(">p2 default\n")
    assert "|move|p1a: Tauros|Blizzard|" in out
    assert s.input_log[-2:] == [">p1 move blizzard", ">p2 move bodyslam"]


def test_channel_views():
    s = kanto.BattleStream()
    msgs = s.write(header(), messages=True)
    update = next(m for m in msgs if m.startswith("update\n"))[len("update\n"):]
    p1 = kanto.extract_channel(update, 1).split("\n")
    spectator = kanto.extract_channel(update, 0).split("\n")
    assert "|switch|p1a: Tauros|Tauros|290/290" in p1 and "|switch|p2a: Bessie|Snorlax|100/100" in p1
    assert "|switch|p1a: Tauros|Tauros|100/100" in spectator and not any(l.startswith("|split|") for l in spectator)


def test_forcewin_ends_the_stream():
    s = kanto.BattleStream()
    s.write(header())
    out = s.write(">forcewin p2\n")
    assert norm(out).startswith("update\n|\n|win|Bob\n\nend\n")
    assert s.ended
    assert s.write(">p1 move 1\n") == ""


def test_unsupported_input_raises():
    s = kanto.BattleStream()
    with pytest.raises(RuntimeError, match="Unrecognized command"):
        s.write(">bogus\n")
    s = kanto.BattleStream()
    s.write('>start {"formatid":"gen1ou","seed":"1,2,3,4"}\n')
    with pytest.raises(RuntimeError, match="ability"):
        # An empty packed ability becomes the species' modern ability in Showdown (Intimidate).
        s.write('>player p1 {"team":"Tauros||||bodyslam|||||||"}\n')


def node_simulate(ps_dir: str, text: str) -> str:
    r = subprocess.run(
        ["node", os.path.join(ps_dir, "pokemon-showdown"), "simulate-battle", "--skip-build"],
        input=text.encode("utf-8"), capture_output=True, timeout=120,
    )
    # Bytes, not text mode: on Windows text mode writes cp1252 and turns "\n" into "\r\n".
    return r.stdout.decode("utf-8")


@pytest.mark.skipif(not (os.environ.get("PS_DIR") and shutil.which("node")), reason="needs PS_DIR and node")
def test_matches_showdown_simulate_battle():
    ps_dir = os.environ["PS_DIR"]
    for seed in range(5):
        _, text = play(kanto.BattleStream(), random.Random(seed))
        text += ">p1 move 9\n>chat bye\n>requestlog\n"
        assert norm(kanto.simulate_battle(text)) == norm(node_simulate(ps_dir, text)), f"seed {seed}"
