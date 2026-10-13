//! Battle construction, the turn loop, the action queue, requests and choices: a port of
//! pokemon-showdown 0.11.11's `Battle`/`BattleQueue`/`Side` flow as it runs for Gen 1.
//!
//! Function names follow Showdown's so the two can be read side by side (`turn_loop` =
//! `Battle#turnLoop`, `run_action` = `Battle#runAction`, ...). Move execution lives in
//! `moves.rs`, event-handler dispatch in `events.rs`.

use crate::state::*;
use sim_core::sort::{compare, speed_sort, Priority, ORDER_UNSET};
use sim_core::{Choice, Event, Outcome, Player, Prng, Request, Sink};
use sim_data::gen1::{self as data, MOVES, SPECIES};

/// One team member as given to [`Battle::new`] (Showdown set semantics).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonSpec {
    pub species: u8,
    pub level: u8,
    pub moves: [u8; 4],
    /// Showdown IVs `[hp, atk, def, spa, spd, spe]` in 0..=31 (Gen 1 rounds them down to even:
    /// DV * 2). Use [`MonSpec::from_dvs`] to build them from Gen 1 DVs.
    pub ivs: [u8; 6],
    /// Showdown EVs in 0..=255 (255 = 65535 stat experience).
    pub evs: [u8; 6],
}

impl MonSpec {
    /// From Gen 1 DVs `[atk, def, spe, spc]` (the HP DV is derived) and stat experience
    /// `[hp, atk, def, spe, spc]`.
    pub fn from_dvs(species: u8, level: u8, moves: &[u8], dvs: [u8; 4], exp: [u16; 5]) -> MonSpec {
        let hp_dv = (dvs[0] & 1) << 3 | (dvs[1] & 1) << 2 | (dvs[2] & 1) << 1 | (dvs[3] & 1);
        let ev = |e: u16| ((e as f64).sqrt().ceil() as u32).min(255) as u8;
        let mut m = [0u8; 4];
        m[..moves.len()].copy_from_slice(moves);
        MonSpec {
            species,
            level,
            moves: m,
            ivs: [hp_dv * 2, dvs[0] * 2, dvs[1] * 2, dvs[3] * 2, dvs[3] * 2, dvs[2] * 2],
            evs: [ev(exp[0]), ev(exp[1]), ev(exp[2]), ev(exp[4]), ev(exp[4]), ev(exp[3])],
        }
    }
}

/// Showdown `statModify` for Gen 1-2 (`base` = species base stats, `[hp, atk, def, spa, spd, spe]`).
pub fn calc_stats(base: [u8; 6], ivs: [u8; 6], evs: [u8; 6], level: u8) -> [u16; 6] {
    let mut out = [0u16; 6];
    for i in 0..6 {
        let core = 2 * base[i] as u32 + (ivs[i] & 30) as u32 + evs[i] as u32 / 4;
        out[i] = if i == 0 {
            ((core + 100) * level as u32 / 100 + 10) as u16
        } else {
            (core * level as u32 / 100 + 5) as u16
        };
    }
    out
}

/// `calculatePP(move, ppUps)` for Gen 1-2.
pub fn calc_pp(pp: u8, pp_ups: u8) -> u8 {
    let mut v = pp as u32 * (5 + pp_ups as u32) / 5;
    if pp == 40 {
        v -= pp_ups as u32;
    }
    v as u8
}

/// A request's move entry: (move id or pseudo id, disabled).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReqMove {
    pub id: u8,
    pub disabled: bool,
}

/// Pseudo entries of request move lists.
pub mod req {
    pub const RECHARGE: u8 = 250;
    pub const FIGHT: u8 = 251;
}

impl Battle {
    /// A new battle (Showdown `Battle` + both `setPlayer` calls, before `start`).
    pub fn new(p1: &[MonSpec], p2: &[MonSpec], rng: Prng, rules: Rules) -> Battle {
        let mut b = Battle {
            sides: [Side::new(p1), Side::new(p2)],
            rng,
            turn: 0,
            last_damage: 0,
            rules,
            queue: Queue::default(),
            mid_turn: false,
            started: false,
            ended: false,
            winner: 0,
            faint_queue: [Faint::default(); 4],
            n_faint: 0,
        };
        for p in 0..2 {
            b.sides[p].active = Active::default();
        }
        b
    }

    /// Outcome so far.
    pub fn outcome(&self) -> Outcome {
        match self.winner {
            1 => Outcome::P1Win,
            2 => Outcome::P2Win,
            3 => Outcome::Tie,
            _ => Outcome::Ongoing,
        }
    }

    // ---- Battle#start / runAction('start') -------------------------------------------------

