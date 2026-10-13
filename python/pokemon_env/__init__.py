"""Fast batched Gen 1 singles environment (pkmn/engine or the native Showdown-exact simulator).

See docs/DESIGN.md for the API contract and docs/OBSERVATION.md for the observation layout.

    env = pokemon_env.VecEnv(num_envs=N, seed=0, teams="data/teams/gen1ou", threads=16)
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

from . import _pokemon_env as _native
from ._pokemon_env import (
    MAX_TURNS,
    MOVE_UNKNOWN,
    NUM_ACTIONS,
    OBS_DIM,
    OBS_VERSION,
    SPECIES_UNKNOWN,
    id_offsets,
    move_names,
    obs_layout,
    species_names,
)

__all__ = [
    "VecEnv",
    "OBS_DIM",
    "OBS_VERSION",
    "NUM_ACTIONS",
    "MAX_TURNS",
    "SPECIES_UNKNOWN",
    "MOVE_UNKNOWN",
    "obs_layout",
    "id_offsets",
    "species_names",
    "move_names",
]

_REPO_ROOT = Path(__file__).resolve().parents[2]
_BUILTIN_SOURCES = {"random", "randbats", "builtin"}


def _resolve_teams(teams: str | os.PathLike[str]) -> str:
    """Team sources: "random", "randbats", "builtin", or a .json/.jsonl file or directory.

    Relative paths are tried against the CWD first, then the repository root.
    """
    s = os.fspath(teams)
    if s in _BUILTIN_SOURCES or os.path.exists(s):
        return s
    alt = _REPO_ROOT / s
    return str(alt) if alt.exists() else s


class VecEnv:
    """N independent two-player Gen 1 battles stepped in parallel (Rust + rayon).

    Args:
        num_envs: number of parallel battles.
        seed: master seed; per-env seeds are derived from it (independent of ``threads``).
        teams: team source (see ``_resolve_teams``).
        threads: worker threads for stepping.
        opponent: ``None`` (caller controls both players), ``"random"`` or ``"max_damage"``
            (player 1 is played in Rust; ``actions[:, 1]`` is ignored and ``obs[:, 1]`` is zero).
        max_turns: games reaching this turn end as truncated ties.
        reveal_hidden: debug only; fill the privileged observation block with the own side's
            hidden counters (sleep/confusion/disable turns left, Substitute HP, Toxic counter).
        backend: battle simulator. ``"pkmn"`` (default): pkmn/engine in Showdown compatibility
            mode (which enforces Sleep and Freeze Clause Mod). ``"native"``: the native Rust
            simulator, matching vanilla Pokémon Showdown 0.11.11 ``[Gen 1] OU`` mechanics
            (Sleep and Freeze Clause Mod; docs/SIM_DESIGN.md). ``"native-nc"``: native
            without the clauses.
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
        backend: str = "pkmn",
    ) -> None:
        self._env = _native.VecEnv(
            num_envs, seed, _resolve_teams(teams), threads, opponent, max_turns, reveal_hidden, backend
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
        (``obs``/``mask`` then already belong to the next game), and ``info`` holds
        ``outcome`` int8[N] (0 ongoing, 1 p1 win, 2 p2 win, 3 tie, 4 truncated),
        ``truncated`` bool[N] and ``turns`` uint32[N] (length of the finished game).
        Raises ``ValueError`` on an illegal action without modifying any env.
        """
        a = np.ascontiguousarray(actions, dtype=np.int16)
        return self._env.step(a)

    def clone_state(self, i: int) -> bytes:
        """Snapshot env ``i`` (battle, reveal memories, RNG) for search."""
        return self._env.clone_state(i)

    def restore_state(self, i: int, blob: bytes) -> None:
        """Restore env ``i``; its rows of the last returned obs/mask buffers are refreshed."""
        self._env.restore_state(i, blob)
