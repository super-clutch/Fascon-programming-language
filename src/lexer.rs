//! 字句解析 / lexer
//!
//! Fascon 1.0 の字句には `'` が存在しない（ライフタイム注釈も文字リテラルも無い）。
//! `'` を見つけたら F0001 を出して読み飛ばす。これが「ソースにライフタイムが 1 文字も無い」ことの
//! 字句レベルの保証になる。
//! Fascon 1.0 has no `'` token at all (neither lifetime annotations nor char literals).
//! A `'` is reported as F0001 and skipped: this is the lexical guarantee that a source contains no lifetime.

use crate::diag::Diag;
use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    Isize,
    U8,
    U16,
    U32,
    U64,
    Usize,
}

impl IntTy {
    pub fn from_name(s: &str) -> Option<IntTy> {
        Some(match s {
            "i8" => IntTy::I8,
            "i16" => IntTy::I16,
            "i32" => IntTy::I32,
            "i64" => IntTy::I64,
            "isize" => IntTy::Isize,
            "u8" => IntTy::U8,
            "u16" => IntTy::U16,
            "u32" => IntTy::U32,
            "u64" => IntTy::U64,
            "usize" => IntTy::Usize,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::Isize => "isize",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::Usize => "usize",
        }
    }
    pub fn signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::Isize)
    }
    /// ビット幅（isize/usize はターゲットのポインタ幅） / bit width (isize/usize = target pointer width)
    pub fn bits(self, ptr_bits: u32) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 => 64,
            IntTy::Isize | IntTy::Usize => ptr_bits,
        }
    }
    pub fn min_max(self, ptr_bits: u32) -> (i128, i128) {
        let b = self.bits(ptr_bits);
        if self.signed() {
            (-(1i128 << (b - 1)), (1i128 << (b - 1)) - 1)
        } else {
            (0, (1i128 << b) - 1)
        }
    }
    pub fn c_name(self) -> &'static str {
        match self {
            IntTy::I8 => "int8_t",
            IntTy::I16 => "int16_t",
            IntTy::I32 => "int32_t",
            IntTy::I64 => "int64_t",
            IntTy::Isize => "intptr_t",
            IntTy::U8 => "uint8_t",
            IntTy::U16 => "uint16_t",
            IntTy::U32 => "uint32_t",
            IntTy::U64 => "uint64_t",
            IntTy::Usize => "uintptr_t",
        }
    }
}

/// 浮動小数点の型（IEEE-754 の binary32 / binary64） / floating-point types (IEEE-754 binary32 / binary64)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F32,
    F64,
}

