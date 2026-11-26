//! コールバックの参照のエスケープ解析（F0520） / escape analysis of references given to callbacks (F0520)
//!
//! 高階関数 `for_each(a: &mut [i32; 4], f: fn(&mut i32))` は `f(&mut a[i])` のように、自分が借りている場所への第二級参照を
//! コールバックへ 1 回の呼び出しの間だけ貸す。コールバック（参照の引数を持つ関数で、値として使われたもの）は、その参照を
//! 自分のスタックフレームの中でだけ評価・消費しなければならない（stack_only）: 参照を通して読み書きし、参照引数として
//! さらに渡すのはよいが、呼び出しより長く生きる場所（アリーナ・static・MMIO・呼び出し元の場所・own の所有リージョン・
//! 値渡しの引数）へ代入・格納したり、戻り値として返したりしてはならない。
//! A higher-order function `for_each(a: &mut [i32; 4], f: fn(&mut i32))` lends a second-class reference to a place it has
//! borrowed, as in `f(&mut a[i])`, to the callback for one call. A callback (a function with reference parameters that is
//! used as a value) must evaluate and consume that reference inside its own stack frame (stack_only): it may read and
//! write through it and pass it on as a reference argument, but it must not assign or store it into anything that
//! outlives the call (an arena, a static, MMIO, the caller's places, an own region, a by-value parameter) or return it.
//!
//! 解析: 関数ごと・参照引数ごとに「どこへ逃げうるか」の要約を、呼び出しグラフ上の最小不動点として求める（参照から作った
//! 参照・参照を包んだ値（Some・構造体・配列・own）・参照を束縛したローカルを流れとして追う）。コールバックとして使われた
//! 関数の参照引数が逃げる道があれば、その道（根本原因のタイムライン）を付けて F0520 を出す。同じ場所の F0515 / F0308 は
//! F0520 にまとめる。
//! Analysis: per function and per reference parameter, a summary of where it can escape, computed as a least fixpoint
//! over the call graph (following references made from references, values wrapping a reference (Some, structs, arrays,
//! own) and locals bound to one). When a reference parameter of a function used as a callback has a way out, F0520 is
//! reported with that path (the root-cause timeline); F0515 / F0308 reported at the same place are folded into it.

use crate::ast::*;
use crate::diag::Diag;
use crate::sema::{CallKind, LocalKind, MethodKind, Res, Sema};
use crate::span::Span;
use crate::types::Ty;
use std::collections::HashMap;

/// タイムラインの 1 段 / one step of the timeline
#[derive(Clone, Debug)]
pub struct Step {
    pub span: Span,
    pub ja: String,
    pub en: String,
}

fn step(span: Span, ja: impl Into<String>, en: impl Into<String>) -> Step {
    Step { span, ja: ja.into(), en: en.into() }
}

/// 解析の結果 / the result of the analysis
#[derive(Default)]
pub struct EscapeResults {
    pub diags: Vec<Diag>,
    /// F0520 にまとめた場所（ここに出た F0515 / F0308 は消す） / spans folded into F0520 (F0515 / F0308 there are removed)
    pub folded: Vec<Span>,
}

impl EscapeResults {
    /// この診断は F0520 にまとめられたか / is this diagnostic folded into an F0520?
    pub fn is_folded(&self, d: &Diag) -> bool {
        matches!(d.code, "F0515" | "F0308") && self.folded.iter().any(|s| s.lo <= d.span.lo && d.span.hi <= s.hi)
    }
}

/// 場所の根 / the root of a place
enum Root {
    /// このフレームが持つローカル / a local owned by this frame
    Frame(BindId),
    /// 呼び出しより長く生きる場所（説明付き） / something that outlives the call (with a description)
    Outer(String, String),
    None,
}

