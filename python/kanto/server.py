"""A local Pokémon Showdown *server* backed by kanto: unmodified Showdown clients (poke-env, and
the agents built on it) play Gen 1 battles simulated by ``kanto.ProtocolBattle`` over a
websocket, with no Node server.

    python -m kanto.server --port 8000 --teams data/teams/gen1ou:eval

    # poke-env side
    from poke_env import ServerConfiguration
    cfg = ServerConfiguration("ws://localhost:8000/showdown/websocket", "")
    player = RandomPlayer(battle_format="gen1ou", server_configuration=cfg, ...)

Needs ``websockets`` (``pip install kanto[server]``). This is a development server for local
self-play and evaluation, not a ladder: there is no authentication (any name is accepted), no
rating, no chat, and the timer is acknowledged but not enforced.

Implemented: ``|challstr|`` + ``/trn`` (no password check), ``|updateuser|``, ``/utm`` (packed
team, or ``null`` for a team from the server's pool), ``/challenge``, ``/accept``, ``/reject``,
``/cancelchallenge``, ``/search``, ``/cancelsearch``, ``|updatechallenges|``,
``|updatesearch|``, battle rooms ``>battle-<format>-N`` with ``|init|battle``, ``|title|``,
``|j|``, ``|request|`` with ``rqid``, ``/choose`` (move by slot, id or name; switch by slot,
nickname or species; ``pass``; ``default``; an optional ``|rqid``), ``|error|[Invalid choice]``,
``/forfeit``, ``/timer`` and ``/leave`` (``|deinit``). Teams sent with ``/utm`` are checked with
``kanto.validate_team`` for ``gen1ou`` unless the server runs with ``--no-validate``.

Formats: ``gen1ou`` and ``gen1nc`` (OU without Sleep/Freeze Clause Mod). The battle messages are
kanto's, i.e. byte-identical to pokemon-showdown's for the same seed and choices.
"""

from __future__ import annotations

import argparse
import asyncio
import itertools
import json
import logging
import random
import re
import time
from dataclasses import dataclass, field
from typing import Dict, List, Optional

try:
    import websockets
except ImportError as e:  # pragma: no cover
    raise ImportError("kanto.server needs the 'websockets' package: pip install kanto[server]") from e

import kanto

log = logging.getLogger("kanto.server")

FORMATS = {"gen1ou": "[Gen 1] OU", "gen1nc": "[Gen 1] OU"}


def to_id(s: str) -> str:
    return re.sub(r"[^a-z0-9]", "", s.lower())


@dataclass
class User:
    ws: object
    name: str = ""
    userid: str = ""
    team: Optional[str] = None  # packed team, or None for the server's pool
    searching: Optional[str] = None
    battles: set = field(default_factory=set)

    async def send(self, msg: str) -> None:
        try:
            await self.ws.send(msg)
        except websockets.ConnectionClosed:
            pass


@dataclass
class Challenge:
    frm: User
    to: User
    fmt: str
    team: Optional[str]


