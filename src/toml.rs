//! 外部クレートなしの小さな TOML の読み手（fascon.toml・board.toml・エラー辞書の上書き用）
//! a small TOML reader without external crates (for fascon.toml, board.toml and error-dictionary overrides)
//!
//! 読めるもの: `[表]` / `[a.b]` / `[[表の配列]]`、`キー = 値`（文字列 "..." と '...'、整数（10 進・0x・0o・0b・`_` 区切り）、
//! 真偽値、配列 `[..]`（複数行可）、インライン表 `{ k = v, .. }`）、`#` の注釈
//! supported: `[table]` / `[a.b]` / `[[array of tables]]`, `key = value` (strings "..." and '...', integers (decimal, 0x, 0o,
//! 0b, `_` separators), booleans, arrays `[..]` (may span lines), inline tables `{ k = v, .. }`), `#` comments
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Int(i128),
    Bool(bool),
    Array(Vec<Value>),
    Table(BTreeMap<String, Value>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i128> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_table(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
    /// `a.b.c` のような道筋で引く / look up a dotted path such as `a.b.c`
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut cur = self;
        for k in path.split('.') {
            cur = cur.as_table()?.get(k)?;
        }
        Some(cur)
    }
}

/// 読み取りの失敗（行番号は 1 から） / a parse failure (lines count from 1)
#[derive(Debug)]
pub struct Error {
    pub line: usize,
    pub msg: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
}