    /// `Battle#start`: queue the start action and run the turn loop until the first request.
    pub fn start<S: Sink>(&mut self, s: &mut S) {
        self.started = true;
        self.mid_turn = true;
        for p in 0..2 {
            self.sides[p].left = self.sides[p].n;
        }
        // runAction('start'): switch in both leads.
        for p in 0..2 {
            let lead = self.sides[p].order[0];
            self.switch_in(p, lead, false, s);
        }
        self.action_tail(None, s);
        if self.pending_request() || self.ended {
            return;
        }
        self.run_queue(s);
    }

    #[inline]
    fn pending_request(&self) -> bool {
        self.sides.iter().any(|sd| matches!(sd.request, Request::Move | Request::Switch))
    }

    /// Run queued actions (`turnLoop` body), then `endTurn` when the queue empties.
    fn run_queue<S: Sink>(&mut self, s: &mut S) {
        while let Some(a) = self.queue.shift() {
            self.run_action(a, s);
            if self.pending_request() || self.ended {
                return;
            }
        }
        self.end_turn(s);
        self.mid_turn = false;
        self.queue.clear();
    }

    // ---- commitChoices -----------------------------------------------------------------------

    /// Apply both players' choices (`Battle#choose` for each side, then `commitChoices`).
    /// Choices for players without a move/switch request are ignored.
    pub fn apply_choices<S: Sink>(&mut self, c1: Choice, c2: Choice, s: &mut S) {
        if !self.started {
            self.start(s);
            return;
        }
        if self.ended {
            return;
        }
        // Side#choose -> chooseMove/chooseSwitch for each side with a request.
        let mut chosen: [Option<Chosen>; 2] = [None, None];
        let choices = [c1, c2];
        for p in 0..2 {
            chosen[p] = match self.sides[p].request {
                Request::Move => Some(self.choose_move_request(p, choices[p])),
                Request::Switch => match choices[p] {
                    Choice::Switch(k) => Some(Chosen::InstaSwitch(self.sides[p].order[k as usize - 1])),
                    c => panic!("player {p} must switch, got {c:?}"),
                },
                _ => None,
            };
        }
        // commitChoices
        self.update_speed();
        debug_assert_eq!(self.queue.len, 0, "Gen 1 never has leftover actions at a request");
        let mut list = Queue::default();
        for p in 0..2 {
            let Some(c) = chosen[p] else { continue };
            let side = p as u8;
            let a = match c {
                Chosen::Switch(t) => Action { kind: ActionKind::Switch, order: 0, priority: 0, speed: 0, side, data: t },
                Chosen::InstaSwitch(t) => Action { kind: ActionKind::InstaSwitch, order: 0, priority: 0, speed: 0, side, data: t },
                _ => {
                    let id = self.commit_move_choice(p, c);
                    Action { kind: ActionKind::Move, order: 0, priority: 0, speed: 0, side, data: id }
                }
            };
            list.push(self.resolve_action(a));
        }
        for sd in self.sides.iter_mut() {
            sd.request = Request::None;
        }
        // queue.sort()
        let n = list.len as usize;
        speed_sort(&mut self.rng, &mut list.list[..n], |a, b| compare(&prio(a), &prio(b)), "queue.sort");
        self.queue = list;
        // turnLoop
        if !self.mid_turn {
            let bt = self.resolve_action(Action { kind: ActionKind::BeforeTurn, order: 0, priority: 0, speed: 0, side: 0, data: 0 });
            self.insert_action(bt, None);
            let res = self.resolve_action(Action { kind: ActionKind::Residual, order: 0, priority: 0, speed: 0, side: 0, data: 0 });
            self.queue.push(res);
            self.mid_turn = true;
        }
        self.run_queue(s);
    }

    /// `chooseMove`/`chooseSwitch` during a move request.
    fn choose_move_request(&mut self, p: Player, c: Choice) -> Chosen {
        match c {
            Choice::Switch(k) => Chosen::Switch(self.sides[p].order[k as usize - 1]),
            Choice::Move(k) => {
                let (moves, n, _) = self.move_request_data(p);
                assert!(k >= 1 && (k as usize) <= n, "player {p}: no move {k}");
                let moveid = moves[k as usize - 1].id;
                // chooseMove: locked > Gen 1 'fight' > struggle > the chosen slot.
                if let Some(l) = self.locked_move(p).or_else(|| self.semi_locked_move(p, false)) {
                    return Chosen::Locked(l);
                }
                let mon = self.sides[p].mon();
                if matches!(mon.status, st::FRZ | st::SLP) || self.sides[p].active.has(v::PARTIALLYTRAPPED) {
                    return Chosen::Fight;
                }
                if !self.has_valid_move(p) {
                    return Chosen::Struggle;
                }
                assert!(!self.sides[p].slot(k as usize - 1).disabled, "player {p}: move {k} is disabled");
                Chosen::Move { id: moveid, slot: k - 1 }
            }
            Choice::Pass => panic!("player {p} must choose (move request)"),
        }
    }

