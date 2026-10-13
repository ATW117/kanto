#!/usr/bin/env node
// Pokémon Showdown runner for the parity harness (docs/SIM_DESIGN.md, "Parity harness").
//
// Reads battle specs as JSON lines on stdin and writes one JSON trace line per battle on stdout.
// It drives Showdown's `Battle` synchronously (no BattleStream), so it is deterministic.
//
// Spec fields:
//   id           any identifier, echoed back
//   format       "gen1ou" (Sleep + Freeze Clause Mod) | "gen1nc" (gen1ou without the clauses)
//   patch        "vanilla" (default) | "pkmn" (pkmn/engine's Showdown patches, see patches.js)
//   seed         battle seed string ("a,b,c,d" Gen 5 LCG, or "sodium,<hex>")
//   p1, p2       teams: arrays of Showdown sets {species, moves, level, ivs, evs}
//   choices      optional [[c1, c2], ...] Showdown choice strings to replay ("" = no choice)
//   choiceSeed   used when `choices` is absent/exhausted: random legal choices from a separate
//                mulberry32 stream, so the battle PRNG is untouched by choice selection
//   maxSteps     cap on decision steps (default 1100)
//   rng          record every battle PRNG call with its call site (default true)
//
// Trace: {id, seed, steps: [Step], end: {winner, turn, ended}, error?}
//   steps[0] is the state after the battle starts (leads sent out); each later step is one
//   commitChoices. Step = {choices, legal, requests, log, rng, state}; `legal[p]` lists the
//   choice strings offered by p's request *before* this step's choices, `rng` is
//   [[label, lo, hi, value], ...] for PRNG calls during the step, `state` is the snapshot after.
'use strict';
const path = require('path');
const readline = require('readline');

const PS_DIR = process.env.PS_DIR || process.argv[2];
if (!PS_DIR) {
  console.error('usage: PS_DIR=<pokemon-showdown dir> runner.js  (or pass the dir as argv[2])');
  process.exit(2);
}
const {Battle, Dex} = require(path.resolve(PS_DIR, 'dist/sim'));
const patches = require('./patches');

const FORMATS = {
  gen1ou: 'gen1ou',
  gen1nc: 'gen1ou@@@!Sleep Clause Mod,!Freeze Clause Mod',
};

function mulberry32(a) {
  return function () {
    a |= 0; a = (a + 0x6D2B79F5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0);
  };
}

// ---- PRNG call-site labels ------------------------------------------------------------------
const FRAME = /^\s+at (?:(\S+) )?\(?(.*?):(\d+):\d+\)?$/;
const SKIP = new Set(['PRNG.random', 'PRNG.randomChance', 'PRNG.sample', 'PRNG.shuffle', 'Battle.random',
  'Battle.randomChance', 'Battle.sample', 'prng.random', 'Object.random', 'traced']);
function callSite() {
  const lines = new Error().stack.split('\n').slice(2);
  const out = [];
  for (const line of lines) {
    const m = FRAME.exec(line);
    if (!m) continue;
    const fn = m[1] || '?';
    const file = path.basename(m[2]);
    if (SKIP.has(fn) || fn.endsWith('.random') || file === 'runner.js' || file === 'prng.js') continue;
    out.push(`${fn}@${file}:${m[3]}`);
    if (out.length === 3) break;
  }
  return out.join(' < ');
}

function traceRng(battle, sink) {
  const prng = battle.prng;
  const random = prng.random.bind(prng);
  prng.random = function traced(from, to) {
    const value = random(from, to);
    let lo = 0;
    let hi = 0;
    if (from !== undefined && to === undefined) hi = from;
    else if (from !== undefined) { lo = from; hi = to; }
    sink.push([callSite(), lo, hi, value]);
    return value;
  };
}

// ---- State snapshot -----------------------------------------------------------------------------
const VOL_KEYS = ['time', 'startTime', 'duration', 'totalDuration', 'damage', 'hp', 'counter', 'accuracy',
  'slotIndex', 'move', 'maybeLocked'];
function volSnap(state) {
  const o = {};
  for (const k of VOL_KEYS) {
    let v = state[k];
    if (v === undefined || v === null) continue;
    if (typeof v === 'object') v = v.id;
    o[k] = v;
  }
  return o;
}

function monSnap(p) {
  const o = {hp: p.hp, status: p.status || '', fainted: !!p.fainted, speed: p.speed,
    pp: p.baseMoveSlots.map(m => m.pp)};
  if (p.status === 'slp') o.slp = [p.statusState.time, p.statusState.startTime];
  return o;
}

function activeSnap(a) {
  if (!a) return null;
  const vols = {};
  for (const id of Object.keys(a.volatiles)) vols[id] = volSnap(a.volatiles[id]);
  return {
    slot: a.__orig,
    species: a.species.id,
    types: a.getTypes(),
    boosts: {...a.boosts},
    stats: {...a.modifiedStats},
    stored: {...a.storedStats},
    moves: a.moveSlots.map(m => [m.id, m.pp, !!m.disabled]),
    volatiles: vols,
    lastMove: a.lastMove ? a.lastMove.id : null,
    transformed: !!a.transformed,
  };
}