impl<'a> P<'a> {
    fn err<T>(&self, m: &str) -> Result<T, Error> {
        Err(Error { line: self.line, msg: m.to_string() })
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        if c == b'\n' {
            self.line += 1;
        }
        Some(c)
    }
    /// 空白と注釈（改行は nl が真のときだけ飛ばす） / skip blanks and comments (newlines only when nl)
    fn ws(&mut self, nl: bool) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\r' => {
                    self.bump();
                }
                b'\n' if nl => {
                    self.bump();
                }
                b'#' => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => break,
            }
        }
    }
    fn key(&mut self) -> Result<String, Error> {
        self.ws(false);
        match self.peek() {
            Some(b'"') | Some(b'\'') => match self.value()? {
                Value::Str(s) => Ok(s),
                _ => self.err("a key is expected"),
            },
            _ => {
                let st = self.i;
                while let Some(c) = self.peek() {
                    if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
                        self.bump();
                    } else {
                        break;
                    }
                }
                if st == self.i {
                    return self.err("a key is expected");
                }
                Ok(String::from_utf8_lossy(&self.s[st..self.i]).to_string())
            }
        }
    }
    fn dotted(&mut self) -> Result<Vec<String>, Error> {
        let mut v = vec![self.key()?];
        loop {
            self.ws(false);
            if self.peek() == Some(b'.') {
                self.bump();
                v.push(self.key()?);
            } else {
                return Ok(v);
            }
        }
    }
    fn value(&mut self) -> Result<Value, Error> {
        self.ws(false);
        match self.peek() {
            Some(b'"') => {
                self.bump();
                let mut out = String::new();
                loop {
                    match self.bump() {
                        None | Some(b'\n') => return self.err("unterminated string"),
                        Some(b'"') => break,
                        Some(b'\\') => match self.bump() {
                            Some(b'n') => out.push('\n'),
                            Some(b't') => out.push('\t'),
                            Some(b'\\') => out.push('\\'),
                            Some(b'"') => out.push('"'),
                            Some(b'u') => {
                                let h = String::from_utf8_lossy(&self.s[self.i..(self.i + 4).min(self.s.len())]).to_string();
                                self.i += 4;
                                match u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                                    Some(c) => out.push(c),
                                    None => return self.err("bad \\u escape"),
                                }
                            }
                            _ => return self.err("bad escape"),
                        },
                        Some(c) => {
                            // UTF-8 のまま足す / keep UTF-8 bytes as they are
                            let st = self.i - 1;
                            let len = if c >= 0xF0 { 4 } else if c >= 0xE0 { 3 } else if c >= 0xC0 { 2 } else { 1 };
                            self.i = st + len;
                            out.push_str(&String::from_utf8_lossy(&self.s[st..self.i.min(self.s.len())]));
                        }
                    }
                }
                Ok(Value::Str(out))
            }
            Some(b'\'') => {
                self.bump();
                let st = self.i;
                while let Some(c) = self.peek() {
                    if c == b'\'' || c == b'\n' {
                        break;
                    }
                    self.bump();
                }
                if self.peek() != Some(b'\'') {
                    return self.err("unterminated literal string");
                }
                let s = String::from_utf8_lossy(&self.s[st..self.i]).to_string();
                self.bump();
                Ok(Value::Str(s))
            }
            Some(b'[') => {
                self.bump();
                let mut v = Vec::new();
                loop {
                    self.ws(true);
                    if self.peek() == Some(b']') {
                        self.bump();
                        return Ok(Value::Array(v));
                    }
                    v.push(self.value()?);
                    self.ws(true);
                    match self.peek() {
                        Some(b',') => {
                            self.bump();
                        }
                        Some(b']') => {}
                        _ => return self.err("`,` or `]` expected in an array"),
                    }
                }
            }
            Some(b'{') => {
                self.bump();
                let mut t = BTreeMap::new();
                loop {
                    self.ws(false);
                    if self.peek() == Some(b'}') {
                        self.bump();
                        return Ok(Value::Table(t));
                    }
                    let k = self.dotted()?;
                    self.ws(false);
                    if self.bump() != Some(b'=') {
                        return self.err("`=` expected");
                    }
                    let v = self.value()?;
                    insert(&mut t, &k, v, self.line)?;
                    self.ws(false);
                    match self.peek() {
                        Some(b',') => {
                            self.bump();
                        }
                        Some(b'}') => {}
                        _ => return self.err("`,` or `}` expected in an inline table"),
                    }
                }
            }
            Some(c) if c == b't' || c == b'f' => {
                if self.s[self.i..].starts_with(b"true") {
                    self.i += 4;
                    Ok(Value::Bool(true))
                } else if self.s[self.i..].starts_with(b"false") {
                    self.i += 5;
                    Ok(Value::Bool(false))
                } else {
                    self.err("a value is expected")
                }
            }
            Some(c) if c.is_ascii_digit() || c == b'-' || c == b'+' => {
                let st = self.i;
                while let Some(c) = self.peek() {
                    if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'+' {
                        self.bump();
                    } else {
                        break;
                    }
                }
                let raw: String = String::from_utf8_lossy(&self.s[st..self.i]).chars().filter(|c| *c != '_').collect();
                let (neg, body) = match raw.strip_prefix('-') {
                    Some(b) => (true, b.to_string()),
                    None => (false, raw.trim_start_matches('+').to_string()),
                };
                let v = if let Some(h) = body.strip_prefix("0x") {
                    i128::from_str_radix(h, 16)
                } else if let Some(o) = body.strip_prefix("0o") {
                    i128::from_str_radix(o, 8)
                } else if let Some(b) = body.strip_prefix("0b") {
                    i128::from_str_radix(b, 2)
                } else {
                    body.parse::<i128>()
                };
                match v {
                    Ok(x) => Ok(Value::Int(if neg { -x } else { x })),
                    Err(_) => self.err(&format!("bad integer `{}`", raw)),
                }
            }
            _ => self.err("a value is expected"),
        }
    }
}

fn insert(t: &mut BTreeMap<String, Value>, path: &[String], v: Value, line: usize) -> Result<(), Error> {
    let (last, head) = path.split_last().unwrap();
    let mut cur = t;
    for k in head {
        let e = cur.entry(k.clone()).or_insert_with(|| Value::Table(BTreeMap::new()));
        cur = match e {
            Value::Table(x) => x,
            _ => return Err(Error { line, msg: format!("`{}` is not a table", k) }),
        };
    }
    if cur.contains_key(last) {
        return Err(Error { line, msg: format!("`{}` is defined twice", last) });
    }
    cur.insert(last.clone(), v);
    Ok(())
}

