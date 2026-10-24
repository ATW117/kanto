"""kanto.server: unmodified poke-env players battle each other through the local server.

Skipped unless ``poke_env`` and ``websockets`` are installed.
"""

import asyncio
import socket

import pytest

poke_env = pytest.importorskip("poke_env")
pytest.importorskip("websockets")

import websockets  # noqa: E402
from poke_env import AccountConfiguration, MaxBasePowerPlayer, RandomPlayer, ServerConfiguration  # noqa: E402

import kanto  # noqa: E402
from kanto.server import Server  # noqa: E402

# poke-env does not strip the export format's trailing double spaces, so drop them.
TEAM = "\n".join(line.rstrip() for line in kanto.convert_team(
    """Bessie (Tauros)
- Body Slam
- Hyper Beam
- Blizzard
- Earthquake

Chansey
- Soft-Boiled
- Thunder Wave
- Ice Beam
- Seismic Toss

Snorlax
- Body Slam
- Rest
- Earthquake
- Reflect
""",
    "export",
).splitlines())


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("localhost", 0))
        return s.getsockname()[1]


async def run(n, team, mode, tag):
    port = free_port()
    server = Server(kanto.TEAMS_EVAL, validate=True, seed=1)
    async with websockets.serve(server.connection, "localhost", port, max_size=None, ping_interval=None):
        cfg = ServerConfiguration(f"ws://localhost:{port}/showdown/websocket", "")
        p1 = RandomPlayer(
            battle_format="gen1ou",
            server_configuration=cfg,
            team=team,
            account_configuration=AccountConfiguration(f"{tag}rand", None),
            max_concurrent_battles=4,
        )
        p2 = MaxBasePowerPlayer(
            battle_format="gen1ou",
            server_configuration=cfg,
            team=team,
            account_configuration=AccountConfiguration(f"{tag}maxbp", None),
            max_concurrent_battles=4,
        )
        if mode == "ladder":
            await asyncio.wait_for(asyncio.gather(p1.ladder(n), p2.ladder(n)), 120)
        else:
            await asyncio.wait_for(p1.battle_against(p2, n_battles=n), 120)
    return server, p1, p2


@pytest.mark.parametrize("mode,team", [("challenge", None), ("challenge", TEAM), ("ladder", None)], ids=["challenge-pool", "challenge-utm", "ladder-pool"])
def test_poke_env_players_finish_battles(mode, team):
    tag = {"challenge": "c", "ladder": "l"}[mode] + ("t" if team else "p")
    server, p1, p2 = asyncio.run(run(12, team, mode, tag))
    assert p1.n_finished_battles == 12
    assert p1.n_won_battles + p2.n_won_battles + p1.n_tied_battles == 12
    assert server.finished == 12
    # MaxBasePower beats Random most of the time (about 90% with TEAM, which has no Self-Destruct
    # for it to pick).
    assert p2.n_won_battles >= 6
