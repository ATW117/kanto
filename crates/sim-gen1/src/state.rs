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

/// A move slot: move id, PP (0..=63, Gen 1 wraps at 64) and the `disabled` flag, in 2 bytes.
///
/// `maxpp` is not stored: nothing in Gen 1 reads it, and it is a function of the move and the
/// slot index ([`Side::slot_maxpp`]).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct MoveSlot {
    pub id: u8,
    /// PP in bits 0..=5, `disabled` in bit 7 (bit 6 is always clear).
    bits: u8,
}

impl MoveSlot {
    const DISABLED: u8 = 0x80;
    const PP: u8 = 0x3F;

    #[inline(always)]
    pub const fn new(id: u8, pp: u8) -> MoveSlot {
        MoveSlot { id, bits: pp & Self::PP }
    }

    #[inline(always)]
    pub const fn pp(&self) -> u8 {
        self.bits & Self::PP
    }

    #[inline(always)]
    pub fn set_pp(&mut self, pp: u8) {
        self.bits = (self.bits & Self::DISABLED) | (pp & Self::PP);
    }

    #[inline(always)]
    pub const fn disabled(&self) -> bool {
        self.bits & Self::DISABLED != 0
    }

    #[inline(always)]
    pub fn set_disabled(&mut self, on: bool) {
        if on {
            self.bits |= Self::DISABLED;
        } else {
            self.bits &= !Self::DISABLED;
        }
    }

    /// The packed byte (serialisation).
    #[inline]
    pub const fn bits(&self) -> u8 {
        self.bits
    }

    /// From the packed byte; `None` if the unused bit 6 is set.
    #[inline]
    pub const fn from_bits(id: u8, bits: u8) -> Option<MoveSlot> {
        if bits & 0x40 != 0 {
            None
        } else {
            Some(MoveSlot { id, bits })
        }
    }
}

impl std::fmt::Debug for MoveSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoveSlot").field("id", &self.id).field("pp", &self.pp()).field("disabled", &self.disabled()).finish()
    }
}