struct Fx<'a, 'p> {
    s: &'a Sema<'p>,
    /// 解析中の関数 / the function being analyzed
    fi: usize,
    /// 参照を運ぶローカル → (引数の番号, ここまでの道) / locals carrying a reference → (parameter index, path so far)
    taint: HashMap<BindId, (usize, Vec<Step>)>,
    /// 引数ごとに最初に見つかった逃げ道 / the first way out found per parameter
    found: HashMap<usize, Vec<Step>>,
    /// 他の関数の要約 / summaries of other functions
    sums: &'a HashMap<(usize, usize), Vec<Step>>,
    changed: bool,
}

impl<'a, 'p> Fx<'a, 'p> {
    fn ty(&self, e: &Expr) -> Ty {
        self.s.expr_ty.get(e.id as usize).cloned().unwrap_or(Ty::Err)
    }
    fn local(&self, b: BindId) -> Option<&crate::sema::LocalInfo> {
        self.s.locals.get(b as usize).and_then(|x| x.as_ref())
    }
    fn lname(&self, b: BindId) -> String {
        self.local(b).map(|l| l.name.clone()).unwrap_or_default()
    }
    fn snippet(&self, e: &Expr) -> String {
        // 式の短い説明（名前・フィールドの鎖） / a short description of an expression (names and field chains)
        match &e.kind {
            ExprKind::Path(n) => n.clone(),
            ExprKind::ArenaRef(n) => n.clone(),
            ExprKind::Field(b, n, _) => format!("{}.{}", self.snippet(b), n),
            ExprKind::Index(b, _) => format!("{}[..]", self.snippet(b)),
            _ => "..".to_string(),
        }
    }

    fn mark(&mut self, b: BindId, origin: (usize, Vec<Step>)) {
        if !self.taint.contains_key(&b) {
            self.taint.insert(b, origin);
            self.changed = true;
        }
    }

    fn escape(&mut self, origin: (usize, Vec<Step>), last: Step) {
        let (i, mut path) = origin;
        if self.found.contains_key(&i) {
            return;
        }
        path.push(last);
        self.found.insert(i, path);
        self.changed = true;
    }

