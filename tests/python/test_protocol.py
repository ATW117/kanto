"""ProtocolBattle (Showdown protocol output), the poke-env bridge and ExternalOpponentPool."""

import json
import random

import numpy as np
import pytest

import pokemon_env

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
        pb = pokemon_env.ProtocolBattle(TEAM, TEAM, seed=seed, names=("Alice", "Bob"))
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
    pb = pokemon_env.ProtocolTeams(pokemon_env.TEAMS_EVAL).battle(3)
    pb.start()
    for p in (0, 1):
        obs, mask = pb.observe(p)
        assert obs.shape == (pokemon_env.OBS_DIM,) and mask.dtype == bool and mask.any()
        for a in np.flatnonzero(mask):
            c = pb.action_to_choice(p, int(a))
            assert pb.choice_to_action(p, c) == a
            assert c in pb.legal_choices(p)
    with pytest.raises(ValueError):
        pb.step("move 9", "move 1")


def test_pokeenv_bridge_selfplay():
    pytest.importorskip("poke_env")
    from agents.external.pokeenv_agent import make

    agents = [make("SimpleHeuristicsPlayer", seed=1), make("MaxBasePowerPlayer", seed=2)]
    T = pokemon_env.ProtocolTeams(pokemon_env.TEAMS_EVAL)
    for g in range(4):
        pb = T.battle(g, names=("Alice", "Bob"))
        out = pb.start()
        for p in range(2):
            agents[p].new_battle(f"battle-gen1ou-{g}", f"p{p + 1}", ["Alice", "Bob"][p])
        while not pb.ended:
            ch = []
            for p in range(2):
                lines = list(out[f"p{p + 1}"])
                if out["requests"][p]:
                    lines.append("|request|" + out["requests"][p])
                agents[p].feed(lines)
                ch.append(agents[p].choose() or "")
            out = pb.step(*ch)
    assert agents[0].decisions > 0 and agents[0].fallbacks == 0 and agents[1].fallbacks == 0


def test_external_opponent_pool():
    pytest.importorskip("poke_env")
    from agents.external.opponents import ExternalOpponentPool

    rng = np.random.default_rng(0)
    with ExternalOpponentPool(3, "random", teams=pokemon_env.TEAMS_EVAL, games_per_env=2) as pool:
        obs, mask = pool.reset()
        while not pool.all_done:
            act = np.array([rng.choice(np.flatnonzero(m)) if m.any() else 0 for m in mask])
            obs, mask, reward, done, info = pool.step(act)
        assert len(pool.results) == 6
        assert {r["seat"] for r in pool.results} == {0, 1}
        assert pool.opp_errors == 0
