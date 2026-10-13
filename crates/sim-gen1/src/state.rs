//! Battle state. Everything is fixed-size and `Copy`; the PRNG lives in the battle.
//!
//! The layout mirrors Pokémon Showdown's objects closely enough that each Showdown field the
//! mechanics read has exactly one home here:
//! - [`Mon`]: a `Pokemon` as it persists on the bench (HP, status, base move slots, the
//!   `speed` field Showdown sorts by, ...), indexed by *original* team slot.
//! - [`Active`]: everything `clearVolatile` resets on switch-out (boosts, modified stats,
//!   volatiles, transformed move slots, ...), one per side.
//! - [`Side`]: Showdown `Side` fields (Gen 1 move-selection memory, team order, ...).

use sim_core::{Prng, Request};

pub const MAX_TEAM: usize = 6;

/// Status ids (`sim_data::gen1::status`), plus Showdown's transient `'fnt'`.
pub mod st {
    pub const NONE: u8 = 0;
    pub const SLP: u8 = 1;
    pub const PSN: u8 = 2;
    pub const BRN: u8 = 3;
    pub const FRZ: u8 = 4;
    pub const PAR: u8 = 5;
    pub const TOX: u8 = 6;
    /// Set by `checkFainted` on a fainted active until it is replaced.
    pub const FNT: u8 = 7;
    pub static IDS: [&str; 8] = ["", "slp", "psn", "brn", "frz", "par", "tox", "fnt"];
}

/// Volatile ids (internal order; [`V_IDS`] gives Showdown's names).
pub mod v {
    pub const CONFUSION: u8 = 0;
    pub const FLINCH: u8 = 1;
    pub const PARTIALLYTRAPPED: u8 = 2;
    pub const FAKEPARTIALLYTRAPPED: u8 = 3;
    pub const PARTIALTRAPPINGLOCK: u8 = 4;
    pub const MUSTRECHARGE: u8 = 5;
    pub const LOCKEDMOVE: u8 = 6;
    pub const TWOTURNMOVE: u8 = 7;
    pub const INVULNERABILITY: u8 = 8;
    pub const BIDE: u8 = 9;
    pub const DISABLE: u8 = 10;
    pub const FOCUSENERGY: u8 = 11;
    pub const MIST: u8 = 12;
    pub const LIGHTSCREEN: u8 = 13;
    pub const REFLECT: u8 = 14;
    pub const LEECHSEED: u8 = 15;
    pub const SUBSTITUTE: u8 = 16;
    pub const RAGE: u8 = 17;
    pub const RESIDUALDMG: u8 = 18;
    pub const MINIMIZE: u8 = 19;
    pub const DEFENSECURL: u8 = 20;
    pub const COUNT: usize = 21;
}

pub static V_IDS: [&str; v::COUNT] = [
    "confusion",
    "flinch",
    "partiallytrapped",
    "fakepartiallytrapped",
    "partialtrappinglock",
    "mustrecharge",
    "lockedmove",
    "twoturnmove",
    "invulnerability",
    "bide",
    "disable",
    "focusenergy",
    "mist",
    "lightscreen",
    "reflect",
    "leechseed",
    "substitute",
    "rage",
    "residualdmg",
    "minimize",
    "defensecurl",
];

/// Pseudo move ids Showdown stores in `lastSelectedMove` & co. (real moves are 1..=165).
pub mod pm {
    /// No move (`null`).
    pub const NONE: u8 = 0;
    pub const STRUGGLE: u8 = 165;
    /// `'nomove'`: initial `lastSelectedMove`; executing it is Gen 1's thawed-without-a-move bug.
    pub const NOMOVE: u8 = 200;
    /// `'cannotmove'`.
    pub const CANNOTMOVE: u8 = 201;
    /// The `nomove` fissure-like ActiveMove (id `fissure`, `???` type, 102 BP): as `lastMove`.
    pub const NOMOVE_HACK: u8 = 202;
}

/// Showdown ID of a (pseudo) move id.
pub fn move_id_str(id: u8) -> &'static str {
    match id {
        pm::NOMOVE => "nomove",
        pm::CANNOTMOVE => "cannotmove",
        pm::NOMOVE_HACK => "fissure",
        0 => "",
        n => sim_data::gen1::MOVES[n as usize].id,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MoveSlot {
    pub id: u8,
    pub pp: u8,
    pub maxpp: u8,
    pub disabled: bool,
}

/// A team member (Showdown `Pokemon`, bench-persistent parts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mon {
    pub species: u8,
    pub level: u8,
    /// Showdown IVs (DV * 2) and EVs per stat `[hp, atk, def, spa, spd, spe]`; kept for
    /// Transform, which recomputes stats from the user's set and the target's species.
    pub ivs: [u8; 6],
    pub evs: [u8; 6],
    /// `baseStoredStats` `[hp, atk, def, spa, spd, spe]` (index 0 = max HP).
    pub stats: [u16; 6],
    pub hp: u16,
    pub status: u8,
    /// Sleep `statusState.time` / `startTime`.
    pub slp_time: i8,
    pub slp_start: u8,
    /// `statusState.source` was an ally (self-inflicted, e.g. Rest): ignored by Sleep Clause.
    pub status_self: bool,
    /// `baseMoveSlots`.
    pub moves: [MoveSlot; 4],
    pub n_moves: u8,
    /// Showdown's `pokemon.speed` (the cached sort key, not a stat).
    pub speed: u16,
    pub fainted: bool,
    pub faint_queued: bool,
    /// `subFainted`.
    pub sub_fainted: bool,
    /// Modified stats `[atk, def, spa, spd, spe]` as left by the last `clearVolatile`.
    pub modified: [u16; 5],
}

