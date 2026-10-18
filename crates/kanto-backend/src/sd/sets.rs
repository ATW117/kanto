//! Showdown team handling as the simulator (not the validator) does it: `Teams.unpack`,
//! `Teams.pack`, and the changes `new Pokemon(set)` makes to each set object, ported from
//! pokemon-showdown 0.11.11 (`sim/teams.ts`, `sim/pokemon.ts`), plus the conversion to the native
//! simulator's [`MonSpec`].

use super::dex;
use super::json::{Obj, Value};
use kanto_data::gen1::{MOVES, SPECIES};
use kanto_gen1::MonSpec;

/// Showdown's `toID`.
pub fn to_id(s: &str) -> String {
    s.chars().filter_map(|c| {
        let c = c.to_ascii_lowercase();
        (c.is_ascii_lowercase() || c.is_ascii_digit()).then_some(c)
    }).collect()
}

/// `Teams.packName`: drop everything but ASCII letters and digits.
pub fn pack_name(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

fn lookup(table: &[(&'static str, &'static str)], id: &str) -> Option<&'static str> {
    table.binary_search_by(|(k, _)| (*k).cmp(id)).ok().map(|i| table[i].1)
}

/// `Teams.unpackName(name, dexTable)`.
pub fn unpack_name(name: &str, table: Option<&[(&'static str, &'static str)]>) -> String {
    if name.is_empty() {
        return String::new();
    }
    if let Some(t) = table {
        if let Some(n) = lookup(t, &to_id(name)) {
            return n.to_string();
        }
    }
    // name.replace(/([0-9]+)/g, " $1 ").replace(/([A-Z])/g, " $1").replace(/[ ][ ]/g, " ").trim()
    let mut a = String::new();
    let mut in_num = false;
    for c in name.chars() {
        if c.is_ascii_digit() {
            if !in_num {
                a.push(' ');
                in_num = true;
            }
            a.push(c);
        } else {
            if in_num {
                a.push(' ');
                in_num = false;
            }
            a.push(c);
        }
    }
    if in_num {
        a.push(' ');
    }
    let mut b = String::new();
    for c in a.chars() {
        if c.is_ascii_uppercase() {
            b.push(' ');
        }
        b.push(c);
    }
    // One pass of "  " -> " " (non-overlapping, like a global regex replace).
    let c = b.replace("  ", " ");
    c.trim_matches(|ch: char| ch.is_whitespace()).to_string()
}

/// JS `Number(s) || 0` for a packed EV/IV field.
fn num_or_zero(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    let v = js_number_parse(t);
    if v.is_nan() {
        0.0
    } else {
        v
    }
}

