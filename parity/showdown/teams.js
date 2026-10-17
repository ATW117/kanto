// Showdown side of `parity teams`: team-format and validator parity.
//
// stdin: JSON lines {id, export, packed}. stdout: JSON lines
//   {id, export: [mon], packed: [mon], repacked, problems}
// where mon = {name, species, level, maxhp, atk, def, spa, spd, spe, moves} is what a Showdown
// Battle builds from Teams.import(<text>) (no validation), `repacked` is Teams.pack of the
// imported export text, and `problems` is TeamValidator('gen1ou').validateTeam on the unpacked
// packed team (what a server validates), or null when legal.
'use strict';
const PS = process.env.PS_DIR;
const {Battle, Teams, TeamValidator, toID} = require(PS + '/dist/sim');
const readline = require('readline');

const FORMAT = process.env.PS_FORMAT || 'gen1ou';
const validator = TeamValidator.get(FORMAT);

function mons(sets) {
  const b = new Battle({formatid: FORMAT});
  b.setPlayer('p1', {name: 'a', team: sets});
  return b.sides[0].pokemon.map(p => ({
    name: p.name, species: p.species.name, level: p.level, maxhp: p.maxhp,
    atk: p.storedStats.atk, def: p.storedStats.def, spa: p.storedStats.spa,
    spd: p.storedStats.spd, spe: p.storedStats.spe, moves: p.baseMoves,
    ability: p.baseAbility,
  }));
}

const rl = readline.createInterface({input: process.stdin});
rl.on('line', line => {
  if (!line.trim()) return;
  const t = JSON.parse(line);
  const out = {id: t.id};
  try {
    const fromExport = Teams.import(t.export);
    out.repacked = Teams.pack(Teams.import(t.export));
    out.export = mons(fromExport);
    out.packed = mons(Teams.unpack(t.packed));
    out.problems = validator.validateTeam(Teams.unpack(t.packed));
  } catch (e) {
    out.error = String(e && e.stack || e);
  }
  process.stdout.write(JSON.stringify(out) + '\n');
});
