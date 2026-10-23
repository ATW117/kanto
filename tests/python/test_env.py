import numpy as np
import pytest

import kanto as pe


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
    assert not obs[:, 1].any() and not mask[:, 1].any()
    rng = np.random.default_rng(0)
    actions = np.zeros((n, 2), dtype=np.int16)
    for _ in range(200):
        actions[:, 0] = sample_legal(mask, rng)[:, 0]
        actions[:, 1] = 99  # ignored with a built-in opponent
        obs, mask, reward, done, info = env.step(actions)
        # Player 2's rows are zero: observation and mask.
        assert not obs[:, 1].any() and not mask[:, 1].any()
        assert mask[:, 0].any(-1).all()


def test_gymnasium_wrapper():
    gym = pytest.importorskip("gymnasium")
    from kanto.gym import Gen1Env

    env = Gen1Env(opponent="max_damage", teams="builtin")
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


def test_gymnasium_final_observation_and_reset():
    pytest.importorskip("gymnasium")
    from kanto.gym import Gen1Env

    env = Gen1Env(opponent="max_damage", teams="builtin", max_turns=3)
    obs, _ = env.reset(seed=1)
    rng = np.random.default_rng(0)
    for _ in range(200):
        prev = obs
        obs, r, term, trunc, info = env.step(int(rng.choice(np.flatnonzero(obs["action_mask"]))))
        if term or trunc:
            break
    assert trunc and not obs["action_mask"].any()  # the turn cap: the final observation, not a new game's
    assert not np.array_equal(obs["obs"], prev["obs"])
    # reset() after a game ended hands out the auto-started game; mid-game it starts a new one
    calls = []
    inner = env._env.reset
    env._env.reset = lambda seed=None: calls.append(seed) or inner(seed)
    nxt, _ = env.reset()
    assert calls == [] and nxt["action_mask"].any() and not np.array_equal(nxt["obs"], obs["obs"])
    env.step(int(np.flatnonzero(nxt["action_mask"])[0]))
    env.reset()
    assert calls == [None]


def test_final_obs_is_the_last_state():
    """``info["final_obs"]`` equals what the env would show if the game had not ended there."""
    short = pe.VecEnv(num_envs=4, seed=2, teams="data/teams/gen1ou", threads=1, max_turns=4, final_obs=True)
    long = pe.VecEnv(num_envs=4, seed=2, teams="data/teams/gen1ou", threads=1, max_turns=1000)
    _, mask = short.reset()
    long.reset()
    rng = np.random.default_rng(5)
    for _ in range(50):
        for i in range(4):
            long.restore_state(i, short.clone_state(i))
        acts = sample_legal(mask, rng)
        _, mask, _, done, info = short.step(acts)
        lobs, *_ = long.step(acts)
        if done.any():
            i = int(np.flatnonzero(done)[0])
            if info["truncated"][i] and info["outcome"][i] == 4:
                np.testing.assert_array_equal(info["final_obs"][i], lobs[i])
                return
    pytest.fail("no truncated game")


def test_restore_with_seed_samples_independent_futures():
    env = pe.VecEnv(num_envs=8, seed=0, teams="data/teams/gen1ou", threads=1)
    _, mask = env.reset()
    rng = np.random.default_rng(1)
    for _ in range(5):
        _, mask, *_ = env.step(sample_legal(mask, rng))
    blob = env.clone_state(0)

    def future(seed):
        for i in range(8):
            env.restore_state(i, blob, seed=seed)  # refreshes the returned obs/mask in place
        r = np.random.default_rng(7)
        m = mask
        for _ in range(6):
            _, m, *_ = env.step(sample_legal(m, r))
        return env.clone_state(0)

    assert future(11) == future(11)
    assert len({future(s) for s in range(8)}) > 1
    assert future(None) == future(None)


def test_release_gil_steps():
    env = pe.VecEnv(num_envs=8, seed=0, teams="data/teams/gen1ou", threads=2, release_gil=True)
    _, mask = env.reset()
    rng = np.random.default_rng(0)
    for _ in range(50):
        _, mask, *_ = env.step(sample_legal(mask, rng))


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


