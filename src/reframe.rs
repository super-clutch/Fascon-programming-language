//! 自動リフレーミングモード（`#![reframing(rust)]` または `--reframing=rust`）
//! automatic reframing mode (`#![reframing(rust)]` or `--reframing=rust`)
//!
//! ふつうの Rust の見た目（static・マクロ・unsafe・pub・use・文字リテラル・ライフタイム）で書いたコードを、構文解析の前に
//! トークン列のまま Fascon の本来の形（arena / bump_arena / mmio / port_io / own_alloc）へ書き換える。意味はすべて Fascon の
//! 規則で検査されるので、安全性の保証は変わらない（`unsafe` は取り除かれ、MMIO も境界検査付きになる）。
//! Code written in ordinary Rust style (statics, macros, unsafe, pub, use, char literals, lifetimes) is rewritten, as a
//! token stream and before parsing, into Fascon's own forms (arena / bump_arena / mmio / port_io / own_alloc). All meaning
//! is then checked by Fascon's rules, so the guarantees do not change (`unsafe` is dropped and MMIO stays bounds-checked).
//!
//! | Rust の見た目 / Rust look                                  | Fascon の形 / Fascon form                         |
//! |------------------------------------------------------------|---------------------------------------------------|
//! | `#[link_section = ".mmio.0x0900_0000"] static mut U: [u32; 7] = ..;` | `mmio U: [u32; 7] at 0x0900_0000;`       |
//! | `#[link_section = ".port.0x3f8"] static mut COM1: u8 = 0;`  | `port_io COM1: u8 at_port 0x3f8;`                 |
//! | `#[link_section = ".uart"] static mut COM1: [u8; 8] = ..;`  | `port_io COM1: [u8; 8] at_port 0x3f8;`（COM1）    |
//! | `#[link_section = ".fb.0x4800_0000"] static mut FB: [[u32; 800]; 600] = ..;` | `#[framebuffer] mmio FB: .. at ..;` |
//! | `arena!(pcb: Pcb[64]);` / `bump_arena!(tmp: Node[256]);`   | `arena pcb: Pcb[64];` / `bump_arena tmp: Node[256];` |
//! | `port_io!(com1, 0x3f8);` / `port_io!(com1: u8, 0x3f8);`    | `port_io com1 at_port 0x3f8;`                     |
//! | `mmio!(uart: [u32; 18], 0x0900_0000);`                     | `mmio uart: [u32; 18] at 0x0900_0000;`            |
//! | `let data = own_alloc!(Block);`                            | `let data = own_alloc(Block);`                    |
//! | `println!("..")` / `print!("..")` / `panic!("..")`         | `print_str("..")` / `put_str("..")` / `panic("..")` |
//! | `println!("pid {} = {:#x}", a, b)` / `{name}`              | `{ put_str("pid "); put(a); put_str(" = "); put_hex(b); print_str(""); }` |
//! | `unsafe { .. }` / `pub` / `use ..;` / `#[no_mangle]` / `extern "C"` / `#![no_std]` | 取り除く / removed                |
//! | `'A'` / `b'A'` / `&'a T` / `&'static str`                  | `65` / `65u8` / `&T` / `Str`                      |
//! | `-> !` / `const fn` / `struct S;` / `char`                 | 戻り値なし / `fn` / `struct S {}` / `u32`          |
//! | `#[panic_handler] fn ..` / `while let P = e { .. }`        | 取り除く / `loop { if let P = e { .. } else { break; } }` |

use crate::diag::Diag;
use crate::lexer::{IntTy, Tok, Token};
use crate::span::Span;

/// ファイルの先頭の内部属性から、リフレーミングの指定を読む（`#![reframing(rust)]` → Some(true)、`(off)` → Some(false)）
/// read the reframing switch from the inner attributes at the top of the file (`#![reframing(rust)]` → Some(true), `(off)` → Some(false))
pub fn detect(text: &str) -> Option<bool> {
    let mut rest = text.trim_start_matches('\u{feff}');
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("//") {
            rest = r.split_once('\n').map(|x| x.1).unwrap_or("");
            continue;
        }
        if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map(|x| x.1).unwrap_or("");
            continue;
        }
        if let Some(r) = rest.strip_prefix("#!") {
            let r = r.trim_start();
            let Some(r) = r.strip_prefix('[') else { return None };
            let (inside, after) = r.split_once(']')?;
            let inside: String = inside.chars().filter(|c| !c.is_whitespace()).collect();
            if let Some(v) = inside.strip_prefix("reframing(").and_then(|x| x.strip_suffix(')')) {
                return Some(!matches!(v, "off" | "none" | "fascon" | "false" | "0"));
            }
            rest = after;
            continue;
        }
        return None;
    }
}

/// リフレーミングで書き換えたことの記録（厳格形の見出しと `fascon regions` に出す） / a record of what reframing rewrote
#[derive(Default, Clone, Debug)]
pub struct Report {
    pub lines: Vec<(Span, String, String)>,
}

impl Report {
    fn note(&mut self, sp: Span, ja: impl Into<String>, en: impl Into<String>) {
        self.lines.push((sp, ja.into(), en.into()));
    }
}

struct W<'a> {
    t: &'a [Token],
    i: usize,
    out: Vec<Token>,
    diags: Vec<Diag>,
    rep: Report,
    /// 生成する名前の通し番号 / serial number for generated names
    fresh: u32,
    /// この位置の `}` の後ろにもう 1 つ `}` を足す（`for b in s.bytes()` の展開） / add one more `}` after the `}` at
    /// these positions (the expansion of `for b in s.bytes()`)
    extra_close: Vec<usize>,
    /// この位置のトークンを置き換える（`v[i]` → `v.byte(i)` の `]` → `)`） / replace the token at these positions
    replace_at: Vec<(usize, Tok)>,
    /// バイト列として使う名前（`&[u8]` の引数、`let v = s.as_bytes();`） / names used as byte views
    byte_views: Vec<String>,
}