/// Volatile state of the active Pokémon (reset by `clearVolatile`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Active {
    pub species: u8,
    pub types: [u8; 2],
    /// `storedStats` `[atk, def, spa, spd, spe]` (Transform replaces them).
    pub stored: [u16; 5],
    /// `modifiedStats` `[atk, def, spa, spd, spe]`.
    pub modified: [u16; 5],
    /// `[atk, def, spa, spd, spe, accuracy, evasion]`.
    pub boosts: [i8; 7],
    pub transformed: bool,
    /// `moveSlots`: for slots with `virt[i] == false` the slot *is* the base move slot (Showdown
    /// shares the object), otherwise `vslots[i]` holds a Mimic/Transform copy.
    pub n_slots: u8,
    pub virt: [bool; 4],
    pub vslots: [MoveSlot; 4],
    /// Volatile bitset (`1 << v::*`) and insertion stamps (Showdown iterates `volatiles` in
    /// insertion order, which decides how tied handlers are shuffled).
    pub vols: u32,
    pub vstamp: [u8; v::COUNT],
    pub next_stamp: u8,
    pub confusion_time: i8,
    pub flinch_dur: u8,
    pub pt_dur: u8,
    pub pt_maybe_locked: bool,
    /// `partiallytrapped.source`: the trapper's original team slot (on the foe side).
    pub pt_source: u8,
    pub fpt_dur: u8,
    /// `fakepartiallytrapped.counterpart`: original team slot (on the foe side).
    pub fpt_counterpart: u8,
    pub ptl_dur: u8,
    pub ptl_total: u8,
    pub ptl_move: u8,
    pub ptl_damage: u32,
    /// Showdown slot of the Pokémon `partialtrappinglock` targets (`locked`): (side, team slot).
    pub ptl_locked: u8,
    pub lockedmove_move: u8,
    pub lockedmove_time: i8,
    pub lockedmove_acc: i32,
    pub twoturn_move: u8,
    pub bide_damage: u32,
    pub bide_time: i8,
    pub disable_move: u8,
    pub disable_slot: u8,
    pub disable_time: i8,
    pub sub_hp: i32,
    pub rage_acc: i32,
    pub residual_counter: u8,
    /// `pokemon.lastMove` (0 = null).
    pub last_move: u8,
    pub move_this_turn: u8,
    pub trapped: bool,
    pub maybe_locked: bool,
    pub maybe_disabled: bool,
    pub switch_flag: bool,
}

impl Default for Active {
    fn default() -> Self {
        Active {
            species: 0,
            types: [0; 2],
            stored: [0; 5],
            modified: [0; 5],
            boosts: [0; 7],
            transformed: false,
            n_slots: 0,
            virt: [false; 4],
            vslots: [MoveSlot::default(); 4],
            vols: 0,
            vstamp: [0; v::COUNT],
            next_stamp: 0,
            confusion_time: 0,
            flinch_dur: 0,
            pt_dur: 0,
            pt_maybe_locked: false,
            pt_source: 0,
            fpt_dur: 0,
            fpt_counterpart: 0,
            ptl_dur: 0,
            ptl_total: 0,
            ptl_move: 0,
            ptl_damage: 0,
            ptl_locked: 0,
            lockedmove_move: 0,
            lockedmove_time: 0,
            lockedmove_acc: 0,
            twoturn_move: 0,
            bide_damage: 0,
            bide_time: 0,
            disable_move: 0,
            disable_slot: 0,
            disable_time: 0,
            sub_hp: 0,
            rage_acc: 0,
            residual_counter: 0,
            last_move: 0,
            move_this_turn: 0,
            trapped: false,
            maybe_locked: false,
            maybe_disabled: false,
            switch_flag: false,
        }
    }
}