function sideSnap(side) {
  const team = [];
  for (const p of side.pokemon) team[p.__orig] = monSnap(p);
  return {
    order: side.pokemon.map(p => p.__orig),
    left: side.pokemonLeft,
    lastSelectedMove: side.lastSelectedMove,
    lastEnemySelectedMove: side.lastEnemySelectedMove,
    lastSelectedMoveSlot: side.lastSelectedMoveSlot,
    lastMove: side.lastMove ? side.lastMove.id : null,
    lastEnemyMove: side.lastEnemyMove ? side.lastEnemyMove.id : null,
    team,
    active: activeSnap(side.active[0]),
  };
}

function snapshot(battle) {
  return {
    turn: battle.turn,
    seed: battle.prng.getSeed(),
    lastDamage: battle.lastDamage,
    ended: !!battle.ended,
    winner: battle.winner || '',
    sides: battle.sides.map(sideSnap),
  };
}

// ---- Requests and legal choices --------------------------------------------------------------------
function requestKind(side) {
  const r = side.activeRequest;
  if (!r) return '';
  if (r.wait) return 'wait';
  if (r.forceSwitch) return 'switch';
  if (r.active) return 'move';
  return '?';
}

function legalChoices(side) {
  const r = side.activeRequest;
  if (!r || r.wait) return [];
  const out = [];
  const bench = [];
  for (let k = 2; k <= side.pokemon.length; k++) {
    if (!side.pokemon[k - 1].fainted) bench.push(`switch ${k}`);
  }
  if (r.forceSwitch) return bench.length ? bench : ['pass'];
  const a = r.active[0];
  a.moves.forEach((m, i) => { if (!m.disabled) out.push(`move ${i + 1}`); });
  if (!a.trapped) out.push(...bench);
  return out;
}

// ---- Main ---------------------------------------------------------------------------------------
function runSpec(spec) {
  const format = FORMATS[spec.format || 'gen1ou'];
  if (!format) throw new Error(`unknown format ${spec.format}`);
  const patch = spec.patch || 'vanilla';
  const dex = Dex.mod('gen1');
  if (patch === 'pkmn') patches.generation(dex);
  else patches.ungeneration(dex);
  const battle = new Battle({formatid: format, seed: spec.seed});
  if (patch === 'pkmn') patches.battle(battle);
  const rngCalls = [];
  if (spec.rng !== false) traceRng(battle, rngCalls);
  const steps = [];
  const trace = {id: spec.id, seed: spec.seed, steps};
  let logPos = 0;
  const takeLog = () => {
    const lines = battle.log.slice(logPos).filter(l => !l.startsWith('|t:|') && l !== '|');
    logPos = battle.log.length;
    return lines;
  };
  const record = (choices, legal, requests) => {
    steps.push({choices, legal, requests, log: takeLog(), rng: rngCalls.splice(0), state: snapshot(battle)});
  };

  battle.setPlayer('p1', {name: 'p1', team: spec.p1});
  battle.setPlayer('p2', {name: 'p2', team: spec.p2});
  // Tag each Pokémon with its original team index (Showdown reorders side.pokemon on switches).
  // The battle already started inside setPlayer, so tag in team-spec order via `set` identity.
  for (const side of battle.sides) {
    for (const p of side.pokemon) {
      p.__orig = side.team.indexOf(p.set);
      if (p.__orig < 0) throw new Error('cannot map Pokémon to its team slot');
    }
  }
  record(['', ''], [[], []], ['', '']);

  const rand = mulberry32((spec.choiceSeed ?? 0) >>> 0);
  const maxSteps = spec.maxSteps ?? 1100;
  for (let step = 0; step < maxSteps && !battle.ended; step++) {
    const requests = battle.sides.map(requestKind);
    const legal = battle.sides.map(legalChoices);
    const given = spec.choices && spec.choices[step];
    const choices = ['', ''];
    for (let p = 0; p < 2; p++) {
      if (!legal[p].length) continue;
      if (given) choices[p] = given[p];
      else choices[p] = legal[p][rand() % legal[p].length];
    }
    for (let p = 0; p < 2; p++) {
      if (!choices[p]) continue;
      if (!battle.choose(battle.sides[p].id, choices[p])) {
        throw new Error(`choice rejected: p${p + 1} '${choices[p]}' (legal ${JSON.stringify(legal[p])}): ${battle.sides[p].choice.error}`);
      }
    }
    record(choices, legal, requests);
  }
  trace.end = {winner: battle.winner || '', turn: battle.turn, ended: !!battle.ended};
  return trace;
}

async function main() {
  // Patches that touch frozen dex objects must be applied before any battle runs.
  if (process.env.PS_PATCH === 'pkmn') patches.generation(Dex.mod('gen1'));
  else patches.ungeneration(Dex.mod('gen1'));
  const rl = readline.createInterface({input: process.stdin, crlfDelay: Infinity});
  for await (const line of rl) {
    if (!line.trim()) continue;
    let spec;
    try {
      spec = JSON.parse(line);
    } catch (e) {
      process.stdout.write(JSON.stringify({error: `bad spec: ${e.message}`}) + '\n');
      continue;
    }
    let out;
    try {
      out = runSpec(spec);
    } catch (e) {
      out = {id: spec.id, seed: spec.seed, error: String(e && e.stack || e)};
    }
    process.stdout.write(JSON.stringify(out) + '\n');
  }
}

if (require.main === module) main();
module.exports = {runSpec, snapshot};
