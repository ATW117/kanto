// Replay the input logs from gen_games.py through Showdown's own BattleStream and time it.
// usage: PS_DIR=/path/to/pokemon-showdown node bench/protocol/showdown_bench.js GAMES.jsonl [REPEATS]
'use strict';
const {BattleStream} = require(process.env.PS_DIR + '/dist/sim/battle-stream');
const fs = require('fs');
const games = fs.readFileSync(process.argv[2], 'utf8').split('\n').filter(Boolean).map(l => JSON.parse(l));
const repeats = Number(process.argv[3] || 3);
const inputs = games.map(g => g.input.join('\n'));
let turns = 0, lines = 0;
// Warm-up (JIT), then timed repeats over all games.
for (const s of inputs.slice(0, 200)) new BattleStream()._writeLines(s);
const times = [];
for (let r = 0; r < repeats; r++) {
  turns = 0; lines = 0;
  const t0 = process.hrtime.bigint();
  for (const s of inputs) {
    const stream = new BattleStream();
    stream._writeLines(s);
    turns += stream.battle.turn;
    lines += stream.battle.log.length;
  }
  times.push(Number(process.hrtime.bigint() - t0) / 1e9);
}
times.sort((a, b) => a - b);
const t = times[Math.floor(times.length / 2)];
console.log(JSON.stringify({engine: 'showdown', games: inputs.length, turns, log_lines: lines, seconds: t,
  turns_per_s: turns / t, games_per_s: inputs.length / t}));
