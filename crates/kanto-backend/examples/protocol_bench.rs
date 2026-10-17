//! Replay Showdown input logs (bench/protocol/gen_games.py) with kanto and time it, two ways:
//!
//! - `protocol`: `ProtocolBattle`, which writes Showdown's battle log and both players' requests
//!   line for line, as Showdown's `BattleStream` does (the like-for-like comparison);
//! - `engine`: the bare simulator, no text (what an RL env uses).
//!
//!     cargo run --release -p kanto-backend --example protocol_bench -- GAMES.jsonl [REPEATS]

use kanto_core::{Choice, Prng, Request};
use kanto_env::showdown_team::parse_packed;
use kanto_env::team::Team;
use kanto_gen1::state::Battle;
use kanto_gen1::{ProtocolBattle, Rules};
use std::time::Instant;

struct Game {
    seed: Prng,
    teams: [Team; 2],
    names: [String; 2],
    /// Choices in input-log order: (player, choice).
    choices: Vec<(usize, Choice)>,
}

fn parse(line: &str) -> Game {
    let v: serde_json::Value = serde_json::from_str(line).expect("json");
    let input = v["input"].as_array().expect("input");
    let (mut seed, mut teams, mut names, mut choices) = (None, Vec::new(), Vec::new(), Vec::new());
    for l in input.iter().map(|x| x.as_str().unwrap()) {
        if let Some(rest) = l.strip_prefix(">start ") {
            let s: serde_json::Value = serde_json::from_str(rest).unwrap();
            seed = Prng::parse(s["seed"].as_str().unwrap());
        } else if let Some(rest) = l.strip_prefix(">player ") {
            let (_, json) = rest.split_once(' ').unwrap();
            let p: serde_json::Value = serde_json::from_str(json).unwrap();
            let spec = parse_packed(p["team"].as_str().unwrap()).expect("packed team");
            teams.push(Team::from_spec_opts(&spec, false).expect("team"));
            names.push(p["name"].as_str().unwrap().to_string());
        } else if let Some(rest) = l.strip_prefix(">p1 ").map(|r| (0, r)).or_else(|| l.strip_prefix(">p2 ").map(|r| (1, r))) {
            choices.push((rest.0, Choice::parse(rest.1).expect("choice")));
        }
    }
    Game {
        seed: seed.expect("seed"),
        teams: [teams[0], teams[1]],
        names: [names[0].clone(), names[1].clone()],
        choices,
    }
}

fn needs(b: &Battle, p: usize) -> bool {
    matches!(b.sides[p].request, Request::Move | Request::Switch)
}

/// Replay one game; `text` selects ProtocolBattle (log + requests) or the bare engine.
fn replay(g: &Game, text: bool) -> u32 {
    let b = kanto_backend::try_new_battle(&g.teams[0], &g.teams[1], 0, Rules::OU).expect("battle");
    let mut b = Battle { rng: g.seed, ..b };
    let mut pending: [Option<Choice>; 2] = [None, None];
    if text {
        let mut pb = ProtocolBattle::new(b, [g.names[0].as_str(), g.names[1].as_str()]);
        pb.start();
        for &(p, c) in &g.choices {
            pending[p] = Some(c);
            let b = &pb.battle;
            if (0..2).all(|q| !needs(b, q) || pending[q].is_some()) {
                pb.step(pending[0].take().unwrap_or(Choice::Pass), pending[1].take().unwrap_or(Choice::Pass)).expect("legal");
            }
        }
        std::hint::black_box(&pb.sink);
        pb.battle.turn as u32
    } else {
        b.apply_choices_unchecked(Choice::Pass, Choice::Pass, &mut ());
        for &(p, c) in &g.choices {
            pending[p] = Some(c);
            if (0..2).all(|q| !needs(&b, q) || pending[q].is_some()) {
                b.apply_choices(pending[0].take().unwrap_or(Choice::Pass), pending[1].take().unwrap_or(Choice::Pass), &mut ()).expect("legal");
            }
        }
        b.turn as u32
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text_in = std::fs::read_to_string(&args[1]).unwrap();
    let lines: Vec<&str> = text_in.lines().filter(|l| !l.is_empty()).collect();
    let games: Vec<Game> = lines.iter().map(|l| parse(l)).collect();
    let repeats: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    for text in [true, false] {
        let mut times = Vec::new();
        let mut turns = 0u64;
        for _ in 0..repeats {
            turns = 0;
            let t0 = Instant::now();
            if text {
                // Like Showdown's timing: parse each input log (seed, packed teams, choices) too.
                for l in &lines {
                    turns += replay(&parse(l), true) as u64;
                }
            } else {
                for g in &games {
                    turns += replay(g, false) as u64;
                }
            }
            times.push(t0.elapsed().as_secs_f64());
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let t = times[times.len() / 2];
        println!(
            "{{\"engine\": \"kanto-{}\", \"games\": {}, \"turns\": {turns}, \"seconds\": {t:.4}, \"turns_per_s\": {:.0}, \"games_per_s\": {:.1}}}",
            if text { "protocol" } else { "engine" },
            games.len(),
            turns as f64 / t,
            games.len() as f64 / t
        );
    }
}
