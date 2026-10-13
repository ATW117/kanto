"""Throughput benchmarks for pokemon_env.

1. Engine only: random self-play straight through libpkmn (no logs/observations/masks).
2. Full env: VecEnv.step with random legal actions for both players, including protocol-log
   parsing, reveal memory, observation encoding into numpy and masks.

Usage: python bench/bench.py [--quick] [--teams PATH]
"""

from __future__ import annotations

import argparse
import os
import platform
import time

import numpy as np

import pokemon_env as pe
from pokemon_env import _pokemon_env as native


def sample_legal(mask: np.ndarray, rng: np.random.Generator) -> np.ndarray:
    scores = rng.random(mask.shape, dtype=np.float32)
    scores[~mask] = -1.0
    return scores.argmax(-1).astype(np.int16)


def bench_engine(teams: str, battles: int, threads: int) -> None:
    secs, updates, turns = native.bench_engine(battles, threads, teams, 0)
    print(
        f"  threads={threads:>2}  {battles / secs:>12,.0f} battles/s  {turns / secs:>14,.0f} turns/s"
        f"  {updates / secs:>14,.0f} updates/s  ({turns / battles:.1f} turns/battle)"
    )


def bench_env(teams: str, n: int, threads: int, seconds: float) -> None:
    env = pe.VecEnv(num_envs=n, seed=0, teams=teams, threads=threads)
    _, mask = env.reset()
    rng = np.random.default_rng(0)
    # Pre-generate action noise so Python-side sampling cost is excluded from the timed loop
    # as far as possible (masking is still applied per step).
    for _ in range(5):  # warm-up
        _, mask, *_ = env.step(sample_legal(mask, rng))
    steps = 0
    env_time = 0.0
    t_end = time.perf_counter() + seconds
    while time.perf_counter() < t_end:
        a = sample_legal(mask, rng)
        t0 = time.perf_counter()
        _, mask, _, done, _ = env.step(a)
        env_time += time.perf_counter() - t0
        steps += 1
    print(
        f"  N={n:>5} threads={threads:>2}  {steps * n / env_time:>12,.0f} env-steps/s"
        f"  ({steps} steps, {env_time / steps * 1e3:.2f} ms/step in env.step)"
    )


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--teams", default="data/teams/gen1ou")
    ap.add_argument("--quick", action="store_true", help="shorter runs")
    args = ap.parse_args()
    seconds = 1.0 if args.quick else 3.0
    battles = 50_000 if args.quick else 200_000

    print(f"pokemon_env OBS_DIM={pe.OBS_DIM} on {platform.machine()} ({os.cpu_count()} CPUs)")
    print(f"teams: {args.teams}")
    print("Engine only (libpkmn, random self-play, no logs):")
    for threads in (1, 4, 16):
        bench_engine(pe._resolve_teams(args.teams), battles, threads)
    print("Full env (VecEnv.step: engine + log parsing + obs encoding + masks, both players):")
    for n in (256, 1024, 4096):
        for threads in (1, 4, 16):
            bench_env(args.teams, n, threads, seconds)


if __name__ == "__main__":
    main()
