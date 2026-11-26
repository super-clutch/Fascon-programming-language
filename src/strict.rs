//! `--emit-strict-fascon`: 機能安全証明形態の厳格な Fascon（完全に型を書いた、糖衣を外した Fascon の全構文テキスト）
//! `--emit-strict-fascon`: strict Fascon for functional-safety review (fully typed, desugared, complete Fascon text)
//!
//! 意味解析が済んだ構文木（名前解決・型推論・リフレーミングの後）から書き出す。書き出した文書は人が書く短い形と同じ
//! 意味で、もう一度コンパイルすると同じ C になる（tests の往復検査で確かめている）:
//!   - すべての `let` に型を書く（推論の結果）。整数リテラルには型の接尾辞（`100u32`）
//!   - アリーナ・バンプアリーナの `R[i]` は世代タグで検証するルックアップを明示:
//!       書き込み・`&mut` → `R.get_mut(i).unwrap()`、読み出し → `R.get(i).unwrap()`
//!     （名前解決がこの 2 つを `R[i]` と同じ式に戻すので、検査の省略も走査の速さも変わらない）
//!   - I/O ポートは `p.read()` / `p.write(v)` / `p.read_at(i)` / `p.write_at(i, v)`
//!   - `@` は書かない。型の別名（`type X<..> = ..;`）は展開し、配列の長さは数で書く
//!   - リフレーミング（Rust の見た目）で書いたコードは Fascon の本来の形で出る（何を書き換えたかは見出しの注釈）
//! Written from the analysed tree (after name resolution, inference and reframing). The text means the same as the short
//! human forms and compiles to the same C again (checked by the round-trip tests):
//!   - every `let` carries its (inferred) type and every integer literal its type suffix (`100u32`)
//!   - `R[i]` on arenas and bump arenas becomes the explicit generation-verified lookup: writes and `&mut` →
//!     `R.get_mut(i).unwrap()`, reads → `R.get(i).unwrap()` (name resolution maps both back to the same expression as
//!     `R[i]`, so check elision and scan speed are unchanged)
//!   - I/O ports become `p.read()` / `p.write(v)` / `p.read_at(i)` / `p.write_at(i, v)`
//!   - no `@`; type aliases (`type X<..> = ..;`) are expanded and array lengths are written as numbers
//!   - code written in Rust style (reframing) comes out in Fascon's own forms (the header notes what was rewritten)

use crate::ast::*;
use crate::diag::Lang;
use crate::reframe;
use crate::sema::{Res, Sema};
use crate::span::SourceFile;
use crate::types::{RegionRef, Ty};
use std::fmt::Write;

struct P<'a, 'p> {
    s: &'a Sema<'p>,
    src: &'a SourceFile,
    lang: Lang,
    /// いま見えているローカル変数の名前（同じ名前のローカルがリージョンを隠す場所では `@名前` と書く）
    /// the local names in scope (where a local hides a region of the same name, the region is written `@name`)
    scopes: std::cell::RefCell<Vec<std::collections::HashSet<String>>>,
}

/// 優先順位（構文解析と同じ。大きいほど強く結びつく） / precedence (as in the parser; higher binds tighter)
fn bin_prec(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
        BinOp::BitOr => 4,
        BinOp::BitXor => 5,
        BinOp::BitAnd => 6,
        BinOp::Shl | BinOp::Shr => 7,
        BinOp::Add | BinOp::Sub => 8,
        BinOp::Mul | BinOp::Div | BinOp::Rem => 9,
    }
}
const PREC_CAST: u8 = 10;
const PREC_UNARY: u8 = 11;
const PREC_POSTFIX: u8 = 12;
const PREC_ATOM: u8 = 13;

fn str_lit(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            '\0' => o.push_str("\\0"),
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(o, "\\x{:02x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// ソースに書かれた整数リテラルの数字部分（16 進などを保つ）。リフレーミングで作ったリテラルなど、そのまま使えないときは None
/// the digits of an integer literal as written (keeping hex and the like); None when the text is not a plain literal
/// (e.g. one made by reframing)
fn literal_digits(text: &str) -> Option<String> {
    let t = text.trim();
    let (prefix, body, radix) = if let Some(b) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        ("0x", b, 16)
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        ("0b", b, 2)
    } else if let Some(b) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        ("0o", b, 8)
    } else {
        ("", t, 10)
    };
    let mut end = body.len();
    for (i, c) in body.char_indices() {
        let ok = c == '_' || c.to_digit(radix).is_some();
        if !ok {
            end = i;
            break;
        }
    }
    let digits = &body[..end];
    let rest = &body[end..];
    if digits.trim_matches('_').is_empty() {
        return None;
    }
    if !rest.is_empty() && crate::lexer::IntTy::from_name(rest).is_none() {
        return None;
    }
    Some(format!("{}{}", prefix, digits.trim_end_matches('_')))
}

impl<'a, 'p> P<'a, 'p> {
    fn c(&self, ja: &str, en: &str) -> String {
        self.lang.pick(ja, en).to_string()
    }

    fn push_scope(&self, names: &[String]) {
        self.scopes.borrow_mut().push(names.iter().cloned().collect());
    }
    fn pop_scope(&self) {
        self.scopes.borrow_mut().pop();
    }
    fn declare(&self, n: &str) {
        if let Some(sc) = self.scopes.borrow_mut().last_mut() {
            sc.insert(n.to_string());
        }
    }
    /// リージョンの名前。同じ名前のローカルが見えているときだけ `@` を付ける（`@` はいつもリージョン）
    /// a region name; `@` is added only while a local of the same name is in scope (`@` always means the region)
    fn rname(&self, n: &str) -> String {
        if self.scopes.borrow().iter().any(|sc| sc.contains(n)) {
            format!("@{}", n)
        } else {
            n.to_string()
        }
    }
    fn pat_names(p: &Pattern) -> Vec<String> {
        match p {
            Pattern::Some(Some((n, _, _)), _) | Pattern::Bind(n, _, _) | Pattern::Ok(Some((n, _, _)), _) | Pattern::Err(Some((n, _, _)), _) => vec![n.clone()],
            _ => Vec::new(),
        }
    }

