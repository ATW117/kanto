"""kanto: a Showdown-exact Gen 1 battle simulator with a fast batched environment.

See docs/DESIGN.md for the API contract and docs/OBSERVATION.md for the observation layout.

    env = kanto.VecEnv(num_envs=N, seed=0, teams="data/teams/gen1ou", threads=16)
    obs, mask = env.reset()                    # float32[N, 2, OBS_DIM], bool[N, 2, NUM_ACTIONS]
    obs, mask, reward, done, info = env.step(actions)   # actions: int16[N, 2]

Output arrays are preallocated and overwritten in place by every ``reset``/``step`` (zero-copy):
copy them if you need to keep values across steps.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any, Optional

import numpy as np

from . import _kanto as _native
from ._kanto import (
    BACKENDS,
    MAX_TURNS,
    BattleStream,
    ClientBattle,
    MOVE_UNKNOWN,
    NUM_ACTIONS,
    OBS_DIM,
    OBS_VERSION,
    OPP_ACTION_CLASSES,
    OPP_ACTION_NONE,
    OPP_ACTION_SWITCH_BASE,
    OUTCOME_ERROR,
    SPECIES_UNKNOWN,
    ProtocolBattle,
    convert_team,
    extract_channel,
    id_offsets,
    simulate_battle,
    move_names,
    obs_layout,
    species_names,
    validate_team,
)

__all__ = [
    "BACKENDS",
    "VecEnv",
    "ProtocolBattle",
    "ProtocolTeams",
    "BattleStream",
    "simulate_battle",
    "extract_channel",
    "ClientBattle",
    "OBS_DIM",
    "OBS_VERSION",
    "NUM_ACTIONS",
    "MAX_TURNS",
    "SPECIES_UNKNOWN",
    "MOVE_UNKNOWN",
    "OUTCOME_ERROR",
    "OPP_ACTION_NONE",
    "OPP_ACTION_SWITCH_BASE",
    "OPP_ACTION_CLASSES",
    "TEAMS_TRAIN",
    "TEAMS_EVAL",
    "obs_layout",
    "convert_team",
    "validate_team",
    "id_offsets",
    "species_names",
    "move_names",
    "replay_html",
]

_REPO_ROOT = Path(__file__).resolve().parents[2]
_BUILTIN_SOURCES = {"random", "randbats", "builtin"}
_SPLITS = ("train", "eval")

#: The gen1ou pool's deterministic 90/10 split (data/teams/gen1ou/split.py): train on
#: ``TEAMS_TRAIN``, report held-out results on ``TEAMS_EVAL``.
TEAMS_TRAIN = "data/teams/gen1ou:train"
TEAMS_EVAL = "data/teams/gen1ou:eval"


def _resolve_teams(teams: str | os.PathLike[str]) -> str:
    """Team sources: "random", "randbats", "builtin", a .json/.jsonl file or directory, or
    ``"<pool dir>:train"`` / ``"<pool dir>:eval"`` (the pool's ``split/{train,eval}.jsonl``).

    Relative paths are tried against the CWD first, then the repository root.
    """
    s = os.fspath(teams)
    if s in _BUILTIN_SOURCES:
        return s
    base, sep, split = s.rpartition(":")
    if sep and split in _SPLITS and base:
        return _resolve_teams(base) + ":" + split
    if os.path.exists(s):
        return s
    alt = _REPO_ROOT / s
    if alt.exists():
        return str(alt)
    # Installed from a wheel (no checkout): the gen1ou pool is embedded in the extension.
    if s.rstrip("/") == "data/teams/gen1ou":
        return "gen1ou"
    return s


class VecEnv:
    """N independent two-player Gen 1 battles stepped in parallel (Rust + rayon).

    Args:
        num_envs: number of parallel battles.
        seed: master seed; per-env seeds are derived from it (independent of ``threads``).
        teams: team source (see ``_resolve_teams``).
        threads: worker threads for stepping.
        opponent: ``None`` (caller controls both players), ``"random"``, ``"max_damage"`` or ``"lookahead"``
            (player 1 is played in Rust; ``actions[:, 1]`` is ignored and ``obs[:, 1]`` and
            ``mask[:, 1]`` are all zero/False).
        max_turns: games reaching this turn end as truncated ties.
        reveal_hidden: debug only; fill the privileged observation block with the own side's
            hidden counters (sleep/confusion/disable turns left, Substitute HP, Toxic counter).
        backend: battle simulator. ``"native"`` (default): kanto's Rust simulator, matching
            vanilla Pokémon Showdown 0.11.11 ``[Gen 1] OU`` exactly (Sleep and Freeze Clause
            Mod; docs/SIM_DESIGN.md). ``"native-nc"``: the same without the clauses.
            ``"pkmn"``: pkmn/engine in Showdown compatibility mode, only in builds with the
            ``pkmn`` feature (``BACKENDS`` lists what this build has).
        validate: check pool teams for Gen 1 legality: Species Clause (no duplicate species)
            and learnsets. ``False`` allows such teams. Unknown names, out-of-range DVs/levels
            and malformed teams are always errors.
    """

    def __init__(
        self,
        num_envs: int,
        seed: int = 0,
        teams: str | os.PathLike[str] = "data/teams/gen1ou",
        threads: int = 16,
        opponent: Optional[str] = None,
        max_turns: int = MAX_TURNS,
        reveal_hidden: bool = False,
        backend: str = "native",
        validate: bool = True,
    ) -> None:
        self._env = _native.VecEnv(
            num_envs, seed, _resolve_teams(teams), threads, opponent, max_turns, reveal_hidden, backend, validate
        )
        self.backend = backend
        self.num_envs = num_envs
        self.obs_dim = OBS_DIM
        self.num_actions = NUM_ACTIONS
        self.opponent = opponent

    def reset(self, seed: Optional[int] = None) -> tuple[np.ndarray, np.ndarray]:
        """Start new games everywhere; ``seed`` re-derives all per-env RNG streams."""
        return self._env.reset(seed)

    def step(
        self, actions: np.ndarray
    ) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray, dict[str, Any]]:
        """Play one update in every env (auto-resetting finished games).

        Returns ``(obs, mask, reward, done, info)``: ``reward`` is float32[N, 2] (+1/-1/0 at game
        end for player 1, negated for player 2), ``done`` marks the transition that ended a game
        (``obs``/``mask`` then already belong to the next game), and ``info`` holds:

        - ``outcome`` int8[N]: 0 ongoing, 1 p1 win, 2 p2 win, 3 tie, 4 truncated (turn limit),
          5 error (``OUTCOME_ERROR``, see below).
        - ``truncated`` bool[N]: the game ended without a result (turn limit or error).
        - ``turns`` uint32[N]: length of the finished game.
        - ``opp_action_obs_frame`` int16[N, 2]: entry ``[i, p]`` is what player ``1 - p`` chose
          this step, in player ``p``'s frame (an auxiliary-prediction target): the move id
          (1..165) for a move, 0 for a move choice that names no move (recharge, the Gen 1
          "Fight" option while asleep/frozen/trapped, locked-in turns), ``166 + j``
          (``OPP_ACTION_SWITCH_BASE + j``) for a switch to the opponent Pokémon in
          appearance-order slot ``j`` (the ``opp_mon`` block ``j`` of ``p``'s observation), and
          -1 (``OPP_ACTION_NONE``) when the opponent had no decision. ``OPP_ACTION_CLASSES`` = 172.
        - ``error`` bool[N] and ``errors`` (list of ``(env index, message)``): the simulator
          panicked in that env. Its game ends with outcome 5 (``done`` and ``truncated`` set,
          reward 0) and the env is reset; other envs are unaffected.

        Raises ``ValueError`` on an illegal action without modifying any env.
        """
        a = np.ascontiguousarray(actions, dtype=np.int16)
        return self._env.step(a)

    @property
    def total_errors(self) -> int:
        """Simulator panics caught (and turned into error outcomes) since construction."""
        return self._env.total_errors

    def clone_state(self, i: int) -> bytes:
        """Snapshot env ``i`` (battle, reveal memories, RNG) for search."""
        return self._env.clone_state(i)

    def restore_state(self, i: int, blob: bytes) -> None:
        """Restore env ``i``; its rows of the last returned obs/mask buffers are refreshed."""
        self._env.restore_state(i, blob)

    def builtin_actions(self, kind: str, seats: np.ndarray) -> np.ndarray:
        """int16[N]: built-in policy ``kind`` ("random" | "max_damage" | "lookahead")'s legal
        action for player ``seats[i]`` (0 or 1) in each env ``i`` (uses the envs' own RNGs).
        Lets a built-in bot play either seat, e.g. bot-vs-bot or seat-swapped evaluation."""
        return self._env.builtin_actions(kind, np.ascontiguousarray(seats, dtype=np.uint8))

    def opp_team_targets(self) -> tuple[np.ndarray, np.ndarray]:
        """Privileged training labels for hidden-information auxiliary heads (never part of the
        observation): ``(species uint8[N, 2, 6], moves uint8[N, 2, 6, 4])``. Row ``[i, p]`` is
        the opponent's true team as seen from player ``p``: its appearance-order slots first
        (the ``opp_mon`` blocks of ``p``'s observation), then its unrevealed Pokémon in party
        order; true species ids and original move ids, 0 = empty."""
        return self._env.opp_team_targets()


def ProtocolTeams(teams: str | os.PathLike[str] = "data/teams/gen1ou", validate: bool = True) -> "_native.ProtocolTeams":
    """A team source that deals :class:`ProtocolBattle` s (``.battle(seed, names=..., format=...)``),
    sampling both teams and the battle seed from ``seed`` the way ``VecEnv`` does. ``teams`` takes
    the same sources as ``VecEnv`` (e.g. ``TEAMS_EVAL``)."""
    return _native.ProtocolTeams(_resolve_teams(teams), validate)


def replay_html(battle: "ProtocolBattle", title: Optional[str] = None) -> str:
    """A finished (or running) battle as a Showdown replay page: the spectator log
    (``battle.view_log(0)``) in the HTML format of Showdown's "download replay", which loads
    play.pokemonshowdown.com's replay player when opened in a browser."""
    import html

    log = "\n".join(battle.view_log(0))
    players = [l.split("|")[3] for l in battle.view_log(0) if l.startswith("|player|")][:2]
    vs = " vs. ".join(players) if players else "kanto battle"
    title = title or f"[Gen 1] OU replay: {vs}"
    return (
        "<!DOCTYPE html>\n<meta charset=\"utf-8\" />\n<!-- version 1 -->\n"
        f"<title>{html.escape(title)}</title>\n"
        "<div class=\"wrapper replay-wrapper\" style=\"max-width:1180px;margin:0 auto\">\n"
        "<div class=\"battle\"></div><div class=\"battle-log\"></div>"
        "<div class=\"replay-controls\"></div><div class=\"replay-controls-2\"></div>\n"
        f"<h1 style=\"font-weight:normal;text-align:center\">{html.escape(vs)}</h1>\n"
        "<script type=\"text/plain\" class=\"battle-log-data\">"
        + log.replace("</", "<\\/")
        + "</script>\n</div>\n"
        "<script src=\"https://play.pokemonshowdown.com/js/replay-embed.js\"></script>\n"
    )