class Room:
    def __init__(self, server: "Server", roomid: str, fmt: str, users: List[User], teams: List[str], seed: int):
        self.server, self.roomid, self.fmt, self.users = server, roomid, fmt, users
        self.pb = kanto.ProtocolBattle(
            teams[0], teams[1], seed=seed, names=(users[0].name, users[1].name), format=fmt, validate=False
        )
        self.pb.set_timestamp(int(time.time()))
        self.rqid = 0
        self.requests: List[Optional[dict]] = [None, None]
        self.choices: List[Optional[str]] = [None, None]
        self.over = False

    async def send(self, i: int, lines: List[str]) -> None:
        if lines:
            await self.users[i].send(f">{self.roomid}\n" + "\n".join(lines))

    async def start(self) -> None:
        a, b = self.users[0].name, self.users[1].name
        for i in (0, 1):
            await self.send(i, ["|init|battle", f"|title|{a} vs. {b}", f"|j|☆{a}", f"|j|☆{b}"])
        await self.emit(self.pb.start())

    async def emit(self, out: dict) -> None:
        reqs = out["requests"]
        self.rqid += 1
        for i in (0, 1):
            self.choices[i] = None
            if reqs[i] is None:
                self.requests[i] = None
                continue
            r = json.loads(reqs[i])
            r["rqid"] = self.rqid
            self.requests[i] = r
            # Showdown sends a request before the update it follows.
            await self.send(i, ["|request|" + json.dumps(r, ensure_ascii=False)])
            if r.get("wait"):
                self.choices[i] = ""
        for i in (0, 1):
            await self.send(i, list(out[f"p{i + 1}"]))
        if self.pb.ended:
            self.over = True
            self.server.finished += 1

    def translate(self, i: int, choice: str) -> str:
        """A Showdown choice (``move thunderbolt``, ``switch Chansey``, ``move 2``, ``default``)
        -> kanto's positional ``move N`` / ``switch N`` / ``""``."""
        r = self.requests[i]
        legal = self.pb.legal_choices(i)
        c = choice.strip()
        if c in ("pass", ""):
            return ""
        if c == "default":
            return "move 1" if "move 1" in legal else legal[0]
        kind, _, arg = c.partition(" ")
        arg = re.sub(r"\s+(mega|zmove|dynamax|max|terastallize)$", "", arg.strip())
        if kind == "move":
            if arg.isdigit():
                return f"move {arg}"
            moves = r["active"][0]["moves"] if r and r.get("active") else []
            for k, m in enumerate(moves, 1):
                if to_id(m.get("id", "")) == to_id(arg) or to_id(m["move"]) == to_id(arg):
                    return f"move {k}"
            if len(moves) == 1:  # Gen 1 locked/recharge/"Fight" requests have one entry
                return "move 1"
            raise ValueError(f"no move '{arg}'")
        if kind == "switch":
            if arg.isdigit():
                return f"switch {arg}"
            side = r["side"]["pokemon"] if r else []
            for k, p in enumerate(side, 1):
                nick = p["ident"].split(": ", 1)[1]
                species = p["details"].split(",")[0]
                if to_id(arg) in (to_id(nick), to_id(species)):
                    return f"switch {k}"
            raise ValueError(f"no Pokémon '{arg}'")
        raise ValueError(f"unrecognized choice '{c}'")

    async def choose(self, user: User, text: str) -> None:
        if self.over:
            return
        i = self.users.index(user)
        choice, _, rqid = text.partition("|")
        if rqid.strip().isdigit() and int(rqid) != self.rqid:
            return  # a stale choice, which Showdown ignores
        if self.requests[i] is None or self.choices[i] is not None:
            await self.send(i, ["|error|[Invalid choice] There's nothing to choose"])
            return
        try:
            c = self.translate(i, choice)
            if c not in self.pb.legal_choices(i):
                raise ValueError(f"'{c}' is not legal now")
        except (ValueError, KeyError, IndexError) as e:
            await self.send(i, [f"|error|[Invalid choice] Can't {choice}: {e}"])
            return
        self.choices[i] = c
        if self.choices[0] is not None and self.choices[1] is not None:
            self.pb.set_timestamp(int(time.time()))
            await self.emit(self.pb.step(self.choices[0], self.choices[1]))

    async def forfeit(self, user: User) -> None:
        if self.over:
            return
        i = self.users.index(user)
        lines = ["|", f"|-message|{user.name} forfeited.", "|", f"|win|{self.users[1 - i].name}"]
        for j in (0, 1):
            await self.send(j, lines)
        self.over = True
        self.server.finished += 1