impl Active {
    #[inline(always)]
    pub fn has(&self, id: u8) -> bool {
        self.vols & (1 << id) != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Side {
    pub mons: [Mon; MAX_TEAM],
    pub n: u8,
    /// Showdown `side.pokemon` order: position -> original team slot.
    pub order: [u8; MAX_TEAM],
    pub active: Active,
    pub left: u8,
    pub last_selected_move: u8,
    pub last_enemy_selected_move: u8,
    pub last_selected_move_slot: u8,
    /// `side.lastMove` / `side.lastEnemyMove` (move ids; 0 = null).
    pub last_move: u8,
    pub last_enemy_move: u8,
    pub fainted_this_turn: bool,
    pub request: Request,
}

impl Side {
    /// Original team slot of the active Pokémon.
    #[inline(always)]
    pub fn active_slot(&self) -> usize {
        self.order[0] as usize
    }

    #[inline(always)]
    pub fn mon(&self) -> &Mon {
        &self.mons[self.order[0] as usize]
    }

    #[inline(always)]
    pub fn mon_mut(&mut self) -> &mut Mon {
        let i = self.order[0] as usize;
        &mut self.mons[i]
    }

    /// The active Pokémon's `moveSlots[i]`.
    #[inline]
    pub fn slot(&self, i: usize) -> MoveSlot {
        if self.active.virt[i] {
            self.active.vslots[i]
        } else {
            self.mon().moves[i]
        }
    }

    #[inline]
    pub fn slot_mut(&mut self, i: usize) -> &mut MoveSlot {
        if self.active.virt[i] {
            &mut self.active.vslots[i]
        } else {
            let a = self.order[0] as usize;
            &mut self.mons[a].moves[i]
        }
    }
}

/// Which reference behaviour to reproduce where vanilla Showdown and pkmn's patched Showdown
/// differ (see parity/showdown/patches.js and docs/SIM_DESIGN.md).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compat {
    /// Vanilla pokemon-showdown 0.11.11.
    #[default]
    Vanilla,
    /// pkmn/engine's patched Showdown (host-ordered events, tie-breaking priorities).
    Pkmn,
}

/// Format rules that change mechanics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rules {
    pub sleep_clause: bool,
    pub freeze_clause: bool,
    pub compat: Compat,
}

impl Rules {
    /// `[Gen 1] OU`: Sleep Clause Mod and Freeze Clause Mod.
    pub const OU: Rules = Rules { sleep_clause: true, freeze_clause: true, compat: Compat::Vanilla };
    /// `[Gen 1] OU` without the clauses (pkmn/engine has no clauses).
    pub const NO_CLAUSES: Rules = Rules { sleep_clause: false, freeze_clause: false, compat: Compat::Vanilla };
}

/// Action kinds in Showdown's queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    BeforeTurn,
    Move,
    Switch,
    InstaSwitch,
    RunSwitch,
    Residual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    pub order: u64,
    pub priority: i32,
    pub speed: i64,
    /// Acting side (for pokemon-bound actions).
    pub side: u8,
    /// Move id (Move), or original team slot of the incoming Pokémon (Switch/InstaSwitch/RunSwitch).
    pub data: u8,
}

pub const QUEUE_CAP: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Queue {
    pub len: u8,
    pub list: [Action; QUEUE_CAP],
}

impl Default for Queue {
    fn default() -> Self {
        Queue {
            len: 0,
            list: [Action { kind: ActionKind::Residual, order: 0, priority: 0, speed: 0, side: 0, data: 0 }; QUEUE_CAP],
        }
    }
}

impl Queue {
    #[inline]
    pub fn as_slice(&self) -> &[Action] {
        &self.list[..self.len as usize]
    }
    pub fn clear(&mut self) {
        self.len = 0;
    }
    pub fn push(&mut self, a: Action) {
        assert!((self.len as usize) < QUEUE_CAP, "action queue overflow");
        self.list[self.len as usize] = a;
        self.len += 1;
    }
    pub fn insert(&mut self, index: usize, a: Action) {
        assert!((self.len as usize) < QUEUE_CAP, "action queue overflow");
        let n = self.len as usize;
        self.list.copy_within(index..n, index + 1);
        self.list[index] = a;
        self.len += 1;
    }
    pub fn shift(&mut self) -> Option<Action> {
        if self.len == 0 {
            return None;
        }
        let a = self.list[0];
        let n = self.len as usize;
        self.list.copy_within(1..n, 0);
        self.len -= 1;
        Some(a)
    }
    pub fn peek(&self) -> Option<&Action> {
        self.as_slice().first()
    }
    pub fn remove(&mut self, index: usize) {
        let n = self.len as usize;
        self.list.copy_within(index + 1..n, index);
        self.len -= 1;
    }
}

/// A queued faint (`faintQueue` entry): (side, original team slot).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faint {
    pub side: u8,
    pub slot: u8,
}

/// The whole battle: a plain `Copy` value (memcpy to clone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battle {
    pub sides: [Side; 2],
    pub rng: Prng,
    pub turn: u16,
    /// `battle.lastDamage`.
    pub last_damage: u32,
    pub rules: Rules,
    pub queue: Queue,
    pub mid_turn: bool,
    pub started: bool,
    pub ended: bool,
    /// 0 none, 1 p1, 2 p2, 3 tie.
    pub winner: u8,
    pub faint_queue: [Faint; 4],
    pub n_faint: u8,
}
