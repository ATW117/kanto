#!/usr/bin/env bash
# Re-run the parity tables of docs/SIM_STATUS.md with their documented seeds.
#
#   PS_DIR=~/ps/node_modules/pokemon-showdown parity/reproduce.sh [state|proto|all] [JOBS]
#
# Specs (teams, seeds, choices) are derived from the seed alone, so each run compares the same
# battles on any machine; the "battles / steps compared / diverged" line of each run should equal
# the table row. Needs node and an npm install of pokemon-showdown (0.11.11 for the documented
# numbers; any other version to check it). Takes a few minutes with 5 jobs.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${PS_DIR:?set PS_DIR to a pokemon-showdown package (npm install pokemon-showdown@0.11.11)}"
what=${1:-all}
jobs=${2:-5}
cargo build --release -q -p kanto-parity --bin parity
P=target/release/parity
out=${OUT:-parity/out/reproduce}
mkdir -p "$out"

run() { # name, then fuzz flags
  local name=$1
  shift
  printf '%-4s %-75s ' "$name" "$*"
  "$P" fuzz --jobs "$jobs" --out "$out/$name" "$@" 2>/dev/null | grep '^compared' || echo "(no report)"
}

if [[ $what == state || $what == all ]]; then
  echo "== state parity, harness v2 (docs/SIM_STATUS.md, first table)"
  run v1 --seed 1001 --battles 5000 --teams mixed --prng mixed
  run v2 --seed 1002 --battles 2000 --teams targeted --prng mixed
  run v3 --seed 1003 --battles 500 --teams thaw
  run v4 --seed 1004 --battles 2000 --teams random
  run v5 --seed 1005 --battles 1000 --teams stall
  run v6 --seed 1006 --battles 1000 --teams legal
  run v7 --seed 1007 --battles 1000 --teams random --format gen1nc --prng sodium
  run v8 --seed 1008 --battles 1000 --teams mixed --patch pkmn
  run v9 --seed 1009 --battles 1000 --teams targeted --format gen1nc --patch pkmn
  echo "== state parity after the performance pass (second table)"
  run p1 --seed 11001 --battles 10000 --teams mixed --prng mixed
  run p2 --seed 11002 --battles 3000 --teams targeted --prng mixed
  run p3 --seed 11003 --battles 1000 --teams thaw
  run p4 --seed 11004 --battles 2000 --teams mixed --patch pkmn
  run p5 --seed 11005 --battles 1000 --teams targeted --format gen1nc --patch pkmn
fi
if [[ $what == proto || $what == all ]]; then
  echo "== protocol parity (fuzz --proto)"
  run q1 --proto --seed 6001 --battles 3000 --teams mixed --prng mixed
  run q2 --proto --seed 6002 --battles 2000 --teams targeted --prng mixed
  run q3 --proto --seed 6003 --battles 500 --teams thaw
  run q4 --proto --seed 6004 --battles 500 --teams stall
  run q5 --proto --seed 6005 --battles 1000 --teams random
  run q6 --proto --seed 6006 --battles 1000 --teams legal
  run q7 --proto --seed 6007 --battles 500 --teams random --format gen1nc --prng sodium
  run q8 --proto --seed 6008 --battles 500 --teams mixed --patch pkmn
fi
echo "== Node-free fixtures"
"$P" fixtures | tail -n 4