def test_backends_listed():
    assert {"native", "native-nc"} <= set(pe.BACKENDS)
    if "pkmn" not in pe.BACKENDS:
        with pytest.raises(ValueError, match="features pkmn"):
            pe.VecEnv(num_envs=1, backend="pkmn")


# ---- Robustness ------------------------------------------------------------------------------

CRASH_TEAMS = (
    '{"name":"mew","pokemon":[{"species":"Mew","moves":["Psychic","Soft-Boiled","Thunder Wave","Transform"]}]}\n'
    '{"name":"jiggly","pokemon":[{"species":"Jigglypuff","moves":["Sing"]}]}\n'
)


def test_move_slot_null_regression(tmp_path):
    """Showdown 0.11.11 crashes ("moveSlot is null") when a Pokémon that selected its 4th move
    (Transform) into a 1-move foe must "Fight" while asleep; the native sim falls back to slot 0
    (docs/SIM_STATUS.md). Before the fix ~55% of these games panicked."""
    teams = tmp_path / "crash.jsonl"
    teams.write_text(CRASH_TEAMS)
    n = 64
    env = pe.VecEnv(num_envs=n, seed=1, teams=str(teams), threads=1, backend="native", validate=False)
    _, mask = env.reset()
    rng = np.random.default_rng(0)
    games = 0
    for _ in range(1500):
        a = sample_legal(mask, rng)
        a[mask[:, :, 3]] = 3  # prefer the 4th move (Transform) whenever legal
        _, mask, _, done, info = env.step(a)
        assert not info["error"].any(), info["errors"]
        games += int(done.sum())
    assert games > 100 and env.total_errors == 0


def test_panic_is_isolated_per_env():
    n = 16
    env = pe.VecEnv(num_envs=n, seed=2, teams="randbats", threads=2, backend="native")
    _, mask = env.reset()
    rng = np.random.default_rng(1)
    for _ in range(5):
        _, mask, *_ = env.step(sample_legal(mask, rng))
    env._env._inject_panic(5)
    obs, mask, reward, done, info = env.step(sample_legal(mask, rng))
    assert info["error"][5] and info["error"].sum() == 1
    assert info["outcome"][5] == pe.OUTCOME_ERROR and done[5] and info["truncated"][5]
    assert reward[5, 0] == 0.0
    assert [i for i, _ in info["errors"]] == [5] and "injected panic" in info["errors"][0][1]
    assert mask.any(-1).all() and np.isfinite(obs).all()
    assert env.total_errors == 1
    # Everything keeps running.
    for _ in range(50):
        obs, mask, reward, done, info = env.step(sample_legal(mask, rng))
        assert not info["error"].any() and info["errors"] == []


@pytest.mark.parametrize("backend", ["pkmn", "native"])
def test_opp_action_obs_frame(backend):
    if backend not in pe.BACKENDS:
        pytest.skip(f"{backend} backend not compiled in")
    n = 64
    env = pe.VecEnv(num_envs=n, seed=3, teams="data/teams/gen1ou", threads=2, backend=backend)
    _, mask = env.reset()
    rng = np.random.default_rng(2)
    seen_switch = seen_move = 0
    for _ in range(300):
        a = sample_legal(mask, rng)
        prev_mask = mask.copy()
        obs, mask, _, done, info = env.step(a)
        lab = info["opp_action_obs_frame"]
        assert lab.shape == (n, 2) and lab.dtype == np.int16
        assert ((lab >= pe.OPP_ACTION_NONE) & (lab < pe.OPP_ACTION_CLASSES)).all()
        for p in range(2):
            q = 1 - p
            switched = a[:, q] >= 4
            only_pass = prev_mask[:, q, 0] & (prev_mask[:, q].sum(-1) == 1)
            # Switches map to appearance slots, moves to move ids (0 = unnamed forced move).
            assert (lab[switched, p] >= pe.OPP_ACTION_SWITCH_BASE).all()
            assert (lab[~switched & ~only_pass, p] <= 165).all()
            assert (lab[~switched & ~only_pass, p] >= 0).all()
            seen_switch += int(switched.sum())
            seen_move += int((~switched).sum())
    assert seen_switch > 100 and seen_move > 1000