    /// `Side#commitChoices` (Gen 1 branch) for a move action; returns the move id to execute
    /// (`choice.moveid = this.lastSelectedMove`).
    fn commit_move_choice(&mut self, p: Player, c: Chosen) -> u8 {
        let side = &mut self.sides[p];
        match c {
            Chosen::Fight => {
                let status = side.mon().status;
                if matches!(status, st::FRZ | st::SLP) {
                    let i = side.last_selected_move_slot as usize;
                    assert!(i < side.active.n_slots as usize, "moveSlot is null");
                    side.last_enemy_selected_move = side.slot(i).id;
                } else if side.active.has(v::PARTIALLYTRAPPED) {
                    side.last_selected_move = pm::CANNOTMOVE;
                    side.last_enemy_selected_move = pm::CANNOTMOVE;
                }
            }
            Chosen::Struggle => {
                side.last_selected_move = pm::STRUGGLE;
                side.last_enemy_selected_move = pm::STRUGGLE;
            }
            Chosen::Move { id, slot } => {
                side.last_selected_move = id;
                side.last_selected_move_slot = slot;
                side.last_move = id;
                let i = side.last_selected_move_slot as usize;
                assert!(i < side.active.n_slots as usize, "moveSlot is null");
                side.last_enemy_selected_move = side.slot(i).id;
            }
            Chosen::Locked(_) => {
                let i = side.last_selected_move_slot as usize;
                assert!(i < side.active.n_slots as usize, "moveSlot is null");
                side.last_enemy_selected_move = side.slot(i).id;
            }
            Chosen::Switch(_) | Chosen::InstaSwitch(_) => unreachable!(),
        }
        side.last_selected_move
    }

    // ---- BattleQueue -------------------------------------------------------------------------

    /// `resolveAction` + `getActionSpeed`: fill in order, priority and speed.
    pub(crate) fn resolve_action(&mut self, mut a: Action) -> Action {
        let p = a.side as usize;
        a.order = match a.kind {
            ActionKind::InstaSwitch => 3,
            ActionKind::BeforeTurn => 4,
            ActionKind::RunSwitch => 101,
            ActionKind::Switch => 103,
            ActionKind::Move => 200,
            ActionKind::Residual => 300,
        };
        a.priority = 0;
        match a.kind {
            ActionKind::Move => {
                if a.data < 166 {
                    a.priority = MOVES[a.data as usize].priority as i32;
                }
                a.speed = self.sides[p].active.modified[4] as i64;
            }
            ActionKind::Switch | ActionKind::InstaSwitch => {
                // resolveAction: action.pokemon.switchFlag = false
                self.sides[p].active.switch_flag = false;
                a.speed = self.sides[p].active.modified[4] as i64;
            }
            ActionKind::RunSwitch => a.speed = self.sides[p].active.modified[4] as i64,
            ActionKind::BeforeTurn | ActionKind::Residual => a.speed = 1,
        }
        a
    }

    /// `BattleQueue#insertChoice` for an already-resolved action. `pokemon` (side) gets its
    /// `speed` updated first, as Showdown calls `updateSpeed` on `choice.pokemon`.
    pub(crate) fn insert_action(&mut self, a: Action, pokemon: Option<Player>) {
        if let Some(p) = pokemon {
            self.update_speed_one(p);
        }
        let pa = prio(&a);
        let mut first = None;
        let mut last = None;
        for (i, cur) in self.queue.as_slice().iter().enumerate() {
            let c = compare(&pa, &prio(cur));
            if c <= 0 && first.is_none() {
                first = Some(i);
            }
            if c < 0 {
                last = Some(i);
                break;
            }
        }
        match first {
            None => self.queue.push(a),
            Some(f) => {
                let l = last.unwrap_or(self.queue.len as usize);
                let index = if f == l {
                    f
                } else if self.rules.compat == Compat::Pkmn {
                    l
                } else {
                    self.rng.random_range(f as u32, l as u32 + 1, "queue.insertChoice") as usize
                };
                self.queue.insert(index, a);
            }
        }
    }

    // ---- runAction ---------------------------------------------------------------------------

    fn run_action<S: Sink>(&mut self, a: Action, s: &mut S) {
        let p = a.side as usize;
        match a.kind {
            ActionKind::Move => {
                let mon = self.sides[p].mon();
                if mon.fainted || a.speed < 0 {
                    return;
                }
                self.run_move(p, a.data, s);
            }
            ActionKind::Switch | ActionKind::InstaSwitch => {
                self.switch_in(p, a.data, a.kind == ActionKind::Switch, s);
            }
            ActionKind::RunSwitch => self.run_switch(p, s),
            ActionKind::BeforeTurn => self.each_event("eachEvent:BeforeTurn"),
            ActionKind::Residual => {
                self.update_speed();
                self.residual_event(s);
            }
        }
        self.action_tail(Some(a), s);
    }

