// Replays Showdown input logs (from kanto's `ProtocolBattle.input_log()`) through Showdown's own
// BattleStream and prints the resulting battle log, for an independent check of kanto's protocol
// output against any Showdown version (no harness patches, no PRNG tracing).
//
// stdin: JSON lines {id, input: [">start ...", ">player p1 ...", ">p1 move 1", ...]}
// stdout: JSON lines {id, log: [Battle#log lines], error?}
// env: PS_DIR = path to a pokemon-showdown package.
'use strict';
const {BattleStream} = require(process.env.PS_DIR + '/dist/sim/battle-stream');
const readline = require('readline');

const rl = readline.createInterface({input: process.stdin});
rl.on('line', line => {
  if (!line.trim()) return;
  const {id, input} = JSON.parse(line);
  const out = {id};
  try {
    const stream = new BattleStream();
    // `_writeLines` processes the input synchronously, as `simulate-battle` does.
    stream._writeLines(input.join('\n'));
    out.log = stream.battle.log;
  } catch (e) {
    out.error = String(e && e.stack || e);
  }
  process.stdout.write(JSON.stringify(out) + '\n');
});