/// 道筋の表を（無ければ作って）返す。配列の表なら最後の要素 / the table at a path (created when missing); for an array of
/// tables, its last element
fn table_at<'t>(root: &'t mut BTreeMap<String, Value>, path: &[String], line: usize) -> Result<&'t mut BTreeMap<String, Value>, Error> {
    let mut cur = root;
    for k in path {
        let e = cur.entry(k.clone()).or_insert_with(|| Value::Table(BTreeMap::new()));
        cur = match e {
            Value::Table(x) => x,
            Value::Array(a) => match a.last_mut() {
                Some(Value::Table(x)) => x,
                _ => return Err(Error { line, msg: format!("`{}` is not a table", k) }),
            },
            _ => return Err(Error { line, msg: format!("`{}` is not a table", k) }),
        };
    }
    Ok(cur)
}

pub fn parse(text: &str) -> Result<Value, Error> {
    let mut p = P { s: text.as_bytes(), i: 0, line: 1 };
    let mut root: BTreeMap<String, Value> = BTreeMap::new();
    let mut cur: Vec<String> = Vec::new();
    loop {
        p.ws(true);
        let Some(c) = p.peek() else { break };
        if c == b'[' {
            p.bump();
            let arr = p.peek() == Some(b'[');
            if arr {
                p.bump();
            }
            let path = p.dotted()?;
            p.ws(false);
            if p.bump() != Some(b']') || (arr && p.bump() != Some(b']')) {
                return p.err("`]` expected after the table name");
            }
            if arr {
                let (last, head) = path.split_last().unwrap();
                let line = p.line;
                let parent = table_at(&mut root, head, line)?;
                let e = parent.entry(last.clone()).or_insert_with(|| Value::Array(Vec::new()));
                match e {
                    Value::Array(a) => a.push(Value::Table(BTreeMap::new())),
                    _ => return p.err(&format!("`{}` is not an array of tables", last)),
                }
            } else {
                let line = p.line;
                table_at(&mut root, &path, line)?;
            }
            cur = path;
            continue;
        }
        let k = p.dotted()?;
        p.ws(false);
        if p.bump() != Some(b'=') {
            return p.err("`=` expected");
        }
        let v = p.value()?;
        let line = p.line;
        let t = table_at(&mut root, &cur, line)?;
        insert(t, &k, v, line)?;
        p.ws(false);
        match p.peek() {
            None | Some(b'\n') | Some(b'#') => {}
            _ => return p.err("end of line expected after the value"),
        }
    }
    Ok(Value::Table(root))
}

/// 文字列を TOML の基本文字列にする / quote a string as a TOML basic string
pub fn quote(s: &str) -> String {
    let mut o = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic() {
        let v = parse("# c\n[board]\nname = \"virt\" # x\ncpus = 4\n[memory]\nram_base = 0x4000_0000\n[[device]]\nname='a'\nbase=1\n[[device]]\nname=\"b\"\nbase=2\n[dependencies]\nnet = { path = \"../net\" }\nlist = [1, 2,\n 3]\n").unwrap();
        assert_eq!(v.get("board.name").and_then(|x| x.as_str()), Some("virt"));
        assert_eq!(v.get("memory.ram_base").and_then(|x| x.as_int()), Some(0x4000_0000));
        assert_eq!(v.get("device").and_then(|x| x.as_array()).map(|a| a.len()), Some(2));
        assert_eq!(v.get("dependencies.net.path").and_then(|x| x.as_str()), Some("../net"));
        assert_eq!(v.get("dependencies.list").and_then(|x| x.as_array()).map(|a| a.len()), Some(3));
    }
}
