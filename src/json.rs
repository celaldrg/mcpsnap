//! A small, strict JSON reader/writer. Objects keep their key order so that
//! cassettes and diffs stay readable.

use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Numbers keep their source text; comparison is numeric when possible.
    Num(String),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => n.parse().ok(),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }
    /// Semantic equality: numbers compare by value, object key order is ignored.
    pub fn same(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => match (a.parse::<f64>(), b.parse::<f64>()) {
                (Ok(x), Ok(y)) => x == y,
                _ => a == b,
            },
            (Value::Arr(a), Value::Arr(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same(y))
            }
            (Value::Obj(a), Value::Obj(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(k, v)| other.get(k).map_or(false, |w| v.same(w)))
            }
            _ => self == other,
        }
    }
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        write_value(&mut s, self);
        s
    }
}

pub fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    write_str(&mut out, s);
    out
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
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

fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => out.push_str(n),
        Value::Str(s) => write_str(out, s),
        Value::Arr(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, x);
            }
            out.push(']');
        }
        Value::Obj(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k);
                out.push(':');
                write_value(out, x);
            }
            out.push('}');
        }
    }
}

pub fn parse(src: &str) -> Result<Value, String> {
    let mut p = Parser { b: src.as_bytes(), i: 0, depth: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing characters at byte {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn err<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("{} at byte {}", msg, self.i))
    }
    fn lit(&mut self, word: &str, v: Value) -> Result<Value, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err("invalid literal")
        }
    }
    fn value(&mut self) -> Result<Value, String> {
        if self.i >= self.b.len() {
            return self.err("unexpected end of input");
        }
        match self.b[self.i] {
            b'n' => self.lit("null", Value::Null),
            b't' => self.lit("true", Value::Bool(true)),
            b'f' => self.lit("false", Value::Bool(false)),
            b'"' => Ok(Value::Str(self.string()?)),
            b'[' => {
                self.nest()?;
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(Value::Arr(a));
                }
                loop {
                    self.ws();
                    a.push(self.value()?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        _ => return self.err("expected ',' or ']'"),
                    }
                }
                self.depth -= 1;
                Ok(Value::Arr(a))
            }
            b'{' => {
                self.nest()?;
                self.i += 1;
                let mut m: Vec<(String, Value)> = Vec::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(Value::Obj(m));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return self.err("expected object key");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.peek() != Some(b':') {
                        return self.err("expected ':'");
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value()?;
                    if let Some(slot) = m.iter_mut().find(|(kk, _)| *kk == k) {
                        slot.1 = v; // last duplicate wins, like most parsers
                    } else {
                        m.push((k, v));
                    }
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        _ => return self.err("expected ',' or '}'"),
                    }
                }
                self.depth -= 1;
                Ok(Value::Obj(m))
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => self.err("unexpected character"),
        }
    }
    fn nest(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > 256 {
            return self.err("nesting too deep");
        }
        Ok(())
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let s = p.i;
            while matches!(p.peek(), Some(b'0'..=b'9')) {
                p.i += 1;
            }
            p.i > s
        };
        if self.peek() == Some(b'0') {
            self.i += 1;
        } else if !digits(self) {
            return self.err("invalid number");
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if !digits(self) {
                return self.err("invalid number");
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return self.err("invalid number");
            }
        }
        Ok(Value::Num(String::from_utf8_lossy(&self.b[start..self.i]).into_owned()))
    }
    fn hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.b.len() {
            return self.err("short \\u escape");
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|e| e.to_string())?;
        let v = u32::from_str_radix(s, 16).or_else(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return self.err("unterminated string"),
            };
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = match self.peek() {
                        Some(e) => e,
                        None => return self.err("unterminated escape"),
                    };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi)
                                && self.b[self.i..].starts_with(b"\\u")
                            {
                                self.i += 2;
                                let lo = self.hex4()?;
                                0x10000 + ((hi - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF)
                            } else {
                                hi
                            };
                            char::from_u32(code).unwrap_or('\u{FFFD}')
                        }
                        _ => return self.err("invalid escape"),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return self.err("control character in string"),
                c => out.push(c),
            }
        }
        String::from_utf8(out).or_else(|_| self.err("invalid UTF-8 in string"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let src = r#"{"a":[1,2.5,-3e2,true,null],"b":{"c":"x\"y\n\u00e9"},"z":{}}"#;
        let v = parse(src).unwrap();
        assert_eq!(parse(&v.to_json()).unwrap(), v);
        assert_eq!(v.get("b").unwrap().get("c").unwrap().as_str(), Some("x\"y\né"));
    }

    #[test]
    fn surrogates() {
        let v = parse(r#""\ud83d\ude80""#).unwrap();
        assert_eq!(v.as_str(), Some("🚀"));
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "{", "[1,]", "{\"a\" 1}", "01", "tru", "\"abc", "1 2", "Loaded 3 notes"] {
            assert!(parse(bad).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn semantic_equality() {
        let a = parse(r#"{"x":1,"y":[1.0,2]}"#).unwrap();
        let b = parse(r#"{"y":[1,2.0],"x":1e0}"#).unwrap();
        assert!(a.same(&b));
        assert!(!a.same(&parse(r#"{"x":1}"#).unwrap()));
    }
}
