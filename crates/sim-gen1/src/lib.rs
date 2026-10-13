//! Native, Showdown-exact Generation I battle simulator.
//!
//! The battle is a fixed-size `Copy` value ([`Battle`]) holding the PRNG, so cloning is a memcpy
//! and the same seed + choices always replays identically. Mechanics are plain Rust functions
//! ported from pokemon-showdown 0.11.11 (`data/mods/gen1`, `sim/battle*.ts`): no event bus, but
//! every place where Showdown's event system sorts handlers or actions (and may consume the PRNG
//! on ties) is reproduced explicitly. See docs/SIM_DESIGN.md.

mod battle;
mod events;
mod moves;
pub mod ser;
pub mod state;

pub use battle::{calc_pp, calc_stats, req, MonSpec, ReqMove, TeamError};
pub use sim_core::{Choice, Event, IllegalChoice, Outcome, Player, Prng, Request, Sink};
pub use state::{Battle, Compat, Rules};

impl sim_core::Generation for Battle {
    const GEN: u8 = 1;

    fn request(&self, player: Player) -> Request {
        if !self.started {
            return Request::Move;
        }
        self.sides[player].request
    }

    fn choices(&self, player: Player, out: &mut [Choice]) -> usize {
        if !self.started {
            out[0] = Choice::Pass;
            return 1;
        }
        let n = self.legal_choices(player, out);
        if n == 0 && !self.ended {
            out[0] = Choice::Pass;
            return 1;
        }
        n
    }

    fn is_legal(&self, player: Player, choice: Choice) -> bool {
        Battle::is_legal(self, player, choice)
    }

    fn update_unchecked<S: Sink>(&mut self, c1: Choice, c2: Choice, sink: &mut S) -> Outcome {
        self.apply_choices_unchecked(c1, c2, sink);
        self.outcome()
    }

    fn turn(&self) -> u32 {
        self.turn as u32
    }

    fn outcome(&self) -> Outcome {
        Battle::outcome(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::Generation;
    use sim_data::gen1::mv;

    fn team() -> Vec<MonSpec> {
        let full = [255u8; 6];
        let iv = [30u8; 6];
        vec![
            MonSpec { species: 128, level: 100, moves: [mv::BODYSLAM, mv::HYPERBEAM, mv::BLIZZARD, mv::EARTHQUAKE], ivs: iv, evs: full },
            MonSpec { species: 113, level: 100, moves: [mv::SOFTBOILED, mv::THUNDERWAVE, mv::ICEBEAM, mv::SEISMICTOSS], ivs: iv, evs: full },
        ]
    }

    #[test]
    fn plays_to_completion() {
        let t = team();
        for seed in 0..50u64 {
            let mut b = Battle::new(&t, &t, Prng::from_u64(seed * 7919 + 1), Rules::OU).unwrap();
            let mut buf = [Choice::Pass; 16];
            let mut steps = 0;
            let mut r = seed;
            while b.outcome() == Outcome::Ongoing && steps < 2000 {
                let mut c = [Choice::Pass; 2];
                for p in 0..2 {
                    let n = b.choices(p, &mut buf);
                    r = r.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    c[p] = buf[(r >> 33) as usize % n];
                }
                b.update(c[0], c[1], &mut ()).unwrap();
                steps += 1;
            }
            assert!(b.outcome() != Outcome::Ongoing, "seed {seed} did not finish");
        }
    }

    #[test]
    fn battle_is_copy_and_small() {
        let size = std::mem::size_of::<Battle>();
        assert!(size < 2048, "battle is {size} bytes");
    }
}
