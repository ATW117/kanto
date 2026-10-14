//! The native battle's state in the runner's snapshot format (parity/showdown/runner.js
//! `snapshot`), so the two can be compared field by field.

use serde_json::{json, Map, Value};
use kanto_data::gen1::{SPECIES, TYPE_NAMES};
use kanto_gen1::state::{move_id_str, st, v, Battle, Side, V_IDS};

fn mid(id: u8) -> Value {
    if id == 0 {
        Value::Null
    } else {
        Value::String(move_id_str(id).to_string())
    }
}

fn stat_obj(s: [u16; 5]) -> Value {
    json!({"atk": s[0], "def": s[1], "spa": s[2], "spd": s[3], "spe": s[4]})
}

fn side_snap(b: &Battle, p: usize) -> Value {
    let sd = &b.sides[p];
    let mut team = Vec::new();
    for i in 0..sd.n as usize {
        let m = &sd.mons[i];
        let mut o = Map::new();
        o.insert("hp".into(), json!(m.hp));
        o.insert("status".into(), json!(st::IDS[m.status as usize]));
        o.insert("fainted".into(), json!(m.fainted()));
        o.insert("speed".into(), json!(sd.mon_speed(i)));
        o.insert("pp".into(), json!(m.moves[..m.n_moves as usize].iter().map(|s| s.pp()).collect::<Vec<_>>()));
        o.insert("subFainted".into(), json!(m.sub_fainted()));
        if m.status == st::SLP {
            o.insert("slp".into(), json!([m.slp_time, m.slp_start]));
        }
        if (st::SLP..=st::TOX).contains(&m.status) {
            o.insert("statusSelf".into(), json!(m.status_self()));
        }
        if i != sd.active_slot() {
            o.insert("modified".into(), stat_obj(m.bench_modified()));
        }
        team.push(Value::Object(o));
    }
    let a = &sd.active;
    let active = if a.species == 0 {
        Value::Null
    } else {
        let types: Vec<&str> = if a.types[0] == a.types[1] {
            vec![TYPE_NAMES[a.types[0] as usize]]
        } else {
            vec![TYPE_NAMES[a.types[0] as usize], TYPE_NAMES[a.types[1] as usize]]
        };
        let bo = a.boosts;
        let moves: Vec<Value> = (0..a.n_slots as usize)
            .map(|i| {
                let s = sd.slot(i);
                json!([move_id_str(s.id), s.pp(), s.disabled(), sd.slot_maxpp(i)])
            })
            .collect();
        let mut vols = Map::new();
        for id in 0..v::COUNT as u8 {
            if a.has(id) {
                vols.insert(V_IDS[id as usize].into(), vol_snap(sd, id));
            }
        }
        // Insertion order, as the simulator itself iterates it for handler sorting.
        let vol_order: Vec<&str> = b.vol_order(p).map(|id| V_IDS[id as usize]).collect();
        json!({
            "slot": sd.order[0],
            "species": SPECIES[a.species as usize].id,
            "types": types,
            "boosts": {"atk": bo[0], "def": bo[1], "spa": bo[2], "spd": bo[3], "spe": bo[4], "accuracy": bo[5], "evasion": bo[6]},
            "stats": stat_obj(a.modified),
            "stored": stat_obj(a.stored),
            "moves": moves,
            "volatiles": vols,
            "volOrder": vol_order,
            "lastMove": mid(a.last_move),
            "moveThisTurn": move_id_str(a.move_this_turn),
            "trapped": sd.mon().trapped(),
            "maybeLocked": sd.mon().maybe_locked(),
            "transformed": a.transformed,
        })
    };
    json!({
        "order": sd.order[..sd.n as usize].to_vec(),
        "left": sd.left,
        "lastSelectedMove": move_id_str(sd.last_selected_move),
        "lastEnemySelectedMove": move_id_str(sd.last_enemy_selected_move),
        "lastSelectedMoveSlot": sd.last_selected_move_slot,
        "lastMove": mid(sd.last_move),
        "lastEnemyMove": mid(sd.last_enemy_move),
        "team": team,
        "active": active,
    })
}

fn vol_snap(sd: &Side, id: u8) -> Value {
    let a = &sd.active;
    match id {
        v::CONFUSION => json!({"time": a.confusion_time}),
        v::FLINCH => json!({"duration": a.flinch_dur}),
        v::PARTIALLYTRAPPED => {
            if a.pt_maybe_locked {
                json!({"duration": a.pt_dur, "maybeLocked": true, "source": a.pt_source})
            } else {
                json!({"duration": a.pt_dur, "source": a.pt_source})
            }
        }
        v::FAKEPARTIALLYTRAPPED => json!({"duration": a.fpt_dur, "counterpart": a.fpt_counterpart}),
        v::PARTIALTRAPPINGLOCK => json!({
            "duration": a.ptl_dur, "totalDuration": a.ptl_total, "damage": a.ptl_damage, "move": move_id_str(a.ptl_move),
            "locked": a.ptl_locked
        }),
        v::LOCKEDMOVE => json!({"time": a.lockedmove_time, "accuracy": a.lockedmove_acc, "move": move_id_str(a.lockedmove_move)}),
        v::TWOTURNMOVE => json!({"move": move_id_str(a.twoturn_move)}),
        v::BIDE => json!({"time": a.bide_time, "damage": a.bide_damage}),
        v::DISABLE => json!({"time": a.disable_time, "slotIndex": a.disable_slot, "move": move_id_str(a.disable_move)}),
        v::SUBSTITUTE => json!({"hp": a.sub_hp}),
        v::RAGE => json!({"accuracy": a.rage_acc, "move": "rage"}),
        v::RESIDUALDMG => json!({"counter": a.residual_counter}),
        _ => json!({}),
    }
}

pub fn snapshot(b: &Battle) -> Value {
    json!({
        "turn": b.turn,
        "seed": b.rng.seed_string(),
        "lastDamage": b.last_damage,
        "ended": b.ended,
        "winner": match b.winner { 1 => "p1", 2 => "p2", _ => "" },
        "sides": [side_snap(b, 0), side_snap(b, 1)],
    })
}

/// Paths where `a` and `b` differ (object keys compared as sets; arrays element-wise).
pub fn diff(a: &Value, b: &Value, path: &mut String, out: &mut Vec<(String, String, String)>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let len = path.len();
                path.push('.');
                path.push_str(k);
                match (x.get(k), y.get(k)) {
                    (Some(u), Some(w)) => diff(u, w, path, out),
                    (u, w) => out.push((path.clone(), fmt(u), fmt(w))),
                }
                path.truncate(len);
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (u, w)) in x.iter().zip(y).enumerate() {
                let len = path.len();
                path.push_str(&format!("[{i}]"));
                diff(u, w, path, out);
                path.truncate(len);
            }
        }
        _ => {
            if !num_eq(a, b) {
                out.push((path.clone(), a.to_string(), b.to_string()));
            }
        }
    }
}

fn num_eq(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn fmt(v: Option<&Value>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "<missing>".into())
}