def _team(*mons):
    import json

    return json.dumps({"name": "t", "pokemon": [dict(species=s, moves=list(m), **kw) for s, m, kw in mons]})


@pytest.mark.parametrize(
    "mons, reason",
    [
        ([("Tauros", ["Body Slam"], {}), ("Tauros", ["Earthquake"], {})], "Species Clause"),
        ([("Tauros", ["Spore"], {})], "cannot learn"),
    ],
)
def test_team_legality_validation(tmp_path, mons, reason):
    f = tmp_path / "t.json"
    f.write_text(_team(*mons))
    with pytest.raises(ValueError, match=reason):
        pe.VecEnv(num_envs=1, teams=str(f), threads=1, backend="native")
    env = pe.VecEnv(num_envs=1, teams=str(f), threads=1, backend="native", validate=False)
    env.reset()


def test_team_always_rejected(tmp_path):
    f = tmp_path / "t.json"
    for mons in ([("Tauros", ["Body Slam"], {"dvs": {"atk": 16}})], [("Nidoran", ["Tackle"], {})]):
        f.write_text(_team(*mons))
        for validate in (True, False):
            with pytest.raises(ValueError):
                pe.VecEnv(num_envs=1, teams=str(f), threads=1, validate=validate)


def test_nidoran_aliases(tmp_path):
    f = tmp_path / "t.json"
    f.write_text(_team(("Nidoran♀", ["Double Kick"], {}), ("Nidoran♂", ["Horn Attack"], {})))
    env = pe.VecEnv(num_envs=1, teams=str(f), threads=1, backend="native")
    obs, _ = env.reset()
    names = pe.species_names()
    sid = [int(obs[0, 0, off]) for off, vocab in pe.id_offsets() if vocab == "species"][:2]
    assert [names[s] for s in sid] == ["Nidoran-F", "Nidoran-M"]


def test_train_eval_split():
    import json

    from pathlib import Path

    root = Path(__file__).resolve().parents[2] / "data/teams/gen1ou"
    pool = [l for l in (root / "teams.jsonl").read_text().splitlines() if l.strip()]
    train = [l for l in (root / "split/train.jsonl").read_text().splitlines() if l.strip()]
    held = [l for l in (root / "split/eval.jsonl").read_text().splitlines() if l.strip()]
    assert sorted(train + held) == sorted(pool) and not set(train) & set(held)
    assert 0.08 < len(held) / len(pool) < 0.12
    for spec in (pe.TEAMS_TRAIN, pe.TEAMS_EVAL):
        env = pe.VecEnv(num_envs=2, teams=spec, threads=1, backend="native")
        env.reset()
    with pytest.raises(ValueError):
        pe.VecEnv(num_envs=1, teams="data/teams/gen1ou:nope", threads=1)
    assert json.loads(held[0])["pokemon"]


def test_corrupted_blobs_never_crash():
    env = pe.VecEnv(num_envs=4, seed=5, teams="randbats", threads=1, backend="native")
    _, mask = env.reset()
    rng = np.random.default_rng(3)
    for _ in range(20):
        _, mask, *_ = env.step(sample_legal(mask, rng))
    blob = bytearray(env.clone_state(1))
    accepted = 0
    for _ in range(3000):
        b = bytearray(blob)
        for i in rng.integers(0, len(b), size=rng.integers(1, 4)):
            b[i] ^= 1 << int(rng.integers(0, 8))
        try:
            env.restore_state(1, bytes(b))
        except ValueError:
            continue
        accepted += 1
        for _ in range(10):
            # `mask` is the env's reused buffer: restore_state refreshed env 1's rows in place.
            _, mask, _, _, info = env.step(sample_legal(mask, rng))
            assert not info["error"].any(), info["errors"]
    assert accepted > 100