/// Getter/setter pairs for single-bit flags packed into an integer field.
macro_rules! flags {
    ($field:ident: $($(#[$doc:meta])* $get:ident / $set:ident = $bit:expr;)*) => {
        $(
            $(#[$doc])*
            #[inline(always)]
            pub const fn $get(&self) -> bool {
                self.$field & $bit != 0
            }
            #[inline(always)]
            pub fn $set(&mut self, on: bool) {
                if on {
                    self.$field |= $bit;
                } else {
                    self.$field &= !$bit;
                }
            }
        )*
    };
}

/// [`Mon::flags`] bits.
pub mod mf {
    pub const STATUS_SELF: u16 = 1 << 0;
    pub const FAINTED: u16 = 1 << 1;
    pub const FAINT_QUEUED: u16 = 1 << 2;
    pub const SUB_FAINTED: u16 = 1 << 3;
    pub const TRAPPED: u16 = 1 << 4;
    pub const MAYBE_LOCKED: u16 = 1 << 5;
    pub const MAYBE_DISABLED: u16 = 1 << 6;
    pub const BENCH_PAR: u16 = 1 << 7;
    pub const BENCH_BRN: u16 = 1 << 8;
    pub const ALL: u16 = (1 << 9) - 1;
}

/// A team member (Showdown `Pokemon`, bench-persistent parts), 32 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mon {
    pub species: u8,
    pub level: u8,
    pub hp: u16,
    /// `baseStoredStats` `[hp, atk, def, spa, spd, spe]` (index 0 = max HP).
    pub stats: [u16; 6],
    pub status: u8,
    /// Sleep `statusState.time` / `startTime`.
    pub slp_time: i8,
    pub slp_start: u8,
    pub n_moves: u8,
    /// `baseMoveSlots`.
    pub moves: [MoveSlot; 4],
    /// [`mf`] bits; see the accessors.
    pub flags: u16,
    /// The Speed DV (IV / 2) and Speed EV of the set: Transform recomputes Speed from the user's
    /// set and the target's species. (The other stats need only their computed values.)
    pub spe_dv: u8,
    pub spe_ev: u8,
}

impl Mon {
    flags! { flags:
        /// `statusState.source` was an ally (self-inflicted, e.g. Rest): ignored by Sleep Clause.
        status_self / set_status_self = mf::STATUS_SELF;
        fainted / set_fainted = mf::FAINTED;
        faint_queued / set_faint_queued = mf::FAINT_QUEUED;
        /// `subFainted`.
        sub_fainted / set_sub_fainted = mf::SUB_FAINTED;
        /// `trapped`, `maybeLocked`, `maybeDisabled`: Pokémon fields that `clearVolatile` does
        /// not reset (endTurn does, for the actives), so a fainted active keeps them.
        trapped / set_trapped = mf::TRAPPED;
        maybe_locked / set_maybe_locked = mf::MAYBE_LOCKED;
        maybe_disabled / set_maybe_disabled = mf::MAYBE_DISABLED;
        /// The last `clearVolatile` re-applied the paralysis Speed drop / burn Attack drop to
        /// the modified stats the Pokémon keeps on the bench.
        bench_par / set_bench_par = mf::BENCH_PAR;
        bench_brn / set_bench_brn = mf::BENCH_BRN;
    }

    /// Modified stats `[atk, def, spa, spd, spe]` as left by the last `clearVolatile` (the
    /// stats, with the paralysis/burn drops it re-applied).
    #[inline]
    pub fn bench_modified(&self) -> [u16; 5] {
        let mut m = [self.stats[1], self.stats[2], self.stats[3], self.stats[4], self.stats[5]];
        if self.bench_par() {
            m[4] = crate::battle::modify_stat(m[4], 0.25);
        }
        if self.bench_brn() {
            m[0] = crate::battle::modify_stat(m[0], 0.5);
        }
        m
    }
}

/// [`Active::virt`] / per-slot helpers.
pub const VIRT_ALL: u8 = 0xF;

/// Volatile state of the active Pokémon (reset by `clearVolatile`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Active {
    pub species: u8,
    pub types: [u8; 2],
    /// `moveSlots`: for slots whose bit in `virt` is clear the slot *is* the base move slot
    /// (Showdown shares the object), otherwise `vslots[i]` holds a Mimic/Transform copy.
    pub n_slots: u8,
    pub virt: u8,
    pub transformed: bool,
    pub switch_flag: bool,
    pub pt_maybe_locked: bool,
    /// `[atk, def, spa, spd, spe, accuracy, evasion]`.
    pub boosts: [i8; 7],
    /// Showdown's `pokemon.speed` of the active Pokémon (the cached sort key, not a stat). Every
    /// benched Pokémon's is its Speed stat (set by `clearVolatile`), so only the active's is
    /// stored.
    pub speed: u16,
    /// `storedStats` `[atk, def, spa, spd, spe]` (Transform replaces them).
    pub stored: [u16; 5],
    /// `modifiedStats` `[atk, def, spa, spd, spe]`.
    pub modified: [u16; 5],
    pub vslots: [MoveSlot; 4],
    /// Volatile bitset (`1 << v::*`) and insertion stamps (Showdown iterates `volatiles` in
    /// insertion order, which decides how tied handlers are shuffled).
    pub vols: u32,
    pub vstamp: [u8; v::COUNT],
    pub next_stamp: u8,
    pub confusion_time: i8,
    pub flinch_dur: u8,
    pub pt_dur: u8,
    /// `partiallytrapped.source`: the trapper's original team slot (on the foe side).
    pub pt_source: u8,
    pub fpt_dur: u8,
    /// `fakepartiallytrapped.counterpart`: original team slot (on the foe side).
    pub fpt_counterpart: u8,
    pub ptl_dur: u8,
    pub ptl_total: u8,
    pub ptl_move: u8,
    /// `partialtrappinglock.locked`: original team slot of the trapped foe.
    pub ptl_locked: u8,
    pub lockedmove_move: u8,
    pub lockedmove_time: i8,
    /// `lockedmove.accuracy` / `rage.accuracy` (0..=255).
    pub lockedmove_acc: u8,
    pub rage_acc: u8,
    pub twoturn_move: u8,
    pub bide_time: i8,
    pub disable_move: u8,
    pub disable_slot: u8,
    pub disable_time: i8,
    pub residual_counter: u8,
    /// `pokemon.lastMove` (0 = null).
    pub last_move: u8,
    pub move_this_turn: u8,
    pub ptl_damage: u32,
    pub bide_damage: u32,
    pub sub_hp: i32,
}

impl Active {
    #[inline(always)]
    pub fn has(&self, id: u8) -> bool {
        self.vols & (1 << id) != 0
    }

    /// Whether move slot `i` is a Mimic/Transform copy.
    #[inline(always)]
    pub fn is_virt(&self, i: usize) -> bool {
        self.virt & (1 << i) != 0
    }

    #[inline(always)]
    pub fn set_virt(&mut self, i: usize, on: bool) {
        if on {
            self.virt |= 1 << i;
        } else {
            self.virt &= !(1 << i);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Side {
    pub mons: [Mon; MAX_TEAM],
    pub active: Active,
    pub n: u8,
    /// Showdown `side.pokemon` order: position -> original team slot.
    pub order: [u8; MAX_TEAM],
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
    ///
    /// `order` entries are always below [`MAX_TEAM`] (`Battle::validate` checks blobs); the
    /// clamp states that to the optimiser, so indexing `mons` needs no bounds check (and no
    /// panic path that blocks reusing the address across calls). It is free on aarch64/x86
    /// (a conditional select).
    #[inline(always)]
    pub fn active_slot(&self) -> usize {
        (self.order[0] as usize).min(MAX_TEAM - 1)
    }

    #[inline(always)]
    pub fn mon(&self) -> &Mon {
        &self.mons[self.active_slot()]
    }

    #[inline(always)]
    pub fn mon_mut(&mut self) -> &mut Mon {
        let i = self.active_slot();
        &mut self.mons[i]
    }

    /// The active Pokémon's `moveSlots[i]`.
    #[inline]
    pub fn slot(&self, i: usize) -> MoveSlot {
        if self.active.is_virt(i) {
            self.active.vslots[i]
        } else {
            self.mon().moves[i]
        }
    }

    #[inline]
    pub fn slot_mut(&mut self, i: usize) -> &mut MoveSlot {
        if self.active.is_virt(i) {
            &mut self.active.vslots[i]
        } else {
            let a = self.active_slot();
            &mut self.mons[a].moves[i]
        }
    }

    /// Showdown's `maxpp` of the active's `moveSlots[i]`: base slots get 3 PP Ups; a Mimic or
    /// Transform copy keeps them only in a slot the Pokémon's own set fills.
    pub fn slot_maxpp(&self, i: usize) -> u8 {
        let id = self.slot(i).id;
        let ups = if i < self.mon().n_moves as usize { 3 } else { 0 };
        crate::battle::calc_pp(sim_data::gen1::MOVES[id as usize].pp, ups)
    }

    /// Showdown's `speed` of team member `i` (only the active's differs from its Speed stat).
    #[inline]
    pub fn mon_speed(&self, i: usize) -> u16 {
        if i == self.active_slot() && self.active.species != 0 {
            self.active.speed
        } else {
            self.mons[i].stats[5]
        }
    }
}

/// The two sides, indexed by [`sim_core::Player`].
///
/// Indexing masks the player to 0/1 instead of bounds-checking it: the simulator only ever
/// indexes with 0, 1 or `1 - p`, and the public entry points check the player once. This
/// removes a branch (and a panic path that blocks optimisation) from every side access.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sides(pub [Side; 2]);

impl std::ops::Index<usize> for Sides {
    type Output = Side;
    #[inline(always)]
    fn index(&self, p: usize) -> &Side {
        &self.0[p & 1]
    }
}

impl std::ops::IndexMut<usize> for Sides {
    #[inline(always)]
    fn index_mut(&mut self, p: usize) -> &mut Side {
        &mut self.0[p & 1]
    }
}

impl Sides {
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, Side> {
        self.0.iter()
    }

    #[inline]
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Side> {
        self.0.iter_mut()
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
#[repr(u8)]
pub enum ActionKind {
    BeforeTurn,
    Move,
    Switch,
    InstaSwitch,
    RunSwitch,
    Residual,
}

/// A queued action with its resolved `order`, `priority` and `speed` (8 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    /// Acting side (for pokemon-bound actions).
    pub side: u8,
    /// Move id (Move), or original team slot of the incoming Pokémon (Switch/InstaSwitch/RunSwitch).
    pub data: u8,
    pub priority: i8,
    /// Showdown's `order` (3..=300 for every Gen 1 action).
    pub order: u16,
    /// The action's speed (a modified Speed stat, or 1).
    pub speed: u16,
}

impl Default for Action {
    fn default() -> Self {
        Action::new(ActionKind::Residual, 0, 0)
    }
}

impl Action {
    /// An unresolved action (`order`, `priority` and `speed` are filled in by `resolveAction`).
    #[inline]
    pub const fn new(kind: ActionKind, side: u8, data: u8) -> Action {
        Action { kind, side, data, priority: 0, order: 0, speed: 0 }
    }

    /// `comparePriority` key ([`sim_core::sort::key`]).
    #[inline(always)]
    pub fn key(&self) -> u64 {
        sim_core::sort::key(self.order, self.priority, self.speed, 0)
    }
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
            list: [Action::new(ActionKind::Residual, 0, 0); QUEUE_CAP],
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
    #[inline]
    pub fn shift(&mut self) -> Option<Action> {
        if self.len == 0 {
            return None;
        }
        let a = self.list[0];
        // A fixed-size move (inlined; the stale tail past `len` is never read).
        self.list.copy_within(1..QUEUE_CAP, 0);
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
    pub sides: Sides,
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
