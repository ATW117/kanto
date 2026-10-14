#!/usr/bin/env node
// Single-threaded Pokémon Showdown (JavaScript) throughput, for comparison with kanto's engine
// benchmark: random legal self-play on a team pool, `[Gen 1] OU`, one Node process.
//
// Usage: node bench/showdown_bench.js <pokemon-showdown dir> [teams.jsonl] [battles]
//   <pokemon-showdown dir>  an npm install of pokemon-showdown@0.11.11 (node_modules/pokemon-showdown)
//   teams.jsonl             default data/teams/gen1ou/teams.jsonl
//   battles                 default 2000
//
// Battles are driven synchronously through `new Battle(...)` and `battle.choose(...)` (no
// BattleStream, no team validation). Each side picks uniformly among its legal choices, read from
// its request. Caveat: Showdown always builds its text log (`Battle#log`), so this measures the
// simulator *with* protocol output; kanto's and pkmn's engine numbers are without it.
'use strict';
const fs = require('fs');
const path = require('path');

const PS = process.argv[2];
if (!PS) {
  console.error('usage: node bench/showdown_bench.js <pokemon-showdown dir> [teams.jsonl] [battles]');
  process.exit(2);
}
const TEAMS = process.argv[3] || path.join(__dirname, '..', 'data/teams/gen1ou/teams.jsonl');
const N = +process.argv[4] || 2000;
const {Battle} = require(path.resolve(PS, 'dist/sim'));
const version = require(path.resolve(PS, 'package.json')).version;

const teams = fs.readFileSync(TEAMS, 'utf8').trim().split('\n').map(l => JSON.parse(l).pokemon.map(p => ({
  species: p.species, moves: p.moves, level: p.level || 100,
})));

// Choice RNG (separate from the battle PRNG).
let s = 12345;
const rand = () => (s = (Math.imul(s, 1103515245) + 12345) >>> 0);

function legal(side) {
  const r = side.activeRequest;
  if (!r || r.wait) return [];
  const bench = [];
  for (let k = 2; k <= side.pokemon.length; k++) if (!side.pokemon[k - 1].fainted) bench.push(`switch ${k}`);
  if (r.forceSwitch) return bench.length ? bench : ['pass'];
  const out = [];
  r.active[0].moves.forEach((m, i) => { if (!m.disabled) out.push(`move ${i + 1}`); });
  if (!r.active[0].trapped) out.push(...bench);
  return out;
}

function play(i) {
  const b = new Battle({formatid: 'gen1ou', seed: [i, i + 1, i + 2, i + 3]});
  b.setPlayer('p1', {name: 'a', team: teams[rand() % teams.length]});
  b.setPlayer('p2', {name: 'b', team: teams[rand() % teams.length]});
  for (let st = 0; st < 1100 && !b.ended; st++) {
    for (const side of b.sides) {
      const L = legal(side);
      if (L.length) b.choose(side.id, L[rand() % L.length]);
    }
  }
  return b.turn;
}

for (let i = 0; i < 100; i++) play(i); // warm-up (JIT)
let turns = 0;
const t0 = process.hrtime.bigint();
for (let i = 0; i < N; i++) turns += play(1000 + i);
const secs = Number(process.hrtime.bigint() - t0) / 1e9;
console.log(`pokemon-showdown ${version} (JS), 1 thread: ${N} battles, ${(N / secs).toFixed(1)} battles/s, ` +
  `${(turns / secs).toFixed(0)} turns/s, ${(turns / N).toFixed(1)} turns/battle`);
