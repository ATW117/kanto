//! Per-player memory of public battle information (docs/OBSERVATION_RULES.md §3).
//!
//! Each player owns one `Memory`. The reveal part is observer-specific (what *this* player has
//! learned about the opponent's team); the public trackers (elapsed counters, last-turn events)
//! are identical in both players' memories but live here so a player's observation is a pure
//! function of (battle, memory). Fixed-size and `Copy`: cloned/restored with the battle.

pub const UNREVEALED: u8 = 0xFF;

/// Public, per-side elapsed counters for the active Pokémon (reset on switch/faint).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SideTrack {
    /// Turns the active Pokémon has been confused.
    pub confusion_elapsed: u8,
    /// Turns the active Pokémon's move has been disabled.
    pub disable_elapsed: u8,
    /// Turns of partial trapping involving this side's active (as user or target).
    pub trap_elapsed: u8,
    /// Turns the active Pokémon has been badly poisoned (observed Toxic counter).
    pub toxic_elapsed: u8,
    /// Move copied by Mimic (0 = none) for the active Pokémon.
    pub mimic_move: u8,
}

/// Public events of the most recent update for one side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnEvents {
    pub moved: bool,
    pub moved_first: bool,
    /// Move id used (including called moves), 0 = none.
    pub mv: u8,
    pub called: bool,
    pub missed: bool,
    pub crit: bool,
    pub super_effective: bool,
    pub resisted: bool,
    pub immune: bool,
    pub failed: bool,
    pub switched: bool,
    /// `|cant|` reason + 1 (0 = none); see `protocol::cant`.
    pub cant: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    // ---- Observer-specific reveals about the opponent ----
    /// Number of opponent Pokémon revealed so far.
    pub n_revealed: u8,
    /// Observation slot -> opponent original party index, in order of first appearance (so the
    /// encoding does not leak the opponent's team order).
    pub reveal_order: [u8; 6],
    /// Opponent original party index -> observation slot (`UNREVEALED` if unseen).
    pub slot_of: [u8; 6],
    /// Revealed move ids per opponent original party index, in reveal order (0 = unknown).
    pub moves: [[u8; 4]; 6],
    /// PP uses counted per revealed move (`|move|` lines without `[from]`).
    pub move_uses: [[u8; 4]; 6],
    /// The opponent's active Pokémon is transformed: its used moves are not its own.
    pub opp_transformed: bool,

    // ---- Public trackers (absolute player index) ----
    /// Observed sleep turns (`cant slp` count) per (player, party index).
    pub sleep_obs: [[u8; 6]; 2],
    pub track: [SideTrack; 2],
    pub last: [TurnEvents; 2],
}

impl Default for Memory {
    fn default() -> Self {
        Memory {
            n_revealed: 0,
            reveal_order: [UNREVEALED; 6],
            slot_of: [UNREVEALED; 6],
            moves: [[0; 4]; 6],
            move_uses: [[0; 4]; 6],
            opp_transformed: false,
            sleep_obs: [[0; 6]; 2],
            track: [SideTrack::default(); 2],
            last: [TurnEvents::default(); 2],
        }
    }
}

impl Memory {
    /// The opponent's party member `idx` (original index) appeared.
    #[inline]
    pub fn reveal(&mut self, idx: u8) {
        let i = idx as usize;
        if i < 6 && self.slot_of[i] == UNREVEALED {
            self.slot_of[i] = self.n_revealed;
            self.reveal_order[self.n_revealed as usize] = idx;
            self.n_revealed += 1;
        }
    }

    #[inline]
    pub fn is_revealed(&self, idx: u8) -> bool {
        (idx as usize) < 6 && self.slot_of[idx as usize] != UNREVEALED
    }

    /// The opponent's party member `idx` is known to have move `m`; `used` counts a PP use.
    #[inline]
    pub fn see_move(&mut self, idx: u8, m: u8, used: bool) {
        let i = idx as usize;
        if i >= 6 || m == 0 {
            return;
        }
        let moves = &mut self.moves[i];
        for k in 0..4 {
            if moves[k] == m {
                if used {
                    self.move_uses[i][k] = self.move_uses[i][k].saturating_add(1);
                }
                return;
            }
            if moves[k] == 0 {
                moves[k] = m;
                self.move_uses[i][k] = used as u8;
                return;
            }
        }
    }

    pub fn n_moves(&self, idx: u8) -> usize {
        self.moves[idx as usize].iter().take_while(|&&m| m != 0).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_order_and_moves() {
        let mut m = Memory::default();
        m.reveal(3);
        m.reveal(0);
        m.reveal(3);
        assert_eq!(m.n_revealed, 2);
        assert_eq!(m.reveal_order[..2], [3, 0]);
        assert_eq!(m.slot_of[3], 0);
        m.see_move(3, 34, true);
        m.see_move(3, 34, true);
        m.see_move(3, 59, false);
        assert_eq!(m.moves[3], [34, 59, 0, 0]);
        assert_eq!(m.move_uses[3], [2, 0, 0, 0]);
        assert_eq!(m.n_moves(3), 2);
    }
}
