//! Built-in opponent policies (played in Rust inside `step`).

use crate::backend::NUM_ACTIONS;
use crate::data;
use crate::obs::View;
use crate::rng::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opponent {
    /// Uniformly random legal action.
    Random,
    /// Highest base power x STAB x type effectiveness against the foe's current typing; random
    /// legal action when no damaging move is available (e.g. forced switches).
    MaxDamage,
    /// One-turn simultaneous-move search (`Env::lookahead_action`): every own action x opponent
    /// action is simulated `LOOKAHEAD_SAMPLES` times from a copy of the battle with a fresh RNG
    /// seed, scored by `position_value`, and the move is drawn from the approximate Nash
    /// strategy of the payoff matrix (regret matching). It sees the full (omniscient) state.
    Lookahead,
}

/// Simulations per (own action, opponent action) pair for `Opponent::Lookahead`.
pub const LOOKAHEAD_SAMPLES: u32 = 2;
/// Regret-matching iterations over the payoff matrix.
pub const LOOKAHEAD_RM_ITERS: usize = 64;

impl Opponent {
    pub fn name(self) -> &'static str {
        match self {
            Opponent::Random => "random",
            Opponent::MaxDamage => "max_damage",
            Opponent::Lookahead => "lookahead",
        }
    }

    pub fn parse(s: &str) -> Result<Opponent, String> {
        match s {
            "random" => Ok(Opponent::Random),
            "max_damage" => Ok(Opponent::MaxDamage),
            "lookahead" => Ok(Opponent::Lookahead),
            _ => Err(format!("unknown opponent '{s}' (expected 'random', 'max_damage' or 'lookahead')")),
        }
    }
}

pub fn random_legal(mask: &[bool; NUM_ACTIONS], rng: &mut Rng) -> usize {
    let n = mask.iter().filter(|&&m| m).count();
    debug_assert!(n > 0);
    let mut k = rng.below(n as u64) as usize;
    for (i, &m) in mask.iter().enumerate() {
        if m {
            if k == 0 {
                return i;
            }
            k -= 1;
        }
    }
    unreachable!("empty action mask")
}

/// Heuristic damage score of the active Pokémon's move slot `k` against the foe's active.
pub fn move_score(v: &View, player: usize, k: usize) -> u32 {
    let me = &v.sides[player].active;
    let foe = &v.sides[1 - player].active;
    let mv = me.moves[k];
    if mv == 0 {
        return 0;
    }
    let info = &data::MOVES[mv as usize];
    if info.bp == 0 {
        return 0;
    }
    let eff = data::effectiveness(info.ty, foe.types[0], foe.types[1]); // x100
                                                                        // Fixed-damage moves ignore resistances but not immunities; value them like a 75 BP move.
    let bp = if data::is_fixed_damage(mv) {
        if eff == 0 {
            0
        } else {
            75 * 100
        }
    } else {
        info.bp as u32 * eff
    };
    let stab = if me.types.contains(&info.ty) { 3 } else { 2 };
    bp * stab
}

pub fn max_damage(v: &View, player: usize, mask: &[bool; NUM_ACTIONS], rng: &mut Rng) -> usize {
    let mut best = 0u32;
    let mut best_actions = [0usize; 4];
    let mut n_best = 0;
    for k in 0..4 {
        if !mask[k] {
            continue;
        }
        let s = move_score(v, player, k);
        if s > best {
            best = s;
            n_best = 0;
        }
        if s == best && s > 0 {
            best_actions[n_best] = k;
            n_best += 1;
        }
    }
    if n_best == 0 {
        // Nothing damaging (or only a forced/locked action): prefer any legal move over switching.
        let mut moves_only = [false; NUM_ACTIONS];
        moves_only[..4].copy_from_slice(&mask[..4]);
        if moves_only.iter().any(|&m| m) {
            return random_legal(&moves_only, rng);
        }
        return random_legal(mask, rng);
    }
    best_actions[rng.below(n_best as u64) as usize]
}

/// The opponent's action index. `v` is only needed (and only decoded) for `MaxDamage`.
/// `Lookahead` needs the backend and is handled by `Env::builtin_action`.
pub fn pick(kind: Opponent, v: Option<&View>, player: usize, mask: &[bool; NUM_ACTIONS], rng: &mut Rng) -> usize {
    match kind {
        Opponent::Random => random_legal(mask, rng),
        Opponent::MaxDamage | Opponent::Lookahead => max_damage(v.expect("view required"), player, mask, rng),
    }
}

/// Heuristic value of a position for `player`: (sum of own HP fractions + 0.5 per Pokémon
/// alive - 0.1 per statused Pokémon) minus the same for the foe.
pub fn position_value(v: &View, player: usize) -> f32 {
    let side = |p: usize| -> f32 {
        let s = &v.sides[p];
        let mut x = 0.0;
        for m in &s.mons[..(s.size as usize).min(6)] {
            if m.max_hp == 0 || m.hp == 0 {
                continue;
            }
            x += m.hp as f32 / m.max_hp as f32 + 0.5;
            if m.status != crate::obs::status::NONE {
                x -= 0.1;
            }
        }
        x
    };
    side(player) - side(1 - player)
}

/// Approximate equilibrium row strategy of the zero-sum game `pay[i][j]` (row maximises) by
/// regret matching+ with linear averaging; returns the average row strategy over the first `n`
/// rows.
pub fn regret_matching(pay: &[[f32; NUM_ACTIONS]; NUM_ACTIONS], n: usize, m: usize, iters: usize) -> [f32; NUM_ACTIONS] {
    let mut rr = [0f32; NUM_ACTIONS];
    let mut rc = [0f32; NUM_ACTIONS];
    let mut avg = [0f32; NUM_ACTIONS];
    let strat = |r: &[f32; NUM_ACTIONS], k: usize| {
        let mut s = [0f32; NUM_ACTIONS];
        let tot: f32 = r[..k].iter().map(|x| x.max(0.0)).sum();
        for i in 0..k {
            s[i] = if tot > 0.0 { r[i].max(0.0) / tot } else { 1.0 / k as f32 };
        }
        s
    };
    for it in 0..iters {
        let (sr, sc) = (strat(&rr, n), strat(&rc, m));
        let w = (it + 1) as f32;
        for i in 0..n {
            avg[i] += w * sr[i];
        }
        let mut ur = [0f32; NUM_ACTIONS];
        let mut uc = [0f32; NUM_ACTIONS];
        for i in 0..n {
            for j in 0..m {
                ur[i] += pay[i][j] * sc[j];
                uc[j] -= pay[i][j] * sr[i];
            }
        }
        let er: f32 = (0..n).map(|i| sr[i] * ur[i]).sum();
        let ec: f32 = (0..m).map(|j| sc[j] * uc[j]).sum();
        for i in 0..n {
            rr[i] = (rr[i] + ur[i] - er).max(0.0);
        }
        for j in 0..m {
            rc[j] = (rc[j] + uc[j] - ec).max(0.0);
        }
    }
    let tot: f32 = avg[..n].iter().sum();
    for x in &mut avg[..n] {
        *x /= tot.max(1e-9);
    }
    avg
}