impl FloatTy {
    pub fn from_name(s: &str) -> Option<FloatTy> {
        match s {
            "f32" => Some(FloatTy::F32),
            "f64" => Some(FloatTy::F64),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }
    pub fn c_name(self) -> &'static str {
        match self {
            FloatTy::F32 => "float",
            FloatTy::F64 => "double",
        }
    }
    pub fn bits(self) -> u32 {
        match self {
            FloatTy::F32 => 32,
            FloatTy::F64 => 64,
        }
    }
    /// 同じ幅の符号なし整数（ビットの並びを読むとき） / the unsigned integer of the same width (to read the bits)
    pub fn bits_int(self) -> IntTy {
        match self {
            FloatTy::F32 => IntTy::U32,
            FloatTy::F64 => IntTy::U64,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    ArenaName(String),
    Int(u128, Option<IntTy>),
    /// 浮動小数点のリテラル（`_` を除いた 10 進の文字列と接尾辞）。値は C コンパイラが型ごとに正しく丸める
    /// a floating-point literal (the decimal text without `_`, and the suffix); the C compiler rounds it per type
    Float(String, Option<FloatTy>),
    Str(String),
    // 予約語 / keywords
    Fn,
    Let,
    Mut,
    Struct,
    Static,
    Const,
    If,
    Else,
    Match,
    While,
    Loop,
    For,
    In,
    Break,
    Continue,
    Return,
    True,
    False,
    As,
    ArenaKw,
    /// `bump_arena`（関数呼び出し単位で O(1) 一括解放するバンプアロケータ） / a bump allocator released in O(1) per call
    BumpArena,
    /// `port_io`（x86 の I/O ポート空間。MMIO とは別の空間） / x86 I/O port space (separate from MMIO)
    PortIo,
    Mmio,
    Own,
    Type,
    VolatileBypass,
    // 0.1 専用・未対応の予約語 / 0.1-only or unsupported reserved words
    Async,
    Await,
    Spawn,
    Reserved(&'static str),
    // 記号 / punctuation
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    ColonColon,
    Dot,
    DotDot,
    DotDotEq,
    Arrow,
    FatArrow,
    Eq,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    AmpAmp,
    Pipe,
    PipePipe,
    Caret,
    Bang,
    Shl,
    Shr,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,
    ShrEq,
    Hash,
    HashBang,
    Question,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("`{}`", s),
            Tok::ArenaName(s) => format!("`@{}`", s),
            Tok::Int(v, _) => format!("`{}`", v),
            Tok::Float(t, _) => format!("`{}`", t),
            Tok::Str(_) => "string literal".to_string(),
            Tok::Eof => "end of file".to_string(),
            other => format!("`{}`", tok_text(other)),
        }
    }
}

pub fn tok_text(t: &Tok) -> &'static str {
    match t {
        Tok::Fn => "fn",
        Tok::Let => "let",
        Tok::Mut => "mut",
        Tok::Struct => "struct",
        Tok::Static => "static",
        Tok::Const => "const",
        Tok::If => "if",
        Tok::Else => "else",
        Tok::Match => "match",
        Tok::While => "while",
        Tok::Loop => "loop",
        Tok::For => "for",
        Tok::In => "in",
        Tok::Break => "break",
        Tok::Continue => "continue",
        Tok::Return => "return",
        Tok::True => "true",
        Tok::False => "false",
        Tok::As => "as",
        Tok::ArenaKw => "arena",
        Tok::BumpArena => "bump_arena",
        Tok::PortIo => "port_io",
        Tok::Mmio => "mmio",
        Tok::Own => "own",
        Tok::Type => "type",
        Tok::VolatileBypass => "volatile_bypass",
        Tok::Async => "async",
        Tok::Await => "await",
        Tok::Spawn => "spawn",
        Tok::Reserved(s) => s,
        Tok::LBrace => "{",
        Tok::RBrace => "}",
        Tok::LParen => "(",
        Tok::RParen => ")",
        Tok::LBracket => "[",
        Tok::RBracket => "]",
        Tok::Comma => ",",
        Tok::Semi => ";",
        Tok::Colon => ":",
        Tok::ColonColon => "::",
        Tok::Dot => ".",
        Tok::DotDot => "..",
        Tok::DotDotEq => "..=",
        Tok::Arrow => "->",
        Tok::FatArrow => "=>",
        Tok::Eq => "=",
        Tok::EqEq => "==",
        Tok::Ne => "!=",
        Tok::Lt => "<",
        Tok::Le => "<=",
        Tok::Gt => ">",
        Tok::Ge => ">=",
        Tok::Plus => "+",
        Tok::Minus => "-",
        Tok::Star => "*",
        Tok::Slash => "/",
        Tok::Percent => "%",
        Tok::Amp => "&",
        Tok::AmpAmp => "&&",
        Tok::Pipe => "|",
        Tok::PipePipe => "||",
        Tok::Caret => "^",
        Tok::Bang => "!",
        Tok::Shl => "<<",
        Tok::Shr => ">>",
        Tok::PlusEq => "+=",
        Tok::MinusEq => "-=",
        Tok::StarEq => "*=",
        Tok::SlashEq => "/=",
        Tok::PercentEq => "%=",
        Tok::AmpEq => "&=",
        Tok::PipeEq => "|=",
        Tok::CaretEq => "^=",
        Tok::ShlEq => "<<=",
        Tok::ShrEq => ">>=",
        Tok::Hash => "#",
        Tok::HashBang => "#!",
        Tok::Question => "?",
        Tok::Ident(_) | Tok::ArenaName(_) | Tok::Int(..) | Tok::Float(..) | Tok::Str(_) | Tok::Eof => "",
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub struct LexResult {
    pub tokens: Vec<Token>,
    pub diags: Vec<Diag>,
    /// ソース中の `'` の数（0 であることがライフタイム 0 文字の証明） / number of `'` in the source
    pub apostrophes: usize,
    /// リフレーミング（Rust 構文）で整数へ変えた文字リテラル・取り除いたライフタイムの数（厳格形には 1 つも残らない）
    /// char literals turned into integers and lifetimes removed by reframing (Rust syntax); none survive in the strict form
    pub reframed_marks: usize,
    /// `@名前` の古い書き方が 1 つでもあったか（エンジンの自動選択に使う） / whether any legacy `@name` was written
    pub had_sigil: bool,
}

/// `'` の後ろの文字リテラル（エスケープ込み）を読む。戻り値は (文字コード, `'` を含めた長さ)。
/// read a char literal after its opening `'` (escapes included); returns (code point, length including both quotes)
fn char_literal(src: &str, start: usize) -> Option<(u32, usize)> {
    let rest = &src[start + 1..];
    let mut it = rest.char_indices();
    let (_, c) = it.next()?;
    let (val, used) = if c == '\\' {
        let (_, e) = it.next()?;
        match e {
            'n' => ('\n' as u32, 2),
            't' => ('\t' as u32, 2),
            'r' => ('\r' as u32, 2),
            '0' => (0, 2),
            '\\' => ('\\' as u32, 2),
            '\'' => ('\'' as u32, 2),
            '"' => ('"' as u32, 2),
            'x' => {
                let h = rest.get(2..4)?;
                (u32::from_str_radix(h, 16).ok()?, 4)
            }
            'u' => {
                let close = rest.find('}')?;
                let h = rest.get(3..close)?;
                (u32::from_str_radix(h, 16).ok()?, close + 1)
            }
            _ => return None,
        }
    } else if c == '\'' {
        return None;
    } else {
        (c as u32, c.len_utf8())
    };
    if rest[used..].starts_with('\'') {
        Some((val, used + 2))
    } else {
        None
    }
}

fn keyword(s: &str) -> Option<Tok> {
    Some(match s {
        "fn" => Tok::Fn,
        "let" => Tok::Let,
        "mut" => Tok::Mut,
        "struct" => Tok::Struct,
        "static" => Tok::Static,
        "const" => Tok::Const,
        "if" => Tok::If,
        "else" => Tok::Else,
        "match" => Tok::Match,
        "while" => Tok::While,
        "loop" => Tok::Loop,
        "for" => Tok::For,
        "in" => Tok::In,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        "return" => Tok::Return,
        "true" => Tok::True,
        "false" => Tok::False,
        "as" => Tok::As,
        "arena" => Tok::ArenaKw,
        "bump_arena" => Tok::BumpArena,
        "port_io" => Tok::PortIo,
        "mmio" => Tok::Mmio,
        "own" => Tok::Own,
        "type" => Tok::Type,
        "volatile_bypass" => Tok::VolatileBypass,
        "async" => Tok::Async,
        "await" => Tok::Await,
        "spawn" => Tok::Spawn,
        "impl" => Tok::Reserved("impl"),
        "enum" => Tok::Reserved("enum"),
        "trait" => Tok::Reserved("trait"),
        "where" => Tok::Reserved("where"),
        "pub" => Tok::Reserved("pub"),
        "use" => Tok::Reserved("use"),
        "mod" => Tok::Reserved("mod"),
        "unsafe" => Tok::Reserved("unsafe"),
        "dyn" => Tok::Reserved("dyn"),
        "ref" => Tok::Reserved("ref"),
        "move" => Tok::Reserved("move"),
        "self" => Tok::Reserved("self"),
        "Self" => Tok::Reserved("Self"),
        "extern" => Tok::Reserved("extern"),
        "crate" => Tok::Reserved("crate"),
        "super" => Tok::Reserved("super"),
        _ => return None,
    })
}

pub fn lex(src: &str) -> LexResult {
    lex_mode(src, false)
}

/// reframe = true: リフレーミング（Rust 構文）モード。`'A'` / `b'A'` は整数、`'a` のライフタイムは取り除く（Fascon の字句には
/// `'` が無いので、変換後のトークン列にも `'` は残らない）
/// reframe = true: reframing (Rust syntax) mode. `'A'` / `b'A'` become integers and `'a` lifetimes are dropped (Fascon has no
/// `'` token, so none remains in the converted token stream either)
pub fn lex_mode(src: &str, reframe: bool) -> LexResult {
    let mut reframed_marks = 0usize;
    let mut had_sigil = false;
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut tokens = Vec::new();
    let mut diags = Vec::new();
    let mut apostrophes = 0usize;
    let n = bytes.len();
    // 先頭の BOM を読み飛ばす / skip a leading BOM
    if src.starts_with('\u{feff}') {
        i = 3;
    }
    while i < n {
        let c = bytes[i];
        // 空白 / whitespace
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            i += 1;
            continue;
        }
        // コメント / comments
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            let start = i;
            let mut depth = 0usize;
            while i < n {
                if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            if depth != 0 {
                diags.push(Diag::error(
                    "F0001",
                    Span::new(start as u32, (start + 2) as u32),
                    "ブロックコメントが閉じていません（`*/` が必要）",
                    "unterminated block comment (`*/` expected)",
                ));
            }
            continue;
        }
        let start = i;
        // 識別子・予約語 / identifiers and keywords
        if reframe && c == b'b' && i + 1 < n && bytes[i + 1] == b'\'' {
            // b'A' → 65u8
            if let Some((v, len)) = char_literal(src, i + 1) {
                reframed_marks += 1;
                i += 1 + len;
                tokens.push(Token { tok: Tok::Int(v as u128, Some(IntTy::U8)), span: Span::new(start as u32, i as u32) });
                continue;
            }
        }
        if reframe && c == b'b' && i + 1 < n && bytes[i + 1] == b'"' {
            // b"..." → 文字列 / a string
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            while i < n && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let s = &src[start..i];
            let tok = keyword(s).unwrap_or_else(|| Tok::Ident(s.to_string()));
            tokens.push(Token { tok, span: Span::new(start as u32, i as u32) });
            continue;
        }
        // @アリーナ名 / @arena-name
        if c == b'@' {
            i += 1;
            let ns = i;
            while i < n && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            if ns == i {
                diags.push(Diag::error(
                    "F0001",
                    Span::new(start as u32, i as u32),
                    "`@` の後にアリーナ名（例: `@process`）が必要です",
                    "an arena name (e.g. `@process`) is expected after `@`",
                ));
                continue;
            }
            had_sigil = true;
            tokens.push(Token { tok: Tok::ArenaName(src[ns..i].to_string()), span: Span::new(start as u32, i as u32) });
            continue;
        }
        // 数値 / numbers
        if c.is_ascii_digit() {
            let (radix, mut j) = if c == b'0' && i + 1 < n && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X') {
                (16u32, i + 2)
            } else if c == b'0' && i + 1 < n && (bytes[i + 1] == b'b' || bytes[i + 1] == b'B') {
                (2, i + 2)
            } else if c == b'0' && i + 1 < n && (bytes[i + 1] == b'o' || bytes[i + 1] == b'O') {
                (8, i + 2)
            } else {
                (10, i)
            };
            let mut val: u128 = 0;
            let mut overflow = false;
            let mut digits = 0;
            while j < n {
                let d = bytes[j];
                if d == b'_' {
                    j += 1;
                    continue;
                }
                let dv = match d {
                    b'0'..=b'9' => (d - b'0') as u32,
                    b'a'..=b'f' => (d - b'a' + 10) as u32,
                    b'A'..=b'F' => (d - b'A' + 10) as u32,
                    _ => break,
                };
                if dv >= radix {
                    // 10 進の後ろの英字は接尾辞の可能性 / letters after a decimal may be a suffix
                    break;
                }
                match val.checked_mul(radix as u128).and_then(|v| v.checked_add(dv as u128)) {
                    Some(v) => val = v,
                    None => overflow = true,
                }
                digits += 1;
                j += 1;
            }
            // 接尾辞 / suffix
            let mut suffix = None;
            if j < n && (bytes[j] == b'i' || bytes[j] == b'u') {
                let ss = j;
                let mut k = j + 1;
                while k < n && bytes[k].is_ascii_alphanumeric() {
                    k += 1;
                }
                if let Some(t) = IntTy::from_name(&src[ss..k]) {
                    suffix = Some(t);
                    j = k;
                }
            }
            // 浮動小数点のリテラル（10 進だけ）: `1.5`・`2e-3`・`1.0e10`・`3f32`・`0.25_f64`。小数点の後ろには数字が要る
            // （`0..n` の範囲と区別する）
            // floating-point literals (decimal only): `1.5`, `2e-3`, `1.0e10`, `3f32`, `0.25_f64`; a digit must follow the
            // point (to tell it from the range `0..n`)
            if radix == 10 && suffix.is_none() && digits > 0 {
                let mut k = j;
                let mut is_float = false;
                if k + 1 < n && bytes[k] == b'.' && bytes[k + 1].is_ascii_digit() {
                    is_float = true;
                    k += 1;
                    while k < n && (bytes[k].is_ascii_digit() || bytes[k] == b'_') {
                        k += 1;
                    }
                }
                if k < n && (bytes[k] == b'e' || bytes[k] == b'E') {
                    let mut m = k + 1;
                    if m < n && (bytes[m] == b'+' || bytes[m] == b'-') {
                        m += 1;
                    }
                    let ds = m;
                    while m < n && (bytes[m].is_ascii_digit() || bytes[m] == b'_') {
                        m += 1;
                    }
                    if src[ds..m].bytes().any(|b| b.is_ascii_digit()) {
                        is_float = true;
                        k = m;
                    }
                }
                let text_end = k;
                let mut fsuf = None;
                if k < n && bytes[k] == b'_' && k + 1 < n && bytes[k + 1] == b'f' {
                    k += 1;
                }
                if k < n && bytes[k] == b'f' {
                    let mut m = k + 1;
                    while m < n && bytes[m].is_ascii_alphanumeric() {
                        m += 1;
                    }
                    if let Some(t) = FloatTy::from_name(&src[k..m]) {
                        fsuf = Some(t);
                        is_float = true;
                        k = m;
                    }
                }
                if is_float {
                    let text: String = src[start..text_end].chars().filter(|c| *c != '_').collect();
                    if text.parse::<f64>().is_err() {
                        diags.push(Diag::error("F0001", Span::new(start as u32, k as u32), "浮動小数点のリテラルが不正です", "malformed floating-point literal"));
                    }
                    i = k;
                    tokens.push(Token { tok: Tok::Float(text, fsuf), span: Span::new(start as u32, i as u32) });
                    continue;
                }
            }
            // 16 進・8 進・2 進の小数は書けない / no hexadecimal, octal or binary fractions
            if j + 1 < n && bytes[j] == b'.' && bytes[j + 1].is_ascii_digit() {
                let mut k = j + 1;
                while k < n && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                diags.push(Diag::error(
                    "F0001",
                    Span::new(start as u32, k as u32),
                    "浮動小数点のリテラルは 10 進で書きます（`0x` などの接頭辞や整数の接尾辞の後ろに小数点は置けません）",
                    "floating-point literals are decimal (no point after a `0x`-style prefix or an integer suffix)",
                ));
                j = k;
            }
            if digits == 0 || overflow || val > u64::MAX as u128 {
                diags.push(Diag::error(
                    "F0001",
                    Span::new(start as u32, j as u32),
                    "整数リテラルが不正、または 64 ビットに収まりません",
                    "malformed integer literal, or it does not fit in 64 bits",
                ));
            }
            i = j;
            tokens.push(Token { tok: Tok::Int(val, suffix), span: Span::new(start as u32, i as u32) });
            continue;
        }
        // 文字列 / strings
        if c == b'"' {
            i += 1;
            let mut s = String::new();
            let mut closed = false;
            while i < n {
                let ch = bytes[i];
                if ch == b'"' {
                    i += 1;
                    closed = true;
                    break;
                }
                if ch == b'\\' && i + 1 < n {
                    let e = bytes[i + 1];
                    let esc_start = i;
                    i += 2;
                    // 未対応のエスケープの後ろが多バイト文字なら、その文字の終わりまで進める（文字の途中で切らない）
                    // after an unsupported escape followed by a multi-byte character, move to that character's end
                    if e >= 0x80 {
                        while i < n && (bytes[i] & 0xC0) == 0x80 {
                            i += 1;
                        }
                    }
                    match e {
                        b'n' => s.push('\n'),
                        b't' => s.push('\t'),
                        b'r' => s.push('\r'),
                        b'0' => s.push('\0'),
                        b'\\' => s.push('\\'),
                        b'"' => s.push('"'),
                        b'x' if i + 1 < n => {
                            let h = src.get(i..(i + 2).min(n)).unwrap_or("");
                            match u8::from_str_radix(h, 16) {
                                Ok(v) if v < 0x80 => s.push(v as char),
                                _ => diags.push(Diag::error(
                                    "F0001",
                                    Span::new((i - 2) as u32, (i + 2) as u32),
                                    "`\\x` の後には 00〜7F の 16 進 2 桁が必要です",
                                    "`\\x` must be followed by two hex digits 00-7F",
                                )),
                            }
                            i += 2;
                            while i < n && (bytes[i] & 0xC0) == 0x80 {
                                i += 1;
                            }
                        }
                        _ => diags.push(Diag::error(
                            "F0001",
                            Span::new(esc_start as u32, i as u32),
                            "未対応のエスケープです（\\n \\t \\r \\0 \\\\ \\\" \\xNN）",
                            "unsupported escape (\\n \\t \\r \\0 \\\\ \\\" \\xNN)",
                        )),
                    }
                    continue;
                }
                // UTF-8 の 1 文字を丸ごと追加 / push one whole UTF-8 character
                let rest = &src[i..];
                let ch = rest.chars().next().unwrap();
                s.push(ch);
                i += ch.len_utf8();
            }
            if !closed {
                diags.push(Diag::error(
                    "F0001",
                    Span::new(start as u32, (start + 1) as u32),
                    "文字列が閉じていません（`\"` が必要）",
                    "unterminated string literal (`\"` expected)",
                ));
            }
            tokens.push(Token { tok: Tok::Str(s), span: Span::new(start as u32, i as u32) });
            continue;
        }
        // `'`: ライフタイム注釈・文字リテラルは存在しない / lifetimes and char literals do not exist
        if c == b'\'' && reframe {
            // リフレーミング: 文字リテラルは整数に、ライフタイムは取り除く / reframing: char literal → integer, lifetime → dropped
            if let Some((v, len)) = char_literal(src, i) {
                reframed_marks += 1;
                i += len;
                tokens.push(Token { tok: Tok::Int(v as u128, None), span: Span::new(start as u32, i as u32) });
                continue;
            }
            reframed_marks += 1;
            i += 1;
            while i < n && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            // `'a ` の後ろの空白は自然に読み飛ばされる / the whitespace after `'a ` is skipped as usual
            continue;
        }
        if c == b'\'' {
            apostrophes += 1;
            i += 1;
            while i < n && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let is_char_lit = i < n && bytes[i] == b'\'';
            if is_char_lit {
                apostrophes += 1;
                i += 1;
            }
            let sp = Span::new(start as u32, i as u32);
            let text = &src[start..i];
            if is_char_lit {
                diags.push(
                    Diag::error(
                        "F0001",
                        sp,
                        format!("文字リテラル `{}` は Fascon にはありません（字句に `'` が存在しない）", text),
                        format!("char literal `{}` does not exist in Fascon (there is no `'` token)", text),
                    )
                    .hint("数値（例: 65）か文字列 \"A\" を使ってください", "use a number (e.g. 65) or a string \"A\""),
                );
            } else {
                diags.push(
                    Diag::error(
                        "F0001",
                        sp,
                        format!("ライフタイム注釈 `{}` は書けません。Fascon は寿命を自動で推論します（参照は関数の引数と一時的なルックアップだけの第二級参照）", text),
                        format!("lifetime annotation `{}` is not allowed. Fascon infers lifetimes automatically (references are second-class: only arguments and temporary lookups)", text),
                    )
                    .hint("注釈を削除してください（Quick Fix）", "delete the annotation (Quick Fix)"),
                );
            }
            continue;
        }
        // 記号 / punctuation
        // 次が全角などの多バイト文字でも切らない（文字の境界でなければ空として扱う） / never split a multi-byte character
        // (a slice that does not end on a character boundary counts as empty)
        let two = src.get(i..i + 2).unwrap_or("");
        let three = src.get(i..i + 3).unwrap_or("");
        let (tok, len) = match three {
            "..=" => (Tok::DotDotEq, 3),
            "<<=" => (Tok::ShlEq, 3),
            ">>=" => (Tok::ShrEq, 3),
            _ => match two {
                "::" => (Tok::ColonColon, 2),
                ".." => (Tok::DotDot, 2),
                "->" => (Tok::Arrow, 2),
                "=>" => (Tok::FatArrow, 2),
                "==" => (Tok::EqEq, 2),
                "!=" => (Tok::Ne, 2),
                "<=" => (Tok::Le, 2),
                ">=" => (Tok::Ge, 2),
                "&&" => (Tok::AmpAmp, 2),
                "||" => (Tok::PipePipe, 2),
                "<<" => (Tok::Shl, 2),
                ">>" => (Tok::Shr, 2),
                "+=" => (Tok::PlusEq, 2),
                "-=" => (Tok::MinusEq, 2),
                "*=" => (Tok::StarEq, 2),
                "/=" => (Tok::SlashEq, 2),
                "%=" => (Tok::PercentEq, 2),
                "&=" => (Tok::AmpEq, 2),
                "|=" => (Tok::PipeEq, 2),
                "^=" => (Tok::CaretEq, 2),
                "#!" => (Tok::HashBang, 2),
                _ => {
                    let t = match c {
                        b'{' => Tok::LBrace,
                        b'}' => Tok::RBrace,
                        b'(' => Tok::LParen,
                        b')' => Tok::RParen,
                        b'[' => Tok::LBracket,
                        b']' => Tok::RBracket,
                        b',' => Tok::Comma,
                        b';' => Tok::Semi,
                        b':' => Tok::Colon,
                        b'.' => Tok::Dot,
                        b'=' => Tok::Eq,
                        b'<' => Tok::Lt,
                        b'>' => Tok::Gt,
                        b'+' => Tok::Plus,
                        b'-' => Tok::Minus,
                        b'*' => Tok::Star,
                        b'/' => Tok::Slash,
                        b'%' => Tok::Percent,
                        b'&' => Tok::Amp,
                        b'|' => Tok::Pipe,
                        b'^' => Tok::Caret,
                        b'!' => Tok::Bang,
                        b'#' => Tok::Hash,
                        b'?' => Tok::Question,
                        _ => {
                            let ch = src[i..].chars().next().unwrap();
                            diags.push(Diag::error(
                                "F0001",
                                Span::new(i as u32, (i + ch.len_utf8()) as u32),
                                format!("使えない文字 `{}` です", ch),
                                format!("unexpected character `{}`", ch),
                            ));
                            i += ch.len_utf8();
                            continue;
                        }
                    };
                    (t, 1)
                }
            },
        };
        i += len;
        tokens.push(Token { tok, span: Span::new(start as u32, i as u32) });
    }
    tokens.push(Token { tok: Tok::Eof, span: Span::new(n as u32, n as u32) });
    LexResult { tokens, diags, apostrophes, reframed_marks, had_sigil }
}

