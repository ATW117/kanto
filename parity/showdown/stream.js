// Runs Showdown battle-stream inputs through pokemon-showdown's own `BattleTextStream` (what
// `pokemon-showdown simulate-battle` runs) and prints the text output, for byte-for-byte checks
// of kanto's `BattleTextStream` / `kanto-simulate`.
//
// stdin: JSON lines {id, input, chunks?}: `input` is the text written to stdin; `chunks` (optional)
//        a list of chunk lengths in UTF-16 units: each chunk is one write (default: one write per
//        line, as an interactive client sends them).
// stdout: JSON lines {id, out, error?}
// env: PS_DIR = path to a pokemon-showdown package.
'use strict';
const {BattleTextStream} = require(process.env.PS_DIR + '/dist/sim/battle-stream');
const readline = require('readline');

let pendingError = null;
process.on('unhandledRejection', e => { pendingError = e; });

async function run(input, chunks) {
  const s = new BattleTextStream({noCatch: true});
  let out = '';
  const reader = (async () => {
    for (;;) {
      const m = await s.read();
      if (m === null || m === undefined) break;
      out += m;
    }
  })().catch(() => {});
  pendingError = null;
  let error = null;
  const parts = [];
  if (chunks) {
    let i = 0;
    for (const n of chunks) { parts.push(input.slice(i, i + n)); i += n; }
    if (i < input.length) parts.push(input.slice(i));
  } else {
    for (const line of input.split(/(?<=\n)/)) parts.push(line);
  }
  for (const p of parts) {
    try {
      await s.write(p);
    } catch (e) {
      error = e;
      break;
    }
    await new Promise(r => setImmediate(r));
    if (pendingError) { error = pendingError; break; }
  }
  if (!error) {
    try { await s.writeEnd(); } catch (e) { error = e; }
  }
  await Promise.race([reader, new Promise(r => setTimeout(r, 50))]);
  return {out, error: error ? String(error && error.message || error) : undefined};
}

const rl = readline.createInterface({input: process.stdin, crlfDelay: Infinity});
(async () => {
  for await (const line of rl) {
    if (!line.trim()) continue;
    const {id, input, chunks} = JSON.parse(line);
    let r;
    try {
      r = await run(input, chunks);
    } catch (e) {
      r = {out: '', error: String(e && e.stack || e)};
    }
    process.stdout.write(JSON.stringify({id, ...r}) + '\n');
  }
})();