    /// The common tail of `runAction` (faints, forced-switch requests, `eachEvent('Update')`).
    fn action_tail<S: Sink>(&mut self, _a: Option<Action>, s: &mut S) {
        self.faint_messages(s);
        if self.ended {
            return;
        }
        match self.queue.peek().map(|a| a.kind) {
            None | Some(ActionKind::Move) | Some(ActionKind::Residual) => self.check_fainted(),
            Some(ActionKind::InstaSwitch) => return,
            _ => {}
        }
        let mut switches = [false; 2];
        for p in 0..2 {
            switches[p] = self.sides[p].active.switch_flag;
            if switches[p] && self.can_switch(p) == 0 {
                self.sides[p].active.switch_flag = false;
                switches[p] = false;
            }
            // (BeforeSwitchOut only runs for non-fainted switchers, which Gen 1 never has.)
        }
        if switches[0] || switches[1] {
            self.make_switch_request();
            return;
        }
        self.each_event("eachEvent:Update");
    }

    /// `possibleSwitches(side).length`.
    pub fn can_switch(&self, p: Player) -> usize {
        let sd = &self.sides[p];
        if sd.left == 0 {
            return 0;
        }
        (1..sd.n as usize).filter(|&i| !sd.mons[sd.order[i] as usize].fainted).count()
    }

    /// `Battle#eachEvent` for events without Gen 1 handlers (Update, BeforeTurn): only the
    /// speed sort of the active Pokémon matters, as a tie consumes a PRNG call.
    pub(crate) fn each_event(&mut self, label: &'static str) {
        if self.rules.compat == Compat::Pkmn {
            return;
        }
        let mut actives = [0u16; 2];
        let mut n = 0;
        for p in 0..2 {
            let m = self.sides[p].mon();
            if self.started_side(p) && !m.fainted {
                actives[n] = m.speed;
                n += 1;
            }
        }
        if n == 2 && actives[0] == actives[1] {
            let mut l = actives;
            speed_sort(&mut self.rng, &mut l, |a, b| *b as i64 - *a as i64, label);
        }
    }

    #[inline]
    fn started_side(&self, p: Player) -> bool {
        self.sides[p].active.species != 0
    }

    /// `Battle#updateSpeed`: refresh the cached `speed` of every non-fainted active.
    pub(crate) fn update_speed(&mut self) {
        for p in 0..2 {
            if self.started_side(p) && !self.sides[p].mon().fainted {
                self.update_speed_one(p);
            }
        }
    }

    #[inline]
    pub(crate) fn update_speed_one(&mut self, p: Player) {
        let spe = self.sides[p].active.modified[4];
        self.sides[p].mon_mut().speed = spe;
    }

    // ---- Switching -------------------------------------------------------------------------

    /// `BattleActions#switchIn(pokemon, 0)`.
    pub(crate) fn switch_in<S: Sink>(&mut self, p: Player, incoming: u8, voluntary: bool, s: &mut S) {
        let q = 1 - p;
        let side_started = self.started_side(p);
        if side_started {
            let out = self.sides[p].active_slot();
            let alive = self.sides[p].mons[out].hp > 0;
            if alive {
                // BeforeSwitchOut / SwitchOut: no Gen 1 handlers. cancelAction(oldActive).
                self.cancel_action(p);
                self.clear_volatile(p, true);
            }
            if self.sides[p].mons[out].fainted {
                self.sides[p].mons[out].status = st::NONE;
            }
        }
        let _ = voluntary;
        // Swap positions in side.pokemon.
        let sd = &mut self.sides[p];
        let pos = sd.order.iter().position(|&o| o == incoming).expect("incoming not in team");
        if side_started {
            sd.order.swap(0, pos);
        }
        // The incoming Pokémon's volatile state is what its last clearVolatile left.
        self.init_active(p);
        s.event(Event::Switch { player: p, slot: incoming });
        let _ = q;
        let rs = self.resolve_action(Action {
            kind: ActionKind::RunSwitch,
            order: 0,
            priority: 0,
            speed: 0,
            side: p as u8,
            data: incoming,
        });
        self.insert_action(rs, Some(p));
    }

