//! `ClientBattle` (observations rebuilt from the Showdown protocol a player receives) against the
//! env's own observations and masks of the same battle (`ProtoGame`), at every step.
//!
//! `CLIENT_GAMES` (default 300) sets the number of battles; `CLIENT_TEAMS` a team pool path.
//!
//! Every field must match except the partial-trapping ones: how long Wrap/Bind/Fire Spin/Clamp
//! lasts (2-5 hits) is hidden from both players, but the env observation shows whether the lock
//! continues. The client predicts it from the hit count; those differences are reported, and must
//! stay rare.
#![allow(clippy::needless_range_loop)]

use kanto_env::backend::NUM_ACTIONS;
use kanto_env::client::ClientBattle;
use kanto_env::obs::{layout, OBS_DIM};
use kanto_env::rng::Rng;
use kanto_env::team::{builtin_teams, TeamSource};
use std::path::PathBuf;

fn pool() -> TeamSource {
    let dir = std::env::var("CLIENT_TEAMS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/teams/gen1ou"));
    if dir.exists() {
        TeamSource::from_spec(dir.to_str().unwrap()).expect("team pool loads")
    } else {
        TeamSource::Pool(builtin_teams())
    }
}

/// Observation fields that depend on the hidden partial-trapping duration.
const HIDDEN: [&str; 6] =
    ["own_active+33", "own_active+34", "own_active+43", "opp_active+33", "opp_active+34", "opp_active+43"];

fn field(off: usize) -> String {
    let mut best = ("?".to_string(), 0usize);
    for (name, o, _) in layout() {
        if o <= off && o >= best.1 {
            best = (name, o);
        }
    }
    format!("{}+{}", best.0, off - best.1)
}

/// A playing policy that mostly uses moves (random play switches too often to reach the
/// mechanics that matter).
fn pick(mask: &[bool; NUM_ACTIONS], rng: &mut Rng) -> usize {
    let legal: Vec<usize> = (0..NUM_ACTIONS).filter(|&a| mask[a]).collect();
    let moves: Vec<usize> = legal.iter().copied().filter(|&a| a < 4).collect();
    if !moves.is_empty() && rng.next_u64() % 10 < 8 {
        return moves[(rng.next_u64() % moves.len() as u64) as usize];
    }
    legal[(rng.next_u64() % legal.len() as u64) as usize]
}

#[test]
fn client_matches_env() {
    let games: u64 = std::env::var("CLIENT_GAMES").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    let strict = std::env::var("CLIENT_STRICT").map(|s| s != "0").unwrap_or(true);
    let source = pool();
    let (mut steps, mut bad_steps, mut hidden_steps) = (0u64, 0u64, 0u64);
    let mut by_field: std::collections::BTreeMap<String, u64> = Default::default();
    for seed in 0..games {
        let mut rng = Rng::new(seed);
        let (p1, p2) = (source.sample(&mut rng), source.sample(&mut rng));
        let b = kanto_backend::new_battle(&p1, &p2, rng.next_u64(), kanto_gen1::Rules::OU);
        let mut g = kanto_backend::ProtoGame::new(b, ["p1", "p2"]);
        let mut cl = [ClientBattle::new("p1"), ClientBattle::new("p2")];
        let mut history: [Vec<String>; 2] = Default::default();
        let mut out = g.start();
        let mut arng = Rng::new(seed ^ 0x5eed);
        for _ in 0..1000 {
            for p in 0..2 {
                let view = out.view(p as i32 + 1);
                history[p].extend(view.iter().cloned());
                // A Showdown server sends the request first, then the update it follows.
                if let Some(r) = &out.requests[p] {
                    cl[p].feed(&format!("|request|{r}")).unwrap();
                }
                cl[p].feed(&view.join("\n")).unwrap();
                cl[p].flush();
            }
            if g.battle().ended {
                break;
            }
            for p in 0..2 {
                steps += 1;
                let (mut want, mut got) = (vec![0f32; OBS_DIM], vec![0f32; OBS_DIM]);
                g.observe(p, &mut want);
                cl[p].observe(&mut got);
                let mask = cl[p].mask();
                let mut diffs = Vec::new();
                let only = std::env::var("CLIENT_FIELD").ok();
                let mut hidden_diff = false;
                for k in 0..OBS_DIM {
                    if want[k] != got[k] && only.is_none() && HIDDEN.contains(&field(k).as_str()) {
                        hidden_diff = true;
                        *by_field.entry(field(k)).or_default() += 1;
                        continue;
                    }
                    if want[k] != got[k] && only.as_deref().is_none_or(|f| field(k) == f) {
                        diffs.push(format!("{} want {} got {}", field(k), want[k], got[k]));
                        *by_field.entry(field(k)).or_default() += 1;
                    }
                }
                if mask != g.mask[p] {
                    diffs.push(format!("mask want {:?} got {:?}", g.mask[p], mask));
                    *by_field.entry("mask".into()).or_default() += 1;
                }
                for a in 0..NUM_ACTIONS {
                    if g.mask[p][a] && g.table[p][a] != kanto_core::Choice::Pass {
                        let want_c = g.table[p][a].to_showdown();
                        if cl[p].choice(a) != Some(want_c.clone()) {
                            diffs.push(format!("choice {a}: want {want_c} got {:?}", cl[p].choice(a)));
                            *by_field.entry("choice".into()).or_default() += 1;
                        }
                    }
                }
                hidden_steps += hidden_diff as u64;
                if !diffs.is_empty() {
                    bad_steps += 1;
                    if strict {
                        let tail: Vec<&String> =
                            history[p].iter().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect();
                        let sv = kanto_backend::view(g.battle());
                        let cv = cl[p].view();
                        eprintln!("sim  own: {:?}\nsim  own active: {:?}", sv.sides[p].mons, sv.sides[p].active);
                        eprintln!("client own: {:?}\nclient own active: {:?}", cv.sides[p].mons, cv.sides[p].active);
                        eprintln!("sim mem: {:?}\nclient mem: {:?}", g.mem[p], cl[p].memory());
                        panic!(
                            "seed {seed} turn {} player {}: {} differences\n{}\n--- recent protocol ---\n{}\n--- request ---\n{:?}",
                            g.battle().turn,
                            p + 1,
                            diffs.len(),
                            diffs.join("\n"),
                            tail.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"),
                            out.requests[p]
                        );
                    }
                }
            }
            if std::env::var("CLIENT_TRACE").ok().and_then(|x| x.parse::<u64>().ok()) == Some(seed) {
                let sv = kanto_backend::view(g.battle());
                for p in 0..2 {
                    eprintln!(
                        "turn {} p{}: sim active pp {:?} stats {:?} | client pp {:?} stats {:?}",
                        g.battle().turn,
                        p + 1,
                        sv.sides[p].active.pp,
                        sv.sides[p].active.stats,
                        cl[p].view().sides[p].active.pp,
                        cl[p].view().sides[p].active.stats
                    );
                }
                for l in out.view(-1) {
                    eprintln!("   {l}");
                }
            }
            let a1 = pick(&g.mask[0], &mut arng);
            let a2 = pick(&g.mask[1], &mut arng);
            out = g.step_actions(a1, a2).unwrap();
        }
    }
    eprintln!(
        "client: {steps} player-steps, {bad_steps} with differences, {hidden_steps} with hidden partial-trapping differences; by field: {by_field:?}"
    );
    assert_eq!(bad_steps, 0);
    assert!(hidden_steps * 100 < steps, "partial-trapping estimates differ too often");
}