    /// ソースの型の書き方（推論済みの意味上の型から） / a type in source syntax (from the inferred semantic type)
    fn ty(&self, t: &Ty) -> String {
        match t {
            Ty::Int(i) => i.name().to_string(),
            Ty::IntVar(_) | Ty::Var(_) => "i32".to_string(),
            Ty::Float(f) => f.name().to_string(),
            Ty::FloatVar(_) => "f64".to_string(),
            Ty::GpuBuf(a) => format!("GpuBuf<{}>", self.s.t.arenas[*a].name),
            Ty::GpuFence(a) => format!("GpuFence<{}>", self.s.t.arenas[*a].name),
            Ty::Bool => "bool".to_string(),
            Ty::Unit => "()".to_string(),
            Ty::Str => "Str".to_string(),
            Ty::Slice(e) => format!("[{}]", self.ty(e)),
            Ty::Dma(a) => format!("Dma<{}>", self.s.t.arenas[*a].name),
            Ty::Never => "()".to_string(),
            Ty::Struct(s) => self.s.t.structs[*s].name.clone(),
            Ty::Option(i) => format!("Option<{}>", self.ty(i)),
            Ty::Own(i) => format!("own {}", self.ty(i)),
            Ty::Id(a) => format!("Id<{}>", self.s.t.arenas[*a].name),
            Ty::Array(e, n) => format!("[{}; {}]", self.ty(e), n),
            Ty::Ref(m, i) => format!("&{}{}", if *m { "mut " } else { "" }, self.ty(i)),
            Ty::Fn(ps, r) => {
                let p: Vec<String> = ps.iter().map(|x| self.ty(x)).collect();
                match &**r {
                    Ty::Unit | Ty::Never | Ty::Err => format!("fn({})", p.join(", ")),
                    other => format!("fn({}) -> {}", p.join(", "), self.ty(other)),
                }
            }
            Ty::Result(a, b) => format!("Result<{}, {}>", self.ty(a), self.ty(b)),
            Ty::Fault => "FaultError".to_string(),
            Ty::User(i) => format!("user {}", self.ty(i)),
            Ty::DmaOwn(i) => format!("dma own {}", self.ty(i)),
            Ty::MmioBind(s) => format!("mmio {}", self.s.t.structs[*s].name),
            Ty::Err => "()".to_string(),
        }
    }

    fn ety(&self, e: &Expr) -> Ty {
        self.s.expr_ty.get(e.id as usize).cloned().unwrap_or(Ty::Err)
    }

    fn port_of(&self, e: &Expr) -> Option<usize> {
        match &e.kind {
            ExprKind::ArenaRef(n) => match self.s.t.regions.get(n) {
                Some(RegionRef::Port(p)) => Some(*p),
                _ => None,
            },
            _ => None,
        }
    }