fn tk(tok: Tok, span: Span) -> Token {
    Token { tok, span }
}

impl<'a> W<'a> {
    fn peek(&self, k: usize) -> &Tok {
        self.t.get(self.i + k).map(|x| &x.tok).unwrap_or(&Tok::Eof)
    }
    fn span(&self, k: usize) -> Span {
        self.t.get(self.i + k).map(|x| x.span).unwrap_or_else(|| self.t.last().map(|x| x.span).unwrap_or(Span::new(0, 0)))
    }
    fn push(&mut self, tok: Tok, span: Span) {
        self.out.push(tk(tok, span));
    }
    fn copy(&mut self) {
        let t = self.t[self.i].clone();
        self.out.push(t);
        self.i += 1;
    }
    fn err(&mut self, sp: Span, ja: impl Into<String>, en: impl Into<String>) {
        self.diags.push(Diag::error("F0001", sp, ja, en));
    }
    /// `(`..`)` / `[`..`]` / `{`..`}` の対応する閉じ括弧の位置（i はその開き括弧） / the matching close of the bracket at i
    fn matching(&self, i: usize) -> usize {
        let mut depth = 0i32;
        let mut k = i;
        while k < self.t.len() {
            match self.t[k].tok {
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return k;
                    }
                }
                // 閉じていない: Eof の手前を「閉じ」とみなす（Eof は必ず本体のループが写す）
                // unclosed: treat the token before Eof as the close (the main loop always copies Eof)
                Tok::Eof => return k.saturating_sub(1).max(i),
                _ => {}
            }
            k += 1;
        }
        self.t.len().saturating_sub(2)
    }
    /// 深さ 0 の `;` まで飛ばす（その `;` も） / skip through the next depth-0 `;`
    fn skip_through_semi(&mut self) {
        let mut depth = 0i32;
        while self.i < self.t.len() {
            match self.t[self.i].tok {
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => depth -= 1,
                Tok::Semi if depth <= 0 => {
                    self.i += 1;
                    return;
                }
                Tok::Eof => return,
                _ => {}
            }
            self.i += 1;
        }
    }
    /// 属性 `#[..]` を読み、その名前と（あれば）`= "文字列"` を返す。i は `#` の位置
    /// read an attribute `#[..]`, returning its name and its `= "string"` value if any; i is at the `#`
    fn read_attr(&mut self) -> (String, Option<String>, Span, usize, usize) {
        let start = self.i;
        let close = self.matching(self.i + 1);
        let name = match self.t.get(self.i + 2).map(|x| &x.tok) {
            Some(Tok::Ident(s)) => s.clone(),
            Some(Tok::Reserved(s)) => s.to_string(),
            _ => String::new(),
        };
        let mut val = None;
        if let (Some(Tok::Eq), Some(Tok::Str(v))) = (self.t.get(self.i + 3).map(|x| &x.tok), self.t.get(self.i + 4).map(|x| &x.tok)) {
            val = Some(v.clone());
        }
        let sp = self.t[start].span.to(self.t[close.min(self.t.len() - 1)].span);
        (name, val, sp, start, close)
    }

    fn run(&mut self) {
        let mut pending_section: Option<(String, Span)> = None;
        let mut drop_next_fn = false;
        while self.i < self.t.len() {
            let here = self.span(0);
            if let Some(k) = self.replace_at.iter().position(|(p, _)| *p == self.i) {
                let (_, tok) = self.replace_at.remove(k);
                if !matches!(self.peek(0), Tok::Eof) {
                    self.push(tok, here);
                    self.i += 1;
                    continue;
                }
            }
            if matches!(self.peek(0), Tok::RBrace) {
                if let Some(k) = self.extra_close.iter().position(|p| *p == self.i) {
                    self.extra_close.remove(k);
                    self.copy();
                    self.push(Tok::RBrace, here);
                    continue;
                }
            }
            match self.peek(0).clone() {
                Tok::Eof => {
                    self.copy();
                    break;
                }
                // 内部属性: #![reframing(..)] / #![fascon(..)] だけ残す / inner attributes: keep #![reframing] / #![fascon]
                Tok::HashBang => {
                    let close = self.matching(self.i + 1);
                    let keep = matches!(self.t.get(self.i + 2).map(|x| &x.tok), Some(Tok::Ident(s)) if s == "reframing" || s == "fascon");
                    if keep {
                        while self.i <= close {
                            self.copy();
                        }
                    } else {
                        self.rep.note(here, "内部属性を取り除いた（#![no_std] など）", "removed an inner attribute (#![no_std] ...)");
                        self.i = close + 1;
                    }
                }
                Tok::Hash if matches!(self.peek(1), Tok::LBracket) => {
                    let (name, val, sp, start, close) = self.read_attr();
                    match name.as_str() {
                        "framebuffer" => {
                            while self.i <= close {
                                self.copy();
                            }
                        }
                        "repr" => {
                            // #[repr(align(N))] は残し、#[repr(C)] などは取り除く / keep #[repr(align(N))], drop #[repr(C)] ...
                            let has_align = self.t[start..=close].iter().any(|x| matches!(&x.tok, Tok::Ident(s) if s == "align"));
                            if has_align {
                                self.push(Tok::Hash, sp);
                                self.push(Tok::LBracket, sp);
                                self.push(Tok::Ident("repr".into()), sp);
                                self.push(Tok::LParen, sp);
                                self.push(Tok::Ident("align".into()), sp);
                                self.push(Tok::LParen, sp);
                                let n = self.t[start..=close]
                                    .windows(3)
                                    .find_map(|w| match (&w[0].tok, &w[1].tok, &w[2].tok) {
                                        (Tok::Ident(a), Tok::LParen, Tok::Int(v, _)) if a == "align" => Some(*v),
                                        _ => None,
                                    })
                                    .unwrap_or(16);
                                self.push(Tok::Int(n, None), sp);
                                self.push(Tok::RParen, sp);
                                self.push(Tok::RParen, sp);
                                self.push(Tok::RBracket, sp);
                            }
                            self.i = close + 1;
                        }
                        "link_section" => {
                            match val {
                                Some(v) => pending_section = Some((v, sp)),
                                None => self.err(sp, "`#[link_section = \"...\"]` の形で書いてください", "write it as `#[link_section = \"...\"]`"),
                            }
                            self.i = close + 1;
                        }
                        "panic_handler" => {
                            // Fascon は自前のパニック処理を持つので、この関数ごと取り除く / Fascon has its own panic path: drop the fn
                            drop_next_fn = true;
                            self.rep.note(sp, "#[panic_handler] の関数を取り除いた（Fascon のパニックは BSP が処理する）", "removed the #[panic_handler] function (Fascon's panics are handled by the BSP)");
                            self.i = close + 1;
                        }
                        "mmio_map" | "isr" => {
                            while self.i <= close {
                                self.copy();
                            }
                        }
                        _ => {
                            // #[no_mangle] / #[inline] / #[derive(..)] / #[allow(..)] ... は意味を持たないので取り除く
                            self.i = close + 1;
                        }
                    }
                }
                Tok::Reserved("pub") => {
                    self.i += 1;
                    // pub(crate) / pub(super)
                    if matches!(self.peek(0), Tok::LParen) {
                        self.i = self.matching(self.i) + 1;
                    }
                }
                Tok::Reserved("use") | Tok::Reserved("mod") if self.depth_zero() => {
                    self.rep.note(here, "`use` / `mod` 宣言を取り除いた（1 ファイルで完結する）", "removed a `use` / `mod` declaration (one file is the whole program)");
                    self.skip_through_semi();
                }
                Tok::Reserved("extern") => {
                    // extern crate x; / extern "C" fn
                    if matches!(self.peek(1), Tok::Reserved("crate")) {
                        self.skip_through_semi();
                    } else {
                        self.i += 1;
                        if matches!(self.peek(0), Tok::Str(_)) {
                            self.i += 1;
                        }
                    }
                }
                Tok::Reserved("unsafe") => {
                    // unsafe { .. } → { .. }、unsafe fn → fn（Fascon の検査がそのまま掛かる） / Fascon's checks still apply
                    self.rep.note(here, "`unsafe` を取り除いた（中身は Fascon の規則で検査される）", "removed `unsafe` (the contents are checked by Fascon's rules)");
                    self.i += 1;
                }
                Tok::Const if matches!(self.peek(1), Tok::Fn) => {
                    self.i += 1;
                }
                Tok::Fn if drop_next_fn => {
                    // 本体の `{..}` の終わりまで取り除く / drop through the end of the body `{..}`
                    drop_next_fn = false;
                    let mut k = self.i;
                    while k < self.t.len() && !matches!(self.t[k].tok, Tok::LBrace) {
                        k += 1;
                    }
                    self.i = self.matching(k) + 1;
                }
                Tok::Static if pending_section.is_some() || matches!(self.peek(1), Tok::Mut) => {
                    let sec = pending_section.take();
                    self.static_item(sec, here);
                }
                Tok::Struct if matches!(self.peek(2), Tok::Semi) => {
                    // struct S; → struct S {}
                    self.copy();
                    self.copy();
                    let sp = self.span(0);
                    self.push(Tok::LBrace, sp);
                    self.push(Tok::RBrace, sp);
                    self.i += 1;
                }
                Tok::Arrow if matches!(self.peek(1), Tok::Bang) => {
                    // -> ! は戻らない関数: Fascon では戻り値なしの関数（loop {} の型は「戻らない」） / a diverging fn
                    self.i += 2;
                }
                Tok::Amp if matches!(self.peek(1), Tok::Ident(ref s) if s == "str") => {
                    // &str / &'static str → Str
                    self.push(Tok::Ident("Str".into()), here.to(self.span(1)));
                    self.i += 2;
                }
                Tok::Ident(ref s) if s == "char" && !matches!(self.peek(1), Tok::LParen) => {
                    self.push(Tok::Ident("u32".into()), here);
                    self.i += 1;
                }
                Tok::Ident(ref s) if matches!(self.peek(1), Tok::Bang) && matches!(self.peek(2), Tok::LParen | Tok::LBracket | Tok::LBrace) => {
                    let name = s.clone();
                    self.macro_call(&name, here);
                }
                Tok::ArenaKw | Tok::BumpArena | Tok::PortIo | Tok::Mmio if matches!(self.peek(1), Tok::Bang) => {
                    let name = crate::lexer::tok_text(self.peek(0)).to_string();
                    self.macro_call(&name, here);
                }
                // ───── バイト列の書き方（Fascon の Str は読み出し専用のバイト列） / byte-string idioms (a Fascon Str is a
                // read-only byte string) ─────
                Tok::Fn => {
                    // 関数ごとにバイト列の名前を数え直す。`名前: &[u8]` の引数はバイト列 / names restart per function;
                    // a `name: &[u8]` parameter is a byte view
                    self.byte_views.clear();
                    let mut k = self.i + 1;
                    while k < self.t.len() && !matches!(self.t[k].tok, Tok::LParen | Tok::LBrace | Tok::Semi | Tok::Eof) {
                        k += 1;
                    }
                    if matches!(self.t.get(k).map(|x| &x.tok), Some(Tok::LParen)) {
                        let close = self.matching(k);
                        for j in k..close {
                            if let Tok::Ident(n) = &self.t[j].tok {
                                if self.is_u8_slice_at(j + 2) && matches!(self.t.get(j + 1).map(|x| &x.tok), Some(Tok::Colon)) {
                                    self.byte_views.push(n.clone());
                                }
                            }
                        }
                    }
                    self.copy();
                }
                Tok::Let => {
                    self.let_byte_view();
                    self.copy();
                }
                Tok::For => {
                    if !self.for_bytes(here) {
                        self.copy();
                    }
                }
                Tok::Dot if matches!(self.peek(1), Tok::Ident(ref s) if s == "as_bytes") && matches!(self.peek(2), Tok::LParen) && matches!(self.peek(3), Tok::RParen) => {
                    // s.as_bytes()[i] → s.byte(i)、s.as_bytes().len() → s.len()、ほかは s のまま
                    // s.as_bytes()[i] → s.byte(i), s.as_bytes().len() → s.len(), otherwise just s
                    self.rep.note(here, "`.as_bytes()` を Str のまま使った（Str は読み出し専用のバイト列）", "used `.as_bytes()` as the Str itself (a Str is a read-only byte string)");
                    if matches!(self.peek(4), Tok::LBracket) {
                        let close = self.matching(self.i + 4);
                        self.push(Tok::Dot, here);
                        self.push(Tok::Ident("byte".into()), self.span(1));
                        self.push(Tok::LParen, self.span(4));
                        self.replace_at.push((close, Tok::RParen));
                        self.i += 5;
                    } else {
                        self.i += 4;
                    }
                }
                Tok::Ident(ref n) if matches!(self.peek(1), Tok::LBracket) && self.byte_views.iter().any(|v| v == n) && !matches!(self.out.last().map(|x| &x.tok), Some(Tok::Dot)) => {
                    // v[i] → v.byte(i)（境界検査付きの読み出し） / v[i] → v.byte(i) (a bounds-checked read)
                    self.copy();
                    let close = self.matching(self.i);
                    let sp = self.span(0);
                    self.push(Tok::Dot, sp);
                    self.push(Tok::Ident("byte".into()), sp);
                    self.push(Tok::LParen, sp);
                    self.replace_at.push((close, Tok::RParen));
                    self.i += 1;
                }
                Tok::Amp if self.is_u8_slice_at(self.i) => {
                    // &[u8] → Str（Fascon にスライスは無い。バイト列は読み出し専用の Str） / no slices in Fascon
                    self.push(Tok::Ident("Str".into()), here.to(self.span(3)));
                    self.i += 4;
                }
                _ => self.copy(),
            }
        }
        if let Some((_, sp)) = pending_section {
            self.err(sp, "`#[link_section]` の後ろに `static mut 名前: 型 = ..;` が必要です", "`#[link_section]` must be followed by `static mut NAME: TYPE = ..;`");
        }
        self.ensure_main();
    }

    /// t[k..] が `& [ u8 ]` か / whether t[k..] is `& [ u8 ]`
    fn is_u8_slice_at(&self, k: usize) -> bool {
        let g = |j: usize| self.t.get(j).map(|x| &x.tok);
        matches!(g(k), Some(Tok::Amp)) && matches!(g(k + 1), Some(Tok::LBracket)) && matches!(g(k + 2), Some(Tok::Ident(s)) if s == "u8") && matches!(g(k + 3), Some(Tok::RBracket))
    }

    /// `let [mut] 名前 [: &[u8]] = .. ;` を見て、バイト列の名前を覚え直す（`.as_bytes()` で終わる初期化も）
    /// track byte views from `let [mut] name [: &[u8]] = .. ;` (also an initializer ending in `.as_bytes()`)
    fn let_byte_view(&mut self) {
        let g = |j: usize| self.t.get(j).map(|x| &x.tok);
        let mut k = self.i + 1;
        if matches!(g(k), Some(Tok::Mut)) {
            k += 1;
        }
        let name = match g(k) {
            Some(Tok::Ident(n)) => n.clone(),
            _ => return,
        };
        self.byte_views.retain(|v| v != &name);
        let mut view = matches!(g(k + 1), Some(Tok::Colon)) && self.is_u8_slice_at(k + 2);
        let mut d = 0i32;
        let mut j = k + 1;
        while let Some(tok) = g(j) {
            match tok {
                Tok::LParen | Tok::LBracket | Tok::LBrace => d += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => d -= 1,
                Tok::Semi if d <= 0 => break,
                Tok::Eof => return,
                _ => {}
            }
            j += 1;
        }
        if j >= 4 && matches!(g(j - 4), Some(Tok::Dot)) && matches!(g(j - 3), Some(Tok::Ident(s)) if s == "as_bytes") && matches!(g(j - 2), Some(Tok::LParen)) && matches!(g(j - 1), Some(Tok::RParen)) {
            view = true;
        }
        if view {
            self.byte_views.push(name);
        }
    }

    /// `for [&]b in s.bytes() {..}` / `s.as_bytes()` / `s.as_bytes().iter()` / バイト列の名前（`v`・`v.iter()`）を
    /// 添字のループに展開する: `{ let rf_bytesN = (s); for rf_iN in 0..rf_bytesN.len() { let b = rf_bytesN.byte(rf_iN); .. } }`
    /// expand a for loop over the bytes of a Str into an index loop
    fn for_bytes(&mut self, here: Span) -> bool {
        let g = |j: usize| self.t.get(j).map(|x| &x.tok);
        let mut k = self.i + 1;
        if matches!(g(k), Some(Tok::Amp)) {
            k += 1;
        }
        let pat = match g(k) {
            Some(Tok::Ident(n)) => n.clone(),
            _ => return false,
        };
        if !matches!(g(k + 1), Some(Tok::In)) {
            return false;
        }
        let start = k + 2;
        let mut d = 0i32;
        let mut j = start;
        loop {
            match g(j) {
                None | Some(Tok::Eof) => return false,
                Some(Tok::LParen | Tok::LBracket) => d += 1,
                Some(Tok::RParen | Tok::RBracket) => d -= 1,
                Some(Tok::LBrace) if d == 0 => break,
                Some(Tok::Semi) if d == 0 => return false,
                _ => {}
            }
            j += 1;
        }
        let lb = j;
        let it: Vec<Token> = self.t[start..lb].to_vec();
        let n = it.len();
        let is = |x: &Token, s: &str| matches!(&x.tok, Tok::Ident(v) if v == s);
        let call = |a: usize, name: &str| -> bool {
            a + 4 <= n && matches!(it[a].tok, Tok::Dot) && is(&it[a + 1], name) && matches!(it[a + 2].tok, Tok::LParen) && matches!(it[a + 3].tok, Tok::RParen)
        };
        let suffix = if n >= 8 && call(n - 8, "as_bytes") && call(n - 4, "iter") {
            8
        } else if n >= 4 && (call(n - 4, "bytes") || call(n - 4, "as_bytes")) {
            4
        } else if n == 1 && matches!(&it[0].tok, Tok::Ident(v) if self.byte_views.contains(v)) {
            0
        } else if n == 5 && matches!(&it[0].tok, Tok::Ident(v) if self.byte_views.contains(v)) && call(1, "iter") {
            4
        } else {
            return false;
        };
        let expr: Vec<Token> = it[..n - suffix].to_vec();
        if expr.is_empty() {
            return false;
        }
        self.fresh += 1;
        let sv = format!("rf_bytes{}", self.fresh);
        let iv = format!("rf_i{}", self.fresh);
        let sp = here;
        self.push(Tok::LBrace, sp);
        self.push(Tok::Let, sp);
        self.push(Tok::Ident(sv.clone()), sp);
        self.push(Tok::Eq, sp);
        self.push(Tok::LParen, sp);
        for t in expr {
            self.out.push(t);
        }
        self.push(Tok::RParen, sp);
        self.push(Tok::Semi, sp);
        self.push(Tok::For, sp);
        self.push(Tok::Ident(iv.clone()), sp);
        self.push(Tok::In, sp);
        self.push(Tok::Int(0, None), sp);
        self.push(Tok::DotDot, sp);
        self.push(Tok::Ident(sv.clone()), sp);
        self.push(Tok::Dot, sp);
        self.push(Tok::Ident("len".into()), sp);
        self.push(Tok::LParen, sp);
        self.push(Tok::RParen, sp);
        self.push(Tok::LBrace, self.t[lb].span);
        if pat != "_" {
            self.push(Tok::Let, sp);
            self.push(Tok::Ident(pat), sp);
            self.push(Tok::Eq, sp);
            self.push(Tok::Ident(sv), sp);
            self.push(Tok::Dot, sp);
            self.push(Tok::Ident("byte".into()), sp);
            self.push(Tok::LParen, sp);
            self.push(Tok::Ident(iv), sp);
            self.push(Tok::RParen, sp);
            self.push(Tok::Semi, sp);
        }
        let close = self.matching(lb);
        self.extra_close.push(close);
        self.rep.note(here, "`for b in s.bytes()` を添字のループ（s.byte(i)）に書き換えた", "rewrote `for b in s.bytes()` as an index loop (s.byte(i))");
        self.i = lb + 1;
        true
    }

    fn depth_zero(&self) -> bool {
        let mut d = 0i32;
        for t in &self.out {
            match t.tok {
                Tok::LBrace => d += 1,
                Tok::RBrace => d -= 1,
                _ => {}
            }
        }
        d == 0
    }

    /// `fn main` が無ければ、ベアメタルの Rust でよく使う入口（_start / kmain / kernel_main / rust_main）を main にする
    /// without `fn main`, the usual bare-metal Rust entry (_start / kmain / kernel_main / rust_main) becomes main
    fn ensure_main(&mut self) {
        let has_main = self.out.windows(2).any(|w| matches!((&w[0].tok, &w[1].tok), (Tok::Fn, Tok::Ident(s)) if s == "main"));
        if has_main {
            return;
        }
        for cand in ["_start", "kmain", "kernel_main", "rust_main", "efi_main", "start"] {
            let pos = self.out.windows(2).position(|w| matches!((&w[0].tok, &w[1].tok), (Tok::Fn, Tok::Ident(s)) if s == cand));
            if let Some(p) = pos {
                let sp = self.out[p + 1].span;
                self.out[p + 1].tok = Tok::Ident("main".into());
                self.rep.note(sp, format!("入口 `{}` を `main` にした", cand), format!("the entry `{}` became `main`", cand));
                // 呼び出し側の名前も合わせる / rename the calls too
                for t in self.out.iter_mut() {
                    if matches!(&t.tok, Tok::Ident(s) if s == cand) {
                        t.tok = Tok::Ident("main".into());
                    }
                }
                return;
            }
        }
    }

    /// `static mut NAME: TYPE = INIT;`（`#[link_section]` 付き）→ mmio / port_io
    fn static_item(&mut self, sec: Option<(String, Span)>, here: Span) {
        let is_mut = matches!(self.peek(1), Tok::Mut);
        let name_k = if is_mut { 2 } else { 1 };
        let (name, nsp) = match self.t.get(self.i + name_k).map(|x| (&x.tok, x.span)) {
            Some((Tok::Ident(n), sp)) => (n.clone(), sp),
            _ => {
                self.copy();
                return;
            }
        };
        if !matches!(self.peek(name_k + 1), Tok::Colon) {
            self.copy();
            return;
        }
        // 型のトークン（`=` か `;` まで） / the type tokens (up to `=` or `;`)
        let ty_start = self.i + name_k + 2;
        let mut k = ty_start;
        let mut depth = 0i32;
        while k < self.t.len() {
            match self.t[k].tok {
                Tok::LBracket | Tok::LParen => depth += 1,
                Tok::RBracket | Tok::RParen => depth -= 1,
                Tok::Eq | Tok::Semi if depth == 0 => break,
                Tok::Eof => break,
                _ => {}
            }
            k += 1;
        }
        let ty: Vec<Token> = self.t[ty_start..k].to_vec();
        let Some((sec, ssp)) = sec else {
            // link_section の無い static mut は Fascon の規則どおりエラー（アリーナを使う） / a static mut without link_section
            self.diags.push(
                Diag::error(
                    "F0001",
                    here,
                    format!("`static mut {}` は使えません（共有の可変状態は arena で表します）", name),
                    format!("`static mut {}` is not allowed (shared mutable state is an arena)", name),
                )
                .hint(
                    format!("デバイスなら `#[link_section = \".mmio.0x...\"]` / `\".port.0x...\"` を付け、データなら `arena!({}: T[N]);` を使ってください", name.to_lowercase()),
                    format!("for a device add `#[link_section = \".mmio.0x...\"]` / `\".port.0x...\"`; for data use `arena!({}: T[N]);`", name.to_lowercase()),
                ),
            );
            self.i = k;
            self.skip_through_semi();
            return;
        };
        // 初期値は捨てる（デバイスのレジスタはプログラムが初期化するものではない） / the initializer is dropped
        self.i = k;
        self.skip_through_semi();
        let s = sec.trim().trim_start_matches('.').to_ascii_lowercase();
        let (kind, rest) = match s.find(|c: char| c == '.' || c == '@' || c == '_') {
            Some(p) => (s[..p].to_string(), s[p + 1..].to_string()),
            None => (s.clone(), String::new()),
        };
        let num = parse_num(&rest);
        let sp = here.to(nsp);
        match (kind.as_str(), num) {
            ("mmio" | "device" | "dev", Some(a)) => {
                self.decl(Tok::Mmio, &name, nsp, &ty, "at", a, sp);
                self.rep.note(ssp, format!("`static mut {}` → `mmio {}: .. at {:#x};`", name, name, a), format!("`static mut {}` → `mmio {}: .. at {:#x};`", name, name, a));
            }
            ("fb" | "framebuffer" | "vram", Some(a)) => {
                self.push(Tok::Hash, sp);
                self.push(Tok::LBracket, sp);
                self.push(Tok::Ident("framebuffer".into()), sp);
                self.push(Tok::RBracket, sp);
                self.decl(Tok::Mmio, &name, nsp, &ty, "at", a, sp);
                self.rep.note(ssp, format!("`static mut {}` → `#[framebuffer] mmio {}: .. at {:#x};`", name, name, a), format!("`static mut {}` → `#[framebuffer] mmio {}: .. at {:#x};`", name, name, a));
            }
            ("port" | "io" | "ioport" | "pio", Some(p)) => {
                self.decl(Tok::PortIo, &name, nsp, &ty, "at_port", p, sp);
                self.rep.note(ssp, format!("`static mut {}` → `port_io {}: .. at_port {:#x};`", name, name, p), format!("`static mut {}` → `port_io {}: .. at_port {:#x};`", name, name, p));
            }
            ("uart" | "uart0" | "serial" | "com1" | "console", _) => {
                // 移植できるシリアル: x86 は COM1（0x3f8）、AArch64 では PL011 へ自動で写す
                // a portable serial port: COM1 (0x3f8) on x86, remapped to the PL011 automatically on AArch64
                let p = num.unwrap_or(0x3f8);
                self.decl(Tok::PortIo, &name, nsp, &ty, "at_port", p, sp);
                self.rep.note(ssp, format!("`static mut {}` → `port_io {}: .. at_port {:#x};`（COM1 / AArch64 は PL011）", name, name, p), format!("`static mut {}` → `port_io {}: .. at_port {:#x};` (COM1; PL011 on AArch64)", name, name, p));
            }
            _ => self.diags.push(
                Diag::error("F0001", ssp, format!("link_section `{}` を読めません", sec), format!("cannot read the link_section `{}`", sec)).hint(
                    "`.mmio.0x0900_0000` / `.port.0x3f8` / `.fb.0x4800_0000` / `.uart` のどれかの形で書いてください",
                    "write one of `.mmio.0x0900_0000` / `.port.0x3f8` / `.fb.0x4800_0000` / `.uart`",
                ),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn decl(&mut self, kw: Tok, name: &str, nsp: Span, ty: &[Token], at: &str, num: u64, sp: Span) {
        self.push(kw, sp);
        self.push(Tok::Ident(name.to_string()), nsp);
        if !ty.is_empty() {
            self.push(Tok::Colon, sp);
            for t in ty {
                let t = match &t.tok {
                    Tok::Ident(s) if s == "char" => tk(Tok::Ident("u32".into()), t.span),
                    _ => t.clone(),
                };
                self.out.push(t);
            }
        }
        self.push(Tok::Ident(at.to_string()), sp);
        self.push(Tok::Int(num as u128, None), sp);
        self.push(Tok::Semi, sp);
    }

    /// `名前!(..)` のマクロ / a macro call `name!(..)`
    fn macro_call(&mut self, name: &str, here: Span) {
        let open = self.i + 2;
        let close = self.matching(open);
        let inner: Vec<Token> = self.t[open + 1..close].to_vec();
        let end_sp = self.t[close.min(self.t.len() - 1)].span;
        let sp = here.to(end_sp);
        self.i = close + 1;
        match name {
            "arena" | "bump_arena" => {
                // arena!(名前: T[N]) → arena 名前: T[N];（文の位置なら後ろの `;` はそのまま使う）
                let kw = if name == "arena" { Tok::ArenaKw } else { Tok::BumpArena };
                self.push(kw, sp);
                self.out.extend(inner);
                self.semi_after_item(sp);
                self.rep.note(sp, format!("`{}!(..)` → `{} ..;`", name, name), format!("`{}!(..)` → `{} ..;`", name, name));
            }
            "mmio" | "port_io" => {
                // mmio!(名前: 型, 番地) / port_io!(名前, ポート) / port_io!(名前: 型, ポート)
                let comma = top_level_comma(&inner);
                let Some(c) = comma else {
                    self.err(sp, format!("`{}!(名前, 番地)` の形で書いてください", name), format!("write `{}!(name, address)`", name));
                    return;
                };
                let kw = if name == "mmio" { Tok::Mmio } else { Tok::PortIo };
                self.push(kw, sp);
                self.out.extend(inner[..c].iter().cloned());
                self.push(Tok::Ident(if name == "mmio" { "at" } else { "at_port" }.into()), sp);
                self.out.extend(inner[c + 1..].iter().cloned());
                self.semi_after_item(sp);
                self.rep.note(sp, format!("`{}!(..)` → `{} ..;`", name, name), format!("`{}!(..)` → `{} ..;`", name, name));
            }
            "own_alloc" => {
                self.push(Tok::Ident("own_alloc".into()), here);
                self.push(Tok::LParen, sp);
                self.out.extend(inner);
                self.push(Tok::RParen, end_sp);
                self.rep.note(sp, "`own_alloc!(T)` → `own_alloc(T)`", "`own_alloc!(T)` → `own_alloc(T)`");
            }
            "println" | "print" | "panic" | "eprintln" | "eprint" | "unreachable" | "todo" | "unimplemented" => {
                let newline = matches!(name, "println" | "eprintln");
                let is_panic = matches!(name, "panic" | "unreachable" | "todo" | "unimplemented");
                let msg = match inner.first().map(|t| &t.tok) {
                    Some(Tok::Str(s)) => s.clone(),
                    None => match name {
                        "println" | "eprintln" | "print" | "eprint" => String::new(),
                        "unreachable" => "entered unreachable code".into(),
                        "todo" | "unimplemented" => "not implemented".into(),
                        _ => "explicit panic".into(),
                    },
                    _ => {
                        self.err(sp, format!("`{}!` の最初の引数は書式の文字列リテラルです", name), format!("the first argument of `{}!` must be a format string literal", name));
                        return;
                    }
                };
                // 書式の引数（深さ 0 のカンマで区切る） / the format arguments (split at depth-0 commas)
                let mut args: Vec<Vec<Token>> = Vec::new();
                if inner.len() > 1 {
                    let mut cur: Vec<Token> = Vec::new();
                    let mut depth = 0i32;
                    for t in &inner[2.min(inner.len())..] {
                        match t.tok {
                            Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                            Tok::RParen | Tok::RBracket | Tok::RBrace => depth -= 1,
                            Tok::Comma if depth == 0 => {
                                args.push(std::mem::take(&mut cur));
                                continue;
                            }
                            _ => {}
                        }
                        cur.push(t.clone());
                    }
                    if !cur.is_empty() {
                        args.push(cur);
                    }
                }
                let pieces = match format_pieces(&msg) {
                    Ok(p) => p,
                    Err(()) => {
                        self.err(sp, format!("`{}!` の書式文字列の `{{` / `}}` が対応していません", name), format!("unbalanced `{{` / `}}` in the format string of `{}!`", name));
                        return;
                    }
                };
                let simple = pieces.iter().all(|p| matches!(p, Piece::Lit(_)));
                if simple && args.is_empty() {
                    // 文字列リテラル 1 つ: print_str / put_str / panic そのまま / a single literal: print_str / put_str / panic as is
                    let lit: String = pieces.iter().map(|p| if let Piece::Lit(l) = p { l.as_str() } else { "" }).collect();
                    let target = if is_panic { "panic" } else if newline { "print_str" } else { "put_str" };
                    self.push(Tok::Ident(target.into()), here);
                    self.push(Tok::LParen, sp);
                    self.push(Tok::Str(lit), sp);
                    self.push(Tok::RParen, end_sp);
                    return;
                }
                // `{}` を含む書式: { put_str("..."); put(x); put_hex(y); .. print_str(""); } に開く。put は整数・Str・bool を型で選ぶ
                // a format with `{}`: expanded into { put_str(".."); put(x); put_hex(y); .. print_str(""); }; put picks integer,
                // Str or bool by type
                self.push(Tok::LBrace, here);
                let mut next_arg = 0usize;
                let mut lit_all = String::new();
                for p in &pieces {
                    match p {
                        Piece::Lit(l) => {
                            lit_all.push_str(l);
                            if !is_panic {
                                self.call1("put_str", vec![tk(Tok::Str(l.clone()), sp)], sp);
                            }
                        }
                        Piece::Hole(sel, hex) => {
                            lit_all.push_str("{}");
                            let arg: Option<Vec<Token>> = match sel {
                                Sel::Next => {
                                    let a = args.get(next_arg).cloned();
                                    next_arg += 1;
                                    a
                                }
                                Sel::Index(k) => args.get(*k).cloned(),
                                Sel::Name(n) => Some(vec![tk(Tok::Ident(n.clone()), sp)]),
                            };
                            let Some(arg) = arg else {
                                self.err(sp, format!("`{}!` の `{{}}` に対応する引数がありません", name), format!("`{}!` has a `{{}}` without a matching argument", name));
                                return;
                            };
                            let mut wrapped = vec![tk(Tok::LParen, sp)];
                            wrapped.extend(arg);
                            wrapped.push(tk(Tok::RParen, sp));
                            self.call1(if *hex { "put_hex" } else { "put" }, wrapped, sp);
                        }
                    }
                }
                if is_panic {
                    self.call1("panic", vec![tk(Tok::Str(lit_all), sp)], sp);
                } else if newline {
                    self.call1("print_str", vec![tk(Tok::Str(String::new()), sp)], sp);
                }
                self.push(Tok::RBrace, end_sp);
                self.rep.note(sp, format!("`{}!` の書式を put / put_str の列へ", name), format!("`{}!` formatting → a sequence of put / put_str", name));
            }
            "assert" => {
                // assert!(c) → if !(c) { panic("assertion failed"); }
                self.push(Tok::If, here);
                self.push(Tok::Bang, sp);
                self.push(Tok::LParen, sp);
                self.out.extend(inner);
                self.push(Tok::RParen, sp);
                self.push(Tok::LBrace, sp);
                self.push(Tok::Ident("panic".into()), sp);
                self.push(Tok::LParen, sp);
                self.push(Tok::Str("assertion failed".into()), sp);
                self.push(Tok::RParen, sp);
                self.push(Tok::Semi, sp);
                self.push(Tok::RBrace, sp);
                if matches!(self.peek(0), Tok::Semi) {
                    self.i += 1;
                }
            }
            _ => self.err(sp, format!("マクロ `{}!` はリフレーミングできません", name), format!("the macro `{}!` cannot be reframed", name)),
        }
    }

    /// `f(引数);` を出す / emit `f(args);`
    fn call1(&mut self, f: &str, arg: Vec<Token>, sp: Span) {
        self.push(Tok::Ident(f.into()), sp);
        self.push(Tok::LParen, sp);
        self.out.extend(arg);
        self.push(Tok::RParen, sp);
        self.push(Tok::Semi, sp);
    }

    /// 項目の位置のマクロの後ろの `;` を 1 つにそろえる / make sure exactly one `;` follows an item-position macro
    fn semi_after_item(&mut self, sp: Span) {
        if matches!(self.peek(0), Tok::Semi) {
            self.copy();
        } else {
            self.push(Tok::Semi, sp);
        }
    }
}

/// 書式文字列の部品 / the pieces of a format string
enum Piece {
    Lit(String),
    /// (どの引数か, 16 進か) / (which argument, hex or not)
    Hole(Sel, bool),
}

enum Sel {
    Next,
    Index(usize),
    Name(String),
}

/// `"a {} b {x} c {:#x} {{d}}"` を部品に分ける / split a format string into pieces
fn format_pieces(s: &str) -> Result<Vec<Piece>, ()> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '{' && cs.get(i + 1) == Some(&'{') {
            lit.push('{');
            i += 2;
            continue;
        }
        if c == '}' && cs.get(i + 1) == Some(&'}') {
            lit.push('}');
            i += 2;
            continue;
        }
        if c == '}' {
            return Err(());
        }
        if c == '{' {
            let mut j = i + 1;
            while j < cs.len() && cs[j] != '}' {
                j += 1;
            }
            if j >= cs.len() {
                return Err(());
            }
            let spec: String = cs[i + 1..j].iter().collect();
            let (arg, fmt) = match spec.split_once(':') {
                Some((a, f)) => (a.trim().to_string(), f.trim().to_string()),
                None => (spec.trim().to_string(), String::new()),
            };
            let hex = fmt.ends_with('x') || fmt.ends_with('X');
            let sel = if arg.is_empty() {
                Sel::Next
            } else if let Ok(k) = arg.parse::<usize>() {
                Sel::Index(k)
            } else {
                Sel::Name(arg)
            };
            if !lit.is_empty() {
                out.push(Piece::Lit(std::mem::take(&mut lit)));
            }
            out.push(Piece::Hole(sel, hex));
            i = j + 1;
            continue;
        }
        lit.push(c);
        i += 1;
    }
    if !lit.is_empty() {
        out.push(Piece::Lit(lit));
    }
    Ok(out)
}

