//! 名前解決（2 周目の前半）: `@` の無い名前を、ローカル変数のスコープを見ながらリージョン参照へ置き換える。
//! name resolution (first half of pass 2): bare names become region references, looking at the scopes of locals.
//!
//! `pcb_table[id].id = 100;` の `pcb_table` は、同じ名前のローカル変数がスコープに無く、1 周目の表でリージョン（アリーナ・
//! バンプアリーナ・MMIO・ポート）なら、そのリージョンを指す（`ExprKind::ArenaRef`）。以降の意味解析・流れ解析・C 生成は
//! 以前の `@pcb_table` と同じ道を通るので、世代タグによる検査・静的な検査の省略・走査の速さはそのまま。
//! 古い書き方の `@名前` は、同じ名前のローカルがあっても必ずリージョンを指す明示の印として残る（書かなくてよい）。
//! In `pcb_table[id].id = 100;`, `pcb_table` denotes the region when no local of that name is in scope and the pass-1
//! table says it is a region (arena, bump arena, MMIO or port); it becomes `ExprKind::ArenaRef`. Semantic analysis, flow
//! analysis and C generation then take the same path as the old `@pcb_table`, so the generation-tag checks, the static
//! elision of checks and the scan speed are unchanged. The legacy `@name` stays as an explicit marker that always
//! denotes the region, even when a local has the same name (it never has to be written).

use crate::ast::*;
use crate::prescan::SymTab;
use std::collections::HashSet;

struct R<'a> {
    syms: &'a SymTab,
    scopes: Vec<HashSet<String>>,
    /// 新しい式の番号（脱糖衣で作る式用） / the next expression id (for expressions made by desugaring)
    next_expr: u32,
    /// `R.get(i).unwrap()`（読み出し専用の形）から作った `R[i]` / `R[i]` made from the read-only form `R.get(i).unwrap()`
    shared: HashSet<ExprId>,
}

