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

const PRIORITIES = [
  ['rule', 'sleepclausemod', 'onSetStatusPriority', -999],
  ['rule', 'freezeclausemod', 'onSetStatusPriority', -998],
  ['condition', 'disable', 'onDisableMovePriority', 7],
];

function setPriorities(dex, on) {
  for (const [kind, id, field, value] of PRIORITIES) {
    const effect = kind === 'rule' ? dex.formats.get(id) : dex.conditions.get(id);
    if (on) effect[field] = value;
    else delete effect[field];
  }
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