/// JS `Number(string)` (decimal, hex `0x`, `Infinity`; NaN otherwise).
pub fn js_number_parse(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(h, 16).map(|v| v as f64).unwrap_or(f64::NAN);
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')) {
        t.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

/// JS `parseInt(s)` (base 10, or 16 with a `0x` prefix); `None` = NaN.
pub fn js_parse_int(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let (radix, t) = if t.starts_with("0x") || t.starts_with("0X") { (16, &t[2..]) } else { (10, t) };
    let digits: String = t.chars().take_while(|c| c.is_digit(radix)).collect();
    if digits.is_empty() {
        return None;
    }
    let v = i64::from_str_radix(&digits[..digits.len().min(15)], radix).ok()?;
    Some(if neg { -v } else { v })
}

/// `Teams.unpack(buf)`: `None` = Showdown's `null` (a malformed team).
pub fn unpack(buf: &str) -> Option<Vec<Value>> {
    if buf.is_empty() {
        return None;
    }
    let owned;
    let mut buf = buf;
    if buf.starts_with('[') && buf.ends_with(']') {
        let v = Value::parse(buf).ok()?;
        owned = pack(v.as_arr()?);
        buf = &owned;
    }
    let mut team = Vec::new();
    let mut i = 0usize;
    let find = |from: usize, c: char| buf[from..].find(c).map(|k| k + from);
    for _ in 0..24 {
        let mut set = Obj::new();
        let j = find(i, '|')?;
        set.set("name", Value::str(&buf[i..j]));
        i = j + 1;
        let j = find(i, '|')?;
        let sp = unpack_name(&buf[i..j], Some(&dex::SPECIES));
        let species = if sp.is_empty() { set.get("name").unwrap().as_str().unwrap().to_string() } else { sp };
        set.set("species", Value::Str(species.clone()));
        i = j + 1;
        let j = find(i, '|')?;
        set.set("item", Value::Str(unpack_name(&buf[i..j], Some(&dex::ITEMS))));
        i = j + 1;
        let j = find(i, '|')?;
        let ability = &buf[i..j];
        let ab = if ["", "0", "1", "H", "S"].contains(&ability) {
            // species.abilities[ability || '0'] in the base dex; only slot 0 is known here.
            let sid = to_id(&species);
            let n = SPECIES.iter().position(|s| s.id == sid);
            match (ability, n) {
                ("" | "0", Some(n)) if n > 0 => dex::ABILITY0[n].to_string(),
                ("", _) => String::new(),
                // Other slots / non-Gen 1 species: not supported by the native simulator.
                _ => "!!!ERROR!!!".to_string(),
            }
        } else {
            unpack_name(ability, Some(&dex::ABILITIES))
        };
        set.set("ability", Value::Str(ab));
        i = j + 1;
        let j = find(i, '|')?;
        let moves: Vec<Value> = buf[i..j].split(',').take(24).map(|m| Value::Str(unpack_name(m, Some(&dex::MOVES)))).collect();
        set.set("moves", Value::Arr(moves));
        i = j + 1;
        let j = find(i, '|')?;
        set.set("nature", Value::Str(unpack_name(&buf[i..j], Some(&dex::NATURES))));
        i = j + 1;
        let j = find(i, '|')?;
        if j != i {
            let evs: Vec<&str> = buf[i..j].split(',').take(6).collect();
            let mut o = Obj::new();
            for (k, stat) in ["hp", "atk", "def", "spa", "spd", "spe"].iter().enumerate() {
                o.set(stat, Value::Num(evs.get(k).map(|s| num_or_zero(s)).unwrap_or(0.0)));
            }
            set.set("evs", Value::Obj(o));
        }
        i = j + 1;
        let j = find(i, '|')?;
        if i != j {
            set.set("gender", Value::str(&buf[i..j]));
        }
        i = j + 1;
        let j = find(i, '|')?;
        if j != i {
            let ivs: Vec<&str> = buf[i..j].split(',').take(6).collect();
            let mut o = Obj::new();
            for (k, stat) in ["hp", "atk", "def", "spa", "spd", "spe"].iter().enumerate() {
                let v = match ivs.get(k) {
                    Some(s) if s.is_empty() => 31.0,
                    Some(s) => num_or_zero(s),
                    None => 0.0,
                };
                o.set(stat, Value::Num(v));
            }
            set.set("ivs", Value::Obj(o));
        }
        i = j + 1;
        let j = find(i, '|')?;
        if i != j {
            set.set("shiny", Value::Bool(true));
        }
        i = j + 1;
        let j = find(i, '|')?;
        if i != j {
            match js_parse_int(&buf[i..j]) {
                Some(l) => set.set("level", Value::Num(l as f64)),
                None => set.set("level", Value::Num(f64::NAN)),
            }
        }
        i = j + 1;
        let jb = find(i, ']');
        let misc: Option<Vec<&str>> = match jb {
            None => (i < buf.len()).then(|| buf[i..].split(',').take(6).collect()),
            Some(j) => (i != j).then(|| buf[i..j].split(',').take(6).collect()),
        };
        if let Some(m) = misc {
            let g = |k: usize| m.get(k).copied().unwrap_or("");
            set.set("happiness", Value::Num(if g(0).is_empty() { 255.0 } else { js_number_parse(g(0)) }));
            set.set("hpType", Value::str(g(1)));
            set.set("pokeball", Value::Str(unpack_name(g(2), Some(&dex::ITEMS))));
            set.set("gigantamax", Value::Bool(!g(3).is_empty()));
            set.set("dynamaxLevel", Value::Num(if g(4).is_empty() { 10.0 } else { js_number_parse(g(4)) }));
            // (`misc[5]` may be undefined: JSON.stringify then leaves the key out.)
            if let Some(t) = m.get(5) {
                set.set("teraType", Value::str(t));
            }
        }
        team.push(Value::Obj(set));
        match jb {
            None => break,
            Some(j) => i = j + 1,
        }
    }
    Some(team)
}

fn field_str(set: &Obj, k: &str) -> String {
    match set.get(k) {
        None | Some(Value::Null) => String::new(),
        Some(v) => v.to_js_string(),
    }
}

fn truthy(set: &Obj, k: &str) -> bool {
    set.get(k).is_some_and(Value::truthy)
}

/// `Teams.pack(team)`.
pub fn pack(team: &[Value]) -> String {
    let mut buf = String::new();
    for set in team {
        let empty = Obj::new();
        let set = set.as_obj().unwrap_or(&empty);
        if !buf.is_empty() {
            buf.push(']');
        }
        let name_or_species = if truthy(set, "name") { field_str(set, "name") } else { field_str(set, "species") };
        buf.push_str(&name_or_species);
        let species_or_name = if truthy(set, "species") { field_str(set, "species") } else { field_str(set, "name") };
        let id = pack_name(&species_or_name);
        buf.push('|');
        if pack_name(&name_or_species) != id {
            buf.push_str(&id);
        }
        buf.push('|');
        buf.push_str(&pack_name(&field_str(set, "item")));
        buf.push('|');
        buf.push_str(&pack_name(&field_str(set, "ability")));
        buf.push('|');
        let moves: Vec<String> = set.get("moves").and_then(Value::as_arr).map(|a| a.iter().map(|m| pack_name(&m.to_js_string())).collect()).unwrap_or_default();
        buf.push_str(&moves.join(","));
        buf.push('|');
        if truthy(set, "nature") {
            buf.push_str(&field_str(set, "nature"));
        }
        let stats = ["hp", "atk", "def", "spa", "spd", "spe"];
        let mut evs = "|".to_string();
        if let Some(e) = set.get("evs").filter(|v| v.truthy()) {
            let parts: Vec<String> = stats.iter().map(|s| e.get(s).filter(|v| v.truthy()).map(Value::to_js_string).unwrap_or_default()).collect();
            evs = format!("|{}", parts.join(","));
        }
        buf.push_str(if evs == "|,,,,," { "|" } else { &evs });
        buf.push('|');
        if truthy(set, "gender") {
            buf.push_str(&field_str(set, "gender"));
        }
        let mut ivs = "|".to_string();
        if let Some(iv) = set.get("ivs").filter(|v| v.truthy()) {
            let parts: Vec<String> = stats
                .iter()
                .map(|s| match iv.get(s) {
                    None => String::new(),
                    Some(Value::Num(n)) if *n == 31.0 => String::new(),
                    Some(v) => v.to_js_string(),
                })
                .collect();
            ivs = format!("|{}", parts.join(","));
        }
        buf.push_str(if ivs == "|,,,,," { "|" } else { &ivs });
        buf.push('|');
        if truthy(set, "shiny") {
            buf.push('S');
        }
        buf.push('|');
        if let Some(l) = set.get("level").filter(|v| v.truthy()) {
            if !matches!(l, Value::Num(n) if *n == 100.0) {
                buf.push_str(&l.to_js_string());
            }
        }
        buf.push('|');
        match set.get("happiness") {
            None => {}
            Some(Value::Num(n)) if *n == 255.0 => {}
            Some(v) => buf.push_str(&v.to_js_string()),
        }
        let dl_set = match set.get("dynamaxLevel") {
            None => false,
            Some(Value::Num(n)) => *n != 10.0,
            Some(_) => true,
        };
        if truthy(set, "pokeball") || truthy(set, "hpType") || truthy(set, "gigantamax") || dl_set || truthy(set, "teraType") {
            buf.push(',');
            buf.push_str(&field_str(set, "hpType"));
            buf.push(',');
            buf.push_str(&pack_name(&field_str(set, "pokeball")));
            buf.push(',');
            if truthy(set, "gigantamax") {
                buf.push('G');
            }
            buf.push(',');
            if dl_set {
                buf.push_str(&field_str(set, "dynamaxLevel"));
            }
            buf.push(',');
            if truthy(set, "teraType") {
                buf.push_str(&field_str(set, "teraType"));
            }
        }
    }
    buf
}

/// What the stream needs about one team member besides its [`MonSpec`].
#[derive(Clone, Debug, Default)]
pub struct SetInfo {
    /// `pokemon.name` (the nickname, or the species name).
    pub name: String,
    /// `toID(set.ability)`, `toID(set.item)`, `toID(set.pokeball) || "pokeball"`.
    pub ability: String,
    pub item: String,
    pub pokeball: String,
    /// `", M"` / `", F"` and `", shiny"` parts of `pokemon.details`.
    pub details_suffix: String,
}

/// `clampIntRange(num, min, max)`.
fn clamp_int(v: &Value, min: f64, max: f64) -> f64 {
    let n = match v {
        Value::Num(n) => n.floor(),
        _ => 0.0,
    };
    if n.is_nan() {
        // Math.floor(NaN) = NaN; NaN < min is false: it stays NaN.
        return n;
    }
    n.max(min).min(max)
}

/// Apply `new Pokemon(set)`'s changes to `set` (Gen 1 format) and return the native simulator's
/// view of it, or an error for sets the native simulator cannot reproduce exactly (abilities,
/// items, non-Gen 1 species or moves, levels above 100, more than 4 moves).
pub fn build_set(set: &mut Value) -> Result<(MonSpec, SetInfo), String> {
    let Some(set) = set.as_obj_mut() else {
        return Err("team member is not an object".into());
    };
    let species_name = if truthy(set, "species") { field_str(set, "species") } else { field_str(set, "name") };
    let sid = to_id(&species_name);
    let Some(species) = SPECIES.iter().position(|s| !s.id.is_empty() && s.id == sid).filter(|&n| (1..=151).contains(&n)) else {
        return Err(format!("unsupported species \"{species_name}\" (Gen 1 species only)"));
    };
    let base_name = SPECIES[species].name;
    let name_v = set.get("name").cloned();
    let species_v = set.get("species").cloned();
    if name_v == species_v || !name_v.as_ref().is_some_and(Value::truthy) {
        set.set("name", Value::str(base_name));
    }
    let full_name = field_str(set, "name");
    // set.name.substr(0, 20) counts UTF-16 code units.
    let units: Vec<u16> = full_name.encode_utf16().take(20).collect();
    let name = String::from_utf16_lossy(&units);
    let adjust = set.get("adjustLevel").filter(|v| v.truthy()).cloned();
    let lv_src = adjust.or_else(|| set.get("level").filter(|v| v.truthy()).cloned()).unwrap_or(Value::Num(100.0));
    let level = clamp_int(&lv_src, 1.0, 9999.0);
    set.set("level", Value::Num(level));
    if !(1.0..=100.0).contains(&level) {
        return Err(format!("{name}: level {} is not supported (1-100)", super::json::js_number(level)));
    }
    let gender = match set.get("gender").and_then(Value::as_str) {
        Some("M") => ", M",
        Some("F") => ", F",
        _ => "",
    };
    let shiny = if truthy(set, "shiny") { ", shiny" } else { "" };
    let pokeball = {
        let p = to_id(&field_str(set, "pokeball"));
        if p.is_empty() { "pokeball".to_string() } else { p }
    };
    // Moves: unknown names are skipped, as Showdown skips moves without an id.
    let mut moves = [0u8; 4];
    let mut n = 0;
    let move_names: Vec<String> = set.get("moves").and_then(Value::as_arr).map(|a| a.iter().map(Value::to_js_string).collect()).unwrap_or_default();
    for m in &move_names {
        let id = to_id(m);
        if id.is_empty() || lookup(&dex::MOVES, &id).is_none() {
            continue;
        }
        let Some(k) = MOVES.iter().position(|d| d.id == id).filter(|&k| (1..=164).contains(&k)) else {
            return Err(format!("{name}: unsupported move \"{m}\" (Gen 1 moves only)"));
        };
        if n == 4 {
            return Err(format!("{name}: more than 4 moves"));
        }
        if moves[..n].contains(&(k as u8)) {
            return Err(format!("{name}: duplicate move \"{m}\""));
        }
        moves[n] = k as u8;
        n += 1;
    }
    if n == 0 {
        return Err(format!("Set {name} has no moves"));
    }
    // EVs / IVs.
    if !truthy(set, "evs") {
        let mut o = Obj::new();
        for s in ["hp", "atk", "def", "spa", "spd", "spe"] {
            o.set(s, Value::Num(0.0));
        }
        set.set("evs", Value::Obj(o));
    }
    if !truthy(set, "ivs") {
        let mut o = Obj::new();
        for s in ["hp", "atk", "def", "spa", "spd", "spe"] {
            o.set(s, Value::Num(31.0));
        }
        set.set("ivs", Value::Obj(o));
    }
    {
        let evs = set.get_mut("evs").unwrap();
        let Some(evs) = evs.as_obj_mut() else { return Err(format!("{name}: bad EVs")) };
        for s in ["hp", "atk", "def", "spe", "spa", "spd"] {
            if !evs.get(s).is_some_and(Value::truthy) {
                evs.set(s, Value::Num(0.0));
            }
        }
        for (_, v) in evs.0.iter_mut() {
            *v = Value::Num(clamp_int(v, 0.0, 255.0));
        }
    }
    {
        let ivs = set.get_mut("ivs").unwrap();
        let Some(ivs) = ivs.as_obj_mut() else { return Err(format!("{name}: bad IVs")) };
        for s in ["hp", "atk", "def", "spe", "spa", "spd"] {
            let keep = ivs.get(s).is_some_and(|v| v.truthy() || matches!(v, Value::Num(n) if *n == 0.0));
            if !keep {
                ivs.set(s, Value::Num(31.0));
            }
        }
        for (_, v) in ivs.0.iter_mut() {
            let c = clamp_int(v, 0.0, 31.0);
            // Gen 1-2: `ivs[stat] &= 30` (NaN & 30 = 0).
            *v = Value::Num(if c.is_nan() { 0.0 } else { ((c as i64) & 30) as f64 });
        }
    }
    let stat = |set: &Obj, k: &str, s: &str| -> f64 {
        match set.get(k).and_then(|o| o.get(s)) {
            Some(Value::Num(n)) if !n.is_nan() => *n,
            _ => 0.0,
        }
    };
    let order = ["hp", "atk", "def", "spa", "spd", "spe"];
    let mut ivs = [0u8; 6];
    let mut evs = [0u8; 6];
    for (k, s) in order.iter().enumerate() {
        ivs[k] = stat(set, "ivs", s) as u8;
        evs[k] = stat(set, "evs", s) as u8;
    }
    let ability = to_id(&field_str(set, "ability"));
    if !ability.is_empty() && ability != "noability" {
        return Err(format!("{name}: ability \"{}\" is not supported (Showdown would apply it; use No Ability)", field_str(set, "ability")));
    }
    let item = to_id(&field_str(set, "item"));
    if !item.is_empty() {
        return Err(format!("{name}: items are not supported"));
    }
    // Showdown applies natures to the stats even in Gen 1; only the neutral ones are supported.
    let nature = to_id(&field_str(set, "nature"));
    if !nature.is_empty() && lookup(&dex::NATURES, &nature).is_some_and(|n| !matches!(n, "Bashful" | "Docile" | "Hardy" | "Quirky" | "Serious")) {
        return Err(format!("{name}: nature \"{}\" is not supported (Showdown applies it to the stats)", field_str(set, "nature")));
    }
    let spec = MonSpec { species: species as u8, level: level as u8, moves, ivs, evs };
    spec.validate().map_err(|e| format!("{name}: {e}"))?;
    Ok((spec, SetInfo { name, ability, item, pokeball, details_suffix: format!("{gender}{shiny}") }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpack_pack() {
        let t = unpack("Mr. Sparkles|snorlax|||bodyslam,rest|||||||").unwrap();
        assert_eq!(
            t[0].stringify(),
            r#"{"name":"Mr. Sparkles","species":"Snorlax","item":"","ability":"Immunity","moves":["Body Slam","Rest"],"nature":""}"#
        );
        let mut t2 = t.clone();
        // build_set rejects the (effective) ability, but still mutates like Showdown up to there.
        assert!(build_set(&mut t2[0]).is_err());
        let packed = "Tauros|||NoAbility|bodyslam,hyperbeam|||||||]Chansey|||NoAbility|softboiled|||||||";
        let mut t = unpack(packed).unwrap();
        for s in t.iter_mut() {
            build_set(s).unwrap();
        }
        assert_eq!(
            Value::Arr(t.clone()).stringify(),
            r#"[{"name":"Tauros","species":"Tauros","item":"","ability":"No Ability","moves":["Body Slam","Hyper Beam"],"nature":"","level":100,"evs":{"hp":0,"atk":0,"def":0,"spa":0,"spd":0,"spe":0},"ivs":{"hp":30,"atk":30,"def":30,"spa":30,"spd":30,"spe":30}},{"name":"Chansey","species":"Chansey","item":"","ability":"No Ability","moves":["Soft-Boiled"],"nature":"","level":100,"evs":{"hp":0,"atk":0,"def":0,"spa":0,"spd":0,"spe":0},"ivs":{"hp":30,"atk":30,"def":30,"spa":30,"spd":30,"spe":30}}]"#
        );
        assert_eq!(unpack_name("BodySlam", None), "Body Slam");
        assert_eq!(unpack_name("Thunderbolt1", None), "Thunderbolt 1");
    }
}
