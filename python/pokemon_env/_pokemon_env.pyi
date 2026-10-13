from typing import Any, Optional

import numpy as np

OBS_DIM: int
OBS_VERSION: int
NUM_ACTIONS: int
MAX_TURNS: int
SPECIES_UNKNOWN: int
MOVE_UNKNOWN: int

class VecEnv:
    num_envs: int
    obs_dim: int
    num_actions: int
    opponent: Optional[str]
    backend: str
    def __init__(
        self,
        num_envs: int,
        seed: int = 0,
        teams: str = "data/teams/gen1ou",
        threads: int = 16,
        opponent: Optional[str] = None,
        max_turns: int = 1000,
        reveal_hidden: bool = False,
        backend: str = "pkmn",
    ) -> None: ...
    def reset(self, seed: Optional[int] = None) -> tuple[np.ndarray, np.ndarray]: ...
    def step(
        self, actions: np.ndarray
    ) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray, dict[str, Any]]: ...
    def clone_state(self, i: int) -> bytes: ...
    def restore_state(self, i: int, blob: bytes) -> None: ...

def obs_layout() -> list[tuple[str, int, int]]: ...
def id_offsets() -> list[tuple[int, str]]: ...
def species_names() -> list[str]: ...
def move_names() -> list[str]: ...
