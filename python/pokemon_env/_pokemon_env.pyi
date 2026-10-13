from typing import Any, Optional

import numpy as np

OBS_DIM: int
OBS_VERSION: int
NUM_ACTIONS: int
MAX_TURNS: int
SPECIES_UNKNOWN: int
MOVE_UNKNOWN: int
OUTCOME_ERROR: int
OPP_ACTION_NONE: int
OPP_ACTION_SWITCH_BASE: int
OPP_ACTION_CLASSES: int

class VecEnv:
    num_envs: int
    obs_dim: int
    num_actions: int
    opponent: Optional[str]
    backend: str
    total_errors: int
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
        validate: bool = True,
    ) -> None: ...
    def reset(self, seed: Optional[int] = None) -> tuple[np.ndarray, np.ndarray]: ...
    def step(
        self, actions: np.ndarray
    ) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray, dict[str, Any]]: ...
    def clone_state(self, i: int) -> bytes: ...
    def restore_state(self, i: int, blob: bytes) -> None: ...
    def _inject_panic(self, i: int) -> None: ...

def obs_layout() -> list[tuple[str, int, int]]: ...
def id_offsets() -> list[tuple[int, str]]: ...
def species_names() -> list[str]: ...
def move_names() -> list[str]: ...
def bench_engine(
    battles: int, threads: int = 1, teams: str = "builtin", seed: int = 0, backend: str = "pkmn"
) -> tuple[float, int, int]: ...
