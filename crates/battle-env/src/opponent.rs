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
}

impl Opponent {
    pub fn parse(s: &str) -> Result<Opponent, String> {
        match s {
            "random" => Ok(Opponent::Random),
            "max_damage" => Ok(Opponent::MaxDamage),
            _ => Err(format!("unknown opponent '{s}' (expected 'random' or 'max_damage')")),
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
pub fn pick(kind: Opponent, v: Option<&View>, player: usize, mask: &[bool; NUM_ACTIONS], rng: &mut Rng) -> usize {
    match kind {
        Opponent::Random => random_legal(mask, rng),
        Opponent::MaxDamage => max_damage(v.expect("view required"), player, mask, rng),
    }
}
