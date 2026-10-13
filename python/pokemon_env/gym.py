"""Optional single-agent Gymnasium wrapper: the agent is player 1 (index 0) against a built-in
Rust opponent. Requires ``gymnasium``."""

from __future__ import annotations

from typing import Any, Optional

import gymnasium as gym
import numpy as np

from . import MAX_TURNS, NUM_ACTIONS, OBS_DIM, VecEnv


class PokemonGen1Env(gym.Env):
    """Single battle vs ``opponent`` ("random" or "max_damage").

    Observation: ``Dict(obs=float32[OBS_DIM], action_mask=int8[NUM_ACTIONS])``.
    Actions: ``Discrete(10)`` — 4 move slots + 6 switch slots (``4 + k`` switches to team slot k).
    Illegal actions raise ``ValueError``; use ``action_mask``.
    """

    metadata = {"render_modes": []}

    def __init__(
        self,
        opponent: str = "max_damage",
        teams: str = "data/teams/gen1ou",
        max_turns: int = MAX_TURNS,
        seed: int = 0,
    ) -> None:
        self._env = VecEnv(1, seed=seed, teams=teams, threads=1, opponent=opponent, max_turns=max_turns)
        self.observation_space = gym.spaces.Dict(
            {
                "obs": gym.spaces.Box(-np.inf, np.inf, shape=(OBS_DIM,), dtype=np.float32),
                "action_mask": gym.spaces.Box(0, 1, shape=(NUM_ACTIONS,), dtype=np.int8),
            }
        )
        self.action_space = gym.spaces.Discrete(NUM_ACTIONS)
        self._actions = np.zeros((1, 2), dtype=np.int16)

    @staticmethod
    def _wrap(obs: np.ndarray, mask: np.ndarray) -> dict[str, np.ndarray]:
        return {"obs": obs[0, 0].copy(), "action_mask": mask[0, 0].astype(np.int8)}

    def reset(self, *, seed: Optional[int] = None, options: Optional[dict] = None):
        super().reset(seed=seed)
        obs, mask = self._env.reset(seed)
        return self._wrap(obs, mask), {}

    def step(self, action: int):
        self._actions[0, 0] = action
        obs, mask, reward, done, info = self._env.step(self._actions)
        truncated = bool(info["truncated"][0])
        terminated = bool(done[0]) and not truncated
        out_info: dict[str, Any] = {"outcome": int(info["outcome"][0]), "turns": int(info["turns"][0])}
        # On done, obs already belongs to the next (auto-reset) game; Gymnasium callers reset
        # anyway, which starts yet another game. The terminal observation is not exposed.
        return self._wrap(obs, mask), float(reward[0, 0]), terminated, truncated, out_info
