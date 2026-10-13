// Showdown-side legality check for Gen 1 OU teams.  Driven by validate.py.
// stdin:  JSON array of {name, export}  (export = Showdown teambuilder text)
// stdout: JSON array of {name, problems: [string]}  (empty problems = legal)
// argv[2]: path to a `pokemon-showdown` package (npm install pokemon-showdown)
// argv[3]: format id (default gen1ou)
'use strict';
const PS = process.argv[2];
const FORMAT = process.argv[3] || 'gen1ou';
const {Teams, TeamValidator} = require(PS + '/dist/sim');

let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', d => { input += d; });
process.stdin.on('end', () => {
  const validator = TeamValidator.get(FORMAT);
  const out = JSON.parse(input).map(({name, export: text}) => {
    const sets = Teams.import(text);
    if (!sets) return {name, problems: ['Showdown could not parse the exported team']};
    return {name, problems: validator.validateTeam(sets) || []};
  });
  process.stdout.write(JSON.stringify(out));
});