    /// Remove `p`'s queued actions (`queue.cancelAction(pokemon)`).
    pub(crate) fn cancel_action(&mut self, p: Player) {
        let mut i = 0;
        while i < self.queue.len as usize {
            let a = self.queue.list[i];
            let bound = matches!(a.kind, ActionKind::Move | ActionKind::Switch | ActionKind::InstaSwitch | ActionKind::RunSwitch);
            if bound && a.side as usize == p {
                self.queue.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Initialise [`Active`] from the team member now in front (`switchIn` keeps whatever the
    /// Pokémon's last `clearVolatile` left: base species/types/moves, unboosted modified stats).
    fn init_active(&mut self, p: Player) {
        let sd = &mut self.sides[p];
        let m = sd.mons[sd.order[0] as usize];
        let sp = &SPECIES[m.species as usize];
        sd.active = Active {
            species: m.species,
            types: sp.types,
            stored: [m.stats[1], m.stats[2], m.stats[3], m.stats[4], m.stats[5]],
            modified: m.modified,
            n_slots: m.n_moves,
            ..Active::default()
        };
    }

    /// `Pokemon#clearVolatile` (+ `setSpecies(baseSpecies)`): reset the active's volatile state;
    /// the bench copy keeps modified stats with burn/paralysis drops re-applied, and `speed`
    /// becomes the unmodified Speed stat.
    pub(crate) fn clear_volatile(&mut self, p: Player, _include_switch_flags: bool) {
        let sd = &mut self.sides[p];
        let i = sd.order[0] as usize;
        let m = &mut sd.mons[i];
        let mut modified = [m.stats[1], m.stats[2], m.stats[3], m.stats[4], m.stats[5]];
        if m.status == st::PAR {
            modified[4] = modify_stat(modified[4], 0.25);
        }
        if m.status == st::BRN {
            modified[0] = modify_stat(modified[0], 0.5);
        }
        m.modified = modified;
        m.speed = m.stats[5];
        let sp = &SPECIES[m.species as usize];
        sd.active = Active {
            species: m.species,
            types: sp.types,
            stored: [m.stats[1], m.stats[2], m.stats[3], m.stats[4], m.stats[5]],
            modified,
            n_slots: m.n_moves,
            ..Active::default()
        };
    }

    /// `BattleActions#runSwitch` (Gen 4 mod, inherited by Gen 1).
    fn run_switch<S: Sink>(&mut self, p: Player, s: &mut S) {
        // runEvent('SwitchIn'): Toxic reverts to regular poison.
        if self.sides[p].mon().status == st::TOX {
            self.sides[p].mon_mut().status = st::PSN;
        }
        // Gen <= 2: lastMove is reset for every active Pokémon.
        for side in self.sides.iter_mut() {
            if side.active.species != 0 && !side.mon().fainted {
                side.active.last_move = 0;
            }
        }
        // Gen 1: lastSelectedMoveSlot resets; a foe partially trapping with Metronome re-selects it.
        self.sides[p].last_selected_move_slot = 0;
        let q = 1 - p;
        if self.sides[q].active.species != 0 && self.sides[q].mon().hp > 0 && self.sides[q].active.has(v::PARTIALTRAPPINGLOCK) {
            let slot = self.sides[q].last_selected_move_slot as usize;
            if slot < self.sides[q].active.n_slots as usize && self.sides[q].slot(slot).id == data::mv::METRONOME {
                self.sides[q].last_selected_move = data::mv::METRONOME;
                self.sides[q].last_enemy_selected_move = data::mv::METRONOME;
                if self.will_move(q) {
                    self.change_action(q, data::mv::METRONOME);
                }
            }
        }
        if !self.sides[p].fainted_this_turn {
            self.after_switch_in_self(p, s);
        }
    }

    /// `queue.willMove(pokemon)`.
    pub(crate) fn will_move(&self, p: Player) -> bool {
        if self.sides[p].mon().fainted {
            return false;
        }
        self.queue.as_slice().iter().any(|a| a.kind == ActionKind::Move && a.side as usize == p)
    }

    /// `queue.changeAction(pokemon, {choice: 'move', moveid})`.
    pub(crate) fn change_action(&mut self, p: Player, moveid: u8) {
        self.cancel_action(p);
        self.update_speed_one(p);
        let a = self.resolve_action(Action { kind: ActionKind::Move, order: 0, priority: 0, speed: 0, side: p as u8, data: moveid });
        self.insert_action(a, None);
    }

    // ---- Fainting ----------------------------------------------------------------------------

    /// `Pokemon#faint`: queue the faint (HP 0).
    pub(crate) fn faint(&mut self, p: Player) {
        let slot = self.sides[p].active_slot();
        let m = &mut self.sides[p].mons[slot];
        if m.fainted || m.faint_queued {
            return;
        }
        m.hp = 0;
        self.sides[p].active.switch_flag = false;
        m.faint_queued = true;
        self.faint_queue[self.n_faint as usize] = Faint { side: p as u8, slot: slot as u8 };
        self.n_faint += 1;
    }

    /// `Battle#faintMessages`.
    pub(crate) fn faint_messages<S: Sink>(&mut self, s: &mut S) -> bool {
        if self.ended {
            return false;
        }
        if self.n_faint == 0 {
            return false;
        }
        let mut last: Option<Faint> = None;
        let n = self.n_faint as usize;
        for k in 0..n {
            let f = self.faint_queue[k];
            let p = f.side as usize;
            let slot = f.slot as usize;
            if self.sides[p].mons[slot].fainted {
                continue;
            }
            s.event(Event::Faint { player: p, slot: f.slot });
            if self.sides[p].left > 0 {
                self.sides[p].left -= 1;
            }
            // clearVolatile(false) runs on the fainted Pokémon (still side.active[0]).
            debug_assert_eq!(self.sides[p].active_slot(), slot);
            let switch_flag = self.sides[p].active.switch_flag;
            self.clear_volatile(p, false);
            self.sides[p].active.switch_flag = switch_flag;
            self.sides[p].mons[slot].fainted = true;
            self.sides[p].fainted_this_turn = true;
            last = Some(f);
        }
        self.n_faint = 0;
        // Gen 1: fainting skips the rest of the turn.
        self.queue.clear();
        let _ = last;
        self.check_win()
    }

    /// `Battle#checkWin`.
    fn check_win(&mut self) -> bool {
        if self.sides[0].left == 0 && self.sides[1].left == 0 {
            self.win(3);
            return true;
        }
        for p in 0..2 {
            if self.sides[1 - p].left == 0 {
                self.win(p as u8 + 1);
                return true;
            }
        }
        false
    }

    fn win(&mut self, w: u8) {
        if self.ended {
            return;
        }
        self.winner = w;
        self.ended = true;
        for sd in self.sides.iter_mut() {
            sd.request = Request::None;
        }
    }

    /// `Battle#checkFainted`.
    fn check_fainted(&mut self) {
        for p in 0..2 {
            if self.started_side(p) && self.sides[p].mon().fainted {
                self.sides[p].mon_mut().status = st::FNT;
                self.sides[p].active.switch_flag = true;
            }
        }
    }

    fn make_switch_request(&mut self) {
        for p in 0..2 {
            let sd = &mut self.sides[p];
            sd.request = if sd.left > 0 && sd.active.switch_flag { Request::Switch } else { Request::Wait };
        }
    }

    // ---- endTurn -------------------------------------------------------------------------------

    fn end_turn<S: Sink>(&mut self, s: &mut S) {
        self.turn += 1;
        // Gen 1 partial trapping ends when either Pokémon is gone.
        for p in 0..2 {
            if !self.started_side(p) || self.sides[p].mon().fainted {
                continue;
            }
            let q = 1 - p;
            let foe_slot = self.sides[q].active_slot();
            let a = self.sides[p].active;
            let foe_has = |b: &Battle, slot: u8, vol: u8| -> bool {
                // A Pokémon that is not active has had its volatiles cleared.
                slot as usize == foe_slot && b.sides[q].mons[slot as usize].hp > 0 && b.sides[q].active.has(vol)
            };
            if a.has(v::PARTIALTRAPPINGLOCK) {
                let t = a.ptl_locked;
                if self.sides[q].mons[t as usize].hp == 0 || !foe_has(self, t, v::PARTIALLYTRAPPED) {
                    self.del_vol(p, v::PARTIALTRAPPINGLOCK);
                }
            }
            if a.has(v::PARTIALLYTRAPPED) {
                let t = a.pt_source;
                if self.sides[q].mons[t as usize].hp == 0 || !foe_has(self, t, v::PARTIALTRAPPINGLOCK) {
                    self.del_vol(p, v::PARTIALLYTRAPPED);
                }
            }
            if self.sides[p].active.has(v::FAKEPARTIALLYTRAPPED) {
                let t = a.fpt_counterpart;
                if self.sides[q].mons[t as usize].hp == 0 || !foe_has(self, t, v::FAKEPARTIALLYTRAPPED) {
                    self.del_vol(p, v::FAKEPARTIALLYTRAPPED);
                }
            }
        }
        for p in 0..2 {
            if !self.started_side(p) {
                continue;
            }
            let sd = &mut self.sides[p];
            sd.active.move_this_turn = 0;
            sd.active.maybe_disabled = false;
            sd.active.maybe_locked = false;
            for i in 0..sd.active.n_slots as usize {
                sd.slot_mut(i).disabled = false;
            }
            if !sd.mon().fainted {
                self.disable_move_event(p);
            }
            let sd = &mut self.sides[p];
            sd.active.trapped = false;
        }
        for sd in self.sides.iter_mut() {
            sd.fainted_this_turn = false;
        }
        if self.endless_battle_check() {
            return;
        }
        if self.turn > 1000 {
            self.win(3);
            return;
        }
        s.event(Event::Turn { turn: self.turn });
        // makeRequest('move')
        for p in 0..2 {
            if self.sides[p].left > 0 {
                self.sides[p].request = Request::Move;
                self.move_request_data(p);
            } else {
                self.sides[p].request = Request::Wait;
            }
        }
    }

    /// Gen 1 Endless Battle Clause check (`maybeTriggerEndlessBattleClause`).
    fn endless_battle_check(&mut self) -> bool {
        let stuck = (0..2).all(|p| {
            let q = 1 - p;
            let foe_all_ghosts = (0..self.sides[q].n as usize).all(|i| {
                let m = &self.sides[q].mons[i];
                m.fainted || self.mon_types(q, i).contains(&7)
            });
            let foe_all_transform = (0..self.sides[q].n as usize).all(|i| {
                self.sides[q].mons[i].fainted || self.mon_move_ids(q, i).iter().all(|&m| m == data::mv::TRANSFORM)
            });
            (0..self.sides[p].n as usize).all(|i| {
                let m = &self.sides[p].mons[i];
                m.fainted
                    || m.status == st::FRZ
                    || (self.mon_move_ids(p, i).iter().all(|&mv| mv == data::mv::TRANSFORM) && foe_all_transform)
                    || (self.mon_move_pps(p, i).iter().all(|&pp| pp == 0) && foe_all_ghosts)
            })
        });
        if stuck {
            self.win(3);
        }
        stuck
    }

    /// Current types of team member `i` (the active's may have changed).
    fn mon_types(&self, p: Player, i: usize) -> [u8; 2] {
        let sd = &self.sides[p];
        if i == sd.active_slot() && sd.active.species != 0 {
            sd.active.types
        } else {
            SPECIES[sd.mons[i].species as usize].types
        }
    }

    /// `pokemon.moves` (current move slot ids).
    fn mon_move_ids(&self, p: Player, i: usize) -> ArrayVec4 {
        let sd = &self.sides[p];
        let mut out = ArrayVec4::default();
        if i == sd.active_slot() && sd.active.species != 0 {
            for k in 0..sd.active.n_slots as usize {
                out.push(sd.slot(k).id);
            }
        } else {
            for k in 0..sd.mons[i].n_moves as usize {
                out.push(sd.mons[i].moves[k].id);
            }
        }
        out
    }

    fn mon_move_pps(&self, p: Player, i: usize) -> ArrayVec4 {
        let sd = &self.sides[p];
        let mut out = ArrayVec4::default();
        if i == sd.active_slot() && sd.active.species != 0 {
            for k in 0..sd.active.n_slots as usize {
                out.push(sd.slot(k).pp);
            }
        } else {
            for k in 0..sd.mons[i].n_moves as usize {
                out.push(sd.mons[i].moves[k].pp);
            }
        }
        out
    }

    // ---- Requests ----------------------------------------------------------------------------

    /// `Pokemon#getLockedMove` (LockMove handlers in volatile insertion order).
    pub(crate) fn locked_move(&self, p: Player) -> Option<u8> {
        let a = &self.sides[p].active;
        let mut best: Option<(u16, u8)> = None;
        for (vol, val) in [
            (v::MUSTRECHARGE, req::RECHARGE),
            (v::LOCKEDMOVE, a.lockedmove_move),
            (v::TWOTURNMOVE, a.twoturn_move),
            (v::RAGE, data::mv::RAGE),
        ] {
            if a.has(vol) && best.is_none_or(|(st, _)| a.vstamp[vol as usize] < st) {
                best = Some((a.vstamp[vol as usize], val));
            }
        }
        best.map(|(_, v)| v)
    }

    /// `Pokemon#getSemiLockedMove(restrictData)`.
    pub(crate) fn semi_locked_move(&self, p: Player, restrict: bool) -> Option<u8> {
        let a = &self.sides[p].active;
        if restrict && a.maybe_locked {
            return None;
        }
        let mut best: Option<(u16, u8)> = None;
        for (vol, val) in [(v::PARTIALTRAPPINGLOCK, a.ptl_move), (v::BIDE, data::mv::BIDE)] {
            if a.has(vol) && best.is_none_or(|(st, _)| a.vstamp[vol as usize] < st) {
                best = Some((a.vstamp[vol as usize], val));
            }
        }
        best.map(|(_, v)| v)
    }

    /// Whether `getMoves()` (unrestricted) has an enabled move.
    fn has_valid_move(&self, p: Player) -> bool {
        let sd = &self.sides[p];
        (0..sd.active.n_slots as usize).any(|i| {
            let s = sd.slot(i);
            !s.disabled && s.pp > 0
        })
    }

    /// `Pokemon#getMoveRequestData` for the active Pokémon: applies its side effects (trapped,
    /// maybeLocked) and returns the request's move list and whether it is `trapped`.
    pub(crate) fn move_request_data(&mut self, p: Player) -> ([ReqMove; 4], usize, bool) {
        let mut locked = self.locked_move(p);
        let hard = locked.is_some();
        if hard {
            self.sides[p].active.trapped = true;
        } else {
            locked = if self.sides[p].active.maybe_locked { None } else { self.semi_locked_move(p, true) };
        }
        let sd = &self.sides[p];
        let mut moves = [ReqMove { id: 0, disabled: false }; 4];
        let mut n = 0;
        if let Some(l) = locked {
            moves[0].id = l;
            n = 1;
        } else {
            let mut valid = false;
            for i in 0..sd.active.n_slots as usize {
                let s = sd.slot(i);
                let disabled = s.disabled || s.pp == 0;
                valid |= !disabled;
                moves[n] = ReqMove { id: s.id, disabled };
                n += 1;
            }
            if !valid {
                n = 0;
            }
        }
        let status = sd.mon().status;
        if locked.is_none() && (matches!(status, st::FRZ | st::SLP) || (sd.active.has(v::PARTIALLYTRAPPED) && !sd.active.maybe_locked)) {
            moves[0] = ReqMove { id: req::FIGHT, disabled: false };
            n = 1;
        } else if n == 0 {
            moves[0] = ReqMove { id: pm::STRUGGLE, disabled: false };
            n = 1;
        }
        let a = &mut self.sides[p].active;
        let trapped;
        if hard {
            a.maybe_disabled = false;
            a.maybe_locked = false;
            trapped = a.trapped;
        } else {
            a.maybe_locked = a.maybe_locked || a.maybe_disabled;
            trapped = a.trapped;
        }
        (moves, n, trapped)
    }

    /// Legal choices for `player`, as offered by Showdown's request.
    pub fn legal_choices(&self, p: Player, out: &mut [Choice]) -> usize {
        let sd = &self.sides[p];
        let mut n = 0;
        let bench = |out: &mut [Choice], n: &mut usize| {
            for k in 2..=sd.n as usize {
                if !sd.mons[sd.order[k - 1] as usize].fainted {
                    out[*n] = Choice::Switch(k as u8);
                    *n += 1;
                }
            }
        };
        match sd.request {
            Request::Switch => {
                bench(out, &mut n);
                if n == 0 {
                    out[0] = Choice::Pass;
                    n = 1;
                }
            }
            Request::Move => {
                // getMoveRequestData is idempotent once applied by makeRequest.
                let mut tmp = *self;
                let (moves, m, trapped) = tmp.move_request_data(p);
                for (i, mv) in moves[..m].iter().enumerate() {
                    if !mv.disabled {
                        out[n] = Choice::Move(i as u8 + 1);
                        n += 1;
                    }
                }
                if !trapped {
                    bench(out, &mut n);
                }
            }
            _ => {}
        }
        n
    }
}

/// A validated choice before `commitChoices`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chosen {
    /// A normal move selection (`moveSlot` set).
    Move { id: u8, slot: u8 },
    /// A locked/semi-locked move (no `moveSlot`).
    Locked(u8),
    /// Gen 1 frozen/asleep/partially trapped `'fight'`.
    Fight,
    Struggle,
    Switch(u8),
    InstaSwitch(u8),
}

impl Side {
    fn new(team: &[MonSpec]) -> Side {
        assert!(!team.is_empty() && team.len() <= MAX_TEAM);
        let mut mons = [Mon::default(); MAX_TEAM];
        for (i, spec) in team.iter().enumerate() {
            let sp = &SPECIES[spec.species as usize];
            let stats = calc_stats(sp.base, spec.ivs, spec.evs, spec.level);
            let mut m = Mon {
                species: spec.species,
                level: spec.level,
                ivs: spec.ivs.map(|x| x & 30),
                evs: spec.evs,
                stats,
                hp: stats[0],
                modified: [stats[1], stats[2], stats[3], stats[4], stats[5]],
                speed: stats[5],
                ..Mon::default()
            };
            for (k, &id) in spec.moves.iter().enumerate() {
                if id == 0 {
                    continue;
                }
                let pp = calc_pp(MOVES[id as usize].pp, 3);
                m.moves[m.n_moves as usize] = MoveSlot { id, pp, maxpp: pp, disabled: false };
                m.n_moves += 1;
                let _ = k;
            }
            mons[i] = m;
        }
        let mut order = [0u8; MAX_TEAM];
        for (i, o) in order.iter_mut().enumerate() {
            *o = i as u8;
        }
        Side {
            mons,
            n: team.len() as u8,
            order,
            active: Active::default(),
            left: team.len() as u8,
            last_selected_move: pm::NOMOVE,
            last_enemy_selected_move: pm::NOMOVE,
            last_selected_move_slot: 0,
            last_move: 0,
            last_enemy_move: 0,
            fainted_this_turn: false,
            request: Request::None,
        }
    }
}

#[inline]
fn prio(a: &Action) -> Priority {
    Priority { order: a.order, priority: a.priority, speed: a.speed, sub_order: 0, effect_order: 0 }
}

/// `modifyStat`: `max(1, floor(stat * modifier))` in floating point, as Showdown computes it.
#[inline]
pub(crate) fn modify_stat(stat: u16, modifier: f64) -> u16 {
    ((stat as f64 * modifier).floor() as i64).max(1) as u16
}

/// A tiny fixed-capacity vec of up to 4 bytes.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ArrayVec4 {
    pub len: usize,
    pub items: [u8; 4],
}

impl ArrayVec4 {
    pub fn push(&mut self, v: u8) {
        self.items[self.len] = v;
        self.len += 1;
    }
    pub fn iter(&self) -> std::slice::Iter<'_, u8> {
        self.items[..self.len].iter()
    }
}

#[allow(dead_code)]
const _: u64 = ORDER_UNSET;