    fn fast_math_mark(&self, e: &Expr) -> &'static str {
        if self.s.prog.fast_math_loops.contains_key(&e.id) {
            "#[fast_math] "
        } else {
            ""
        }
    }

    fn int_lit(&self, e: &Expr, v: u128) -> String {
        let digits = if e.span.hi > e.span.lo { literal_digits(self.src.text.get(e.span.lo as usize..e.span.hi as usize).unwrap_or("")) } else { None };
        let digits = digits.unwrap_or_else(|| v.to_string());
        match self.ety(e) {
            Ty::Int(it) => format!("{}{}", digits, it.name()),
            Ty::IntVar(_) => format!("{}i32", digits),
            _ => digits,
        }
    }

    fn attr(a: &Attr) -> String {
        if let Some((n, _)) = a.repr_align {
            return format!("#[repr(align({}))]", n);
        }
        // `#[asm(aarch64 = "..", effects = "..")]` のような `名前 = 値` の引数 / `key = value` arguments
        if !a.kv.is_empty() {
            let mut parts: Vec<String> = Vec::new();
            if let Some((v, _)) = a.int_arg {
                parts.push(format!("{:#x}", v));
            }
            if let Some(w) = &a.ident_arg {
                parts.push(w.clone());
            }
            for (k, v, _) in &a.kv {
                let val = match v {
                    AttrVal::Str(x) => format!("\"{}\"", x.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")),
                    AttrVal::Int(n) => format!("{:#x}", n),
                    AttrVal::Ident(x) => x.clone(),
                };
                parts.push(if k.is_empty() { val } else { format!("{} = {}", k, val) });
            }
            return format!("#[{}({})]", a.name, parts.join(", "));
        }
        if let Some(w) = &a.ident_arg {
            return format!("#[{}({})]", a.name, w);
        }
        match a.int_arg {
            Some((v, _)) => format!("#[{}({:#x})]", a.name, v),
            None => format!("#[{}]", a.name),
        }
    }

    // ───── 式 / expressions ─────

    fn prec(&self, e: &Expr) -> u8 {
        // `*b`（own の中身）は前置演算子 / `*b` (the contents of an own) is a prefix operator
        if self.s.own_derefs.contains(&e.id) {
            return PREC_UNARY;
        }
        match &e.kind {
            ExprKind::Binary(op, ..) => bin_prec(*op),
            ExprKind::Cast(..) => PREC_CAST,
            ExprKind::Unary(..) | ExprKind::Ref { .. } | ExprKind::Own(_) => PREC_UNARY,
            ExprKind::Int(..) if self.s.neg_lits.contains(&e.id) => PREC_UNARY,
            ExprKind::Field(..) | ExprKind::MethodCall { .. } | ExprKind::Call { .. } | ExprKind::CallIndirect { .. } | ExprKind::OwnAlloc(_) => PREC_POSTFIX,
            ExprKind::Index(..) => {
                if self.s.arena_index.contains_key(&e.id) || self.s.port_index.contains_key(&e.id) {
                    PREC_POSTFIX
                } else {
                    PREC_POSTFIX
                }
            }
            ExprKind::Assign { .. } => 0,
            ExprKind::If { .. } | ExprKind::IfLet { .. } | ExprKind::Match { .. } | ExprKind::While { .. } | ExprKind::Loop { .. } | ExprKind::For { .. } | ExprKind::Block(_) | ExprKind::VolatileBypass(_) | ExprKind::Critical(_) => 0,
            ExprKind::Return(_) | ExprKind::Break | ExprKind::Continue => 0,
            _ => PREC_ATOM,
        }
    }

    /// 必要なら括弧で包む / wrap in parentheses when needed
    fn sub(&self, e: &Expr, min: u8, write: bool, ns: bool, ind: usize) -> String {
        let p = self.prec(e);
        if p < min {
            format!("({})", self.expr(e, write, false, ind))
        } else {
            self.expr(e, write, ns, ind)
        }
    }

    /// write: この場所に書き込む（get_mut を使う） / write: this place is written (use get_mut)
    /// ns: 構造体リテラルを裸で書けない位置（if / while の条件など） / ns: a position where a bare struct literal is not allowed
    fn expr(&self, e: &Expr, write: bool, ns: bool, ind: usize) -> String {
        // own の中身 `*b`: 厳格形では `*` を省かない / the contents of an own `*b`: the strict form always writes the `*`
        if self.s.own_derefs.contains(&e.id) {
            let inner = self.expr_raw(e, write, ns, ind);
            let ip = self.prec_raw(e);
            return if ip < PREC_UNARY { format!("*({})", inner) } else { format!("*{}", inner) };
        }
        self.expr_raw(e, write, ns, ind)
    }

    fn prec_raw(&self, e: &Expr) -> u8 {
        match &e.kind {
            ExprKind::Binary(op, ..) => bin_prec(*op),
            ExprKind::Cast(..) => PREC_CAST,
            ExprKind::Unary(..) | ExprKind::Ref { .. } | ExprKind::Own(_) => PREC_UNARY,
            ExprKind::Path(_) | ExprKind::Field(..) | ExprKind::Index(..) | ExprKind::MethodCall { .. } | ExprKind::Call { .. } | ExprKind::CallIndirect { .. } | ExprKind::OwnAlloc(_) => PREC_POSTFIX,
            _ => 0,
        }
    }

    fn expr_raw(&self, e: &Expr, write: bool, ns: bool, ind: usize) -> String {
        match &e.kind {
            ExprKind::Int(v, _) => {
                let lit = self.int_lit(e, *v);
                if self.s.neg_lits.contains(&e.id) {
                    format!("-{}", lit)
                } else {
                    lit
                }
            }
            // 浮動小数点のリテラル: 書いた文字列に型の接尾辞を付ける（`1.5f32`） / a float literal: the written text with
            // the type suffix (`1.5f32`)
            ExprKind::Float(text, _) => match self.ety(e) {
                Ty::Float(ft) => format!("{}{}", text, ft.name()),
                _ => format!("{}f64", text),
            },
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::Str(s) => str_lit(s),
            ExprKind::Unit => "()".to_string(),
            ExprKind::Path(n) => match self.s.res.get(&e.id) {
                Some(Res::NoneLit) => "None".to_string(),
                _ => n.clone(),
            },
            ExprKind::ArenaRef(n) => match self.port_of(e) {
                // ポートを値として使う = 読み出し / a port used as a value = a read
                Some(_) if self.s.port_reads.contains_key(&e.id) => format!("{}.read()", self.rname(n)),
                _ => self.rname(n),
            },
            ExprKind::Call { callee, args, turbofish, .. } => {
                let a: Vec<String> = args.iter().map(|x| self.expr(x, false, false, ind)).collect();
                let tf = match turbofish {
                    Some(t) => format!("::<{}>", self.type_expr_text(t)),
                    None => String::new(),
                };
                format!("{}{}({})", callee, tf, a.join(", "))
            }
            ExprKind::CallIndirect { callee, args } => {
                let a: Vec<String> = args.iter().map(|x| self.expr(x, false, false, ind)).collect();
                // 呼ばれる式は名前でも括弧で包む（`(h.on_click)(..)`。名前だけなら関数の呼び出しに見えるため）
                // the callee is always parenthesized (`(h.on_click)(..)`; a bare name would read as a function call)
                format!("({})({})", self.expr(callee, false, false, ind), a.join(", "))
            }
            ExprKind::MethodCall { recv, method, args, .. } => {
                let w = method == "take";
                let r = self.sub(recv, PREC_POSTFIX, w, ns, ind);
                let a: Vec<String> = args.iter().map(|x| self.expr(x, false, false, ind)).collect();
                format!("{}.{}({})", r, method, a.join(", "))
            }
            ExprKind::Field(b, name, _) => {
                let base = self.sub(b, PREC_POSTFIX, write, ns, ind);
                format!("{}.{}", base, name)
            }
            ExprKind::Index(b, i) => {
                let idx = self.expr(i, false, false, ind);
                if let Some(&a) = self.s.arena_index.get(&e.id) {
                    // 世代タグで検証するルックアップ（R[i] と同じ式） / a generation-verified lookup (the same expression as R[i])
                    let n = self.rname(&self.s.t.arenas[a].name);
                    return format!("{}.{}({}).unwrap()", n, if write { "get_mut" } else { "get" }, idx);
                }
                if self.s.port_index.contains_key(&e.id) {
                    let base = self.expr(b, false, false, ind);
                    return format!("{}.read_at({})", base, idx);
                }
                let base = self.sub(b, PREC_POSTFIX, write, ns, ind);
                format!("{}[{}]", base, idx)
            }
            ExprKind::StructLit { name, fields, .. } => {
                let fs: Vec<String> = fields.iter().map(|(f, _, v)| format!("{}: {}", f, self.expr(v, false, false, ind))).collect();
                let lit = format!("{} {{ {} }}", name, fs.join(", "));
                if ns {
                    format!("({})", lit)
                } else {
                    lit
                }
            }
            ExprKind::ArrayLit(xs) => {
                let v: Vec<String> = xs.iter().map(|x| self.expr(x, false, false, ind)).collect();
                format!("[{}]", v.join(", "))
            }
            ExprKind::ArrayRepeat(x, n) => {
                let len = match self.ety(e) {
                    Ty::Array(_, k) => k.to_string(),
                    _ => self.expr(n, false, false, ind),
                };
                format!("[{}; {}]", self.expr(x, false, false, ind), len)
            }
            ExprKind::Own(x) => format!("own {}", self.sub(x, PREC_UNARY, false, ns, ind)),
            ExprKind::OwnAlloc(_) => match self.ety(e) {
                Ty::Own(i) => format!("own_alloc({})", self.ty(&i)),
                _ => "own_alloc()".to_string(),
            },
            ExprKind::Unary(op, x) => {
                let o = match op {
                    UnOp::Neg => "-",
                    UnOp::Not => "!",
                };
                // `-5` は負のリテラル（neg_lits）なので、式の側に `-` を付ける / `-5` is a negative literal (neg_lits)
                if let ExprKind::Int(..) = x.kind {
                    if *op == UnOp::Neg && self.s.neg_lits.contains(&x.id) {
                        return self.expr(x, false, ns, ind);
                    }
                }
                format!("{}{}", o, self.sub(x, PREC_UNARY, false, ns, ind))
            }
            ExprKind::Binary(op, a, b) => {
                let p = bin_prec(*op);
                let (la, lb) = if op.is_cmp() { (p + 1, p + 1) } else { (p, p + 1) };
                format!("{} {} {}", self.sub(a, la, false, ns, ind), op.text(), self.sub(b, lb, false, ns, ind))
            }
            ExprKind::Cast(x, _) => format!("{} as {}", self.sub(x, PREC_UNARY, false, ns, ind), self.ty(&self.ety(e))),
            ExprKind::Ref { mutable, inner } => format!("&{}{}", if *mutable { "mut " } else { "" }, self.sub(inner, PREC_UNARY, *mutable, ns, ind)),
            ExprKind::Assign { op, lhs, rhs } => self.assign(*op, lhs, rhs, ind),
            ExprKind::If { .. } | ExprKind::IfLet { .. } => self.if_expr(e, ind),
            ExprKind::Match { scrut, arms } => {
                let pad = "    ".repeat(ind + 1);
                let mut o = format!("match {} {{\n", self.expr(scrut, false, true, ind));
                for a in arms {
                    self.push_scope(&Self::pat_names(&a.pat));
                    let body = match &a.body.kind {
                        ExprKind::Block(b) => format!("{}\n", self.block(b, ind + 1)),
                        _ => format!("{},\n", self.expr(&a.body, false, false, ind + 1)),
                    };
                    self.pop_scope();
                    let _ = write!(o, "{}{} => {}", pad, self.pat(&a.pat), body);
                }
                let _ = write!(o, "{}}}", "    ".repeat(ind));
                o
            }
            // `#[fast_math]` を付けたループは属性ごと書き出す / loops written with `#[fast_math]` keep the attribute
            ExprKind::While { cond, body } => format!("{}while {} {}", self.fast_math_mark(e), self.expr(cond, false, true, ind), self.block(body, ind)),
            ExprKind::Loop { body } => format!("{}loop {}", self.fast_math_mark(e), self.block(body, ind)),
            ExprKind::For { var, iter, body, .. } => {
                let it = match iter {
                    ForIter::Range { lo, hi, inclusive } => format!(
                        "{}{}{}",
                        self.sub(lo, bin_prec(BinOp::Or) + 1, false, true, ind),
                        if *inclusive { "..=" } else { ".." },
                        self.sub(hi, bin_prec(BinOp::Or) + 1, false, true, ind)
                    ),
                    ForIter::Arena(n, _) if self.s.par_fors.contains_key(&e.id) => format!("{}.par()", n),
                    ForIter::Arena(n, _) => n.clone(),
                };
                self.push_scope(&[var.clone()]);
                let b = self.block(body, ind);
                self.pop_scope();
                format!("{}for {} in {} {}", self.fast_math_mark(e), var, it, b)
            }
            ExprKind::Break => "break".to_string(),
            ExprKind::Continue => "continue".to_string(),
            ExprKind::Return(x) => match x {
                Some(x) => format!("return {}", self.expr(x, false, false, ind)),
                None => "return".to_string(),
            },
            ExprKind::Block(b) => self.block(b, ind),
            ExprKind::VolatileBypass(b) => format!("volatile_bypass {}", self.block(b, ind)),
            ExprKind::Critical(b) => format!("critical {}", self.block(b, ind)),
            ExprKind::Atomic(b) => format!("atomic {}", self.block(b, ind)),
            ExprKind::RcuRead(b) => format!("rcu_read {}", self.block(b, ind)),
            ExprKind::CatchFault(b) => format!("catch_fault {}", self.block(b, ind)),
            ExprKind::TypeCall { name, ty, args, .. } => {
                let mut a = vec![self.type_expr_text(ty)];
                a.extend(args.iter().map(|x| self.expr(x, false, false, ind)));
                format!("{}({})", name, a.join(", "))
            }
            ExprKind::Unsupported(w) => format!("/* {} */", w),
        }
    }

    fn assign(&self, op: Option<BinOp>, lhs: &Expr, rhs: &Expr, ind: usize) -> String {
        let r = self.expr(rhs, false, false, ind);
        // I/O ポート / I/O ports
        if let Some(_p) = self.port_of(lhs) {
            let n = self.expr_name(lhs);
            return match op {
                None => format!("{}.write({})", n, r),
                Some(o) => format!("{n}.write({n}.read() {} {})", o.text(), self.paren_for(rhs, bin_prec(o) + 1, ind), n = n),
            };
        }
        if let (ExprKind::Index(b, i), true) = (&lhs.kind, self.s.port_index.contains_key(&lhs.id)) {
            let n = self.expr_name(b);
            let idx = self.expr(i, false, false, ind);
            return match op {
                None => format!("{}.write_at({}, {})", n, idx, r),
                // 添字が副作用を持たないときだけ read_at / write_at に開く / expand into read_at / write_at only when the
                // index has no side effects
                Some(o) if matches!(i.kind, ExprKind::Int(..) | ExprKind::Path(_)) => {
                    format!("{n}.write_at({i}, {n}.read_at({i}) {} {})", o.text(), self.paren_for(rhs, bin_prec(o) + 1, ind), n = n, i = idx)
                }
                Some(o) => format!("{}[{}] {}= {}", n, idx, o.text(), r),
            };
        }
        let l = self.expr(lhs, true, false, ind);
        match op {
            None => format!("{} = {}", l, r),
            Some(o) => format!("{} {}= {}", l, o.text(), r),
        }
    }

    fn paren_for(&self, e: &Expr, min: u8, ind: usize) -> String {
        self.sub(e, min, false, false, ind)
    }

    fn expr_name(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::ArenaRef(n) => self.rname(n),
            ExprKind::Path(n) => n.clone(),
            _ => self.expr(e, false, false, 0),
        }
    }

    fn if_expr(&self, e: &Expr, ind: usize) -> String {
        let (head, then, els) = match &e.kind {
            ExprKind::If { cond, then, els } => (format!("if {}", self.expr(cond, false, true, ind)), then, els),
            ExprKind::IfLet { pat, scrut, then, els } => (format!("if let {} = {}", self.pat(pat), self.expr(scrut, false, true, ind)), then, els),
            _ => return self.expr(e, false, false, ind),
        };
        let binds = match &e.kind {
            ExprKind::IfLet { pat, .. } => Self::pat_names(pat),
            _ => Vec::new(),
        };
        self.push_scope(&binds);
        let then_text = self.block(then, ind);
        self.pop_scope();
        let mut o = format!("{} {}", head, then_text);
        if let Some(x) = els {
            match &x.kind {
                ExprKind::If { .. } | ExprKind::IfLet { .. } => {
                    let _ = write!(o, " else {}", self.if_expr(x, ind));
                }
                ExprKind::Block(b) => {
                    let _ = write!(o, " else {}", self.block(b, ind));
                }
                _ => {
                    let _ = write!(o, " else {{ {} }}", self.expr(x, false, false, ind));
                }
            }
        }
        o
    }

    fn pat(&self, p: &Pattern) -> String {
        match p {
            Pattern::Wild(_) => "_".to_string(),
            Pattern::Int(v, _) => v.to_string(),
            Pattern::Bool(b, _) => b.to_string(),
            Pattern::None(_) => "None".to_string(),
            Pattern::Some(None, _) => "Some(_)".to_string(),
            Pattern::Some(Some((n, _, m)), _) => format!("Some({}{})", if *m { "mut " } else { "" }, n),
            Pattern::Bind(n, _, _) => n.clone(),
            Pattern::Ok(None, _) => "Ok(_)".to_string(),
            Pattern::Ok(Some((n, _, m)), _) => format!("Ok({}{})", if *m { "mut " } else { "" }, n),
            Pattern::Err(None, _) => "Err(_)".to_string(),
            Pattern::Err(Some((n, _, m)), _) => format!("Err({}{})", if *m { "mut " } else { "" }, n),
        }
    }

    // ───── 文とブロック / statements and blocks ─────

    fn block(&self, b: &Block, ind: usize) -> String {
        if b.stmts.is_empty() && b.tail.is_none() {
            return "{}".to_string();
        }
        let pad = "    ".repeat(ind + 1);
        let mut o = String::from("{\n");
        self.push_scope(&[]);
        for st in &b.stmts {
            match st {
                Stmt::Let(l) => {
                    let la: String = l.attrs.iter().map(|a| format!("{} ", Self::attr(a))).collect();
                    let _ = writeln!(o, "{}{}{}", pad, la, self.let_stmt(l, ind + 1));
                    self.declare(&l.name);
                }
                Stmt::Expr(e) => {
                    let t = self.expr(e, false, false, ind + 1);
                    if e.kind.is_block_like() {
                        let _ = writeln!(o, "{}{}", pad, t);
                    } else {
                        let _ = writeln!(o, "{}{};", pad, t);
                    }
                }
            }
        }
        if let Some(t) = &b.tail {
            let _ = writeln!(o, "{}{}", pad, self.expr(t, false, false, ind + 1));
        }
        self.pop_scope();
        let _ = write!(o, "{}}}", "    ".repeat(ind));
        o
    }

    fn let_stmt(&self, l: &LetStmt, ind: usize) -> String {
        let lt = self.s.locals.get(l.bind as usize).and_then(|x| x.as_ref()).map(|x| x.ty.clone()).unwrap_or(Ty::Err);
        let ann = match lt {
            Ty::Never | Ty::Err => String::new(),
            t => format!(": {}", self.ty(&t)),
        };
        let m = if l.mutable { "mut " } else { "" };
        match &l.init {
            Some(i) => format!("let {}{}{} = {};", m, l.name, ann, self.expr(i, false, false, ind)),
            None => format!("let {}{}{};", m, l.name, ann),
        }
    }

    /// 型の別名を経由しない書き方が無い場所（turbofish）だけで使う / only for turbofish, where no semantic type is recorded
    fn type_expr_text(&self, t: &TypeExpr) -> String {
        match t {
            TypeExpr::Path { name, args, .. } => {
                if args.is_empty() {
                    name.clone()
                } else {
                    let a: Vec<String> = args
                        .iter()
                        .map(|g| match g {
                            GenericArg::Type(t) => self.type_expr_text(t),
                            GenericArg::Arena(n, _) => n.clone(),
                        })
                        .collect();
                    format!("{}<{}>", name, a.join(", "))
                }
            }
            TypeExpr::Own(i, _) => format!("own {}", self.type_expr_text(i)),
            TypeExpr::Ref { mutable, inner, .. } => format!("&{}{}", if *mutable { "mut " } else { "" }, self.type_expr_text(inner)),
            TypeExpr::Array { elem, len, .. } => format!("[{}; {}]", self.type_expr_text(elem), self.expr(len, false, false, 0)),
            TypeExpr::Slice { elem, .. } => format!("[{}]", self.type_expr_text(elem)),
            TypeExpr::Unit(_) => "()".to_string(),
            TypeExpr::Fn { params, ret, .. } => {
                let p: Vec<String> = params.iter().map(|x| self.type_expr_text(x)).collect();
                match ret {
                    Some(r) => format!("fn({}) -> {}", p.join(", "), self.type_expr_text(r)),
                    None => format!("fn({})", p.join(", ")),
                }
            }
            TypeExpr::User(i, _) => format!("user {}", self.type_expr_text(i)),
            TypeExpr::DmaOwn(i, _) => format!("dma own {}", self.type_expr_text(i)),
            TypeExpr::MmioBind(i, _) => format!("mmio {}", self.type_expr_text(i)),
        }
    }

    // ───── 宣言 / items ─────

    fn items(&self, out: &mut String) {
        let prog = self.s.prog;
        let t = &self.s.t;
        // 構造体 / structs
        for it in &prog.items {
            if let Item::Struct(sd) = it {
                let Some(&sid) = t.struct_by_name.get(&sd.name) else { continue };
                for a in &sd.attrs {
                    let _ = writeln!(out, "{}", Self::attr(a));
                }
                let info = &t.structs[sid];
                if info.fields.is_empty() {
                    let _ = writeln!(out, "struct {} {{}}\n", info.name);
                    continue;
                }
                let _ = writeln!(out, "struct {} {{", info.name);
                for (k, f) in info.fields.iter().enumerate() {
                    // フィールドの属性（#[no_column] / #[secret]） / field attributes
                    let fa: String = sd.fields.get(k).map(|x| x.attrs.iter().map(|a| format!("{} ", Self::attr(a))).collect()).unwrap_or_default();
                    let _ = writeln!(out, "    {}{}: {},", fa, f.name, self.ty(&f.ty));
                }
                let _ = writeln!(out, "}}\n");
            }
        }
        // static（定数） / statics (constants)
        let mut any = false;
        for it in &prog.items {
            if let Item::Static(sd) = it {
                let Some(&si) = t.static_by_name.get(&sd.name) else { continue };
                let st = &t.statics[si];
                if self.s.embeds.contains_key(&si) {
                    // 埋め込むファイルは絶対パスで書く（厳格形をどこへ写しても同じ中身を読む） / the embedded file by absolute path
                    if let ExprKind::Call { args, .. } = &sd.value.kind {
                        if let Some(ExprKind::Str(p)) = args.first().map(|a| &a.kind) {
                            let full = match &self.s.base_dir {
                                Some(d) if !std::path::Path::new(p).is_absolute() => d.join(p),
                                _ => std::path::PathBuf::from(p),
                            };
                            let full = std::fs::canonicalize(&full).unwrap_or(full);
                            let shown = full.to_string_lossy().replace('\\', "/");
                            let shown = shown.strip_prefix("//?/").unwrap_or(&shown).to_string();
                            let _ = writeln!(out, "static {}: {} = embed(\"{}\");", st.name, self.ty(&st.ty), shown);
                            any = true;
                            continue;
                        }
                    }
                }
                let _ = writeln!(out, "static {}: {} = {};", st.name, self.ty(&st.ty), self.expr(&sd.value, false, false, 0));
                any = true;
            }
        }
        if any {
            out.push('\n');
        }
        // リージョン / regions
        let mut any = false;
        for it in &prog.items {
            match it {
                Item::Arena(ad) => {
                    let Some(ai) = t.arena_of_name(&ad.name) else { continue };
                    let a = &t.arenas[ai];
                    let kind = if a.gc && a.shared { "shared gc_arena" } else if a.gc { "gc_arena" } else if a.dma { "dma_arena" } else if a.gpu { "gpu_arena" } else if a.task_stack.is_some() { "task_arena" } else if a.bump { "bump_arena" } else if a.shared { "shared arena" } else { "arena" };
                    let note = if a.gc {
                        self.c("fascon-gc が回収する（手動の free / remove は F2621）", "reclaimed by fascon-gc (manual free / remove is F2621)")
                    } else if a.bump {
                        self.c("Id は 64 ビット（世代 + 添字）。呼び出しリージョンの出口で O(1) で一括解放", "64-bit Ids (generation + index); released in O(1) at the exit of each call region")
                    } else {
                        self.c("Id は 32 ビット（世代 + 添字）", "32-bit Ids (generation + index)")
                    };
                    let stack = a.task_stack.map(|s| format!(" stack {}", s)).unwrap_or_default();
                    if let Some((v, _)) = ad.numa_node {
                        let _ = writeln!(out, "#[numa_node({})]", v);
                    }
                    let _ = writeln!(out, "{} {}: {}[{}]{}; // {}", kind, a.name, self.ty(&a.elem), a.cap, stack, note);
                    any = true;
                }
                Item::Chan(cd) => {
                    let Some(RegionRef::Chan(ci)) = t.regions.get(&cd.name) else { continue };
                    let c = &t.chans[*ci];
                    let _ = writeln!(out, "chan {}: {}[{}]; // {}", c.name, self.ty(&c.elem), c.cap, self.c("コア間のロックフリーなチャネル（送った値は移動する）", "a lock-free channel between cores (sent values move)"));
                    any = true;
                }
                Item::UserMem(ud) => {
                    let Some(RegionRef::UserMem(ui)) = t.regions.get(&ud.name) else { continue };
                    let u = &t.umems[*ui];
                    if u.task == usize::MAX {
                        continue;
                    }
                    let _ = writeln!(out, "user_mem {} for {}: {:#x} at {:#x}; // {}", u.name, t.arenas[u.task].name, u.size, u.at, self.c("タスクごとのユーザーメモリ（EL0 からは同じ仮想番地）", "per-task user memory (the same virtual address from EL0)"));
                    any = true;
                }
                Item::Dma(dd) => {
                    let Some(RegionRef::Dma(di)) = t.regions.get(&dd.name) else { continue };
                    let d = &t.dmas[*di];
                    let _ = writeln!(out, "dma {}: [{}; {}]; // {}", d.name, self.ty(&d.elem), d.len, self.c("デバイスと共有する RAM（常に volatile）", "RAM shared with a device (always volatile)"));
                    any = true;
                }
                Item::Mmio(md) => {
                    let Some(RegionRef::Mmio(mi)) = t.regions.get(&md.name) else { continue };
                    let m = &t.mmios[*mi];
                    if m.framebuffer {
                        let _ = writeln!(out, "#[framebuffer]");
                    }
                    let shape = m.dims.iter().rev().fold(m.elem.name().to_string(), |acc, d| format!("[{}; {}]", acc, d));
                    let _ = writeln!(out, "mmio {}: {} at {:#x};", m.name, shape, m.addr);
                    any = true;
                }
                Item::Port(pd) => {
                    let Some(RegionRef::Port(pi)) = t.regions.get(&pd.name) else { continue };
                    let p = &t.ports[*pi];
                    if let Some(m) = p.mmio_map {
                        let _ = writeln!(out, "#[mmio_map({:#x})]", m);
                    }
                    let shape = match p.len {
                        Some(l) => format!("[{}; {}]", p.elem.name(), l),
                        None => p.elem.name().to_string(),
                    };
                    let _ = writeln!(
                        out,
                        "port_io {}: {} at_port {:#x}; // {}",
                        p.name,
                        shape,
                        p.port,
                        self.c("x86_64: in / out 命令、AArch64: コンパイラが MMIO へ写す", "x86_64: in / out instructions; AArch64: remapped to MMIO by the compiler")
                    );
                    any = true;
                }
                _ => {}
            }
        }
        if any {
            out.push('\n');
        }
        // 関数 / functions
        for it in &prog.items {
            if let Item::Fn(fd) = it {
                let Some(&fi) = t.fn_by_name.get(&fd.name) else { continue };
                let f = &t.fns[fi];
                if let Some(regs) = self.s.call_regions.get(fi) {
                    for &b in regs {
                        let bn = &t.arenas[b].name;
                        let _ = writeln!(
                            out,
                            "// {}",
                            self.c(
                                &format!("呼び出しリージョン: この関数が {} から確保したものは、戻るとき（すべての出口）に先頭を入口の印へ戻す 1 回の格納でまとめて解放される", bn),
                                &format!("call region: what this function allocates from {} is released at once when it returns (every exit) by one store resetting the top to the mark taken at entry", bn)
                            )
                        );
                    }
                }
                for a in &fd.attrs {
                    let _ = writeln!(out, "{}", Self::attr(a));
                }
                let ps: Vec<String> = f.params.iter().map(|p| format!("{}{}: {}", if p.mutable { "mut " } else { "" }, p.name, self.ty(&p.ty))).collect();
                let ret = match &f.ret {
                    Ty::Unit | Ty::Never | Ty::Err => String::new(),
                    r => format!(" -> {}", self.ty(r)),
                };
                // 本体の無い境界の関数: 安全インラインアセンブリと C からの取り込み / bodiless boundary functions
                if f.asm.is_some() {
                    let _ = writeln!(out, "fn {}({}){};\n", f.name, ps.join(", "), ret);
                    continue;
                }
                if f.extern_c == Some(false) {
                    let _ = writeln!(out, "extern \"C\" fn {}({}){};\n", f.name, ps.join(", "), ret);
                    continue;
                }
                if f.stub {
                    // ユーザー側のシステムコールの宣言 / a system call declaration on the user side
                    let _ = writeln!(out, "syscall {} at {} ({}){};\n", f.name, f.syscall.unwrap_or(0), ps.join(", "), ret);
                    continue;
                }
                let pnames: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();
                self.push_scope(&pnames);
                let body = self.block(&fd.body, 0);
                self.pop_scope();
                if let Some(intid) = f.irq {
                    // 割り込みハンドラ / an interrupt handler
                    let _ = writeln!(out, "irq {} at {} {}\n", f.name, intid, body);
                    continue;
                }
                if f.extern_c == Some(true) {
                    let _ = writeln!(out, "extern \"C\" fn {}({}){} {}\n", f.name, ps.join(", "), ret, body);
                    continue;
                }
                // 属性の形（#[syscall(N)] fn / #[trap] fn）は属性をすでに書いたので fn のまま / the attribute forms stay `fn`
                if let Some(nr) = f.syscall.filter(|_| !f.syscall_v2) {
                    let _ = writeln!(out, "syscall {} at {} ({}){} {}\n", f.name, nr, ps.join(", "), ret, body);
                    continue;
                }
                if f.trap && !f.trap_v2 {
                    let _ = writeln!(out, "trap {}({}) {}\n", f.name, ps.join(", "), body);
                    continue;
                }
                let _ = writeln!(out, "{}fn {}({}){} {}\n", if fd.is_async { "async " } else { "" }, f.name, ps.join(", "), ret, body);
            }
        }
    }
}

