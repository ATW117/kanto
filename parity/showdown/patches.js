// pkmn/engine's "patched Showdown" (third_party/pkmn-engine/src/test/showdown.ts, docs/TESTING.md
// "Patches"), ported to pokemon-showdown 0.11.11 for Gen 1. Selected with spec.patch = "pkmn".
//
// The patches remove Showdown's artificial speed ties in Gen 1-2:
//  1. `Battle#eachEvent` and `Battle#fieldEvent` do not speed-sort before Gen 3 (handlers run in
//     the order they were found: "host" order, P1 before P2).
//  2. `BattleQueue#insertChoice` inserts ties at the end of the tied range before Gen 3 instead of
//     at a random position.
//  3. Handler priorities break ties that would otherwise be shuffled: Sleep/Freeze Clause Mod's
//     onSetStatus, and Gen 1 Disable's onDisableMove vs Bide.
// Vanilla Showdown (the default) is the harness's ground truth; these exist so the native sim's
// `Compat::Pkmn` mode (and pkmn/engine itself) can be checked against the same reference.
'use strict';

// Showdown deep-freezes Condition objects once built, so the condition priority must be set on
// the dex *data* before the condition is first used: the runner applies the patch once at
// process start (PS_PATCH=pkmn) and every spec in that process must use the same mode.
let applied = null;

function setPriorities(dex, on) {
  if (applied !== null && applied !== on) {
    throw new Error('vanilla and pkmn-patched specs cannot share a runner process (set PS_PATCH)');
  }
  if (applied === on) return;
  applied = on;
  if (!on) return;
  dex.includeData();
  // Rules with handlers become field pseudo-weathers whose Condition is built from the rulesets
  // data, so the priority goes on the data (and on the Format object for good measure).
  for (const [id, prio] of [['sleepclausemod', -999], ['freezeclausemod', -998]]) {
    dex.data.Rulesets[id].onSetStatusPriority = prio;
    dex.formats.get(id).onSetStatusPriority = prio;
  }
  dex.data.Moves.disable.condition.onDisableMovePriority = 7;
}

function insertChoice(choices, midTurn = false) {
  if (Array.isArray(choices)) {
    for (const choice of choices) this.insertChoice(choice);
    return;
  }
  const choice = choices;
  if (choice.pokemon) choice.pokemon.updateSpeed();
  const actions = this.resolveAction(choice, midTurn);
  let firstIndex = null;
  let lastIndex = null;
  for (const [i, curAction] of this.list.entries()) {
    const compared = this.battle.comparePriority(actions[0], curAction);
    if (compared <= 0 && firstIndex === null) firstIndex = i;
    if (compared < 0) {
      lastIndex = i;
      break;
    }
  }
  if (firstIndex === null) {
    this.list.push(...actions);
  } else {
    if (lastIndex === null) lastIndex = this.list.length;
    // PATCH: "host" ordering before Gen 3
    const index = firstIndex === lastIndex ? firstIndex :
      this.battle.gen > 2 ? this.battle.random(firstIndex, lastIndex + 1) : lastIndex;
    this.list.splice(index, 0, ...actions);
  }
}

function patchBattle(battle) {
  const origEach = battle.eachEvent.bind(battle);
  const origField = battle.fieldEvent.bind(battle);
  const speedSort = battle.speedSort.bind(battle);
  battle.queue.insertChoice = insertChoice.bind(battle.queue);
  // PATCH: no speed sort in eachEvent/fieldEvent before Gen 3. In both functions the first
  // speedSort call is the one sorting the actives/handlers (handler discovery never sorts), so
  // skip exactly that call; nested runEvent sorts are unaffected.
  battle.speedSort = function (list, cmp) {
    if (this.__skipSort) {
      this.__skipSort = false;
      return;
    }
    return speedSort(list, cmp);
  };
  battle.eachEvent = function (...args) {
    if (this.gen < 3) this.__skipSort = true;
    return origEach(...args);
  };
  battle.fieldEvent = function (...args) {
    if (this.gen < 3) this.__skipSort = true;
    return origField(...args);
  };
}

module.exports = {
  generation: dex => setPriorities(dex, true),
  ungeneration: dex => setPriorities(dex, false),
  battle: patchBattle,
};
