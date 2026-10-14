"""ProtocolBattle: Showdown protocol output, views, requests and the choice <-> action mapping."""

import json
import random

import numpy as np
import pytest

import kanto

TEAM = json.dumps(
    {
        "name": "t",
        "pokemon": [
            {"species": "Tauros", "moves": ["Body Slam", "Hyper Beam", "Blizzard", "Earthquake"]},
            {"species": "Chansey", "moves": ["Soft-Boiled", "Thunder Wave", "Ice Beam", "Seismic Toss"]},
            {"species": "Snorlax", "moves": ["Body Slam", "Rest", "Self-Destruct", "Reflect"]},
        ],
    }
)


def play_random(pb, rng):
    out = pb.start()
    outs = [out]
    while not pb.ended:
        c = [rng.choice(pb.legal_choices(p)) for p in range(2)]
        out = pb.step(*c)
        outs.append(out)
    return outs


def test_protocol_battle_plays_and_views_are_private():
    rng = random.Random(0)
    for seed in range(20):
        pb = kanto.ProtocolBattle(TEAM, TEAM, seed=seed, names=("Alice", "Bob"))
        outs = play_random(pb, rng)
        first = outs[0]
        assert first["update"][0].startswith("|t:|")
        assert "|player|p1|Alice||" in first["update"]
        req = json.loads(first["requests"][0])
        assert req["side"]["name"] == "Alice" and req["side"]["id"] == "p1"
        for out in outs:
            for p in (0, 1):
                me, foe = f"p{p + 1}a: ", f"p{2 - p}a: "
                for line in out[f"p{p + 1}"]:
                    parts = line.split("|")
                    if len(parts) > 3 and parts[1] in ("-damage", "-heal") and parts[3] != "0 fnt":
                        hp = parts[3].split(" ")[0]
                        if parts[2].startswith(foe):
                            assert hp.endswith("/100"), line  # foe HP only as a percentage
                assert not any(l.startswith("|split|") for l in out[f"p{p + 1}"])
        last = outs[-1]
        assert last["requests"] == (None, None)
        assert last["update"][-1] in ("|tie",) or last["update"][-1].startswith("|win|")
        assert pb.outcome in (1, 2, 3)


def test_choices_and_actions_roundtrip():
    pb = kanto.ProtocolTeams(kanto.TEAMS_EVAL).battle(3)
    pb.start()
    for p in (0, 1):
        obs, mask = pb.observe(p)
        assert obs.shape == (kanto.OBS_DIM,) and mask.dtype == bool and mask.any()
        for a in np.flatnonzero(mask):
            c = pb.action_to_choice(p, int(a))
            assert pb.choice_to_action(p, c) == a
            assert c in pb.legal_choices(p)
    with pytest.raises(ValueError):
        pb.step("move 9", "move 1")