/// 1.0 構文の目印があるか（エンジンの自動選択に使う）
/// Does the source contain a 1.0 marker? (used for automatic engine selection)
pub fn has_v2_markers(toks: &[Token]) -> bool {
    for (k, t) in toks.iter().enumerate() {
        let next = toks.get(k + 1).map(|t| &t.tok);
        match &t.tok {
            Tok::ArenaName(_) => return true,
            Tok::BumpArena | Tok::PortIo => return true,
            Tok::ArenaKw | Tok::Mmio if matches!(next, Some(Tok::ArenaName(_))) => return true,
            // `@` の要らない宣言 `arena 名前: T[N];` / `mmio 名前: ..` / declarations without `@`
            Tok::ArenaKw | Tok::Mmio if matches!(next, Some(Tok::Ident(_))) && matches!(toks.get(k + 2).map(|t| &t.tok), Some(Tok::Colon)) => return true,
            Tok::Ident(s) if s == "own_alloc" && matches!(next, Some(Tok::LParen) | Some(Tok::Bang)) => return true,
            Tok::Own if matches!(next, Some(Tok::Ident(s)) if s.chars().next().map_or(false, |c| c.is_ascii_uppercase())) => return true,
            Tok::Own if matches!(next, Some(Tok::LBracket) | Some(Tok::LParen)) => return true,
            // `own i32` / `own Str` / `own bool` / `own Option<..>` / `own Id<..>`: own の中身は構造体でなくてもよい
            // the contents of an own need not be a struct
            Tok::Own if matches!(next, Some(Tok::Ident(s)) if IntTy::from_name(s).is_some() || matches!(s.as_str(), "bool" | "Str" | "Option" | "Id")) => return true,
            // 関数ポインタ型 `fn(&mut i32)`（`fn` の直後が名前ではなく `(`） / a function pointer type (`(` right after `fn`)
            Tok::Fn if matches!(next, Some(Tok::LParen)) => return true,
            Tok::Type if matches!(next, Some(Tok::Ident(_))) => return true,
            // カーネル拡張の項目（文脈依存キーワード） / kernel-extension items (contextual keywords)
            Tok::Ident(s) if (s == "task_arena" || s == "dma_arena" || s == "gc_arena" || s == "gpu_arena") && matches!(next, Some(Tok::Ident(_)) | Some(Tok::ArenaName(_))) => return true,
            Tok::Ident(s) if s == "dma" && matches!(next, Some(Tok::Ident(_)) | Some(Tok::ArenaName(_))) && matches!(toks.get(k + 2).map(|t| &t.tok), Some(Tok::Colon)) => return true,
            Tok::Ident(s) if s == "chan" && matches!(next, Some(Tok::Ident(_)) | Some(Tok::ArenaName(_))) && matches!(toks.get(k + 2).map(|t| &t.tok), Some(Tok::Colon)) => return true,
            Tok::Ident(s) if s == "user_mem" && matches!(next, Some(Tok::Ident(_)) | Some(Tok::ArenaName(_))) && matches!(toks.get(k + 2).map(|t| &t.tok), Some(Tok::For)) => return true,
            Tok::Ident(s) if s == "syscall" && matches!(next, Some(Tok::Ident(_))) && matches!(toks.get(k + 2).map(|t| &t.tok), Some(Tok::Ident(a)) if a == "at") => return true,
            Tok::Ident(s) if s == "irq" && matches!(next, Some(Tok::Ident(_))) && matches!(toks.get(k + 2).map(|t| &t.tok), Some(Tok::Ident(a)) if a == "at") => return true,
            Tok::Ident(s) if s == "critical" && matches!(next, Some(Tok::Fn)) => return true,
            Tok::HashBang => {
                // #![fascon(1)] / #![reframing(rust)]
                if let (Some(Tok::LBracket), Some(Tok::Ident(s))) = (next, toks.get(k + 2).map(|t| &t.tok)) {
                    if s == "fascon" || s == "reframing" {
                        return true;
                    }
                }
            }
            Tok::LBracket => {
                // 配列型 `[T; N]` / array type `[T; N]`
                if let Some(Tok::Ident(_)) = next {
                    if let Some(Tok::Semi) = toks.get(k + 2).map(|t| &t.tok) {
                        return true;
                    }
                }
            }
            Tok::If if matches!(next, Some(Tok::Let)) => return true,
            Tok::Ident(s) if (s == "Id" || s == "Str") && matches!(next, Some(Tok::Lt) | Some(Tok::Comma) | Some(Tok::RParen) | Some(Tok::Gt) | Some(Tok::LBrace) | Some(Tok::Semi) | Some(Tok::Eq)) => {
                // 型位置の Id<..> / Str（直前が `:` か `->` か `<`） / Id<..> / Str in a type position
                if k > 0 && matches!(toks[k - 1].tok, Tok::Colon | Tok::Arrow | Tok::Lt | Tok::Own) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// 0.1 専用構文（1.0 エンジンは未対応）を使っているか
/// Does the source use 0.1-only syntax (not supported by the 1.0 engine)?
#[allow(dead_code)]
pub fn has_v1_only(toks: &[Token]) -> bool {
    for (k, t) in toks.iter().enumerate() {
        match &t.tok {
            Tok::Spawn | Tok::Async | Tok::Await => return true,
            Tok::Ident(s) if (s == "chan" || s == "block_on") && matches!(toks.get(k + 1).map(|t| &t.tok), Some(Tok::ColonColon) | Some(Tok::LParen)) => return true,
            Tok::Ident(s) if s == "isr" && k > 0 && matches!(toks[k - 1].tok, Tok::LBracket) => return true,
            _ => {}
        }
    }
    false
}
