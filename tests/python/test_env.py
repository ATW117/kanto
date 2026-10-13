import numpy as np
import pytest

import pokemon_env as pe


def sample_legal(mask: np.ndarray, rng: np.random.Generator) -> np.ndarray:
    """Uniform legal action per (env, player) from bool[N, 2, A] masks."""
    scores = rng.random(mask.shape, dtype=np.float32)
    scores[~mask] = -1.0
    return scores.argmax(-1).astype(np.int16)


def test_constants_and_layout():
    assert pe.NUM_ACTIONS == 10
    assert pe.OBS_VERSION == 1
    layout = pe.obs_layout()
    assert layout[0][1] == 0
    end = layout[-1][1] + layout[-1][2]
    assert end == pe.OBS_DIM
    # Blocks tile the vector exactly.
    for (_, off, size), (_, nxt, _) in zip(layout, layout[1:]):
        assert off + size == nxt
    assert len(pe.species_names()) == 152 and len(pe.move_names()) == 166


def test_shapes_and_dtypes():
    n = 8
    env = pe.VecEnv(num_envs=n, seed=0, teams="data/teams/gen1ou", threads=2)
    obs, mask = env.reset()
    assert obs.shape == (n, 2, pe.OBS_DIM) and obs.dtype == np.float32
    assert mask.shape == (n, 2, pe.NUM_ACTIONS) and mask.dtype == np.bool_
    assert mask.any(-1).all()
    rng = np.random.default_rng(0)
    obs, mask, reward, done, info = env.step(sample_legal(mask, rng))
    assert reward.shape == (n, 2) and reward.dtype == np.float32
    assert done.shape == (n,) and done.dtype == np.bool_
    assert info["outcome"].dtype == np.int8 and info["outcome"].shape == (n,)
    assert info["truncated"].dtype == np.bool_
    assert info["turns"].shape == (n,)
    assert np.isfinite(obs).all()


def test_illegal_action_raises():
    env = pe.VecEnv(num_envs=2, seed=1, teams="builtin", threads=1)
    _, mask = env.reset()
    actions = np.zeros((2, 2), dtype=np.int16)
    illegal = int(np.flatnonzero(~mask[1, 0])[0])
    actions[1, 0] = illegal
    with pytest.raises(ValueError):
        env.step(actions)
    with pytest.raises(ValueError):
        env.step(np.zeros((3, 2), dtype=np.int16))


def test_seeding_is_deterministic():
    def run(seed, threads):
        env = pe.VecEnv(num_envs=32, seed=seed, teams="randbats", threads=threads)
        _, mask = env.reset()
        rng = np.random.default_rng(123)
        acc = []
        for _ in range(100):
            obs, mask, reward, done, _ = env.step(sample_legal(mask, rng))
            acc.append(obs.copy())
        return np.stack(acc)

    a = run(5, 1)
    assert np.array_equal(a, run(5, 4))
    assert not np.array_equal(a, run(6, 4))


def test_clone_restore():
    env = pe.VecEnv(num_envs=4, seed=3, teams="builtin", threads=1)
    _, mask = env.reset()
    rng = np.random.default_rng(1)
    for _ in range(10):
        _, mask, *_ = env.step(sample_legal(mask, rng))
    blob = env.clone_state(2)
    obs_before = env.step(sample_legal(mask, rng))[0][2].copy()
    env.restore_state(2, blob)
    assert env.clone_state(2) == blob
    with pytest.raises(ValueError):
        env.restore_state(0, b"junk")
    with pytest.raises(IndexError):
        env.clone_state(99)
    assert obs_before.shape == (2, pe.OBS_DIM)


def test_random_rollout_10k_games():
    n = 1024
    env = pe.VecEnv(num_envs=n, seed=0, teams="data/teams/gen1ou", threads=8)
    _, mask = env.reset()
    rng = np.random.default_rng(0)
    games = 0
    reward_sum = 0.0
    wins = losses = 0
    while games < 10_000:
        _, mask, reward, done, info = env.step(sample_legal(mask, rng))
        # Zero-sum, and rewards only at game end.
        assert np.array_equal(reward[:, 0], -reward[:, 1])
        assert not reward[~done].any()
        oc = info["outcome"]
        assert ((oc != 0) == done).all()
        assert (info["truncated"] == (oc == 4)).all()
        assert (info["turns"][done] > 0).all() and (info["turns"][done] <= pe.MAX_TURNS).all()
        games += int(done.sum())
        reward_sum += float(reward[:, 0].sum())
        wins += int((oc == 1).sum())
        losses += int((oc == 2).sum())
        assert mask.any(-1).all()
    assert reward_sum == wins - losses
    assert wins > 0 and losses > 0


@pytest.mark.parametrize("opponent", ["random", "max_damage"])
def test_builtin_opponent(opponent):
    n = 64
    env = pe.VecEnv(num_envs=n, seed=0, teams="builtin", threads=2, opponent=opponent)
    obs, mask = env.reset()
    assert not obs[:, 1].any()
    rng = np.random.default_rng(0)
    actions = np.zeros((n, 2), dtype=np.int16)
    for _ in range(200):
        actions[:, 0] = sample_legal(mask, rng)[:, 0]
        actions[:, 1] = 99  # ignored with a built-in opponent
        obs, mask, reward, done, info = env.step(actions)
        assert not obs[:, 1].any()


def test_gymnasium_wrapper():
    gym = pytest.importorskip("gymnasium")
    from pokemon_env.gym import PokemonGen1Env

    env = PokemonGen1Env(opponent="max_damage", teams="builtin")
    obs, _ = env.reset(seed=0)
    assert env.observation_space.contains(obs)
    rng = np.random.default_rng(0)
    finished = 0
    for _ in range(3000):
        legal = np.flatnonzero(obs["action_mask"])
        obs, r, term, trunc, info = env.step(int(rng.choice(legal)))
        if term or trunc:
            finished += 1
            assert r in (-1.0, 0.0, 1.0)
            obs, _ = env.reset()
    assert finished > 0


# ---- Native (Showdown-exact) backend ---------------------------------------------------------


@pytest.mark.parametrize("backend", ["native", "native-nc"])
def test_native_backend_rollout(backend):
    n = 64
    env = pe.VecEnv(num_envs=n, seed=0, teams="data/teams/gen1ou", threads=2, backend=backend)
    assert env.backend == backend
    obs, mask = env.reset()
    assert obs.shape == (n, 2, pe.OBS_DIM) and mask.any(-1).all()
    rng = np.random.default_rng(0)
    games = 0
    for _ in range(400):
        obs, mask, reward, done, info = env.step(sample_legal(mask, rng))
        assert np.isfinite(obs).all() and mask.any(-1).all()
        games += int(done.sum())
        assert np.array_equal(reward[:, 0], -reward[:, 1])
    assert games > n


def test_native_backend_deterministic_and_clonable():
    def run(threads):
        env = pe.VecEnv(num_envs=16, seed=9, teams="randbats", threads=threads, backend="native")
        _, mask = env.reset()
        rng = np.random.default_rng(4)
        acc = []
        for _ in range(60):
            obs, mask, *_ = env.step(sample_legal(mask, rng))
            acc.append(obs.copy())
        return env, mask, np.stack(acc)

    env, mask, a = run(1)
    assert np.array_equal(a, run(3)[2])
    blob = env.clone_state(3)
    env.restore_state(3, blob)
    assert env.clone_state(3) == blob
    with pytest.raises(ValueError):
        env.restore_state(0, b"junk")


def test_unknown_backend_rejected():
    with pytest.raises(ValueError):
        pe.VecEnv(num_envs=1, backend="nope")
