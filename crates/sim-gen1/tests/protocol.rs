//! Showdown protocol output (`ProtocolBattle`): a golden check against pokemon-showdown 0.11.11's
//! own output, plus invariants over many random games. The exhaustive check is `parity fuzz
//! --proto` against Showdown and the Node-free `parity/fixtures/corpus-proto.jsonl`.

use sim_core::proto::extract_channel;
use sim_core::{Choice, Generation};
use sim_data::{gen1_move_id, gen1_species_id};
use sim_gen1::{Battle, MonSpec, Prng, ProtocolBattle, Rules};

fn mon(species: &str, level: u8, moves: &[&str]) -> MonSpec {
    let mut m = [0u8; 4];
    for (k, name) in moves.iter().enumerate() {
        m[k] = gen1_move_id(name).unwrap();
    }
    MonSpec { species: gen1_species_id(species).unwrap(), level, moves: m, ivs: [31; 6], evs: [0; 6] }
}

fn lines(s: &str) -> Vec<String> {
    s.lines().map(|l| l.to_string()).collect()
}

/// Recorded from Showdown (parity/showdown/runner.js, `proto: true`): seed "1,2,3,4", no EVs.
#[test]
fn golden_matches_showdown() {
    let p1 = [mon("Tauros", 100, &["bodyslam", "hyperbeam", "blizzard", "earthquake"]), mon("Chansey", 80, &["softboiled", "thunderwave", "icebeam", "seismictoss"])];
    let p2 = [mon("Gengar", 100, &["hypnosis", "nightshade", "thunderbolt", "explosion"]), mon("Farfetch’d", 74, &["swordsdance", "slash"])];
    let b = Battle::new(&p1, &p2, Prng::parse("1,2,3,4").unwrap(), Rules::OU).unwrap();
    let mut pb = ProtocolBattle::new(b, ["p1", "p2"]);
    let out = pb.start();
    assert_eq!(
        out.update,
        lines(
            "|t:|0
|gametype|singles
|player|p1|p1||
|player|p2|p2||
|gen|1
|tier|[Gen 1] OU
|rule|Desync Clause Mod: Desyncs resolve to link battle behavior from the acting player's perspective.
|rule|HP Percentage Mod: HP is shown in percentages
|rule|Endless Battle Clause: Forcing endless battles is banned
|rule|Sleep Clause Mod: Limit one foe put to sleep
|rule|Freeze Clause Mod: Limit one foe frozen
|rule|Species Clause: Limit one of each Pokémon
|rule|OHKO Clause: OHKO moves are banned
|rule|Evasion Moves Clause: Evasion moves are banned
|
|t:|0
|teamsize|p1|2
|teamsize|p2|2
|start
|split|p1
|switch|p1a: Tauros|Tauros|290/290
|switch|p1a: Tauros|Tauros|100/100
|split|p2
|switch|p2a: Gengar|Gengar|260/260
|switch|p2a: Gengar|Gengar|100/100
|turn|1"
        )
    );
    assert_eq!(out.requests[1].as_deref(), Some(r#"{"active":[{"moves":[{"move":"Hypnosis","id":"hypnosis","pp":32,"maxpp":32,"target":"normal","disabled":false},{"move":"Night Shade","id":"nightshade","pp":24,"maxpp":24,"target":"normal","disabled":false},{"move":"Thunderbolt","id":"thunderbolt","pp":24,"maxpp":24,"target":"normal","disabled":false},{"move":"Explosion","id":"explosion","pp":8,"maxpp":8,"target":"normal","disabled":false}]}],"side":{"name":"p2","id":"p2","pokemon":[{"ident":"p2: Gengar","details":"Gengar","condition":"260/260","active":true,"stats":{"atk":165,"def":155,"spa":295,"spd":295,"spe":255},"moves":["hypnosis","nightshade","thunderbolt","explosion"],"baseAbility":"","item":"","pokeball":"pokeball"},{"ident":"p2: Farfetch’d","details":"Farfetch’d, L74","condition":"183/183","active":false,"stats":{"atk":123,"def":108,"spa":113,"spd":113,"spe":116},"moves":["swordsdance","slash"],"baseAbility":"","item":"","pokeball":"pokeball"}]}}"#));
    assert_eq!(
        out.view(2)[18..],
        lines("|start\n|switch|p1a: Tauros|Tauros|100/100\n|switch|p2a: Gengar|Gengar|260/260\n|turn|1")[..]
    );
    let out = pb.step_str("move 3", "move 1").unwrap();
    assert_eq!(
        out.update,
        lines("|\n|t:|0\n|move|p2a: Gengar|Hypnosis|p1a: Tauros\n|-status|p1a: Tauros|slp|[from] move: Hypnosis\n|cant|p1a: Tauros|slp\n|\n|upkeep\n|turn|2")
    );
    assert_eq!(out.requests[0].as_deref().map(|r| &r[..48]), Some(r#"{"active":[{"moves":[{"move":"Fight","id":"fight"#));
    let out = pb.step_str("move 1", "move 1").unwrap();
    assert!(out.update.contains(&"|-fail|p1a: Tauros|slp".to_string()));
}

fn teams(seed: u64) -> (Vec<MonSpec>, Vec<MonSpec>) {
    // A fixed mixed bag (status moves, trapping, Substitute, Transform, two-turn moves).
    let a = vec![
        mon("Jynx", 100, &["lovelykiss", "blizzard", "psychic", "rest"]),
        mon("Tauros", 100, &["bodyslam", "hyperbeam", "earthquake", "blizzard"]),
        mon("Dragonite", 100, &["wrap", "agility", "hyperbeam", "thunderwave"]),
        mon("Gengar", 100, &["hypnosis", "nightshade", "thunderbolt", "explosion"]),
        mon("Ditto", 100, &["transform"]),
        mon("Slowbro", 100, &["amnesia", "surf", "thunderwave", "rest"]),
    ];
    let b = vec![
        mon("Chansey", 100, &["softboiled", "thunderwave", "icebeam", "seismictoss"]),
        mon("Snorlax", 100, &["bodyslam", "rest", "selfdestruct", "reflect"]),
        mon("Zapdos", 100, &["thunderbolt", "drillpeck", "thunderwave", "agility"]),
        mon("Cloyster", 100, &["clamp", "blizzard", "explosion", "supersonic"]),
        mon("Alakazam", 100, &["psychic", "recover", "seismictoss", "reflect"]),
        mon("Persian", 100, &["slash", "bubblebeam", "substitute", "skyattack"]),
    ];
    if seed % 2 == 0 {
        (a, b)
    } else {
        (b, a)
    }
}

/// The protocol sink does not change the game, every live step ends with requests for both
/// players, and the views keep each side's exact HP private.
#[test]
fn protocol_invariants_over_random_games() {
    let mut buf = [Choice::Pass; 16];
    for seed in 0..300u64 {
        let (t1, t2) = teams(seed);
        let b = Battle::new(&t1, &t2, Prng::from_u64(seed * 7919 + 3), Rules::OU).unwrap();
        let mut silent = b;
        let mut pb = ProtocolBattle::new(b, ["Red", "Blue"]);
        pb.start();
        silent.update(Choice::Pass, Choice::Pass, &mut ()).unwrap();
        let mut r = seed;
        for _ in 0..2000 {
            if pb.battle.ended {
                break;
            }
            let mut c = [Choice::Pass; 2];
            for p in 0..2 {
                let n = pb.battle.choices(p, &mut buf);
                r = r.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                c[p] = buf[(r >> 33) as usize % n];
            }
            let out = pb.step(c[0], c[1]).unwrap();
            silent.update(c[0], c[1], &mut ()).unwrap();
            assert_eq!(pb.battle, silent, "seed {seed}: the protocol sink changed the battle");
            assert_eq!(out.update.first().map(|s| s.as_str()), Some("|"), "seed {seed}: each update starts a turnLoop");
            if pb.battle.ended {
                assert!(out.requests.iter().all(|r| r.is_none()));
                let last = out.update.last().unwrap();
                assert!(last == "|tie" || last.starts_with("|win|"), "seed {seed}: ends with {last}");
            } else {
                assert!(out.requests.iter().all(|r| r.as_deref().is_some_and(|j| j.starts_with('{') && j.ends_with('}'))));
            }
            // Split blocks: `|split|pN`, secret, shared; the shared copy never has exact HP of N.
            let u = &out.update;
            let mut i = 0;
            while i < u.len() {
                if u[i].starts_with("|split|p") {
                    assert!(i + 2 < u.len());
                    i += 3;
                } else {
                    assert!(!u[i].is_empty());
                    i += 1;
                }
            }
            for ch in [0, 1, 2] {
                let v = extract_channel(u, ch);
                assert!(v.iter().all(|l| !l.starts_with("|split|")));
            }
            let spec = extract_channel(u, 0);
            for l in &spec {
                let parts: Vec<&str> = l.split('|').collect();
                if matches!(parts.get(1), Some(&"-damage") | Some(&"-heal") | Some(&"switch")) {
                    let hp = parts[if parts[1] == "switch" { 4 } else { 3 }];
                    assert!(hp == "0 fnt" || hp.split(' ').next().unwrap().ends_with("/100"), "spectator sees exact HP: {l}");
                }
            }
        }
    }
}