impl<'a> R<'a> {
    fn bound(&self, n: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.contains(n))
    }
    fn declare(&mut self, n: &str) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(n.to_string());
        }
    }
    fn block(&mut self, b: &mut Block) {
        self.scopes.push(HashSet::new());
        for st in &mut b.stmts {
            match st {
                Stmt::Let(l) => {
                    if let Some(i) = &mut l.init {
                        self.expr(i);
                    }
                    let n = l.name.clone();
                    self.declare(&n);
                }
                Stmt::Expr(e) => self.expr(e),
            }
        }
        if let Some(t) = &mut b.tail {
            self.expr(t);
        }
        self.scopes.pop();
    }
    fn pat_names(p: &Pattern) -> Vec<String> {
        match p {
            Pattern::Some(Some((n, _, _)), _) | Pattern::Bind(n, _, _) | Pattern::Ok(Some((n, _, _)), _) | Pattern::Err(Some((n, _, _)), _) => vec![n.clone()],
            _ => Vec::new(),
        }
    }
    fn arm_block(&mut self, pat: &Pattern, b: &mut Block) {
        self.scopes.push(HashSet::new());
        for n in Self::pat_names(pat) {
            self.declare(&n);
        }
        self.block(b);
        self.scopes.pop();
    }
    fn expr(&mut self, e: &mut Expr) {
        // 名前だけの式: ローカルが優先、無ければリージョン / a bare name: locals first, then regions
        if let ExprKind::Path(n) = &e.kind {
            if !self.bound(n) && self.syms.is_region(n) {
                e.kind = ExprKind::ArenaRef(n.clone());
            }
            return;
        }
        match &mut e.kind {
            ExprKind::Int(..) | ExprKind::Float(..) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Unit | ExprKind::Path(_) | ExprKind::ArenaRef(_) | ExprKind::Break | ExprKind::Continue | ExprKind::Unsupported(_) | ExprKind::OwnAlloc(_) => {}
            ExprKind::Call { args, .. } => args.iter_mut().for_each(|a| self.expr(a)),
            ExprKind::CallIndirect { callee, args } => {
                self.expr(callee);
                args.iter_mut().for_each(|a| self.expr(a));
            }
            ExprKind::MethodCall { recv, args, .. } => {
                self.expr(recv);
                args.iter_mut().for_each(|a| self.expr(a));
            }
            ExprKind::Field(b, _, _) => self.expr(b),
            ExprKind::Index(b, i) => {
                self.expr(b);
                self.expr(i);
            }
            ExprKind::StructLit { fields, .. } => fields.iter_mut().for_each(|(_, _, x)| self.expr(x)),
            ExprKind::ArrayLit(xs) => xs.iter_mut().for_each(|x| self.expr(x)),
            ExprKind::ArrayRepeat(x, n) => {
                self.expr(x);
                self.expr(n);
            }
            ExprKind::Own(x) | ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => self.expr(x),
            ExprKind::Ref { inner, .. } => self.expr(inner),
            ExprKind::Binary(_, a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Assign { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                if let Some(x) = els {
                    self.expr(x);
                }
            }
            ExprKind::IfLet { pat, scrut, then, els } => {
                self.expr(scrut);
                let p = pat.clone();
                self.arm_block(&p, then);
                if let Some(x) = els {
                    self.expr(x);
                }
            }
            ExprKind::Match { scrut, arms } => {
                self.expr(scrut);
                for a in arms.iter_mut() {
                    self.scopes.push(HashSet::new());
                    for n in Self::pat_names(&a.pat) {
                        self.declare(&n);
                    }
                    self.expr(&mut a.body);
                    self.scopes.pop();
                }
            }
            ExprKind::While { cond, body } => {
                self.expr(cond);
                self.block(body);
            }
            ExprKind::Loop { body } => self.block(body),
            ExprKind::For { var, iter, body, .. } => {
                if let ForIter::Range { lo, hi, .. } = iter {
                    self.expr(lo);
                    self.expr(hi);
                }
                // `for id in 名前` の名前はいつもリージョン（ローカルを回す構文は無い） / the name in `for id in name` is
                // always a region (there is no syntax for iterating a local)
                self.scopes.push(HashSet::new());
                let v = var.clone();
                self.declare(&v);
                self.block(body);
                self.scopes.pop();
            }
            ExprKind::Return(x) => {
                if let Some(x) = x {
                    self.expr(x);
                }
            }
            ExprKind::Block(b) | ExprKind::VolatileBypass(b) | ExprKind::Critical(b) | ExprKind::Atomic(b) | ExprKind::RcuRead(b) | ExprKind::CatchFault(b) => self.block(b),
            ExprKind::TypeCall { args, .. } => args.iter_mut().for_each(|a| self.expr(a)),
        }
        self.desugar(e);
    }

    /// 厳格形（`--emit-strict-fascon` が書き出す形）を、人間が書く短い形と同じ式に戻す:
    ///   `R.get_mut(i).unwrap()` → `R[i]`（世代タグで検証するルックアップ。書き込み・&mut に使える）
    ///   `R.get(i).unwrap()`     → `R[i]`（読み出し専用。書き込むと F0596）
    ///   `port.read()` / `port.write(v)` / `port.read_at(i)` / `port.write_at(i, v)` → `port` / `port = v` / `port[i]` / `port[i] = v`
    /// map the strict forms (what `--emit-strict-fascon` writes) back to the same expressions as the short human forms
    fn desugar(&mut self, e: &mut Expr) {
        let ExprKind::MethodCall { recv, method, args, .. } = &mut e.kind else { return };
        // R.get(i).unwrap() / R.get_mut(i).unwrap()
        if method == "unwrap" && args.is_empty() {
            if let ExprKind::MethodCall { recv: r2, method: m2, args: a2, .. } = &mut recv.kind {
                if let ExprKind::ArenaRef(n) = &r2.kind {
                    if (m2 == "get" || m2 == "get_mut") && a2.len() == 1 && self.syms.has_ids(n) {
                        let shared = m2 == "get";
                        let base = std::mem::replace(&mut **r2, Expr { id: u32::MAX, kind: ExprKind::Unit, span: e.span });
                        let idx = a2.pop().unwrap();
                        e.kind = ExprKind::Index(Box::new(base), Box::new(idx));
                        if shared {
                            self.shared.insert(e.id);
                        }
                        return;
                    }
                }
            }
        }
        // ポートの厳格形 / the strict port forms
        let port = match &recv.kind {
            ExprKind::ArenaRef(n) if self.syms.kind(n) == Some(crate::prescan::SymKind::Port) => true,
            _ => false,
        };
        if !port {
            return;
        }
        let m = method.clone();
        let sp = e.span;
        match (m.as_str(), args.len()) {
            ("read", 0) => {
                let r = std::mem::replace(&mut **recv, Expr { id: u32::MAX, kind: ExprKind::Unit, span: sp });
                *e = Expr { id: e.id, kind: r.kind, span: sp };
                // 元の受け手の番号は捨て、この式の番号で読む / read under this expression's id
            }
            ("write", 1) => {
                let v = args.pop().unwrap();
                let r = std::mem::replace(&mut **recv, Expr { id: u32::MAX, kind: ExprKind::Unit, span: sp });
                e.kind = ExprKind::Assign { op: None, lhs: Box::new(r), rhs: Box::new(v) };
            }
            ("read_at", 1) => {
                let i = args.pop().unwrap();
                let r = std::mem::replace(&mut **recv, Expr { id: u32::MAX, kind: ExprKind::Unit, span: sp });
                e.kind = ExprKind::Index(Box::new(r), Box::new(i));
            }
            ("write_at", 2) => {
                let v = args.pop().unwrap();
                let i = args.pop().unwrap();
                let r = std::mem::replace(&mut **recv, Expr { id: u32::MAX, kind: ExprKind::Unit, span: sp });
                let id = self.next_expr;
                self.next_expr += 1;
                let lhs = Expr { id, kind: ExprKind::Index(Box::new(r), Box::new(i)), span: sp };
                e.kind = ExprKind::Assign { op: None, lhs: Box::new(lhs), rhs: Box::new(v) };
            }
            _ => {}
        }
    }
}

/// プログラム全体の名前解決。戻り値は読み出し専用の形（`R.get(i).unwrap()`）から作った場所の式
/// name resolution for the whole program; returns the place expressions made from the read-only form `R.get(i).unwrap()`
pub fn resolve(prog: &mut Program, syms: &SymTab) -> HashSet<ExprId> {
    let mut r = R { syms, scopes: Vec::new(), next_expr: prog.n_exprs, shared: HashSet::new() };
    for it in prog.items.iter_mut() {
        if let Item::Fn(f) = it {
            r.scopes.clear();
            let mut top = HashSet::new();
            for p in &f.params {
                top.insert(p.name.clone());
            }
            r.scopes.push(top);
            r.block(&mut f.body);
            r.scopes.clear();
        }
    }
    prog.n_exprs = r.next_expr;
    r.shared
}
