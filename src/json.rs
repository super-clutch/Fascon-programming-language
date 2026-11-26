//! 最小の JSON（LSP 用。外部クレートなし） / minimal JSON (for LSP; no external crates)

use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(BTreeMap<String, J>),
}

impl J {
    pub fn get(&self, k: &str) -> &J {
        static NULL: J = J::Null;
        match self {
            J::Obj(m) => m.get(k).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    pub fn at(&self, i: usize) -> &J {
        static NULL: J = J::Null;
        match self {
            J::Arr(v) => v.get(i).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            J::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        self.as_f64().map(|f| f as i64)
    }
    pub fn is_null(&self) -> bool {
        matches!(self, J::Null)
    }
    pub fn obj(pairs: Vec<(&str, J)>) -> J {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        J::Obj(m)
    }
    pub fn s(v: impl Into<String>) -> J {
        J::Str(v.into())
    }
    pub fn n(v: f64) -> J {
        J::Num(v)
    }
    #[allow(dead_code)]
    pub fn set(&mut self, k: &str, v: J) {
        if let J::Obj(m) = self {
            m.insert(k.to_string(), v);
        }
    }
    pub fn dump(&self) -> String {
        let mut out = String::new();
        write_json(self, &mut out);
        out
    }
}

fn write_str(s: &str, out: &mut String) {
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

fn write_json(v: &J, out: &mut String) {
    match v {
        J::Null => out.push_str("null"),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Num(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                let _ = write!(out, "{}", *n as i64);
            } else {
                let _ = write!(out, "{}", n);
            }
        }
        J::Str(s) => write_str(s, out),
        J::Arr(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(x, out);
            }
            out.push(']');
        }
        J::Obj(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(k, out);
                out.push(':');
                write_json(x, out);
            }
            out.push('}');
        }
    }
}

pub fn parse(s: &str) -> Result<J, String> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let v = parse_value(b, &mut i)?;
    skip_ws(b, &mut i);
    Ok(v)
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

fn parse_value(b: &[u8], i: &mut usize) -> Result<J, String> {
    skip_ws(b, i);
    if *i >= b.len() {
        return Err("unexpected end".into());
    }
    match b[*i] {
        b'n' => {
            *i += 4;
            Ok(J::Null)
        }
        b't' => {
            *i += 4;
            Ok(J::Bool(true))
        }
        b'f' => {
            *i += 5;
            Ok(J::Bool(false))
        }
        b'"' => parse_string(b, i).map(J::Str),
        b'[' => {
            *i += 1;
            let mut v = Vec::new();
            skip_ws(b, i);
            if *i < b.len() && b[*i] == b']' {
                *i += 1;
                return Ok(J::Arr(v));
            }
            loop {
                v.push(parse_value(b, i)?);
                skip_ws(b, i);
                if *i >= b.len() {
                    return Err("unterminated array".into());
                }
                if b[*i] == b',' {
                    *i += 1;
                } else if b[*i] == b']' {
                    *i += 1;
                    return Ok(J::Arr(v));
                } else {
                    return Err("bad array".into());
                }
            }
        }
        b'{' => {
            *i += 1;
            let mut m = BTreeMap::new();
            skip_ws(b, i);
            if *i < b.len() && b[*i] == b'}' {
                *i += 1;
                return Ok(J::Obj(m));
            }
            loop {
                skip_ws(b, i);
                let k = parse_string(b, i)?;
                skip_ws(b, i);
                if *i >= b.len() || b[*i] != b':' {
                    return Err("expected ':'".into());
                }
                *i += 1;
                let v = parse_value(b, i)?;
                m.insert(k, v);
                skip_ws(b, i);
                if *i >= b.len() {
                    return Err("unterminated object".into());
                }
                if b[*i] == b',' {
                    *i += 1;
                } else if b[*i] == b'}' {
                    *i += 1;
                    return Ok(J::Obj(m));
                } else {
                    return Err("bad object".into());
                }
            }
        }
        _ => {
            let st = *i;
            while *i < b.len() && matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                *i += 1;
            }
            let t = std::str::from_utf8(&b[st..*i]).map_err(|_| "bad number")?;
            t.parse::<f64>().map(J::Num).map_err(|_| format!("bad number {}", t))
        }
    }
}

fn parse_string(b: &[u8], i: &mut usize) -> Result<String, String> {
    if b[*i] != b'"' {
        return Err("expected string".into());
    }
    *i += 1;
    let mut out: Vec<u8> = Vec::new();
    while *i < b.len() {
        let c = b[*i];
        if c == b'"' {
            *i += 1;
            return String::from_utf8(out).map_err(|_| "bad utf8".into());
        }
        if c == b'\\' {
            *i += 1;
            let e = b[*i];
            *i += 1;
            match e {
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                b'b' => out.push(8),
                b'f' => out.push(12),
                b'/' => out.push(b'/'),
                b'\\' => out.push(b'\\'),
                b'"' => out.push(b'"'),
                b'u' => {
                    let h = std::str::from_utf8(&b[*i..*i + 4]).map_err(|_| "bad escape")?;
                    let mut cp = u32::from_str_radix(h, 16).map_err(|_| "bad escape")?;
                    *i += 4;
                    if (0xD800..0xDC00).contains(&cp) && *i + 6 <= b.len() && b[*i] == b'\\' && b[*i + 1] == b'u' {
                        let h2 = std::str::from_utf8(&b[*i + 2..*i + 6]).map_err(|_| "bad escape")?;
                        let lo = u32::from_str_radix(h2, 16).map_err(|_| "bad escape")?;
                        *i += 6;
                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                    }
                    let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => return Err("bad escape".into()),
            }
            continue;
        }
        out.push(c);
        *i += 1;
    }
    Err("unterminated string".into())
}