/// 厳格形の全文 / the whole strict text
pub fn print(s: &Sema, src: &SourceFile, reframed: bool, rep: &reframe::Report, marks: usize, lang: Lang) -> String {
    let p = P { s, src, lang, scopes: std::cell::RefCell::new(Vec::new()) };
    let mut out = String::new();
    let file = std::path::Path::new(&src.path).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| src.path.clone());
    let _ = writeln!(out, "#![fascon(1)]");
    // プロファイル（rtos / generic-os）は厳格形にも残す（同じ C に戻るように） / keep the profile in the strict form too
    match s.profile {
        crate::target::Profile::Rtos => {
            let _ = writeln!(out, "// fascon-target: rtos");
        }
        crate::target::Profile::GenericOs => {
            let _ = writeln!(out, "// fascon-target: generic-os");
        }
        // hpc: 計算ノードの形も残す（FASCON2_MAX_CPUS と F3903 が同じになる） / hpc: keep the compute node's shape too
        // (FASCON2_MAX_CPUS and F3903 stay the same)
        crate::target::Profile::Hpc => {
            let _ = writeln!(out, "// fascon-target: hpc");
            if let Some(t) = s.topology.filter(|t| t.source != 0) {
                let _ = writeln!(out, "// fascon-topology: {}", t.spec());
            }
        }
        crate::target::Profile::Classic => {}
    }
    let _ = writeln!(
        out,
        "// {} — {}",
        file,
        p.c("機能安全証明形態の厳格な Fascon（fascon --emit-strict-fascon が生成）", "strict Fascon for functional-safety review (generated by fascon --emit-strict-fascon)")
    );
    for (ja, en) in [
        ("すべての let に型、すべての整数リテラルに型の接尾辞（推論の結果を書き出したもの）", "every let has its type and every integer literal its type suffix (the inference results written out)"),
        ("R.get(i).unwrap() / R.get_mut(i).unwrap() = 世代タグで検証するルックアップ（短い形 R[i] と同じ式。古い Id は次のコミット点で止まる）", "R.get(i).unwrap() / R.get_mut(i).unwrap() = the generation-verified lookup (the same expression as the short form R[i]; a stale Id stops at the next commit point)"),
        ("p.read() / p.write(v) / p.read_at(i) / p.write_at(i, v) = I/O ポート（x86_64: in / out、AArch64: コンパイラが MMIO へ写す）", "p.read() / p.write(v) / p.read_at(i) / p.write_at(i, v) = I/O ports (x86_64: in / out; AArch64: remapped to MMIO by the compiler)"),
        ("型の別名は展開済み、配列の長さは数で書く。`@` は書かない。宣言の順序は自由（2 パス解析）", "type aliases are expanded and array lengths are numbers; no `@`; declarations may come in any order (two-pass analysis)"),
        ("このファイルをもう一度コンパイルすると、元のソースと同じ C になる", "compiling this file again gives the same C as the original source"),
    ] {
        let _ = writeln!(out, "//   - {}", p.c(ja, en));
    }
    if reframed {
        let _ = writeln!(
            out,
            "// {}",
            p.c(
                &format!("自動リフレーミング（Rust の見た目 → Fascon）: {} 箇所を書き換え、文字リテラル・ライフタイム {} 個を整理", rep.lines.len(), marks),
                &format!("automatic reframing (Rust look → Fascon): {} rewrites, {} char literals / lifetimes reframed", rep.lines.len(), marks)
            )
        );
        let mut lines: Vec<&(crate::span::Span, String, String)> = rep.lines.iter().collect();
        lines.sort_by_key(|x| x.0.lo);
        for (sp, ja, en) in lines.into_iter().take(200) {
            let line = src.line_of(sp.lo) + 1;
            let _ = writeln!(out, "//   {} {}: {}", p.c("行", "line"), line, p.c(ja, en));
        }
    }
    out.push('\n');
    p.items(&mut out);
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}