    /// 場所の根 / the root of a place expression
    fn root(&self, e: &Expr) -> Root {
        match &e.kind {
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Local(b)) => match self.local(*b) {
                    // 参照の束縛（引数・ルックアップ・場所の束縛）を通した先は呼び出し元の場所 / through a reference binding
                    // lies the caller's place
                    Some(l) if matches!(l.ty, Ty::Ref(..)) => Root::Outer(
                        format!("参照 `{}` の先（呼び出し元の場所）", l.name),
                        format!("the place behind the reference `{}` (the caller's)", l.name),
                    ),
                    Some(_) => Root::Frame(*b),
                    None => Root::None,
                },
                Some(Res::Static(si)) => {
                    let n = self.s.t.statics[*si].name.clone();
                    Root::Outer(format!("static `{}`（プログラムの最後まで生きる）", n), format!("static `{}` (lives until the program ends)", n))
                }
                _ => Root::None,
            },
            ExprKind::Field(b, _, _) => self.root(b),
            ExprKind::Index(b, _) => {
                if let Some(&a) = self.s.arena_index.get(&e.id) {
                    let n = self.s.t.arenas[a].name.clone();
                    return Root::Outer(format!("アリーナ `{}` のスロット（解放されるまで生きる）", n), format!("a slot of the arena `{}` (lives until freed)", n));
                }
                if let Some(&(m, _)) = self.s.mmio_index.get(&e.id) {
                    let n = self.s.t.mmios[m].name.clone();
                    return Root::Outer(format!("MMIO リージョン `{}`", n), format!("the MMIO region `{}`", n));
                }
                if let Some(&p) = self.s.port_index.get(&e.id) {
                    let n = self.s.t.ports[p].name.clone();
                    return Root::Outer(format!("ポート `{}`", n), format!("the port `{}`", n));
                }
                self.root(b)
            }
            ExprKind::ArenaRef(n) => Root::Outer(format!("リージョン `{}`", n), format!("the region `{}`", n)),
            _ => Root::None,
        }
    }

    /// 場所の根のローカル（参照を作る `&x.f` の x） / the root local of a place (the x of `&x.f`)
    fn root_local(&self, e: &Expr) -> Option<BindId> {
        match &e.kind {
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Local(b)) => Some(*b),
                _ => None,
            },
            ExprKind::Field(b, _, _) => self.root_local(b),
            ExprKind::Index(b, _) => {
                if self.s.arena_index.contains_key(&e.id) || self.s.mmio_index.contains_key(&e.id) || self.s.port_index.contains_key(&e.id) {
                    None
                } else {
                    self.root_local(b)
                }
            }
            _ => None,
        }
    }

    /// 式の値が引数の参照（から作ったもの）を運ぶか / does the value of an expression carry a reference (made) from a
    /// parameter?
    fn carries(&self, e: &Expr) -> Option<(usize, Vec<Step>)> {
        match &e.kind {
            ExprKind::Path(_) => {
                // コピーできる値への参照は値の位置では中身を読むだけ（`*p`） / a reference to a copyable value is read
                // through in value positions (`*p`)
                if self.s.auto_deref.contains(&e.id) || self.s.own_derefs.contains(&e.id) {
                    return None;
                }
                match self.s.res.get(&e.id) {
                    Some(Res::Local(b)) => self.taint.get(b).cloned(),
                    _ => None,
                }
            }
            ExprKind::Ref { inner, mutable } => {
                let b = self.root_local(inner)?;
                let (i, mut p) = self.taint.get(&b).cloned()?;
                if !matches!(inner.kind, ExprKind::Path(_)) {
                    let t = format!("&{}{}", if *mutable { "mut " } else { "" }, self.snippet(inner));
                    p.push(step(e.span, format!("`{}` はその参照の一部を指す参照", t), format!("`{}` is a reference into part of it", t)));
                }
                Some((i, p))
            }
            ExprKind::Call { callee, args, .. } if callee == "Some" && matches!(self.s.calls.get(&e.id), Some(CallKind::Some)) => {
                let (i, mut p) = self.carries(args.first()?)?;
                p.push(step(e.span, "`Some(..)` に包まれる", "wrapped in `Some(..)`"));
                Some((i, p))
            }
            ExprKind::StructLit { name, fields, .. } => {
                for (f, _, v) in fields {
                    if let Some((i, mut p)) = self.carries(v) {
                        p.push(step(e.span, format!("`{} {{ .. }}` のフィールド `{}` に入る", name, f), format!("put into the field `{}` of `{} {{ .. }}`", f, name)));
                        return Some((i, p));
                    }
                }
                None
            }
            ExprKind::ArrayLit(xs) => {
                for x in xs {
                    if let Some((i, mut p)) = self.carries(x) {
                        p.push(step(e.span, "配列に入る", "put into an array"));
                        return Some((i, p));
                    }
                }
                None
            }
            ExprKind::ArrayRepeat(x, _) => {
                let (i, mut p) = self.carries(x)?;
                p.push(step(e.span, "配列に入る", "put into an array"));
                Some((i, p))
            }
            ExprKind::Own(x) => {
                let (i, mut p) = self.carries(x)?;
                p.push(step(e.span, "`own` の所有リージョンに入る", "put into an `own` region"));
                Some((i, p))
            }
            ExprKind::Block(b) | ExprKind::Critical(b) => b.tail.as_ref().and_then(|t| self.carries(t)),
            ExprKind::If { then, els, .. } | ExprKind::IfLet { then, els, .. } => then.tail.as_ref().and_then(|t| self.carries(t)).or_else(|| els.as_ref().and_then(|x| self.carries(x))),
            ExprKind::Match { arms, .. } => arms.iter().find_map(|a| self.carries(&a.body)),
            ExprKind::MethodCall { recv, .. } => match self.s.methods.get(&e.id) {
                Some(MethodKind::Unwrap) | Some(MethodKind::UnwrapOr) | Some(MethodKind::Take) => {
                    // 取り出した値が参照を含まなければ、それは所有された値（`x.take()` で取り外した own の子など）
                    // a value taken out that holds no reference is an owned value (an own child detached with `x.take()`
                    // ...)
                    let t = self.ty(e);
                    if !t.holds_ref() && !t.is_err() {
                        return None;
                    }
                    let (i, mut p) = self.carries(recv)?;
                    p.push(step(e.span, "取り出される", "taken out"));
                    Some((i, p))
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn block(&mut self, b: &Block, tail_returns: bool) {
        for st in &b.stmts {
            match st {
                Stmt::Let(l) => {
                    if let Some(i) = &l.init {
                        self.expr(i);
                        if let Some((k, mut p)) = self.carries(i) {
                            p.push(step(l.name_span, format!("ローカル `{}` に束縛される", l.name), format!("bound to the local `{}`", l.name)));
                            self.mark(l.bind, (k, p));
                        }
                    }
                }
                Stmt::Expr(e) => self.expr(e),
            }
        }
        if let Some(t) = &b.tail {
            self.expr(t);
            if tail_returns {
                self.ret_sink(t);
            }
        }
    }

    /// 戻り値として外へ出る / leaving as the return value
    fn ret_sink(&mut self, v: &Expr) {
        if let Some(o) = self.carries(v) {
            let fname = self.s.t.fns[self.fi].name.clone();
            self.escape(
                o,
                step(v.span, format!("`{}` の戻り値として返される（戻り値は呼び出しより長く生きる）", fname), format!("returned from `{}` (a return value outlives the call)", fname)),
            );
        }
    }

    fn arm_binding(&mut self, pat: &Pattern, scrut: &Expr) {
        let b = match pat {
            Pattern::Some(Some((_, b, _)), _) | Pattern::Bind(_, b, _) | Pattern::Ok(Some((_, b, _)), _) => *b,
            _ => return,
        };
        let name = self.lname(b);
        let by_ref = self.local(b).map_or(false, |l| l.kind == LocalKind::PatRef);
        if by_ref {
            // 場所の束縛は、その場所の根が参照なら、参照から作った参照 / a binding of a place rooted at a reference is a
            // reference made from it
            if let Some(r) = self.root_local(scrut) {
                if let Some((i, mut p)) = self.taint.get(&r).cloned() {
                    p.push(step(pat.span(), format!("`{}` はその参照の先を束縛する参照", name), format!("`{}` binds a reference into it", name)));
                    self.mark(b, (i, p));
                }
            }
        } else if let Some((i, mut p)) = self.carries(scrut) {
            p.push(step(pat.span(), format!("`{}` に取り出される", name), format!("taken out into `{}`", name)));
            self.mark(b, (i, p));
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Return(Some(v)) => {
                self.expr(v);
                self.ret_sink(v);
            }
            ExprKind::Assign { lhs, rhs, .. } => {
                self.expr(rhs);
                if let ExprKind::Index(_, i) = &lhs.kind {
                    self.expr(i);
                }
                if let Some(o) = self.carries(rhs) {
                    match self.root(lhs) {
                        Root::Outer(ja, en) => {
                            let t = self.snippet(lhs);
                            self.escape(o, step(e.span, format!("`{}` に代入される: {}は呼び出しより長く生きる", t, ja), format!("assigned to `{}`: {} outlives the call", t, en)));
                        }
                        Root::Frame(b) => {
                            let (k, mut p) = o;
                            let n = self.lname(b);
                            p.push(step(e.span, format!("ローカル `{}` に入る", n), format!("stored into the local `{}`", n)));
                            self.mark(b, (k, p));
                        }
                        Root::None => {}
                    }
                }
            }
            ExprKind::Own(x) => {
                self.expr(x);
                if let Some((i, mut p)) = self.carries(x) {
                    let last = step(e.span, "`own` の所有リージョンに入れられる（own は移動・返却・格納できるので呼び出しより長く生きうる）", "boxed into an `own` region (an own can be moved, returned or stored, so it can outlive the call)");
                    // 包んだ段（carries が足した最後の段）は own そのものなので重ねない / the wrapping step carries added is the
                    // own itself, so it is not repeated
                    if p.last().map_or(false, |x| x.span == e.span) {
                        p.pop();
                    }
                    self.escape((i, p), last);
                }
            }
            ExprKind::Call { args, .. } | ExprKind::CallIndirect { args, .. } => {
                if let ExprKind::CallIndirect { callee, .. } = &e.kind {
                    self.expr(callee);
                }
                for a in args {
                    self.expr(a);
                }
                match self.s.calls.get(&e.id).copied() {
                    Some(CallKind::User(g)) => {
                        let gname = self.s.t.fns[g].name.clone();
                        let params: Vec<Ty> = self.s.t.fns[g].params.iter().map(|p| p.ty.clone()).collect();
                        for (k, a) in args.iter().enumerate() {
                            let Some(o) = self.carries(a) else { continue };
                            match params.get(k) {
                                Some(Ty::Ref(..)) => {
                                    if let Some(sp) = self.sums.get(&(g, k)).cloned() {
                                        let pn = self.s.t.fns[g].params[k].name.clone();
                                        let (i, mut p) = o;
                                        p.push(step(a.span, format!("`{}` の参照引数 `{}` として渡される", gname, pn), format!("passed to `{}` as its reference parameter `{}`", gname, pn)));
                                        if let Some((last, mid)) = sp.split_last() {
                                            p.extend(mid.iter().cloned());
                                            self.escape((i, p), last.clone());
                                        }
                                    }
                                }
                                _ => {
                                    let pn = self.s.t.fns[g].params.get(k).map(|p| p.name.clone()).unwrap_or_default();
                                    self.escape(
                                        o,
                                        step(
                                            a.span,
                                            format!("`{}` の値渡しの引数 `{}` へ移される（呼び出された関数が所有し、保存しうる）", gname, pn),
                                            format!("moved into `{}`'s by-value parameter `{}` (the callee owns it and may keep it)", gname, pn),
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    Some(CallKind::Indirect) => {
                        // 関数ポインタの参照引数は契約（F0520）で守られる。値渡しは行き先が分からない
                        // reference parameters of a function pointer are covered by the contract (F0520); a by-value
                        // argument goes somewhere unknown
                        let ct = match &e.kind {
                            ExprKind::CallIndirect { callee, .. } => self.ty(callee),
                            _ => match self.s.call_targets.get(&e.id) {
                                Some(Res::Local(b)) => self.local(*b).map(|l| l.ty.clone()).unwrap_or(Ty::Err),
                                Some(Res::Static(si)) => self.s.t.statics[*si].ty.clone(),
                                _ => Ty::Err,
                            },
                        };
                        let params = match ct {
                            Ty::Fn(ps, _) => ps,
                            _ => Vec::new(),
                        };
                        for (k, a) in args.iter().enumerate() {
                            if matches!(params.get(k), Some(Ty::Ref(..)) | None) {
                                continue;
                            }
                            if let Some(o) = self.carries(a) {
                                self.escape(o, step(a.span, "関数ポインタの値渡しの引数へ移される（行き先が保存しうる）", "moved into a by-value parameter of a function pointer (the callee may keep it)"));
                            }
                        }
                    }
                    _ => {}
                }
            }
            ExprKind::MethodCall { recv, args, .. } => {
                self.expr(recv);
                for a in args {
                    self.expr(a);
                }
                if let Some(MethodKind::ArenaAlloc(ar)) = self.s.methods.get(&e.id) {
                    let an = self.s.t.arenas[*ar].name.clone();
                    for a in args {
                        if let Some(o) = self.carries(a) {
                            self.escape(
                                o,
                                step(e.span, format!("アリーナ `{}` に格納される（スロットは解放されるまで、呼び出しより長く生きる）", an), format!("stored into the arena `{}` (its slot lives until freed, longer than the call)", an)),
                            );
                        }
                    }
                }
            }
            ExprKind::IfLet { pat, scrut, then, els } => {
                self.expr(scrut);
                self.arm_binding(pat, scrut);
                self.block(then, false);
                if let Some(x) = els {
                    self.expr(x);
                }
            }
            ExprKind::Match { scrut, arms } => {
                self.expr(scrut);
                for a in arms {
                    self.arm_binding(&a.pat, scrut);
                    self.expr(&a.body);
                }
            }
            ExprKind::If { cond, then, els } => {
                self.expr(cond);
                self.block(then, false);
                if let Some(x) = els {
                    self.expr(x);
                }
            }
            ExprKind::While { cond, body } => {
                self.expr(cond);
                self.block(body, false);
            }
            ExprKind::Loop { body } | ExprKind::Block(body) | ExprKind::VolatileBypass(body) | ExprKind::Critical(body) => self.block(body, false),
            ExprKind::For { iter, body, .. } => {
                if let ForIter::Range { lo, hi, .. } = iter {
                    self.expr(lo);
                    self.expr(hi);
                }
                self.block(body, false);
            }
            _ => {
                for c in crate::flow::children(e) {
                    match c {
                        crate::flow::Child::E(x) => self.expr(x),
                        crate::flow::Child::B(b) => self.block(b, false),
                    }
                }
            }
        }
    }
}

/// 関数 fi の参照引数ごとの逃げ道（1 回の解析） / the ways out per reference parameter of function fi (one pass)
fn analyze_fn(s: &Sema, fi: usize, sums: &HashMap<(usize, usize), Vec<Step>>) -> HashMap<usize, Vec<Step>> {
    let f = &s.t.fns[fi];
    let fd = match &s.prog.items[f.item] {
        Item::Fn(fd) => fd,
        _ => return HashMap::new(),
    };
    let mut fx = Fx { s, fi, taint: HashMap::new(), found: HashMap::new(), sums, changed: false };
    for (i, p) in f.params.iter().enumerate() {
        if let Ty::Ref(..) = p.ty {
            fx.taint.insert(p.bind, (i, Vec::new()));
        }
    }
    if fx.taint.is_empty() {
        return HashMap::new();
    }
    let returns = !matches!(f.ret, Ty::Unit | Ty::Never);
    // 束縛の伝わり方が落ち着くまで繰り返す（ループの中で後ろの文から前の文へ流れる場合） / repeat until the taint settles
    // (flows from later statements back to earlier ones inside loops)
    for _ in 0..8 {
        fx.changed = false;
        fx.block(&fd.body, returns);
        if !fx.changed {
            break;
        }
    }
    fx.found
}

/// 関数ポインタとして使われた場所の説明（タイムラインの最初の段） / a description of where a function became a function
/// pointer (the first step of the timeline)
fn use_site(s: &Sema, fi: usize) -> Option<Step> {
    let (_, eid, sp) = s.fn_value_sites.iter().find(|(f, _, _)| *f == fi)?;
    let name = s.t.fns[fi].name.clone();
    let ty = s.t.display(&crate::callback::fn_ptr_ty(s, fi));
    // どの高階関数のどの引数へ渡されたか / which higher-order function and parameter it was passed to
    let mut hof: Option<(String, String)> = None;
    for f in &s.t.fns {
        if hof.is_some() {
            break;
        }
        if let Item::Fn(fd) = &s.prog.items[f.item] {
            crate::sema::walk_block(&fd.body, &mut |e: &Expr| {
                if hof.is_some() {
                    return;
                }
                if let ExprKind::Call { args, .. } = &e.kind {
                    if let Some(CallKind::User(g)) = s.calls.get(&e.id) {
                        if let Some(k) = args.iter().position(|a| a.id == *eid) {
                            let pn = s.t.fns[*g].params.get(k).map(|p| p.name.clone()).unwrap_or_default();
                            hof = Some((s.t.fns[*g].name.clone(), pn));
                        }
                    }
                }
            });
        }
    }
    Some(match hof {
        Some((h, p)) => step(
            *sp,
            format!("`{}` は高階関数 `{}` の引数 `{}` へコールバック（`{}`）として渡される", name, h, p, ty),
            format!("`{}` is passed to the higher-order function `{}` as its callback `{}` (`{}`)", name, h, p, ty),
        ),
        None => step(*sp, format!("`{}` はここで関数ポインタ（`{}`）になる", name, ty), format!("`{}` becomes a function pointer (`{}`) here", name, ty)),
    })
}

/// プログラム全体のエスケープ解析 / escape analysis of the whole program
pub fn analyze(s: &Sema) -> EscapeResults {
    let n = s.t.fns.len();
    let mut sums: HashMap<(usize, usize), Vec<Step>> = HashMap::new();
    // 要約の最小不動点（逃げ道は増えるだけ） / least fixpoint of the summaries (ways out only get added)
    for _ in 0..(n + 2) {
        let mut changed = false;
        for fi in 0..n {
            let found = analyze_fn(s, fi, &sums);
            for (i, p) in found {
                if !sums.contains_key(&(fi, i)) {
                    sums.insert((fi, i), p);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut out = EscapeResults::default();
    // コールバック: 参照の引数を持ち、値として使われた関数 / callbacks: functions with reference parameters used as values
    let mut callbacks: Vec<usize> = s.fn_value_sites.iter().map(|(f, _, _)| *f).collect();
    callbacks.sort_unstable();
    callbacks.dedup();
    for fi in callbacks {
        let f = &s.t.fns[fi];
        for (i, p) in f.params.iter().enumerate() {
            let Ty::Ref(m, inner) = &p.ty else { continue };
            let Some(path) = sums.get(&(fi, i)).cloned() else { continue };
            let Some((last, mid)) = path.split_last() else { continue };
            let pty = format!("&{}{}", if *m { "mut " } else { "" }, s.t.display(inner));
            let mut d = Diag::error(
                "F0520",
                last.span,
                format!("参照のエスケープ制約違反: コールバック `{}` が受け取った参照 `{}: {}` を外へ逃がしています — {}", f.name, p.name, pty, last.ja),
                format!("reference escape constraint violated: the callback `{}` lets its reference `{}: {}` escape — {}", f.name, p.name, pty, last.en),
            );
            if let Some(u) = use_site(s, fi) {
                d = d.note(u.span, u.ja, u.en);
            }
            d = d.note(
                p.span,
                format!("`{}: {}` は第二級参照: 高階関数が 1 回の呼び出しの間だけ貸す（stack_only: このフレームの中で読み書きして消費する）", p.name, pty),
                format!("`{}: {}` is a second-class reference that the higher-order function lends for one call only (stack_only: read, write and consume it inside this frame)", p.name, pty),
            );
            for st in mid {
                d = d.note(st.span, st.ja.clone(), st.en.clone());
            }
            d = d.hint(
                format!(
                    "コールバックの中では参照を通して読み書きし、参照引数としてさらに渡すだけにしてください。値を残したいなら中身を写す（`*{n}` / `{n}.clone()`）か、相手を `Id`・`own` で持ちます",
                    n = p.name
                ),
                format!(
                    "inside a callback, only read and write through the reference and pass it on as a reference argument; to keep the data, copy the contents (`*{n}` / `{n}.clone()`) or hold the target by `Id` or `own`",
                    n = p.name
                ),
            );
            out.folded.push(last.span);
            for st in mid {
                out.folded.push(st.span);
            }
            out.diags.push(d);
        }
    }
    out
}
