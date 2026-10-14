#!/usr/bin/env python3
"""Mutation testing of the parity harness: does `parity` catch deliberate simulator bugs?

Each mutant is a one-place source edit of the native simulator (crates/kanto-gen1, kanto-core). For
every mutant the script builds `parity` in a scratch copy of the repository, replays recorded
Showdown traces without Node (`parity mutscan`, over dumps made by `parity fuzz --dump`) and the
Node-free fixtures (`parity fixtures`), and reports the mutant as caught if anything diverges.
A surviving mutant is either a blind spot of the harness or an equivalent mutant (no observable
behaviour change); the table's notes say which, where known.

    python3 parity/mutate.py                         # all mutants
    python3 parity/mutate.py --only freeze-thaw-wrong vol-order-reverse
    python3 parity/mutate.py --requested             # the audit's survivors and weak catches
    python3 parity/mutate.py --list                  # names, and whether each pattern applies
    python3 parity/mutate.py --compare before.json   # add a "before" column from an earlier run

Dumps are generated on first use (needs Node and pokemon-showdown: PS_DIR, NODE) into
--dumps (default parity/out/mutate/dumps); delete them after changing the runner, the snapshot
or the fuzz team generators. The source tree is never modified: mutants are applied to the copy
in --work (incremental builds there take ~10 s per mutant; a full run takes ~15 minutes).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
CARGO = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")

# Corpora replayed for every mutant: (name, parity fuzz arguments).
CORPORA = [
    ("mixed", ["--battles", "1000", "--teams", "mixed", "--prng", "mixed", "--seed", "7101"]),
    ("stall", ["--battles", "400", "--teams", "stall", "--seed", "7102"]),
    ("random_nc", ["--battles", "400", "--teams", "random", "--format", "gen1nc", "--prng", "sodium", "--seed", "7103"]),
    ("targeted", ["--battles", "2000", "--teams", "targeted", "--prng", "mixed", "--seed", "7104"]),
    ("thaw", ["--battles", "2000", "--teams", "thaw", "--seed", "7105"]),
]

G = "crates/kanto-gen1/src/"
C = "crates/kanto-core/src/"

# (name, file, old text, new text, note). `old` must occur exactly once in the file.
MUTANTS = [
    # -- damage, crits, accuracy
    ("dmg-roll-range", G + "moves.rs", 'random_range(217, 256, "getDamage.random")', 'random_range(217, 255, "getDamage.random")', ""),
    ("dmg-roll-div256", G + "moves.rs", "as u64 / 255;", "as u64 / 256;", ""),
    ("dmg-997cap", G + "moves.rs", "d = (d / 50).min(997) + 2;", "d = (d / 50).min(998) + 2;", ""),
    ("crit-odd-basespe", G + "moves.rs", "let mut cc = base_spe / 2;", "let mut cc = (base_spe + 1) / 2;", ""),
    ("crit-high-x8", G + "moves.rs", "cc = (cc * 4).clamp(1, 255);", "cc = (cc * 8).clamp(1, 255);", ""),
    ("crit-focus-energy", G + "moves.rs", "if ua.has(v::FOCUSENERGY) {\n                cc /= 2;", "if ua.has(v::FOCUSENERGY) {\n                cc /= 4;", "Focus Energy crit /4"),
    ("crit-uses-modified", G + "moves.rs", "attack = self.sides[p].mon().stats[ai + 1] as u64;", "attack = ua.modified[ai] as u64;", ""),
    ("acc-no-1in256", G + "moves.rs", "acc = acc.clamp(1, 255);", "acc = acc.clamp(1, 256);", ""),
    ("acc-skip-roll-at-255", G + "moves.rs", "if let Some(a) = acc_int {\n            if !self.rng", "if let Some(a) = acc_int.filter(|&a| a < 255) {\n            if !self.rng", ""),
    ("recoil-round", G + "events.rs", "let recoil = (dealt as u64 * r[0] as u64 / r[1] as u64).max(1) as u32;", "let recoil = ((dealt as u64 * r[0] as u64 + r[1] as u64 / 2) / r[1] as u64).max(1) as u32;", ""),
    ("psywave-hi-plus1", G + "moves.rs", "let hi = (1.5 * lvl as f64) as u32;", "let hi = (1.5 * lvl as f64) as u32 + 1;", ""),
    ("counter-any-type", G + "moves.rs", "(d.type_ == TYPE_NORMAL || d.type_ == TYPE_FIGHTING) && d.base_power > 0", "d.base_power > 0", ""),
    ("counter-vs-counter-allowed", G + "moves.rs", "let selected_ok = foe.last_enemy_selected_move != mv::COUNTER;", "let selected_ok = true;", ""),
    ("confusion-selfhit-def", G + "moves.rs", "let def = self.sides[p].active.modified[1] as u64;", "let def = self.sides[p].mon().stats[2] as u64;", ""),
    ("bide-into-sub-x1", G + "moves.rs", "uncapped = R::N(self.sides[p].active.bide_damage.wrapping_mul(2));", "uncapped = R::N(self.sides[p].active.bide_damage);", ""),
    # -- speed ties / ordering
    ("speedtie-reverse", C + "sort.rs", "rng.shuffle(list, start, start + len, label);", "rng.shuffle(list, start, start + len, label); list[start..start + len].reverse();", ""),
    ("speedtie-no-rng", C + "sort.rs", "if len > 1 {\n        rng.shuffle", "if len > 2 {\n        rng.shuffle", ""),
    ("vol-order-reverse", G + "events.rs", "ids[..n].sort_by_key(|&id| a.vstamp[id as usize]);", "ids[..n].sort_by_key(|&id| std::cmp::Reverse(a.vstamp[id as usize]));", "volatile order"),
    # -- secondaries, status
    ("sec-skip-roll-if-statused", G + "moves.rs", "if !blocked {\n                        if sec.chance == 0", "if !blocked && !(sec.status != 0 && self.sides[t].mon().status != st::NONE) {\n                        if sec.chance == 0", ""),
    ("sec-confusion-no-minus1", G + "moves.rs", "if sec.volatile == data::vol::CONFUSION {\n                                chance -= 1;", "if sec.volatile == data::vol::CONFUSION {\n                                chance -= 0;", "confusion 1/256"),
    ("freeze-thaw-wrong", G + "moves.rs", "if sec.status == st::BRN {\n                    self.cure_status(t);", "if sec.status == st::BRN && m.d.base_power > 100 {\n                    self.cure_status(t);", "Fire thaw"),
    ("status-self-ignored", G + "events.rs", "m.set_status_self(self_inflicted);", "m.set_status_self(false);", "status_self"),
    ("slp-rest-3", G + "moves.rs", "mm.slp_time = 2;", "mm.slp_time = 3;", ""),
    ("slp-range-1-7", G + "events.rs", 'random_range(1, 8, "slp.onStart")', 'random_range(1, 7, "slp.onStart")', ""),
    ("slp-wake-ge", G + "moves.rs", "if m.slp_time > 0 {\n                        s.event(Event::Cant { player: p, slot, reason: Cant::Sleep", "if m.slp_time >= 0 {\n                        s.event(Event::Cant { player: p, slot, reason: Cant::Sleep", ""),
    ("toxic-leechseed-no-inc", G + "events.rs", "a.residual_counter = a.residual_counter.wrapping_add(1);\n                        counter = a.residual_counter as u32;", "counter = a.residual_counter as u32;", "Leech Seed Toxic counter"),
    ("bench-par-speed-not-reapplied", G + "battle.rs", "m.set_bench_par(m.status == st::PAR);", "m.set_bench_par(false);", ""),
    ("stat-999-cap-1000", G + "events.rs", "if delta > 0 && a.modified[i] > 999 {\n                a.modified[i] = 999;", "if delta > 0 && a.modified[i] > 1000 {\n                a.modified[i] = 1000;", ""),
    ("rest-255-bug-removed", G + "moves.rs", "if (hp == max - 255 || hp == max - 511) && hp % 256 != 0 {", "if false {", ""),
    # -- PP
    ("pp-no-wrap", G + "moves.rs", "slot.set_pp((slot.pp() as i32 - amount).rem_euclid(64) as u8);", "slot.set_pp((slot.pp() as i32 - amount).clamp(0, 63) as u8);", ""),
    ("pp-twoturn-both", G + "moves.rs", "if (locked.is_none() && !is_two_turn(id)) || self.sides[p].active.has(v::TWOTURNMOVE) {", "if locked.is_none() || self.sides[p].active.has(v::TWOTURNMOVE) {", ""),
    ("pp-metronome-refund", G + "moves.rs", "slot.set_pp((slot.pp() + 1) % 64);", "slot.set_pp(slot.pp() % 64);", "Metronome/Mirror PP refund"),
    ("pp-transform-5to4", G + "moves.rs", "MoveSlot::new(id, MOVES[id as usize].pp.min(5))", "MoveSlot::new(id, MOVES[id as usize].pp.min(4))", ""),
    ("transform-mimic-pp-writeback", G + "moves.rs", "if sd.active.is_virt(i) && !sd.active.transformed && i < sd.mon().n_moves as usize {", "if sd.active.is_virt(i) && i < sd.mon().n_moves as usize {", ""),
    # maxpp is derived (Side::slot_maxpp) since it is not stored; these replace the former
    # mimic-maxpp-ups0 / transform-maxpp-ups0 mutants of the code that stored it.
    ("maxpp-virt-ups0", G + "state.rs", "let ups = if i < self.mon().n_moves as usize { 3 } else { 0 };", "let ups = if i < self.mon().n_moves as usize && !self.active.is_virt(i) { 3 } else { 0 };", "maxpp (hidden, derived)"),
    ("maxpp-beyond-set-ups3", G + "state.rs", "let ups = if i < self.mon().n_moves as usize { 3 } else { 0 };", "let ups = 3;", "maxpp (hidden, derived)"),
    # -- volatiles, trapping, Substitute
    ("sub-hp-no-plus1", G + "events.rs", "(maxhp / 4) as i32 + 1;", "(maxhp / 4) as i32;", ""),
    ("sub-fainted-unset", G + "moves.rs", "self.sides[t].mon_mut().set_sub_fainted(true);", "self.sides[t].mon_mut().set_sub_fainted(false);", "sub_fainted"),
    ("hyperbeam-recharge-on-ko", G + "events.rs", "v::MUSTRECHARGE => {\n                    if target_hp == 0 {\n                        self.del_vol(p, id);", "v::MUSTRECHARGE => {\n                    if target_hp == 0 && false {\n                        self.del_vol(p, id);", ""),
    ("pt-source-ignored", G + "moves.rs", "ta.has(v::PARTIALLYTRAPPED) && ta.pt_source == self.sides[p].order[0]", "ta.has(v::PARTIALLYTRAPPED)", "pt_source (accuracy)"),
    ("pt-source-hp-ignored", G + "battle.rs", "if self.sides[q].mons[t as usize].hp == 0 || !foe_has(self, t, v::PARTIALTRAPPINGLOCK) {", "if !foe_has(self, t, v::PARTIALTRAPPINGLOCK) {", "pt_source (end of turn)"),
    ("fpt-counterpart-ignored", G + "battle.rs", "if self.sides[q].mons[t as usize].hp == 0 || !foe_has(self, t, v::FAKEPARTIALLYTRAPPED) {", "if !foe_has(self, self.sides[q].order[0], v::FAKEPARTIALLYTRAPPED) {", "fpt_counterpart"),
    ("pt-maybelocked-unset", G + "events.rs", "self.sides[target].active.pt_maybe_locked = true;", "self.sides[target].active.pt_maybe_locked = false;", ""),
    ("disable-bide-check", G + "moves.rs", "let used = if a.has(v::BIDE) { mv::BIDE } else { id };", "let used = id;", "Disable/Bide"),
    ("disable-picks-0pp-slots", G + "events.rs", "if sd.slot(i).pp() > 0 {\n                        cand[n]", "if sd.slot(i).pp() < 64 {\n                        cand[n]", ""),
    ("move-this-turn-ignored", G + "moves.rs", "if self.sides[p].active.move_this_turn != 0 || !self.before_move(p, target, id, s) {", "if !self.before_move(p, target, id, s) {", "move_this_turn guard"),
    ("mirror-mirror-aftermove", G + "moves.rs", "if m.id != mv::MIRRORMOVE || foe_last == 0 || foe_last == mv::MIRRORMOVE {", "if m.id != mv::MIRRORMOVE || foe_last == 0 {", "Mirror-Mirror AfterMove"),
    ("endless-frz-ignored", G + "battle.rs", "|| m.status == st::FRZ\n", "|| (m.status == st::FRZ && false)\n", "Endless Clause freeze"),
    ("trapped-cleared-on-faint", G + "battle.rs", "        m.set_bench_brn(m.status == st::BRN);\n", "        m.set_bench_brn(m.status == st::BRN);\n        m.set_trapped(false);\n", "trapped (hidden), new"),
    # -- public events (the env's reveal memory)
    ("sub-fail-event-dropped", G + "moves.rs", "if self.sides[t].active.has(v::SUBSTITUTE) {\n                    s.event(Event::Fail);", "if self.sides[t].active.has(v::SUBSTITUTE) {", "event, new"),
    ("status-immune-event-dropped", G + "events.rs", "if status_move {\n                    s.event(Event::Immune);\n                }", "", "event, new"),
    # -- performance pass: fast paths, packed keys and packed state (each one-place edit)
    ("fast-beforemove-no-confusion", G + "moves.rs", "    | 1 << v::CONFUSION\n", "\n", "BeforeMove volatile mask"),
    ("fast-beforemove-skips-par", G + "moves.rs", "st::FRZ | st::SLP | st::PAR) && self.sides[p].active.vols & BEFORE_MOVE_VOLS == 0", "st::FRZ | st::SLP) && self.sides[p].active.vols & BEFORE_MOVE_VOLS == 0", "BeforeMove fast path"),
    ("fast-aftermoveself-skips-slp", G + "events.rs", "st::SLP | st::PSN | st::BRN | st::TOX) && self.sides[p].active.vols & VOLS == 0", "st::PSN | st::BRN | st::TOX) && self.sides[p].active.vols & VOLS == 0", "AfterMoveSelf fast path"),
    ("fast-residual-no-flinch", G + "events.rs", "const DURATION_VOLS: u32 = 1 << v::FLINCH | ", "const DURATION_VOLS: u32 = ", "Residual volatile mask"),
    ("fast-lock-no-rage", G + "battle.rs", "1 << v::TWOTURNMOVE | 1 << v::RAGE;", "1 << v::TWOTURNMOVE;", "getLockedMove fast path"),
    ("fast-eachevent-inverted", G + "battle.rs", "self.sides[0].active.speed != self.sides[1].active.speed {", "self.sides[0].active.speed == self.sides[1].active.speed {", "eachEvent fast path"),
    ("sort2-no-swap", C + "sort.rs", "if delta > 0 {\n            list.swap(0, 1);", "if delta > 0 && false {\n            list.swap(0, 1);", "two-item speedSort"),
    ("key-priority-inverted", C + "sort.rs", "((127 - priority as i16) as u64) << 40", "((priority as i16 + 128) as u64) << 40", "packed sort key"),
    ("stamp-renumber-reversed", G + "events.rs", "a.vstamp[id as usize] = k as u8;", "a.vstamp[id as usize] = (n - 1 - k) as u8;", "8-bit insertion stamps; renumbering needs 255 volatile adds in one switch-in, which no corpus reaches: covered by the kanto-gen1 unit test volatile_insertion_order_survives_stamp_renumbering"),
    ("bench-brn-not-reapplied", G + "battle.rs", "m.set_bench_brn(m.status == st::BRN);", "m.set_bench_brn(false);", "packed bench stats"),
    ("disable-event-wrong-move", G + "moves.rs", "s.event(Event::DisableStart { player: t, slot: self.sides[t].order[0], mv: a.disable_move });", "s.event(Event::DisableStart { player: t, slot: self.sides[t].order[0], mv: a.disable_slot });", "event, new"),
]

# The audit's surviving or weakly caught mutants (re-run with --requested).
REQUESTED = [
    "freeze-thaw-wrong", "crit-focus-energy", "sec-confusion-no-minus1", "sub-fainted-unset", "status-self-ignored",
    "pt-source-ignored", "pt-source-hp-ignored", "fpt-counterpart-ignored", "move-this-turn-ignored", "disable-bide-check",
    "vol-order-reverse", "mirror-mirror-aftermove", "endless-frz-ignored", "toxic-leechseed-no-inc", "pp-metronome-refund",
    "acc-no-1in256", "maxpp-virt-ups0", "bide-into-sub-x1", "counter-vs-counter-allowed", "disable-picks-0pp-slots",
    "dmg-997cap",
]

# Mutants believed to be equivalent in Gen 1 (no reachable state tells them apart).
EQUIVALENT = {
    "pt-source-hp-ignored": "removes a redundant check: foe_has() already requires hp > 0",
    "fpt-counterpart-ignored": "the fake-trap pair is always the two actives, and re-adding updates the counterpart",
    "pt-source-ignored": "a trapped target's trapper is always the active foe within a turn (switching ends the trap)",
    "disable-bide-check": "while Bide is active the executed move is always lastSelectedMove = Bide",
    "move-this-turn-ignored": "no Gen 1 path runs a second move action for the same Pokémon in a turn",
    "acc-no-1in256": "Gen 1 has no accuracy boosts or evasion drops, so the computed accuracy never exceeds 255",
}


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def make_work_copy(work: Path) -> Path:
    src = work / "src"
    ignore = shutil.ignore_patterns("target", ".git", ".venv", "out", "runs", ".claude", "__pycache__", "*.so")
    if src.exists():
        # Refresh sources, keep the copy's target dir for incremental builds.
        for sub in ["crates", "parity", "tests", "tools", "Cargo.toml", "Cargo.lock", "rustfmt.toml"]:
            p = src / sub
            if p.is_dir():
                shutil.rmtree(p)
            elif p.exists():
                p.unlink()
            s = REPO / sub
            if s.is_dir():
                shutil.copytree(s, p, ignore=ignore)
            elif s.exists():
                shutil.copy2(s, p)
    else:
        shutil.copytree(REPO, src, ignore=ignore, symlinks=True)
    return src


def build(src: Path, jobs: int) -> tuple[bool, str]:
    env = dict(os.environ, CARGO_TARGET_DIR=str(src / "target"))
    r = run([CARGO, "build", "--release", "-j", str(jobs), "-p", "kanto-parity"], cwd=src, env=env)
    return r.returncode == 0, r.stderr[-3000:]


def parity_bin(src: Path) -> str:
    return str(src / "target/release/parity")


def ensure_dumps(src: Path, dumps: Path, jobs: int) -> list[Path]:
    out = []
    for name, args in CORPORA:
        f = dumps / f"{name}.jsonl"
        if not f.exists() or f.stat().st_size == 0:
            if not os.environ.get("PS_DIR"):
                sys.exit(f"dump {f} missing and PS_DIR is not set (needed to run Showdown)")
            print(f"recording dump {name} ...", flush=True)
            dumps.mkdir(parents=True, exist_ok=True)
            r = run([parity_bin(src), "fuzz", *args, "--jobs", str(jobs), "--dump", str(f), "--out", str(dumps / f"{name}.out")], cwd=src)
            if r.returncode != 0:
                sys.exit(f"fuzz for dump {name} failed:\n{r.stdout[-2000:]}\n{r.stderr[-2000:]}")
            print("  " + "\n  ".join(l for l in r.stdout.splitlines()[:2]), flush=True)
        out.append(f)
    return out


def scan(src: Path, dumps: list[Path], jobs: int) -> dict:
    """Replay every dump (in parallel) and the fixtures; returns per-corpus results."""
    def one(d: Path):
        r = run([parity_bin(src), "mutscan", str(d)])
        try:
            return d.stem, json.loads(r.stdout.strip().splitlines()[-1])
        except Exception:
            return d.stem, {"battles": 0, "diverged": -1, "first": (r.stdout + r.stderr)[-300:], "top": []}

    with ThreadPoolExecutor(max_workers=max(1, min(jobs, len(dumps)))) as ex:
        res = dict(ex.map(one, dumps))
    r = run([parity_bin(src), "fixtures", "--dir", str(src / "parity/fixtures")])
    res["fixtures"] = {"regressions": sum("REGRESSION" in l for l in r.stdout.splitlines())}
    return res


def caught(res: dict) -> bool:
    return any(v.get("diverged", 0) != 0 for k, v in res.items() if k != "fixtures") or res["fixtures"]["regressions"] > 0


def fmt_counts(res: dict, corpora: list[str]) -> str:
    cells = [str(res[c]["diverged"]) if c in res else "-" for c in corpora]
    return " / ".join(cells) + f" / fx {res['fixtures']['regressions']}"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--only", nargs="*", default=None, help="mutant names")
    ap.add_argument("--requested", action="store_true", help="only the audit's survivors and weak catches")
    ap.add_argument("--list", action="store_true", help="list mutants and check their patterns")
    ap.add_argument("--work", type=Path, default=REPO / "parity/out/mutate")
    ap.add_argument("--dumps", type=Path, default=None, help="dump dir (default <work>/dumps)")
    ap.add_argument("--jobs", type=int, default=5, help="parallel scans / Showdown workers / cargo jobs")
    ap.add_argument("--out", type=Path, default=None, help="write results JSON here (default <work>/results.json)")
    ap.add_argument("--compare", type=Path, default=None, help="earlier results JSON for a before/after table")
    args = ap.parse_args()

    names = [m[0] for m in MUTANTS]
    assert len(names) == len(set(names)), "duplicate mutant names"
    if args.list:
        for name, f, old, _new, note in MUTANTS:
            n = (REPO / f).read_text().count(old)
            print(f"{name:32} {'ok' if n == 1 else f'PATTERN COUNT {n}':16} {f}  {note}")
        return 0
    selected = MUTANTS
    if args.requested:
        selected = [m for m in MUTANTS if m[0] in REQUESTED]
    if args.only:
        unknown = set(args.only) - set(names)
        if unknown:
            sys.exit(f"unknown mutants: {sorted(unknown)}")
        selected = [m for m in MUTANTS if m[0] in args.only]

    work = args.work
    work.mkdir(parents=True, exist_ok=True)
    dumps_dir = args.dumps or work / "dumps"
    t0 = time.time()
    src = make_work_copy(work)
    ok, err = build(src, args.jobs)
    if not ok:
        sys.exit("baseline build failed:\n" + err)
    dumps = ensure_dumps(src, dumps_dir, args.jobs)
    corpora = [d.stem for d in dumps]
    base = scan(src, dumps, args.jobs)
    if caught(base):
        sys.exit(f"the unmutated simulator diverges on the dumps: {base}")
    sizes = {k: v["battles"] for k, v in base.items() if k != "fixtures"}
    print(f"baseline clean; corpora {sizes}; {time.time() - t0:.0f}s", flush=True)

    results = {}
    for name, f, old, new, note in selected:
        path = src / f
        text = path.read_text()
        if text.count(old) != 1:
            results[name] = {"status": f"pattern count {text.count(old)}", "note": note}
            print(f"{name:32} SKIPPED (pattern count {text.count(old)})", flush=True)
            continue
        t1 = time.time()
        try:
            path.write_text(text.replace(old, new))
            ok, err = build(src, args.jobs)
            if not ok:
                results[name] = {"status": "build failed", "note": note, "error": err[-500:]}
                print(f"{name:32} BUILD FAILED", flush=True)
                continue
            res = scan(src, dumps, args.jobs)
        finally:
            path.write_text(text)
        status = "caught" if caught(res) else "survived"
        results[name] = {"status": status, "note": note, "equivalent": EQUIVALENT.get(name, ""), **res}
        first = next((v["first"] for k, v in res.items() if k != "fixtures" and v.get("diverged")), "")
        print(f"{name:32} {status.upper():9} {fmt_counts(res, corpora):40} {time.time() - t1:4.0f}s  {first[:90]}", flush=True)
    ok, _ = build(src, args.jobs)  # leave the copy unmutated

    out = args.out or work / "results.json"
    out.write_text(json.dumps({"corpora": sizes, "mutants": results}, indent=1))
    before = json.loads(args.compare.read_text())["mutants"] if args.compare else {}
    print(f"\n| mutant | {'before | ' if before else ''}after (divergent battles: {' / '.join(corpora)} / fixtures) | note |")
    print(f"| --- | {'--- | ' if before else ''}--- | --- |")
    for name, r in results.items():
        after = r["status"] if r["status"] not in ("caught", "survived") else f"{r['status']} ({fmt_counts(r, corpora)})"
        b = before.get(name, {})
        bcell = (b.get("status", "-") + (f" ({b['summary']})" if b.get("summary") else "")) + " | " if before else ""
        note = "; ".join(x for x in [r.get("note", ""), ("equivalent: " + r["equivalent"]) if r.get("equivalent") and r["status"] == "survived" else ""] if x)
        print(f"| {name} | {bcell}{after} | {note} |")
    n_caught = sum(r["status"] == "caught" for r in results.values())
    print(f"\ncaught {n_caught}/{len(results)} mutants; results in {out}; {time.time() - t0:.0f}s total")
    return 0


if __name__ == "__main__":
    sys.exit(main())