class Server:
    """Users, challenges, the ladder queue and battle rooms. ``teams``: any kanto team source
    (``"data/teams/gen1ou:eval"``, ``"randbats"``, a file of JSON or Showdown teams, ...) for
    players that send ``/utm null``."""

    def __init__(self, teams: str = kanto.TEAMS_EVAL, validate: bool = True, seed: int = 0):
        self.pool = kanto.ProtocolTeams(teams)
        self.validate = validate
        self.rng = random.Random(seed)
        self.users: Dict[str, User] = {}
        self.rooms: Dict[str, Room] = {}
        self.challenges: List[Challenge] = []
        self.search_queue: Dict[str, List[tuple]] = {}
        self.counter = itertools.count(1)
        self.finished = 0

    async def updatechallenges(self, u: User) -> None:
        frm = {c.frm.userid: c.fmt for c in self.challenges if c.to is u}
        to = next(({"to": c.to.userid, "format": c.fmt} for c in self.challenges if c.frm is u), None)
        await u.send("|updatechallenges|" + json.dumps({"challengesFrom": frm, "challengeTo": to}))

    async def updatesearch(self, u: User) -> None:
        games = {
            r: f"{FORMATS[self.rooms[r].fmt]} {self.rooms[r].users[0].name} vs. {self.rooms[r].users[1].name}"
            for r in u.battles
            if r in self.rooms
        } or None
        await u.send("|updatesearch|" + json.dumps({"searching": [u.searching] if u.searching else [], "games": games}))

    async def start_battle(self, fmt: str, a: User, b: User, ta: Optional[str], tb: Optional[str]) -> None:
        roomid = f"battle-{fmt}-{next(self.counter)}"
        pool = self.pool.battle(seed=self.rng.getrandbits(63))
        teams = [ta or pool.packed_team(0), tb or pool.packed_team(1)]
        try:
            room = Room(self, roomid, fmt, [a, b], teams, self.rng.getrandbits(63))
        except ValueError as e:
            for u in (a, b):
                await u.send(f"|popup|Your team was rejected: {e}")
            return
        self.rooms[roomid] = room
        for u in (a, b):
            u.battles.add(roomid)
            await self.updatesearch(u)
        await room.start()

    async def handle(self, u: User, raw: str) -> None:
        for line in raw.split("\n"):
            room, _, text = line.partition("|")
            if text.startswith("/"):
                cmd, _, arg = text[1:].partition(" ")
                await self.command(u, room, cmd.lower(), arg)

    async def command(self, u: User, room: str, cmd: str, arg: str) -> None:
        if cmd == "trn":
            name = arg.split(",")[0].strip()
            u.name, u.userid = name, to_id(name)
            self.users[u.userid] = u
            await u.send(f"|updateuser| {name}|1|1|" + json.dumps({"blockChallenges": False}))
            await self.updatesearch(u)
        elif cmd == "utm":
            team = arg.strip()
            if team in ("", "null"):
                u.team = None
                return
            try:
                problems = kanto.validate_team(team, "gen1ou") if self.validate else []
                kanto.convert_team(team, "packed", validate=False)
            except ValueError as e:
                problems = [str(e)]
            if problems:
                u.team = None
                await u.send("|popup|Your team was rejected for the following reasons:||||- " + "||- ".join(problems))
            else:
                u.team = team
        elif cmd == "challenge":
            target, _, fmt = arg.partition(",")
            fmt = to_id(fmt) or "gen1ou"
            t = self.users.get(to_id(target))
            if t is None or fmt not in FORMATS:
                await u.send(f"|popup|Cannot challenge {target} to {fmt}.")
                return
            self.challenges.append(Challenge(u, t, fmt, u.team))
            await self.updatechallenges(u)
            await self.updatechallenges(t)
        elif cmd == "accept":
            c = next((c for c in self.challenges if c.to is u and c.frm.userid == to_id(arg)), None)
            if c is None:
                await u.send(f"|popup|No challenge from {arg}.")
                return
            self.challenges.remove(c)
            await self.updatechallenges(u)
            await self.updatechallenges(c.frm)
            await self.start_battle(c.fmt, c.frm, u, c.team, u.team)
        elif cmd in ("reject", "cancelchallenge"):
            self.challenges = [
                c
                for c in self.challenges
                if not ((c.to is u and c.frm.userid == to_id(arg)) or (c.frm is u and c.to.userid == to_id(arg)))
            ]
            await self.updatechallenges(u)
        elif cmd == "search":
            fmt = to_id(arg) or "gen1ou"
            if fmt not in FORMATS:
                await u.send(f"|popup|Unknown format {fmt}.")
                return
            q = self.search_queue.setdefault(fmt, [])
            other = next((e for e in q if e[0] is not u), None)
            if other:
                q.remove(other)
                other[0].searching = None
                await self.start_battle(fmt, other[0], u, other[1], u.team)
            else:
                q.append((u, u.team))
                u.searching = fmt
                await self.updatesearch(u)
        elif cmd == "cancelsearch":
            for q in self.search_queue.values():
                q[:] = [e for e in q if e[0] is not u]
            u.searching = None
            await self.updatesearch(u)
        elif cmd in ("choose", "choice"):
            r = self.rooms.get(room)
            if r and u in r.users:
                await r.choose(u, arg)
        elif cmd == "forfeit":
            r = self.rooms.get(room)
            if r and u in r.users:
                await r.forfeit(u)
        elif cmd == "timer":
            r = self.rooms.get(room)
            if r and u in r.users:
                state = "ON" if arg.strip() in ("on", "start") else "OFF"
                await r.send(r.users.index(u), [f"|inactive|Battle timer is {state} (not enforced by kanto.server)."])
        elif cmd == "leave":
            rid = arg.strip() or room
            r = self.rooms.get(rid)
            if r and u in r.users:
                if not r.over:
                    await r.forfeit(u)
                await u.send(f">{rid}\n|deinit")
                u.battles.discard(rid)
                if all(rid not in x.battles for x in r.users):
                    del self.rooms[rid]
                await self.updatesearch(u)
        else:
            log.debug("ignored command /%s %s", cmd, arg)

    async def connection(self, ws) -> None:
        u = User(ws)
        await u.send("|challstr|4|" + "%032x" % self.rng.getrandbits(128))
        try:
            async for msg in ws:
                await self.handle(u, msg)
        except websockets.ConnectionClosed:
            pass
        finally:
            if self.users.get(u.userid) is u:
                del self.users[u.userid]
            self.challenges = [c for c in self.challenges if c.frm is not u and c.to is not u]
            for q in self.search_queue.values():
                q[:] = [e for e in q if e[0] is not u]
            for rid in list(u.battles):
                r = self.rooms.get(rid)
                if r and not r.over:
                    await r.forfeit(u)


async def serve(port: int = 8000, teams: str = kanto.TEAMS_EVAL, validate: bool = True, host: str = "localhost", seed: int = 0) -> None:
    """Run the server until cancelled."""
    server = Server(teams, validate, seed)
    async with websockets.serve(server.connection, host, port, max_size=None, ping_interval=None):
        log.info("kanto.server on ws://%s:%d/showdown/websocket", host, port)
        await asyncio.Future()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", type=int, default=8000)
    ap.add_argument("--teams", default=kanto.TEAMS_EVAL, help="team source for players that send /utm null")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--no-validate", action="store_true", help="accept /utm teams without the gen1ou checks")
    a = ap.parse_args()
    logging.basicConfig(level=logging.INFO)
    asyncio.run(serve(a.port, a.teams, not a.no_validate, a.host, a.seed))


if __name__ == "__main__":
    main()
