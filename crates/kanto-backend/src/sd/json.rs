//! A small JSON value with JavaScript semantics where Showdown's output depends on them: objects
//! keep insertion order (as JS objects do for non-index keys), numbers are `f64`, and
//! [`Value::stringify`] formats like `JSON.stringify`.

use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Obj),
}

/// An insertion-ordered object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Obj(pub Vec<(String, Value)>);

impl Obj {
    pub fn new() -> Obj {
        Obj(Vec::new())
    }

    pub fn get(&self, k: &str) -> Option<&Value> {
        self.0.iter().find(|(key, _)| key == k).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, k: &str) -> Option<&mut Value> {
        self.0.iter_mut().find(|(key, _)| key == k).map(|(_, v)| v)
    }

    /// `obj[k] = v`: replaces in place, or appends a new key.
    pub fn set(&mut self, k: &str, v: Value) {
        match self.get_mut(k) {
            Some(slot) => *slot = v,
            None => self.0.push((k.to_string(), v)),
        }
    }

    /// `delete obj[k]`.
    pub fn delete(&mut self, k: &str) -> bool {
        let n = self.0.len();
        self.0.retain(|(key, _)| key != k);
        self.0.len() != n
    }

    pub fn has(&self, k: &str) -> bool {
        self.get(k).is_some()
    }
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&Obj> {
        match self {
            Value::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_obj_mut(&mut self) -> Option<&mut Obj> {
        match self {
            Value::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn get(&self, k: &str) -> Option<&Value> {
        self.as_obj().and_then(|o| o.get(k))
    }

    /// JavaScript truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::Arr(_) | Value::Obj(_) => true,
        }
    }

    /// `String(value)` / template-literal conversion.
    pub fn to_js_string(&self) -> String {
        match self {
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => js_number(*n),
            Value::Str(s) => s.clone(),
            Value::Arr(a) => a.iter().map(|v| if matches!(v, Value::Null) { String::new() } else { v.to_js_string() }).collect::<Vec<_>>().join(","),
            Value::Obj(_) => "[object Object]".into(),
        }
    }

    /// `JSON.stringify(value)`.
    pub fn stringify(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Num(n) => {
                if n.is_finite() {
                    out.push_str(&js_number(*n))
                } else {
                    out.push_str("null")
                }
            }
            Value::Str(s) => quote(out, s),
            Value::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Value::Obj(o) => {
                out.push('{');
                for (i, (k, v)) in o.0.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    quote(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    /// `JSON.parse(text)`.
    pub fn parse(text: &str) -> Result<Value, String> {
        let mut p = Parser { s: text.as_bytes(), i: 0, text };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.s.len() {
            return Err(format!("Unexpected non-whitespace character after JSON at position {}", p.i));
        }
        Ok(v)
    }
}

/// `JSON.stringify` of a string.
pub fn quote(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `Number#toString()` for the values Showdown handles (integers and short decimals).
pub fn js_number(n: f64) -> String {
    if n == 0.0 {
        return "0".into();
    }
    if n.fract() == 0.0 && n.abs() < 1e21 {
        return format!("{}", n as i128);
    }
    // Rust's shortest round-trip formatting agrees with JS for non-exponent ranges.
    let s = format!("{n}");
    if n.abs() >= 1e21 || (n.abs() < 1e-6) {
        // JS switches to exponent notation here; rare enough to approximate.
        let e = format!("{n:e}");
        return e.replace("e", "e+").replace("e+-", "e-");
    }
    s
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    text: &'a str,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn err<T>(&self) -> Result<T, String> {
        if self.i >= self.s.len() {
            Err("Unexpected end of JSON input".into())
        } else {
            let c = self.text[self.i..].chars().next().unwrap_or('?');
            Err(format!("Unexpected token '{c}', ... is not valid JSON"))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        if self.i >= self.s.len() {
            return self.err();
        }
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                let mut o = Obj::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Obj(o));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return self.err();
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return self.err();
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value()?;
                    // Duplicate keys: the last value wins, at the first key's position.
                    o.set(&k, v);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(o));
                        }
                        _ => return self.err(),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Value::Arr(a));
                }
                loop {
                    self.ws();
                    a.push(self.value()?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Arr(a));
                        }
                        _ => return self.err(),
                    }
                }
            }
            b'"' => Ok(Value::Str(self.string()?)),
            b't' if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Value::Bool(true))
            }
            b'f' if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Value::Bool(false))
            }
            b'n' if self.s[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Value::Null)
            }
            b'-' | b'0'..=b'9' => {
                let start = self.i;
                if self.s[self.i] == b'-' {
                    self.i += 1;
                }
                let digits = |p: &mut Self| {
                    let st = p.i;
                    while p.i < p.s.len() && p.s[p.i].is_ascii_digit() {
                        p.i += 1;
                    }
                    p.i - st
                };
                if self.s.get(self.i) == Some(&b'0') {
                    self.i += 1;
                } else if digits(self) == 0 {
                    return self.err();
                }
                if self.s.get(self.i) == Some(&b'.') {
                    self.i += 1;
                    if digits(self) == 0 {
                        return self.err();
                    }
                }
                if matches!(self.s.get(self.i), Some(b'e') | Some(b'E')) {
                    self.i += 1;
                    if matches!(self.s.get(self.i), Some(b'+') | Some(b'-')) {
                        self.i += 1;
                    }
                    if digits(self) == 0 {
                        return self.err();
                    }
                }
                self.text[start..self.i].parse::<f64>().map(Value::Num).map_err(|e| e.to_string())
            }
            _ => self.err(),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let Some(&c) = self.s.get(self.i) else {
                return Err("Unterminated string in JSON".into());
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let Some(&e) = self.s.get(self.i) else { return self.err() };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = |p: &mut Self| -> Result<u32, String> {
                                let h = p.text.get(p.i..p.i + 4).ok_or("Bad Unicode escape in JSON")?;
                                p.i += 4;
                                u32::from_str_radix(h, 16).map_err(|_| "Bad Unicode escape in JSON".to_string())
                            };
                            let u = hex(self)?;
                            if (0xD800..0xDC00).contains(&u) && self.s[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = hex(self)?;
                                let cp = 0x10000 + ((u - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                                out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            } else {
                                out.push(char::from_u32(u).unwrap_or('\u{FFFD}'));
                            }
                        }
                        _ => return self.err(),
                    }
                }
                c if c < 0x20 => return self.err(),
                _ => {
                    // Copy one UTF-8 character.
                    let ch = self.text[self.i..].chars().next().unwrap();
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_order() {
        let t = r#"{"name":"Bob","avatar":"red","rating":1500,"team":"x|y","nested":{"b":1,"a":[true,null,-2.5]}}"#;
        assert_eq!(Value::parse(t).unwrap().stringify(), t);
        assert_eq!(Value::parse(r#"{"s":"a\"\\\n\u0001é"}"#).unwrap().stringify(), r#"{"s":"a\"\\\n\u0001é"}"#);
    }
}