fn top_level_comma(ts: &[Token]) -> Option<usize> {
    let mut d = 0i32;
    for (k, t) in ts.iter().enumerate() {
        match t.tok {
            Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::Lt => d += 1,
            Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::Gt => d -= 1,
            Tok::Comma if d == 0 => return Some(k),
            _ => {}
        }
    }
    None
}

/// `0x0900_0000` / `3f8` / `1016` などを読む / read `0x0900_0000`, `3f8`, `1016` ...
fn parse_num(s: &str) -> Option<u64> {
    let t: String = s.chars().filter(|c| *c != '_').collect();
    if t.is_empty() {
        return None;
    }
    if let Some(h) = t.strip_prefix("0x") {
        return u64::from_str_radix(h, 16).ok();
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        return t.parse().ok();
    }
    u64::from_str_radix(&t, 16).ok()
}

/// トークン列のリフレーミング / reframe a token stream
pub fn rewrite(toks: &[Token]) -> (Vec<Token>, Vec<Diag>, Report) {
    let mut w = W {
        t: toks,
        i: 0,
        out: Vec::with_capacity(toks.len()),
        diags: Vec::new(),
        rep: Report::default(),
        fresh: 0,
        extra_close: Vec::new(),
        replace_at: Vec::new(),
        byte_views: Vec::new(),
    };
    w.run();
    let _ = IntTy::U8;
    // 構文解析は最後のトークンが Eof であることを前提にする（括弧の閉じ忘れがあっても必ず 1 個で終える）
    // the parser relies on the last token being Eof (always end with exactly one, even with unbalanced brackets)
    while matches!(w.out.last().map(|t| &t.tok), Some(Tok::Eof)) && w.out.len() >= 2 && matches!(w.out[w.out.len() - 2].tok, Tok::Eof) {
        w.out.pop();
    }
    if !matches!(w.out.last().map(|t| &t.tok), Some(Tok::Eof)) {
        let sp = toks.last().map(|t| t.span).unwrap_or(Span::new(0, 0));
        w.out.push(tk(Tok::Eof, sp));
    }
    (w.out, w.diags, w.rep)
}
