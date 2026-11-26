//! 流れ解析: 移動・初期化・可変性・借用衝突・アリーナ効果・世代検査の静的消去・境界検査の静的消去
//! flow analysis: moves, initialization, mutability, borrow conflicts, arena effects,
//! static elision of generation checks and of bounds checks
//!
//! 参照は第二級（引数と if let / match の束縛だけ）なので、借用はすべてレキシカルに閉じる。
//! そのため「借用の有効範囲」は構文木の入れ子そのもので、ライフタイム変数を一切使わずに検査できる。
//! References are second-class (arguments and if-let / match bindings only), so every borrow is lexically
//! scoped: the extent of a borrow is the nesting of the syntax tree itself, checked without any lifetime variable.

use crate::ast::*;
use crate::diag::Diag;
use crate::sema::{CallKind, LocalKind, MethodKind, Res, Sema};
use crate::span::Span;
use crate::types::*;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct FlowResults {
    /// どこかで移動されるローカル（C では移動元を NULL / ゼロにする） / locals moved somewhere
    pub moved_locals: HashSet<BindId>,
    /// ローカルを移動する Path 式 / Path exprs that move a local
    pub move_exprs: HashSet<ExprId>,
    /// 生存が静的に証明された `@a[id]`（世代検査を省く） / `@a[id]` with statically proven liveness
    pub proven: HashSet<ExprId>,
    /// 範囲が静的に証明された添字（境界検査を省く） / indices statically proven in bounds
    pub bounds_elided: HashSet<ExprId>,
    /// 宣言時に初期値の無い（後で代入する）ローカル / locals declared without an initializer
    pub deferred_init: HashSet<BindId>,
    pub diags: Vec<Diag>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VS {
    Init,
    Uninit,
    Moved(Span),
    MaybeMoved(Span),
    MaybeUninit,
}

fn combine(a: VS, b: VS) -> VS {
    use VS::*;
    if a == b {
        return a;
    }
    match (a, b) {
        (Moved(s), Moved(_)) => Moved(s),
        (Moved(s), _) | (_, Moved(s)) | (MaybeMoved(s), _) | (_, MaybeMoved(s)) => MaybeMoved(s),
        _ => MaybeUninit,
    }
}

#[derive(Clone, PartialEq)]
struct State {
    live: bool,
    vars: HashMap<BindId, VS>,
    proven: HashSet<(usize, BindId)>,
}

impl State {
    fn dead() -> State {
        State { live: false, vars: HashMap::new(), proven: HashSet::new() }
    }
    fn join(&self, o: &State) -> State {
        if !self.live {
            return o.clone();
        }
        if !o.live {
            return self.clone();
        }
        let mut vars = self.vars.clone();
        for (k, v) in &o.vars {
            let nv = match vars.get(k) {
                Some(x) => combine(*x, *v),
                None => *v,
            };
            vars.insert(*k, nv);
        }
        let proven = self.proven.intersection(&o.proven).cloned().collect();
        State { live: true, vars, proven }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum PRoot {
    Local(BindId),
    Static(usize),
    Arena(usize),
    Mmio(usize),
    Port(usize),
}

#[derive(Clone, Debug, PartialEq)]
enum Proj {
    Field(usize),
    Index,
}

#[derive(Clone, Debug)]
struct Place {
    root: PRoot,
    proj: Vec<Proj>,
}

fn overlaps(a: &Place, b: &Place) -> bool {
    if a.root != b.root {
        return false;
    }
    if matches!(a.root, PRoot::Arena(_)) {
        // 異なる Id かどうかは静的に分からない / whether two Ids differ is not known statically
        return true;
    }
    let n = a.proj.len().min(b.proj.len());
    for i in 0..n {
        match (&a.proj[i], &b.proj[i]) {
            (Proj::Field(x), Proj::Field(y)) if x != y => return false,
            _ => {}
        }
    }
    true
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Acc {
    Read,
    Write,
    MutBorrow,
    SharedBorrow,
    Move,
}

#[derive(Clone)]
struct ActiveBorrow {
    place: Place,
    mutable: bool,
    span: Span,
    what_ja: String,
    what_en: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    /// 値が消費される（移動） / the value is consumed (moved)
    Move,
    /// 読むだけ・コピー / read or copied
    Read,
}

struct LoopCtx {
    breaks: Vec<State>,
    conts: Vec<State>,
}

pub struct Flow<'s, 'p> {
    s: &'s Sema<'p>,
    st: State,
    loops: Vec<LoopCtx>,
    active: Vec<ActiveBorrow>,
    ranges: HashMap<BindId, (i128, i128)>,
    pub r: FlowResults,
    proven_dec: HashMap<ExprId, bool>,
    errs: HashSet<(Span, &'static str)>,
    /// 報告済みの値の使用エラー（F0382 / F0381）の位置 / positions of reported use errors (F0382 / F0381)
    use_errs: HashSet<(u32, &'static str)>,
    /// submit_dma でデバイスへ貸し出した束縛（以後の使用は F0382 ではなく F3401） / bindings lent to a device by submit_dma
    /// (later use is F3401 rather than F0382)
    lent: HashMap<BindId, Span>,
    /// copy_from_user で消費した `user T` の束縛（以後の使用は F3101） / `user T` bindings consumed by copy_from_user (later use is F3101)
    user_consumed: HashMap<BindId, Span>,
}

/// 関数ごとの直接効果と呼び出し先を集め、推移閉包をとる
/// collect each function's direct effects and callees, then take the transitive closure
pub fn compute_effects(s: &mut Sema) {
    let prog = s.prog;
    let n = s.t.fns.len();
    let mut direct: Vec<Effects> = vec![Effects::default(); n];
    let mut callees: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (fi, f) in s.t.fns.iter().enumerate() {
        if let Item::Fn(fd) = &prog.items[f.item] {
            let mut w = EffWalk { s, eff: Effects::default(), callees: Vec::new() };
            w.block(&fd.body);
            direct[fi] = w.eff;
            callees[fi] = w.callees;
        }
    }
    let mut total = direct.clone();
    loop {
        let mut changed = false;
        for fi in 0..n {
            for &c in &callees[fi].clone() {
                let ce = total[c].clone();
                for a in ce.reads {
                    changed |= Effects::add(&mut total[fi].reads, a);
                }
                for a in ce.writes {
                    changed |= Effects::add(&mut total[fi].writes, a);
                }
                for a in ce.frees {
                    changed |= Effects::add(&mut total[fi].frees, a);
                }
                for a in ce.allocs {
                    changed |= Effects::add(&mut total[fi].allocs, a);
                }
            }
        }
        if !changed {
            break;
        }
    }
    for fi in 0..n {
        s.t.fns[fi].effects = total[fi].clone();
        s.t.fns[fi].callees = callees[fi].clone();
    }
}

struct EffWalk<'a, 'p> {
    s: &'a Sema<'p>,
    eff: Effects,
    callees: Vec<usize>,
}

impl<'a, 'p> EffWalk<'a, 'p> {
    fn block(&mut self, b: &Block) {
        for st in &b.stmts {
            match st {
                Stmt::Let(l) => {
                    if let Some(i) = &l.init {
                        self.expr(i, false);
                    }
                }
                Stmt::Expr(e) => self.expr(e, false),
            }
        }
        if let Some(t) = &b.tail {
            self.expr(t, false);
        }
    }
    fn expr(&mut self, e: &Expr, write: bool) {
        match &e.kind {
            ExprKind::Index(base, idx) => {
                if let Some(&a) = self.s.arena_index.get(&e.id) {
                    Effects::add(&mut self.eff.reads, a);
                    if write {
                        Effects::add(&mut self.eff.writes, a);
                    }
                } else {
                    self.expr(base, write);
                }
                self.expr(idx, false);
            }
            ExprKind::Field(b, _, _) => self.expr(b, write),
            ExprKind::Assign { lhs, rhs, .. } => {
                self.expr(lhs, true);
                self.expr(rhs, false);
            }
            ExprKind::Ref { mutable, inner } => self.expr(inner, *mutable),
            ExprKind::Call { args, .. } | ExprKind::CallIndirect { args, .. } => {
                match self.s.calls.get(&e.id) {
                    Some(CallKind::User(f)) => {
                        if !self.callees.contains(f) {
                            self.callees.push(*f);
                        }
                    }
                    // 関数ポインタを通した呼び出し: 呼ばれうる関数の効果をすべて足す（候補は callback.rs）
                    // a call through a function pointer: add the effects of every function it may reach (callback.rs)
                    Some(CallKind::Indirect) => {
                        for g in self.s.indirect_cands.get(&e.id).cloned().unwrap_or_default() {
                            if !self.callees.contains(&g) {
                                self.callees.push(g);
                            }
                        }
                    }
                    _ => {}
                }
                if let ExprKind::CallIndirect { callee, .. } = &e.kind {
                    self.expr(callee, false);
                }
                for a in args {
                    self.expr(a, false);
                }
            }
            ExprKind::MethodCall { recv, args, .. } => {
                match self.s.methods.get(&e.id) {
                    Some(MethodKind::ArenaGet(a)) => {
                        Effects::add(&mut self.eff.reads, *a);
                    }
                    Some(MethodKind::ArenaGetMut(a)) => {
                        Effects::add(&mut self.eff.reads, *a);
                        Effects::add(&mut self.eff.writes, *a);
                    }
                    Some(MethodKind::ArenaFree(a)) | Some(MethodKind::ArenaRemove(a)) => {
                        Effects::add(&mut self.eff.writes, *a);
                        Effects::add(&mut self.eff.frees, *a);
                    }
                    Some(MethodKind::ArenaAlloc(a)) => {
                        Effects::add(&mut self.eff.allocs, *a);
                    }
                    Some(MethodKind::DmaLend(a)) | Some(MethodKind::DmaReclaim(a)) | Some(MethodKind::GpuDispatch(a)) | Some(MethodKind::GpuReclaim(a)) | Some(MethodKind::GpuReclaimWait(a)) => {
                        Effects::add(&mut self.eff.writes, *a);
                    }
                    Some(MethodKind::ProcMapPage(a)) | Some(MethodKind::ProcSharePage(a)) | Some(MethodKind::ProcUnmapPage(a)) | Some(MethodKind::ProcOnCpu(a)) | Some(MethodKind::ProcOffCpu(a)) => {
                        Effects::add(&mut self.eff.writes, *a);
                    }
                    Some(MethodKind::Take) => self.expr(recv, true),
                    _ => self.expr(recv, false),
                }
                for a in args {
                    self.expr(a, false);
                }
            }
            _ => {
                for c in children(e) {
                    match c {
                        Child::E(x) => self.expr(x, false),
                        Child::B(b) => self.block(b),
                    }
                }
            }
        }
    }
}

/// 子の式とブロック / child expressions and blocks
pub enum Child<'a> {
    E(&'a Expr),
    B(&'a Block),
}

pub fn children(e: &Expr) -> Vec<Child<'_>> {
    let mut v = Vec::new();
    match &e.kind {
        ExprKind::Int(..) | ExprKind::Float(..) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Unit | ExprKind::Path(_) | ExprKind::ArenaRef(_) | ExprKind::Break | ExprKind::Continue | ExprKind::Unsupported(_) | ExprKind::OwnAlloc(_) => {}
        ExprKind::Call { args, .. } => args.iter().for_each(|a| v.push(Child::E(a))),
        ExprKind::CallIndirect { callee, args } => {
            v.push(Child::E(callee));
            args.iter().for_each(|a| v.push(Child::E(a)));
        }
        ExprKind::MethodCall { recv, args, .. } => {
            v.push(Child::E(recv));
            args.iter().for_each(|a| v.push(Child::E(a)));
        }
        ExprKind::Field(b, _, _) => v.push(Child::E(b)),
        ExprKind::Index(b, i) => {
            v.push(Child::E(b));
            v.push(Child::E(i));
        }
        ExprKind::StructLit { fields, .. } => fields.iter().for_each(|(_, _, x)| v.push(Child::E(x))),
        ExprKind::ArrayLit(xs) => xs.iter().for_each(|x| v.push(Child::E(x))),
        ExprKind::ArrayRepeat(x, n) => {
            v.push(Child::E(x));
            v.push(Child::E(n));
        }
        ExprKind::Own(x) | ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => v.push(Child::E(x)),
        ExprKind::Ref { inner, .. } => v.push(Child::E(inner)),
        ExprKind::Binary(_, a, b) => {
            v.push(Child::E(a));
            v.push(Child::E(b));
        }
        ExprKind::Assign { lhs, rhs, .. } => {
            v.push(Child::E(lhs));
            v.push(Child::E(rhs));
        }
        ExprKind::If { cond, then, els } => {
            v.push(Child::E(cond));
            v.push(Child::B(then));
            if let Some(x) = els {
                v.push(Child::E(x));
            }
        }
        ExprKind::IfLet { scrut, then, els, .. } => {
            v.push(Child::E(scrut));
            v.push(Child::B(then));
            if let Some(x) = els {
                v.push(Child::E(x));
            }
        }
        ExprKind::Match { scrut, arms } => {
            v.push(Child::E(scrut));
            arms.iter().for_each(|a| v.push(Child::E(&a.body)));
        }
        ExprKind::While { cond, body } => {
            v.push(Child::E(cond));
            v.push(Child::B(body));
        }
        ExprKind::Loop { body } => v.push(Child::B(body)),
        ExprKind::For { iter, body, .. } => {
            if let ForIter::Range { lo, hi, .. } = iter {
                v.push(Child::E(lo));
                v.push(Child::E(hi));
            }
            v.push(Child::B(body));
        }
        ExprKind::Return(x) => {
            if let Some(x) = x {
                v.push(Child::E(x));
            }
        }
        ExprKind::Block(b) | ExprKind::VolatileBypass(b) | ExprKind::Critical(b) | ExprKind::Atomic(b) | ExprKind::RcuRead(b) | ExprKind::CatchFault(b) => v.push(Child::B(b)),
        ExprKind::TypeCall { args, .. } => args.iter().for_each(|a| v.push(Child::E(a))),
    }
    v
}

impl<'s, 'p> Flow<'s, 'p> {
    pub fn new(s: &'s Sema<'p>) -> Flow<'s, 'p> {
        Flow {
            s,
            st: State { live: true, vars: HashMap::new(), proven: HashSet::new() },
            loops: Vec::new(),
            active: Vec::new(),
            ranges: HashMap::new(),
            r: FlowResults::default(),
            proven_dec: HashMap::new(),
            errs: HashSet::new(),
            use_errs: HashSet::new(),
            lent: HashMap::new(),
            user_consumed: HashMap::new(),
        }
    }

    fn err(&mut self, d: Diag) {
        // 値の使用のエラーは 1 か所 1 回だけ（`w.v = 5;` は根の `w` と場所 `w.v` の両方から同じ使用に届く）
        // a use error is reported once per position (`w.v = 5;` reaches the same use through the root `w` and the place `w.v`)
        if matches!(d.code, "F0382" | "F0381") && !self.use_errs.insert((d.span.lo, d.code)) {
            return;
        }
        if self.errs.insert((d.span, d.code)) {
            self.r.diags.push(d);
        }
    }

    pub fn run(mut self) -> FlowResults {
        let prog = self.s.prog;
        for (fi, f) in self.s.t.fns.iter().enumerate() {
            if !self.s.fn_ok.get(fi).copied().unwrap_or(false) {
                continue;
            }
            if let Item::Fn(fd) = &prog.items[f.item] {
                self.st = State { live: true, vars: HashMap::new(), proven: HashSet::new() };
                self.active.clear();
                self.ranges.clear();
                for p in &f.params {
                    self.st.vars.insert(p.bind, VS::Init);
                }
                let tail_use = if f.ret == Ty::Unit { Use::Read } else { Use::Move };
                self.block(&fd.body, tail_use);
            }
        }
        for (id, ok) in std::mem::take(&mut self.proven_dec) {
            if ok {
                self.r.proven.insert(id);
            }
        }
        self.r
    }

    fn ty(&self, e: &Expr) -> &Ty {
        &self.s.expr_ty[e.id as usize]
    }

    fn local(&self, b: BindId) -> Option<&crate::sema::LocalInfo> {
        self.s.locals.get(b as usize).and_then(|x| x.as_ref())
    }

    fn is_copy_local(&self, b: BindId) -> bool {
        match self.local(b) {
            Some(l) => matches!(l.ty, Ty::Ref(..)) || self.s.t.is_copy(&l.ty),
            None => true,
        }
    }

    fn block(&mut self, b: &Block, tail: Use) {
        for st in &b.stmts {
            match st {
                Stmt::Let(l) => {
                    if let Some(i) = &l.init {
                        self.expr(i, Use::Move);
                        self.st.vars.insert(l.bind, VS::Init);
                        // let x = @a.alloc(v).unwrap(); → x は生存が証明済み / x is proven live
                        if let Some(a) = proven_alloc_unwrap(self.s, i) {
                            self.st.proven.insert((a, l.bind));
                        }
                    } else {
                        self.st.vars.insert(l.bind, VS::Uninit);
                        self.r.deferred_init.insert(l.bind);
                    }
                    // 同じ名前の再束縛は新しいローカル（BindId が違う） / shadowing creates a new local
                    self.st.proven.retain(|(_, x)| *x != l.bind);
                }
                Stmt::Expr(e) => self.expr(e, Use::Read),
            }
        }
        if let Some(t) = &b.tail {
            self.expr(t, tail);
        }
    }

    fn place_of(&self, e: &Expr) -> Option<Place> {
        match &e.kind {
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Local(b)) => Some(Place { root: PRoot::Local(*b), proj: Vec::new() }),
                Some(Res::Static(s)) => Some(Place { root: PRoot::Static(*s), proj: Vec::new() }),
                _ => None,
            },
            ExprKind::Field(b, _, _) => {
                let mut p = self.place_of(b)?;
                let fi = self.s.fields.get(&e.id).map(|x| x.1).unwrap_or(usize::MAX);
                p.proj.push(Proj::Field(fi));
                Some(p)
            }
            ExprKind::Index(b, _) => {
                if let Some(&a) = self.s.arena_index.get(&e.id) {
                    return Some(Place { root: PRoot::Arena(a), proj: Vec::new() });
                }
                if let Some(&(m, _)) = self.s.mmio_index.get(&e.id) {
                    return Some(Place { root: PRoot::Mmio(m), proj: Vec::new() });
                }
                if let Some(&p) = self.s.port_index.get(&e.id) {
                    return Some(Place { root: PRoot::Port(p), proj: Vec::new() });
                }
                let mut p = self.place_of(b)?;
                p.proj.push(Proj::Index);
                Some(p)
            }
            ExprKind::ArenaRef(_) => self.s.port_reads.get(&e.id).map(|p| Place { root: PRoot::Port(*p), proj: Vec::new() }),
            _ => None,
        }
    }

    fn root_name(&self, p: &Place) -> String {
        match &p.root {
            PRoot::Local(b) => self.local(*b).map(|l| l.name.clone()).unwrap_or_default(),
            PRoot::Static(s) => self.s.t.statics[*s].name.clone(),
            PRoot::Arena(a) => self.s.t.arenas[*a].name.clone(),
            PRoot::Mmio(m) => self.s.t.mmios[*m].name.clone(),
            PRoot::Port(p) => self.s.t.ports[*p].name.clone(),
        }
    }

    /// 有効な借用との衝突を検査する / check an access against the active borrows
    fn check_access(&mut self, p: &Place, acc: Acc, span: Span) {
        let active = self.active.clone();
        for b in active.iter().rev() {
            if !overlaps(&b.place, p) {
                continue;
            }
            let conflict = if b.mutable { true } else { matches!(acc, Acc::Write | Acc::MutBorrow | Acc::Move) };
            if !conflict {
                continue;
            }
            let name = self.root_name(p);
            let is_arena = matches!(p.root, PRoot::Arena(_));
            let code = if is_arena {
                "F2502"
            } else if acc == Acc::Move {
                "F0505"
            } else {
                "F0502"
            };
            let (aj, ae) = match acc {
                Acc::Read => ("読み取り", "read"),
                Acc::Write => ("書き換え", "write"),
                Acc::MutBorrow => ("可変借用", "mutable borrow"),
                Acc::SharedBorrow => ("共有借用", "shared borrow"),
                Acc::Move => ("移動", "move"),
            };
            let d = Diag::error(
                code,
                span,
                format!("`{}` は{}の間です（{}）。ここでの{}は衝突します", name, if b.mutable { "可変借用" } else { "共有借用" }, b.what_ja, aj),
                format!("`{}` is {} borrowed ({}); this {} conflicts", name, if b.mutable { "mutably" } else { "immutably" }, b.what_en, ae),
            )
            .note(b.span, "借用はここで始まり、このブロックの終わりまで続きます", "the borrow starts here and lasts until the end of this block");
            let d = if is_arena {
                d.hint(
                    "ルックアップの束縛を使うか、ブロックの外で操作してください（2 つの Id が同じスロットかもしれないため）",
                    "use the lookup binding, or do this outside the block (the two Ids may denote the same slot)",
                )
            } else {
                d.hint("束縛した参照を通して操作するか、ブロックの外で行ってください", "go through the bound reference, or do it outside the block")
            };
            self.err(d);
            return;
        }
    }

    /// 呼び出し先の効果が有効なアリーナ借用と衝突しないか / does a callee's effect conflict with an active arena borrow?
    fn check_call_effects(&mut self, f: usize, span: Span, extra: &[(usize, bool, Span)]) {
        let eff = self.s.t.fns[f].effects.clone();
        let fname = self.s.t.fns[f].name.clone();
        self.check_effects(&eff, &fname, None, span, extra);
    }

    /// 関数ポインタの呼び出しで呼ばれうる関数と、その効果の和 / the functions an indirect call may reach and the union of
    /// their effects
    fn indirect_effects(&self, e: &Expr) -> (Vec<usize>, Effects) {
        let cands = self.s.indirect_cands.get(&e.id).cloned().unwrap_or_default();
        let mut eff = Effects::default();
        for &g in &cands {
            let ge = &self.s.t.fns[g].effects;
            for a in &ge.reads {
                Effects::add(&mut eff.reads, *a);
            }
            for a in &ge.writes {
                Effects::add(&mut eff.writes, *a);
            }
            for a in &ge.frees {
                Effects::add(&mut eff.frees, *a);
            }
            for a in &ge.allocs {
                Effects::add(&mut eff.allocs, *a);
            }
        }
        (cands, eff)
    }

    /// 関数ポインタの呼び出しの引数の型 / the parameter types of a call through a function pointer
    fn indirect_params(&self, e: &Expr) -> Vec<Ty> {
        let t = match &e.kind {
            ExprKind::CallIndirect { callee, .. } => self.ty(callee).clone(),
            _ => match self.s.call_targets.get(&e.id) {
                Some(Res::Local(b)) => self.local(*b).map(|l| l.ty.clone()).unwrap_or(Ty::Err),
                Some(Res::Static(si)) => self.s.t.statics[*si].ty.clone(),
                _ => Ty::Err,
            },
        };
        match t {
            Ty::Fn(ps, _) => ps,
            _ => Vec::new(),
        }
    }

    /// 効果と有効なアリーナ借用の衝突 / an effect against the active arena borrows. `via`: for a call through a function
    /// pointer, the candidate whose effect conflicts is named and the place where it became a callback is noted
    fn check_effects(&mut self, eff: &Effects, fname: &str, via: Option<&[usize]>, span: Span, extra: &[(usize, bool, Span)]) {
        let mut list: Vec<(usize, bool, Span, String, String)> = Vec::new();
        for b in &self.active {
            if let PRoot::Arena(a) = b.place.root {
                list.push((a, b.mutable, b.span, b.what_ja.clone(), b.what_en.clone()));
            }
        }
        for (a, m, sp) in extra {
            list.push((*a, *m, *sp, "この呼び出しの参照引数".to_string(), "a reference argument of this call".to_string()));
        }
        for (a, m, bsp, wj, we) in list {
            let bad = if m { eff.touches(a) } else { eff.mutates(a) };
            if bad {
                let an = self.s.t.arenas[a].name.clone();
                let what = if eff.frees.contains(&a) { ("解放", "free") } else if eff.writes.contains(&a) { ("書き換え", "write") } else { ("読み取り", "read") };
                // 関数ポインタの呼び出し: 衝突する効果を持つ候補を名指しする / a call through a function pointer: name the
                // candidate whose effect conflicts
                let culprit = via.and_then(|cs| {
                    cs.iter().copied().find(|g| {
                        let ge = &self.s.t.fns[*g].effects;
                        if m {
                            ge.touches(a)
                        } else {
                            ge.mutates(a)
                        }
                    })
                });
                let (shown_ja, shown_en) = match culprit {
                    Some(g) => (
                        format!("コールバック `{}`（`{}` が入りうる）", fname, self.s.t.fns[g].name),
                        format!("the callback `{}` (which may be `{}`)", fname, self.s.t.fns[g].name),
                    ),
                    None => (format!("`{}`", fname), format!("`{}`", fname)),
                };
                let mut d = Diag::error(
                    "F2502",
                    span,
                    format!("{} は `{}` のスロットを{}する可能性があります。`{}` への参照（{}）が有効な間は呼べません", shown_ja, an, what.0, an, wj),
                    format!("{} may {} slots of `{}`, so it cannot be called while a reference into `{}` is live ({})", shown_en, what.1, an, an, we),
                )
                .note(bsp, "参照はここで作られました", "the reference was created here");
                if let Some(g) = culprit {
                    let gname = self.s.t.fns[g].name.clone();
                    if let Some((_, _, sp)) = self.s.fn_value_sites.iter().find(|(f, _, _)| *f == g) {
                        d = d.note(
                            *sp,
                            format!("`{}` はここでコールバック（関数ポインタ）になりました", gname),
                            format!("`{}` became a callback (a function pointer) here", gname),
                        );
                    }
                }
                d = d.hint(
                    format!("参照のブロックを閉じてから呼ぶか、必要な値を先にローカルへ写してください（`{}` の効果は呼び出し先まで推移的に計算されています）", fname),
                    format!("call it after the reference's block ends, or copy what you need into locals first (the effect of `{}` is computed transitively through its callees)", fname),
                );
                self.err(d);
                return;
            }
        }
    }

    fn use_local(&mut self, b: BindId, span: Span, u: Use) {
        if self.is_copy_local(b) {
            return;
        }
        let name = self.local(b).map(|l| l.name.clone()).unwrap_or_default();
        match self.st.vars.get(&b).copied().unwrap_or(VS::Init) {
            VS::Init => {}
            VS::Moved(ms) | VS::MaybeMoved(ms) if self.user_consumed.contains_key(&b) => {
                // `user T` の 2 回目の読み出し（TOCTOU）/ a second read of a `user T` (TOCTOU)
                self.err(
                    Diag::error("F3101", span, format!("`{}`（ユーザー空間のバッファ）はすでに copy_from_user で写しました。もう一度読むと、その間にユーザーが書き換えた値（TOCTOU）を読みます", name), format!("`{}` (a user-space buffer) was already copied by copy_from_user; reading it again could see a value the user changed in between (TOCTOU)", name))
                        .note(ms, "1 回目の写しはここ", "the first copy is here")
                        .hint("1 回目に写したカーネル側の値を使ってください（ユーザーメモリは 1 回だけ読む）", "use the kernel-side copy made the first time (user memory is read once)"),
                );
                return;
            }
            VS::Moved(ms) | VS::MaybeMoved(ms) if self.lent.contains_key(&b) => {
                self.err(
                    Diag::error("F3401", span, format!("`{}` はデバイスへ貸し出し中です（submit_dma 〜 complete_dma の間は CPU から読み書き・解放できない）", name), format!("`{}` is lent to a device (between submit_dma and complete_dma the CPU cannot read, write or free it)", name))
                        .note(ms, "ここで submit_dma に渡しました", "handed to submit_dma here")
                        .hint("完了の通知を受けてから `complete_dma(T, iova)` で取り戻した値を使ってください", "after the completion, use the value taken back with `complete_dma(T, iova)`"),
                );
                return;
            }
            VS::Moved(ms) | VS::MaybeMoved(ms) => {
                self.err(
                    Diag::error("F0382", span, format!("移動後の値 `{}` を使用しています（Use-After-Move）", name), format!("use of moved value `{}` (use after move)", name))
                        .note(ms, format!("`{}` はここで移動しました", name), format!("`{}` was moved here", name))
                        .hint(format!("移動前に明示的に複製してください: `{}.clone()`（暗黙のコピーは禁止）", name), format!("clone it explicitly before the move: `{}.clone()` (there is no implicit copy)", name)),
                );
                return;
            }
            VS::Uninit | VS::MaybeUninit => {
                self.err(Diag::error("F0381", span, format!("初期化されていない可能性のある `{}` を使用しています", name), format!("use of possibly-uninitialized `{}`", name)));
                return;
            }
        }
        if u == Use::Move {
            if let Some(PRoot::Local(_)) = Some(PRoot::Local(b)) {
                let p = Place { root: PRoot::Local(b), proj: Vec::new() };
                self.check_access(&p, Acc::Move, span);
            }
            self.st.vars.insert(b, VS::Moved(span));
            self.r.moved_locals.insert(b);
        }
    }

    /// 場所式を「値として」使う / use a place expression as a value
    fn place_value(&mut self, e: &Expr, u: Use) {
        let t = self.ty(e).clone();
        let copy = self.s.t.is_copy(&t);
        // `*b`（own の中身）: コピーできる値は読むだけ。コピーできない値を取り出すと own が空になる → F0507
        // `*b` (the contents of an own): copyable values are just read; taking a non-copyable value out would leave the
        // own empty → F0507
        if self.s.own_derefs.contains(&e.id) && u == Use::Move && !copy && !t.is_err() {
            self.place_base(e);
            let d = self.s.t.display(&t);
            self.err(
                Diag::error(
                    "F0507",
                    e.span,
                    format!("own の中身から `{}` の値を移動できません（own が空になってしまいます）", d),
                    format!("cannot move a `{}` value out of the contents of an own (the own would be left empty)", d),
                )
                .hint("own ごと移動するか（`*` を付けない）、`.clone()` で複製してください", "move the whole own (without `*`), or copy it with `.clone()`"),
            );
            return;
        }
        match &e.kind {
            ExprKind::Path(_) => {
                if let Some(Res::Local(b)) = self.s.res.get(&e.id) {
                    let b = *b;
                    let p = Place { root: PRoot::Local(b), proj: Vec::new() };
                    if u == Use::Move && !copy {
                        self.use_local(b, e.span, Use::Move);
                        self.r.move_exprs.insert(e.id);
                    } else {
                        self.use_local(b, e.span, Use::Read);
                        self.check_access(&p, Acc::Read, e.span);
                    }
                } else if let Some(Res::Static(si)) = self.s.res.get(&e.id) {
                    if u == Use::Move && !copy {
                        let n = self.s.t.statics[*si].name.clone();
                        self.err(
                            Diag::error("F0507", e.span, format!("static `{}` から値を移動できません", n), format!("cannot move out of static `{}`", n))
                                .hint(format!("`{}.clone()` で複製してください", n), format!("copy it with `{}.clone()`", n)),
                        );
                    }
                }
            }
            ExprKind::Field(..) | ExprKind::Index(..) => {
                self.place_base(e);
                if let Some(p) = self.place_of(e) {
                    self.check_access(&p, if u == Use::Move && !copy { Acc::Move } else { Acc::Read }, e.span);
                }
                if u == Use::Move && !copy && !t.is_err() {
                    let what = if self.s.arena_index.contains_key(&e.id) {
                        ("アリーナのスロット", "an arena slot", "`a.remove(id)` で取り出すか `.clone()` で複製してください", "take it out with `a.remove(id)` or copy it with `.clone()`")
                    } else if matches!(t, Ty::Own(_)) {
                        ("own フィールド", "an own field", "子を差し替えるには代入を、取り外せる子は `Option<own T>` と `.take()` を使ってください", "assign to replace a child; use `Option<own T>` with `.take()` for detachable children")
                    } else {
                        ("フィールド・参照先", "a field or a reference", "`.clone()` で複製するか、`Option` のフィールドなら `.take()` で取り出してください", "copy it with `.clone()`, or `.take()` it if the field is an `Option`")
                    };
                    let d = self.s.t.display(&t);
                    self.err(
                        Diag::error("F0507", e.span, format!("{}から `{}` の値を移動できません", what.0, d), format!("cannot move a `{}` value out of {}", d, what.1))
                            .hint(what.2, what.3),
                    );
                }
            }
            _ => self.expr(e, u),
        }
    }

    /// 場所式の土台部分（添字式・アリーナの生存証明）を処理する / walk the base parts of a place expression
    fn place_base(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Path(_) => {
                if let Some(Res::Local(b)) = self.s.res.get(&e.id) {
                    self.use_local(*b, e.span, Use::Read);
                }
            }
            ExprKind::Field(b, _, _) => self.place_base(b),
            ExprKind::Index(b, idx) => {
                if let Some(&a) = self.s.arena_index.get(&e.id) {
                    self.expr(idx, Use::Read);
                    // 世代検査の静的消去 / static elision of the generation check
                    let mut ok = false;
                    if let ExprKind::Path(_) = &idx.kind {
                        if let Some(Res::Local(lb)) = self.s.res.get(&idx.id) {
                            ok = self.st.proven.contains(&(a, *lb));
                            // 検査付きアクセスを通った後に生存が証明されるのは、その場で止まる検査（own を持つ要素）だけ。
                            // それ以外は遅延フォールト（ラッチ）なので、古い Id のまま先へ進むことがある → 証明にしない
                            // only a check that stops on the spot (elements holding own) proves liveness afterwards; the others
                            // latch a deferred fault and keep going with a possibly stale Id, so they prove nothing
                            if self.s.t.needs_drop(&self.s.t.arenas[a].elem) {
                                self.st.proven.insert((a, *lb));
                            }
                        }
                    }
                    // ループの各反復の訪問すべてで成り立つときだけ省く（AND） / elide only if it holds on every visit (AND over loop iterations)
                    let slot = self.proven_dec.entry(e.id).or_insert(true);
                    *slot = *slot && ok;
                    return;
                }
                if let Some(&p) = self.s.port_index.get(&e.id) {
                    self.expr(idx, Use::Read);
                    let len = self.s.t.ports[p].len.unwrap_or(1);
                    self.bounds(e.id, idx, len);
                    return;
                }
                if let Some(&(m, depth)) = self.s.mmio_index.get(&e.id) {
                    if depth > 1 {
                        // 2 次元: 1 つ目の添字 / 2-D: the first index
                        if let ExprKind::Index(_, i0) = &b.kind {
                            self.expr(i0, Use::Read);
                            self.bounds(b.id, i0, self.s.t.mmios[m].dims[0]);
                        }
                    }
                    self.expr(idx, Use::Read);
                    self.bounds(e.id, idx, self.s.t.mmios[m].dims[depth - 1]);
                    return;
                }
                self.place_base(b);
                self.expr(idx, Use::Read);
                let bt = self.ty(b).clone();
                if let Ty::Array(_, n) = bt.deref_all() {
                    self.bounds(e.id, idx, *n);
                }
            }
            _ => self.expr(e, Use::Read),
        }
    }

    // ───────────── 範囲解析（境界検査の静的消去） / range analysis (static elision of bounds checks) ─────────────

    fn range_of(&self, e: &Expr) -> Option<(i128, i128)> {
        match &e.kind {
            ExprKind::Int(v, _) => {
                let v = if self.s.neg_lits.contains(&e.id) { -(*v as i128) } else { *v as i128 };
                Some((v, v))
            }
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Local(b)) => self.ranges.get(b).copied(),
                Some(Res::Static(si)) => self.s.t.statics[*si].int_value.map(|v| (v, v)),
                _ => None,
            },
            ExprKind::Cast(x, _) => {
                let (lo, hi) = self.range_of(x)?;
                if let Ty::Int(it) = self.ty(e) {
                    let (mn, mx) = it.min_max(self.s.t.ptr_bits);
                    if lo >= mn && hi <= mx {
                        return Some((lo, hi));
                    }
                }
                None
            }
            ExprKind::Unary(UnOp::Neg, x) => {
                if let ExprKind::Int(..) = x.kind {
                    return self.range_of(x);
                }
                let (lo, hi) = self.range_of(x)?;
                Some((-hi, -lo))
            }
            ExprKind::Binary(op, a, b) => {
                let (al, ah) = self.range_of(a)?;
                let (bl, bh) = self.range_of(b)?;
                match op {
                    BinOp::Add => Some((al + bl, ah + bh)),
                    BinOp::Sub => Some((al - bh, ah - bl)),
                    BinOp::Mul if al >= 0 && bl >= 0 => Some((al * bl, ah * bh)),
                    BinOp::Div if bl > 0 && al >= 0 => Some((al / bh, ah / bl)),
                    BinOp::Rem if bl > 0 && bl == bh && al >= 0 => Some((0, (bl - 1).min(ah))),
                    BinOp::BitAnd if bl >= 0 && bl == bh => Some((0, bl)),
                    BinOp::Shr if al >= 0 && bl == bh && (0..64).contains(&bl) => Some((al >> bl, ah >> bl)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn bounds(&mut self, id: ExprId, idx: &Expr, len: u64) {
        if let Some((lo, hi)) = self.range_of(idx) {
            if lo >= 0 && hi < len as i128 {
                self.r.bounds_elided.insert(id);
                return;
            }
            // 定数の添字が範囲外ならコンパイル時エラー / a constant index out of range is a compile-time error
            if lo == hi && (lo < 0 || lo >= len as i128) {
                self.err(Diag::error(
                    if self.s.mmio_index.contains_key(&id) { "F2201" } else { "F0608" },
                    idx.span,
                    format!("添字 {} は長さ {} の範囲外です（コンパイル時に検出）", lo, len),
                    format!("index {} is out of bounds for length {} (detected at compile time)", lo, len),
                ));
            }
        }
    }

    // ───────────── 式 / expressions ─────────────

    fn expr(&mut self, e: &Expr, u: Use) {
        if !self.st.live {
            // 到達しないコードでも子は見る（エラー検出のため状態は捨てる） / unreachable: still visit for errors
            self.st.live = true;
            self.st.vars.clear();
            let saved = std::mem::replace(&mut self.st, State { live: true, vars: HashMap::new(), proven: HashSet::new() });
            self.expr_inner(e, u);
            self.st = saved;
            self.st.live = false;
            return;
        }
        self.expr_inner(e, u);
    }

    fn expr_inner(&mut self, e: &Expr, u: Use) {
        match &e.kind {
            ExprKind::Int(..) | ExprKind::Float(..) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Unit | ExprKind::ArenaRef(_) | ExprKind::Unsupported(_) | ExprKind::OwnAlloc(_) => {}
            ExprKind::Path(_) | ExprKind::Field(..) => self.place_value(e, u),
            ExprKind::Index(..) => {
                if let Some(&(m, depth)) = self.s.mmio_index.get(&e.id) {
                    if depth < self.s.t.mmios[m].dims.len() {
                        let n = self.s.t.mmios[m].name.clone();
                        self.err(Diag::error(
                            "F2204",
                            e.span,
                            format!("`{}` の行は値として使えません（`{}[y][x]` まで添字を付けてください）", n, n),
                            format!("a row of `{}` cannot be used as a value (index it fully: `{}[y][x]`)", n, n),
                        ));
                    }
                }
                self.place_value(e, u)
            }
            ExprKind::Call { args, .. } => self.call(e, args),
            ExprKind::CallIndirect { callee, args } => {
                // 呼ばれる式（関数ポインタ）を読んでから引数 / read the callee (a function pointer), then the arguments
                self.expr(callee, Use::Read);
                self.call(e, args)
            }
            ExprKind::MethodCall { recv, args, .. } => self.method(e, recv, args, u),
            ExprKind::StructLit { fields, .. } => {
                for (_, _, v) in fields {
                    self.expr(v, Use::Move);
                }
            }
            ExprKind::ArrayLit(v) => v.iter().for_each(|x| self.expr(x, Use::Move)),
            ExprKind::ArrayRepeat(v, _) => self.expr(v, Use::Move),
            ExprKind::Own(x) => self.expr(x, Use::Move),
            ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => self.expr(x, Use::Read),
            ExprKind::Binary(op, a, b) => {
                self.expr(a, Use::Read);
                if matches!(op, BinOp::And | BinOp::Or) {
                    // 右辺は条件付きで評価される / the right operand runs conditionally
                    let before = self.st.clone();
                    self.expr(b, Use::Read);
                    let after = self.st.clone();
                    self.st = before.join(&after);
                } else {
                    self.expr(b, Use::Read);
                }
            }
            ExprKind::Ref { inner, mutable } => {
                // 呼び出し引数以外は sema が F0515 を出している / outside call args sema already reported F0515
                self.borrow_arg(inner, *mutable);
            }
            ExprKind::Assign { op, lhs, rhs } => {
                self.expr(rhs, Use::Move);
                self.assign_target(lhs, op.is_some());
            }
            ExprKind::If { cond, then, els } => {
                self.expr(cond, Use::Read);
                // if @a.contains(x) { .. } → then 側で x の生存が証明される / x is proven in the then-branch
                let fact = contains_fact(self.s, cond);
                let base = self.st.clone();
                if let Some(f) = fact {
                    self.st.proven.insert(f);
                }
                self.block(then, u);
                let after_then = std::mem::replace(&mut self.st, base);
                if let Some(el) = els {
                    self.expr(el, u);
                }
                self.st = after_then.join(&self.st);
            }
            ExprKind::IfLet { pat, scrut, then, els } => {
                let binds_by_ref = self.scrutinee(scrut, std::slice::from_ref(pat));
                let base = self.st.clone();
                self.enter_arm(pat, scrut, binds_by_ref);
                let pushed = self.push_arm_borrow(pat, scrut, binds_by_ref);
                self.block(then, u);
                if pushed {
                    self.active.pop();
                }
                let after_then = std::mem::replace(&mut self.st, base);
                if let Some(el) = els {
                    self.expr(el, u);
                }
                self.st = after_then.join(&self.st);
            }
            ExprKind::Match { scrut, arms } => {
                let pats: Vec<Pattern> = arms.iter().map(|a| a.pat.clone()).collect();
                let binds_by_ref = self.scrutinee(scrut, &pats);
                let base = self.st.clone();
                let mut out = State::dead();
                for arm in arms {
                    self.st = base.clone();
                    self.enter_arm(&arm.pat, scrut, binds_by_ref);
                    let pushed = self.push_arm_borrow(&arm.pat, scrut, binds_by_ref);
                    self.expr(&arm.body, u);
                    if pushed {
                        self.active.pop();
                    }
                    out = out.join(&self.st);
                }
                if arms.is_empty() {
                    out = base;
                }
                self.st = out;
            }
            ExprKind::While { cond, body } => {
                let entry = self.st.clone();
                let mut head = entry.clone();
                let mut exit = State::dead();
                for _ in 0..4 {
                    self.st = head.clone();
                    self.expr(cond, Use::Read);
                    let after_cond = self.st.clone();
                    self.loops.push(LoopCtx { breaks: Vec::new(), conts: Vec::new() });
                    self.block(body, Use::Read);
                    let ctx = self.loops.pop().unwrap();
                    let mut end = self.st.clone();
                    for c in &ctx.conts {
                        end = end.join(c);
                    }
                    let new_head = entry.join(&end);
                    exit = after_cond;
                    for b in &ctx.breaks {
                        exit = exit.join(b);
                    }
                    if new_head == head {
                        break;
                    }
                    head = new_head;
                }
                self.st = exit;
            }
            ExprKind::Loop { body } => {
                let entry = self.st.clone();
                let mut head = entry.clone();
                let mut exit = State::dead();
                for _ in 0..4 {
                    self.st = head.clone();
                    self.loops.push(LoopCtx { breaks: Vec::new(), conts: Vec::new() });
                    self.block(body, Use::Read);
                    let ctx = self.loops.pop().unwrap();
                    let mut end = self.st.clone();
                    for c in &ctx.conts {
                        end = end.join(c);
                    }
                    let new_head = entry.join(&end);
                    exit = State::dead();
                    for b in &ctx.breaks {
                        exit = exit.join(b);
                    }
                    if new_head == head {
                        break;
                    }
                    head = new_head;
                }
                self.st = exit;
            }
            ExprKind::For { bind, iter, body, .. } => {
                let mut range = None;
                if let ForIter::Range { lo, hi, inclusive } = iter {
                    self.expr(lo, Use::Read);
                    self.expr(hi, Use::Read);
                    if let (Some((l0, _)), Some((_, h1))) = (self.range_of(lo), self.range_of(hi)) {
                        let top = if *inclusive { h1 } else { h1 - 1 };
                        if top >= l0 {
                            range = Some((l0, top));
                        }
                    }
                }
                if let Some(r) = range {
                    self.ranges.insert(*bind, r);
                }
                let entry = self.st.clone();
                let mut head = entry.clone();
                let mut exit = State::dead();
                for _ in 0..4 {
                    self.st = head.clone();
                    self.st.vars.insert(*bind, VS::Init);
                    // for id in @a: 各反復の先頭で id は生存スロット / at the top of each iteration id is a live slot
                    if let ForIter::Arena(name, _) = iter {
                        if let Some(a) = self.s.t.arena_of_name(name) {
                            self.st.proven.insert((a, *bind));
                        }
                    }
                    let after_head = self.st.clone();
                    self.loops.push(LoopCtx { breaks: Vec::new(), conts: Vec::new() });
                    self.block(body, Use::Read);
                    let ctx = self.loops.pop().unwrap();
                    let mut end = self.st.clone();
                    for c in &ctx.conts {
                        end = end.join(c);
                    }
                    let new_head = entry.join(&end);
                    exit = after_head.join(&end);
                    for b in &ctx.breaks {
                        exit = exit.join(b);
                    }
                    if new_head == head {
                        break;
                    }
                    head = new_head;
                }
                self.ranges.remove(bind);
                self.st = exit;
            }
            ExprKind::Break => {
                let s = self.st.clone();
                if let Some(l) = self.loops.last_mut() {
                    l.breaks.push(s);
                }
                self.st = State::dead();
            }
            ExprKind::Continue => {
                let s = self.st.clone();
                if let Some(l) = self.loops.last_mut() {
                    l.conts.push(s);
                }
                self.st = State::dead();
            }
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    self.expr(v, Use::Move);
                }
                self.st = State::dead();
            }
            ExprKind::Block(b) | ExprKind::VolatileBypass(b) => self.block(b, u),
            // 閉じた区間: 出口の先では、ほかの実行ルートがスロットを解放しうる。中で得た生存証明は外へ持ち出さない
            // a closed section: past its end other execution roots may free slots, so liveness proofs made inside do not
            // leave it
            ExprKind::Critical(b) => {
                self.block(b, u);
                if self.s.ctx.any_roots {
                    self.st.proven.clear();
                }
            }
            // atomic / rcu_read: 中の文はふつうに検査する。rcu_read を出た後は、ほかのコアが解放しうるので証明を持ち出さない
            // atomic / rcu_read: the statements inside are checked as usual; past rcu_read other cores may free slots, so
            // proofs do not leave it
            ExprKind::Atomic(b) => self.block(b, u),
            ExprKind::RcuRead(b) => {
                self.block(b, u);
                self.st.proven.clear();
            }
            // catch_fault: 異常で途中から抜けうるので、外の値を中で移動すると「動いたかどうか」が実行時に決まる（F3106）
            // catch_fault: it may be left halfway on a fault, so moving an outer value inside would make "was it moved"
            // depend on the run (F3106)
            ExprKind::CatchFault(b) => {
                let before: HashMap<BindId, VS> = self.st.vars.clone();
                self.block(b, u);
                let mut bad: Vec<(BindId, Span)> = Vec::new();
                for (bid, vs) in &self.st.vars {
                    if let (VS::Moved(sp) | VS::MaybeMoved(sp), Some(VS::Init) | None) = (vs, before.get(bid)) {
                        if before.contains_key(bid) || self.local(*bid).map_or(false, |l| l.kind == LocalKind::Param) {
                            bad.push((*bid, *sp));
                        }
                    }
                }
                for (bid, sp) in bad {
                    let name = self.local(bid).map(|l| l.name.clone()).unwrap_or_default();
                    self.err(
                        Diag::error("F3106", sp, format!("`catch_fault` の外の値 `{}` を中で移動しています（異常で途中から抜けると移動したかどうかが実行時に決まる）", name), format!("the outer value `{}` is moved inside `catch_fault` (leaving halfway on a fault would make the move depend on the run)", name))
                            .hint("移動する値は `catch_fault` の中で作るか、`catch_fault` の外で移動してください", "create the value inside `catch_fault`, or move it outside `catch_fault`"),
                    );
                }
            }
            ExprKind::TypeCall { args, .. } => {
                for a in args {
                    self.expr(a, Use::Read);
                }
            }
        }
        if *self.ty(e) == Ty::Never {
            self.st = State::dead();
        }
    }

    /// 対象式の処理。戻り値: アームの束縛が参照（借用）になるか / returns whether arm bindings borrow
    fn scrutinee(&mut self, scrut: &Expr, pats: &[Pattern]) -> bool {
        let binds = pats.iter().any(|p| matches!(p, Pattern::Some(Some(_), _) | Pattern::Bind(..) | Pattern::Ok(Some(_), _) | Pattern::Err(Some(_), _)));
        if self.s.lookup_scrut.contains(&scrut.id) {
            // ルックアップ自体はアリーナへのアクセス / the lookup itself accesses the arena
            if let ExprKind::MethodCall { args, .. } = &scrut.kind {
                for a in args {
                    self.expr(a, Use::Read);
                }
            }
            if let Some(k) = self.s.methods.get(&scrut.id) {
                let (a, m) = match k {
                    MethodKind::ArenaGet(a) => (*a, false),
                    MethodKind::ArenaGetMut(a) => (*a, true),
                    // ビュー・窓: 元のバイト列の場所を借りる（束縛が生きている間、その場所の解放・移動・重なる書き込みは F2502 など）
                    // a view or window borrows the place of its bytes (while the binding lives, freeing, moving or
                    // overlapping writes of that place are rejected)
                    // ユーザーメモリの窓: タスク表を共有で借りる（窓が生きている間、そのタスクの解放は F2502）
                    // a window over user memory borrows the task arena shared (freeing the task while it lives is F2502)
                    MethodKind::UserMemOf(u) => {
                        let a = self.s.t.umems[*u].task;
                        if a != usize::MAX {
                            self.check_access(&Place { root: PRoot::Arena(a), proj: Vec::new() }, Acc::SharedBorrow, scrut.span);
                        }
                        return true;
                    }
                    MethodKind::View(m, _) | MethodKind::Window(m) => {
                        let m = *m;
                        if let ExprKind::MethodCall { recv, .. } = &scrut.kind {
                            if let Some(p) = self.place_of(recv) {
                                self.check_access(&p, if m { Acc::MutBorrow } else { Acc::SharedBorrow }, scrut.span);
                            } else {
                                self.expr(recv, Use::Read);
                            }
                        }
                        return true;
                    }
                    _ => return true,
                };
                // 生存が証明済みの Id なら世代検査を省ける（None の腕は到達しない）
                // with an Id proven live the generation check can be dropped (the None arm is unreachable)
                let mut ok = false;
                if let ExprKind::MethodCall { args, .. } = &scrut.kind {
                    if let Some(ExprKind::Path(_)) = args.first().map(|x| &x.kind) {
                        if let Some(Res::Local(lb)) = self.s.res.get(&args[0].id) {
                            ok = self.st.live && self.st.proven.contains(&(a, *lb));
                        }
                    }
                }
                let slot = self.proven_dec.entry(scrut.id).or_insert(true);
                *slot = *slot && ok;
                let p = Place { root: PRoot::Arena(a), proj: Vec::new() };
                self.check_access(&p, if m { Acc::MutBorrow } else { Acc::SharedBorrow }, scrut.span);
            }
            return true;
        }
        let st = self.ty(scrut).clone();
        let payload_copy = match st.strip_ref() {
            Ty::Option(i) | Ty::Result(i, _) => self.s.t.is_copy(i),
            t => self.s.t.is_copy(t),
        };
        if crate::sema::is_place_expr(scrut) && (payload_copy || !binds) {
            self.place_value(scrut, Use::Read);
            false
        } else if crate::sema::is_place_expr(scrut) {
            // 場所の借用（束縛は参照） / the place is borrowed (bindings are references)
            self.place_base(scrut);
            if let Some(p) = self.place_of(scrut) {
                let m = self.s.place_is_mutable(scrut);
                self.check_access(&p, if m { Acc::MutBorrow } else { Acc::SharedBorrow }, scrut.span);
            }
            true
        } else {
            self.expr(scrut, Use::Move);
            false
        }
    }

    fn enter_arm(&mut self, pat: &Pattern, scrut: &Expr, _by_ref: bool) {
        match pat {
            Pattern::Ok(Some((_, b, _)), _) | Pattern::Err(Some((_, b, _)), _) => {
                self.st.vars.insert(*b, VS::Init);
            }
            Pattern::Some(Some((_, b, _)), _) | Pattern::Bind(_, b, _) => {
                self.st.vars.insert(*b, VS::Init);
                // if let Some(p) = @a.get(x) / if let Some(x) = @a.alloc(v) / @a.validate(raw)
                if let ExprKind::MethodCall { args, .. } = &scrut.kind {
                    match self.s.methods.get(&scrut.id) {
                        Some(MethodKind::ArenaGet(a)) | Some(MethodKind::ArenaGetMut(a)) => {
                            if let Some(ExprKind::Path(_)) = args.first().map(|x| &x.kind) {
                                if let Some(Res::Local(lb)) = self.s.res.get(&args[0].id) {
                                    if matches!(pat, Pattern::Some(..)) {
                                        self.st.proven.insert((*a, *lb));
                                    }
                                }
                            }
                        }
                        Some(MethodKind::ArenaAlloc(a)) | Some(MethodKind::ArenaValidate(a)) => {
                            if matches!(pat, Pattern::Some(..)) {
                                self.st.proven.insert((*a, *b));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn push_arm_borrow(&mut self, pat: &Pattern, scrut: &Expr, by_ref: bool) -> bool {
        let binds = match pat {
            Pattern::Some(Some((_, b, _)), _) | Pattern::Bind(_, b, _) | Pattern::Ok(Some((_, b, _)), _) => Some(*b),
            _ => None,
        };
        let b = match binds {
            Some(b) => b,
            None => return false,
        };
        let is_ref = self.local(b).map_or(false, |l| l.kind == LocalKind::PatRef);
        if !by_ref || !is_ref {
            return false;
        }
        let mutable = matches!(self.local(b).map(|l| &l.ty), Some(Ty::Ref(true, _)));
        let bname = self.local(b).map(|l| l.name.clone()).unwrap_or_default();
        let place = if self.s.lookup_scrut.contains(&scrut.id) {
            match self.s.methods.get(&scrut.id) {
                Some(MethodKind::ArenaGet(a)) | Some(MethodKind::ArenaGetMut(a)) => Some(Place { root: PRoot::Arena(*a), proj: Vec::new() }),
                Some(MethodKind::View(..)) | Some(MethodKind::Window(_)) => match &scrut.kind {
                    ExprKind::MethodCall { recv, .. } => self.place_of(recv),
                    _ => None,
                },
                Some(MethodKind::UserMemOf(u)) if self.s.t.umems[*u].task != usize::MAX => Some(Place { root: PRoot::Arena(self.s.t.umems[*u].task), proj: Vec::new() }),
                _ => None,
            }
        } else {
            self.place_of(scrut)
        };
        match place {
            Some(p) => {
                self.active.push(ActiveBorrow {
                    place: p,
                    mutable,
                    span: scrut.span,
                    what_ja: format!("束縛 `{}` が参照中", bname),
                    what_en: format!("referenced by the binding `{}`", bname),
                });
                true
            }
            None => false,
        }
    }

    /// 場所の式の鎖の中に読み出し専用の形（`R.get(i).unwrap()`）があれば、その位置 / where a read-only form sits in a place chain
    fn shared_root(&self, e: &Expr) -> Option<Span> {
        match &e.kind {
            ExprKind::Field(b, _, _) => self.shared_root(b),
            ExprKind::Index(b, _) => {
                if self.s.shared_places.contains(&e.id) {
                    Some(e.span)
                } else {
                    self.shared_root(b)
                }
            }
            _ => None,
        }
    }

    /// 代入先 / an assignment target
    fn assign_target(&mut self, lhs: &Expr, compound: bool) {
        let place = self.place_of(lhs);
        match &lhs.kind {
            // `*b = v`: own の中身への書き込み（own そのものは差し替えない）。移動済みの own には書けない（F0382）
            // `*b = v`: a write into the contents of an own (the own itself stays); a moved own cannot be written (F0382)
            ExprKind::Path(_) if self.s.own_derefs.contains(&lhs.id) => {
                if let Some(Res::Local(b)) = self.s.res.get(&lhs.id) {
                    let b = *b;
                    let (name, ty, mutable) = match self.local(b) {
                        Some(l) => (l.name.clone(), l.ty.clone(), l.mutable),
                        None => return,
                    };
                    match ty {
                        Ty::Ref(false, _) => {
                            self.err(Diag::error("F0596", lhs.span, format!("共有参照 `{}` の先は書き換えられません", name), format!("cannot assign through the shared reference `{}`", name)).hint("引数を `&mut own T` にしてください", "make the parameter `&mut own T`"));
                        }
                        Ty::Ref(true, _) => {}
                        _ => {
                            if !mutable {
                                self.err(
                                    Diag::error("F0384", lhs.span, format!("不変な変数 `{}` の own の中身は書き換えられません", name), format!("cannot assign to the contents of the own in the immutable variable `{}`", name))
                                        .hint(format!("`let mut {}` にしてください", name), format!("declare it as `let mut {}`", name)),
                                );
                            }
                        }
                    }
                    self.use_local(b, lhs.span, Use::Read);
                    if let Some(p) = &place {
                        self.check_access(p, Acc::Write, lhs.span);
                    }
                }
            }
            ExprKind::Path(_) => {
                if let Some(Res::Local(b)) = self.s.res.get(&lhs.id) {
                    let b = *b;
                    let (name, ty, mutable) = match self.local(b) {
                        Some(l) => (l.name.clone(), l.ty.clone(), l.mutable),
                        None => return,
                    };
                    if let Ty::Ref(m, inner) = ty {
                        // 参照を通した書き込み（`*p = v` に相当） / a write through the reference (`*p = v`)
                        if !m {
                            self.err(Diag::error("F0596", lhs.span, format!("共有参照 `{}` を通して書き換えることはできません", name), format!("cannot write through the shared reference `{}`", name)));
                        } else if let Ty::Own(_) = *inner {
                            // own の場所の束縛（`match w.ok { b => .. }`・`if let Some(b) = w.status`）は own の中身を指す参照
                            // （`&mut w.ok` と同じ）。own そのものの差し替えは元の場所への代入で行う
                            // a binding of an own place (`match w.ok { b => .. }`, `if let Some(b) = w.status`) refers to the
                            // contents of the own (as `&mut w.ok` does); the own itself is replaced by assigning the place
                            self.err(
                                Diag::error(
                                    "F0507",
                                    lhs.span,
                                    format!("参照 `{}` は own の中身を指しているので、own そのものは差し替えられません", name),
                                    format!("the reference `{}` points at the contents of an own, so the own itself cannot be replaced through it", name),
                                )
                                .hint(
                                    "own を差し替えるときは元の場所へ代入してください（例: `w.ok = own Button { .. }`）。中身だけを変えるならフィールドへ代入します",
                                    "to replace the own, assign to the original place (e.g. `w.ok = own Button { .. }`); to change its contents, assign its fields",
                                ),
                            );
                        }
                        return;
                    }
                    let state = self.st.vars.get(&b).copied().unwrap_or(VS::Init);
                    if compound {
                        self.use_local(b, lhs.span, Use::Read);
                    }
                    let first_init = matches!(state, VS::Uninit) && self.r.deferred_init.contains(&b);
                    if !mutable && !first_init {
                        let kind = self.local(b).map(|l| l.kind);
                        let (j, en) = if kind == Some(LocalKind::ForVar) {
                            ("ループ変数", "loop variable")
                        } else {
                            ("不変な変数", "immutable variable")
                        };
                        self.err(
                            Diag::error("F0384", lhs.span, format!("{} `{}` に代入できません", j, name), format!("cannot assign twice to {} `{}`", en, name))
                                .hint(format!("`let mut {}` にしてください", name), format!("declare it as `let mut {}`", name)),
                        );
                    }
                    if let Some(p) = &place {
                        self.check_access(p, Acc::Write, lhs.span);
                    }
                    self.st.vars.insert(b, VS::Init);
                    self.st.proven.retain(|(_, x)| *x != b);
                } else if let Some(Res::Static(si)) = self.s.res.get(&lhs.id) {
                    let n = self.s.t.statics[*si].name.clone();
                    self.err(Diag::error("F0384", lhs.span, format!("static `{}` は不変です（Kernel リージョン）", n), format!("static `{}` is immutable (Kernel region)", n)));
                }
            }
            // ポートへの書き込み（`com1 = c;` → out） / a write to a port (`com1 = c;` → out)
            ExprKind::ArenaRef(_) if self.s.port_reads.contains_key(&lhs.id) => {}
            ExprKind::Field(..) | ExprKind::Index(..) => {
                // 読み出し専用の形 `R.get(i).unwrap()` を通した書き込みは不可 / no writes through the read-only `R.get(i).unwrap()`
                if let Some(sp) = self.shared_root(lhs) {
                    self.err(
                        Diag::error("F0596", sp, "`.get(id).unwrap()` は読み出し専用の場所です。書き込むには `.get_mut(id).unwrap()`（または `[id]`）を使ってください", "`.get(id).unwrap()` is a read-only place; write through `.get_mut(id).unwrap()` (or `[id]`)")
                    );
                }
                self.place_base(lhs);
                // ルート変数の可変性 / mutability of the root variable
                if let Some(p) = &place {
                    match &p.root {
                        PRoot::Local(b) => {
                            if let Some(l) = self.local(*b) {
                                let ok = match &l.ty {
                                    Ty::Ref(m, _) => *m,
                                    // bind_mmio のハンドル: 装置のレジスタへの書き込みは値の変更ではない（ハンドルは不変のまま）
                                    // a bind_mmio handle: writing device registers does not change the handle itself
                                    Ty::MmioBind(_) => true,
                                    _ => l.mutable,
                                };
                                if !ok {
                                    let n = l.name.clone();
                                    let is_ref = matches!(l.ty, Ty::Ref(..));
                                    if is_ref {
                                        self.err(Diag::error("F0596", lhs.span, format!("共有参照 `{}` の先は書き換えられません", n), format!("cannot assign through the shared reference `{}`", n)).hint("引数を `&mut T` にしてください", "make the parameter `&mut T`"));
                                    } else {
                                        self.err(Diag::error("F0384", lhs.span, format!("不変な変数 `{}` の中身は書き換えられません", n), format!("cannot assign to a part of the immutable variable `{}`", n)).hint(format!("`let mut {}` にしてください", n), format!("declare it as `let mut {}`", n)));
                                    }
                                }
                            }
                            let b = *b;
                            if !self.is_copy_local(b) {
                                self.use_local(b, lhs.span, Use::Read);
                            }
                        }
                        PRoot::Static(si) => {
                            let n = self.s.t.statics[*si].name.clone();
                            self.err(Diag::error("F0384", lhs.span, format!("static `{}` は不変です（Kernel リージョン）", n), format!("static `{}` is immutable (Kernel region)", n)));
                        }
                        _ => {}
                    }
                    self.check_access(p, Acc::Write, lhs.span);
                }
            }
            _ => {
                self.expr(lhs, Use::Read);
                self.err(Diag::error("F0070", lhs.span, "この式には代入できません（変数・フィールド・添字に代入します）", "invalid assignment target (assign to a variable, a field or an index)"));
            }
        }
    }

    /// `&x` / `&mut x` 引数（借用は呼び出しの間だけ） / a `&x` / `&mut x` argument (borrowed for the call only)
    fn borrow_arg(&mut self, inner: &Expr, mutable: bool) -> Option<(Place, bool)> {
        if self.s.port_index.contains_key(&inner.id) || self.s.port_reads.contains_key(&inner.id) {
            self.err(Diag::error(
                "F2204",
                inner.span,
                "I/O ポートへの参照は作れません（値を読んで渡してください）",
                "references to I/O ports cannot be created (read the value and pass it)",
            ));
            return None;
        }
        if mutable {
            if let Some(sp) = self.shared_root(inner) {
                self.err(Diag::error("F0596", sp, "`.get(id).unwrap()` は読み出し専用の場所なので `&mut` で借りられません（`.get_mut(id).unwrap()` を使う）", "`.get(id).unwrap()` is read-only and cannot be borrowed as `&mut` (use `.get_mut(id).unwrap()`)"));
            }
        }
        if let Some(&(m, _)) = self.s.mmio_index.get(&inner.id) {
            let n = self.s.t.mmios[m].name.clone();
            self.err(Diag::error(
                "F2204",
                inner.span,
                format!("MMIO リージョン `{}` の中への参照は作れません（値を読んで渡してください）", n),
                format!("references into the MMIO region `{}` cannot be created (read the value and pass it)", n),
            ));
            return None;
        }
        if !crate::sema::is_place_expr(inner) {
            // 一時値への参照（`&make()`）: 値を作って借りるだけ / a reference to a temporary
            self.expr(inner, Use::Read);
            return None;
        }
        self.place_base(inner);
        let p = self.place_of(inner)?;
        if mutable {
            match &p.root {
                PRoot::Local(b) => {
                    if let Some(l) = self.local(*b) {
                        let ok = match &l.ty {
                            Ty::Ref(m, _) => *m,
                            _ => l.mutable,
                        };
                        if !ok {
                            let n = l.name.clone();
                            self.err(
                                Diag::error("F0596", inner.span, format!("不変な `{}` を `&mut` で借用できません", n), format!("cannot borrow the immutable `{}` as mutable", n))
                                    .hint(format!("`let mut {}` にしてください", n), format!("declare it as `let mut {}`", n)),
                            );
                        }
                    }
                }
                PRoot::Static(si) => {
                    let n = self.s.t.statics[*si].name.clone();
                    self.err(Diag::error("F0596", inner.span, format!("static `{}` は `&mut` で借用できません（不変）", n), format!("static `{}` cannot be borrowed mutably (immutable)", n)));
                }
                _ => {}
            }
        }
        self.check_access(&p, if mutable { Acc::MutBorrow } else { Acc::SharedBorrow }, inner.span);
        Some((p, mutable))
    }

    fn call(&mut self, e: &Expr, args: &[Expr]) {
        let kind = self.s.calls.get(&e.id).copied();
        // 引数ごとの (場所, アクセス) を集めて、同じ呼び出しの中の衝突を調べる
        // collect (place, access) per argument and check conflicts within the same call
        let mut accs: Vec<(Place, Acc, Span)> = Vec::new();
        let mut arena_refs: Vec<(usize, bool, Span)> = Vec::new();
        let params: Vec<Ty> = match kind {
            Some(CallKind::User(f)) => self.s.t.fns[f].params.iter().map(|p| p.ty.clone()).collect(),
            Some(CallKind::Indirect) => self.indirect_params(e),
            _ => Vec::new(),
        };
        for (i, a) in args.iter().enumerate() {
            let is_ref_param = matches!(params.get(i), Some(Ty::Ref(..)));
            match &a.kind {
                ExprKind::Ref { mutable, inner } if is_ref_param || matches!(kind, Some(CallKind::User(_)) | Some(CallKind::Indirect)) => {
                    if let Some((p, m)) = self.borrow_arg(inner, *mutable) {
                        if let PRoot::Arena(ar) = p.root {
                            arena_refs.push((ar, m, inner.span));
                        }
                        accs.push((p, if m { Acc::MutBorrow } else { Acc::SharedBorrow }, a.span));
                    }
                }
                ExprKind::Path(_) if is_ref_param => {
                    // 参照の束縛をそのまま渡す（再借用） / pass an existing reference binding on (reborrow)
                    if let Some(Res::Local(b)) = self.s.res.get(&a.id) {
                        let m = matches!(params.get(i), Some(Ty::Ref(true, _)));
                        let p = Place { root: PRoot::Local(*b), proj: Vec::new() };
                        self.check_access(&p, if m { Acc::MutBorrow } else { Acc::SharedBorrow }, a.span);
                        accs.push((p, if m { Acc::MutBorrow } else { Acc::SharedBorrow }, a.span));
                    }
                }
                // `copy_to_user(ubuf, ..)` の ubuf: ユーザーの番地への書き出し（ユーザーメモリは読まない）。copy_from_user の後でもよい
                // the ubuf of `copy_to_user(ubuf, ..)`: a write to the user address (user memory is not read); fine after copy_from_user
                ExprKind::Path(_) if i == 0 && matches!(kind, Some(CallKind::Builtin(crate::sema::Builtin::CopyToUser))) => {}
                _ => {
                    let consume = match kind {
                        Some(CallKind::User(_)) | Some(CallKind::Some) | Some(CallKind::Indirect) | Some(CallKind::Ok) | Some(CallKind::Err) => true,
                        Some(CallKind::Builtin(crate::sema::Builtin::Drop)) => true,
                        // copy_from_user はユーザーメモリを 1 回だけ読む（2 回目は F3101）・submit_dma は所有権をデバイスへ（以後は F3401）
                        // copy_from_user reads user memory once (a second time is F3101); submit_dma hands ownership to the device (F3401 after)
                        Some(CallKind::Builtin(crate::sema::Builtin::CopyFromUser)) | Some(CallKind::Builtin(crate::sema::Builtin::SubmitDma)) => {
                            if let Some(Res::Local(lb)) = self.s.res.get(&a.id) {
                                if matches!(kind, Some(CallKind::Builtin(crate::sema::Builtin::SubmitDma))) {
                                    self.lent.insert(*lb, a.span);
                                } else {
                                    self.user_consumed.insert(*lb, a.span);
                                }
                            }
                            true
                        }
                        // 別のコアへ渡した値は移動して消える（以後の使用は F0382） / a value handed to another core is moved away
                        Some(CallKind::Builtin(crate::sema::Builtin::SpawnOn)) => i == 2,
                        _ => false,
                    };
                    if let Some(p) = self.place_of(a) {
                        let copy = self.s.t.is_copy(self.ty(a));
                        accs.push((p, if consume && !copy { Acc::Move } else { Acc::Read }, a.span));
                    }
                    self.expr(a, if consume { Use::Move } else { Use::Read });
                }
            }
        }
        for i in 0..accs.len() {
            for j in 0..i {
                let (pa, aa, sa) = &accs[j];
                let (pb, ab, sb) = &accs[i];
                if !overlaps(pa, pb) {
                    continue;
                }
                let both_mut = *aa == Acc::MutBorrow && *ab == Acc::MutBorrow;
                let one_mut = *aa == Acc::MutBorrow || *ab == Acc::MutBorrow;
                let moved = *aa == Acc::Move || *ab == Acc::Move;
                let any_borrow = matches!(aa, Acc::MutBorrow | Acc::SharedBorrow) || matches!(ab, Acc::MutBorrow | Acc::SharedBorrow);
                let name = self.root_name(pb);
                let arena = matches!(pb.root, PRoot::Arena(_));
                if both_mut {
                    let mut d = Diag::error(
                        "F0499",
                        *sb,
                        format!("同じ呼び出しで `{}` を `&mut` で 2 回借用しています", name),
                        format!("`{}` is mutably borrowed twice in the same call", name),
                    )
                    .note(*sa, "1 回目の可変借用", "first mutable borrow");
                    if arena {
                        d = d.hint(
                            "2 つの Id が同じスロットを指すかもしれません。片方の値を先にローカルへ写すか、呼び出しを分けてください",
                            "the two Ids may denote the same slot; copy one side into a local first, or split the call",
                        );
                    } else {
                        d = d.hint("呼び出しを分けるか、別々のフィールドを渡してください", "split the call, or pass separate fields");
                    }
                    self.err(d);
                } else if moved && any_borrow {
                    self.err(Diag::error("F0505", *sb, format!("借用中の `{}` を同じ呼び出しで移動しています", name), format!("`{}` is moved while borrowed in the same call", name)).note(*sa, "借用はここ", "borrowed here").hint("`.clone()` するか呼び出しを分けてください", "`.clone()` it or split the call"));
                } else if one_mut {
                    self.err(Diag::error("F0502", *sb, format!("`{}` を可変借用している呼び出しの中で、同時に読んでいます", name), format!("`{}` is read while mutably borrowed in the same call", name)).note(*sa, "可変借用はここ", "mutably borrowed here").hint("読む値を呼び出しの前に一時変数へ写してください", "copy the value into a temporary before the call"));
                }
            }
        }
        if let Some(CallKind::User(f)) = kind {
            self.check_call_effects(f, e.span, &arena_refs);
            // 解放の効果を持つ関数を呼ぶと、そのアリーナの生存証明は消える / calling a freeing function kills proofs
            let frees = self.s.t.fns[f].effects.frees.clone();
            self.st.proven.retain(|(a, _)| !frees.contains(a));
        }
        // wait_irq(): 待っている間だけ割り込みが入る（閉じた区間の中から呼んでも同じ）。すべての実行ルートの効果を持つ
        // 呼び出しとして検査する（借用 F2502・生存証明の失効）
        // wait_irq(): interrupts arrive while it waits (even when called inside a closed section); checked as a call carrying
        // the effects of every execution root (borrows F2502, expiry of liveness proofs)
        if let Some(CallKind::Builtin(crate::sema::Builtin::WaitIrq)) = kind {
            let eff = self.s.ctx.env_all.clone();
            self.check_effects(&eff, "wait_irq", None, e.span, &arena_refs);
            self.st.proven.clear();
        }
        if let Some(CallKind::Indirect) = kind {
            // 関数ポインタの呼び出し: 呼ばれうる関数すべての効果の和で検査する（F2502）
            // a call through a function pointer: checked against the union of the effects of every function it may
            // reach (F2502)
            let (cands, eff) = self.indirect_effects(e);
            let name = match &e.kind {
                ExprKind::Call { callee, .. } => callee.clone(),
                _ => "fn(..)".to_string(),
            };
            self.check_effects(&eff, &name, Some(&cands), e.span, &arena_refs);
            let frees = eff.frees.clone();
            self.st.proven.retain(|(a, _)| !frees.contains(a));
        }
    }

    fn method(&mut self, e: &Expr, recv: &Expr, args: &[Expr], u: Use) {
        let k = self.s.methods.get(&e.id).copied();
        match k {
            Some(MethodKind::ArenaAlloc(_)) => {
                for a in args {
                    self.expr(a, Use::Move);
                }
            }
            // ページの所有権はアドレス空間へ移る（以後の使用は F0382） / the page's ownership moves into the address space
            Some(MethodKind::ProcMapPage(_)) => {
                for (i, a) in args.iter().enumerate() {
                    self.expr(a, if i == 2 { Use::Move } else { Use::Read });
                }
            }
            Some(MethodKind::ArenaFree(a)) | Some(MethodKind::ArenaRemove(a)) => {
                // dma_arena・gpu_arena の free はハンドルを消費する（移動） / free on a dma_arena / gpu_arena consumes the
                // handle (a move)
                let u_arg = if self.s.t.arenas[a].dma || self.s.t.arenas[a].gpu { Use::Move } else { Use::Read };
                for x in args {
                    self.expr(x, u_arg);
                }
                let p = Place { root: PRoot::Arena(a), proj: Vec::new() };
                self.check_access(&p, Acc::Write, e.span);
                self.st.proven.retain(|(x, _)| *x != a);
            }
            Some(MethodKind::ArenaGet(a)) | Some(MethodKind::ArenaGetMut(a)) => {
                for x in args {
                    self.expr(x, Use::Read);
                }
                let p = Place { root: PRoot::Arena(a), proj: Vec::new() };
                self.check_access(&p, Acc::Read, e.span);
            }
            Some(MethodKind::ArenaContains(_)) | Some(MethodKind::ArenaLen(_)) | Some(MethodKind::ArenaCapacity(_)) | Some(MethodKind::ArenaValidate(_)) => {
                for x in args {
                    self.expr(x, Use::Read);
                }
            }
            // チャネルへ送った値は移動して消える（以後の使用は F0382。エスケープするので、フレームではなくヒープに置かれる）
            // a value sent into a channel is moved away (later use is F0382; it escapes, so it lives in the heap, not the frame)
            Some(MethodKind::ChanSend(_)) => {
                for x in args {
                    self.expr(x, Use::Move);
                }
            }
            Some(MethodKind::ChanRecv(_)) | Some(MethodKind::ChanRecvWait(_)) => {}
            // 貸し出し: ハンドルは移動して消える（以後の使用は F0382） / lending: the handle is moved away (later use is F0382)
            Some(MethodKind::DmaLend(a)) => {
                for x in args {
                    self.expr(x, Use::Move);
                }
                let p = Place { root: PRoot::Arena(a), proj: Vec::new() };
                self.check_access(&p, Acc::Write, e.span);
            }
            // 装置へ出す: ハンドル（4 つ目の引数）は移動して消える（以後の使用は F0382） / offloading: the handle (the fourth
            // argument) is moved away (later use is F0382)
            Some(MethodKind::GpuDispatch(a)) => {
                for (i, x) in args.iter().enumerate() {
                    self.expr(x, if i == 3 { Use::Move } else { Use::Read });
                }
                let p = Place { root: PRoot::Arena(a), proj: Vec::new() };
                self.check_access(&p, Acc::Write, e.span);
            }
            Some(MethodKind::GpuReclaim(_)) | Some(MethodKind::GpuReclaimWait(_)) => {
                for x in args {
                    self.expr(x, Use::Read);
                }
            }
            Some(MethodKind::DmaReclaim(_)) | Some(MethodKind::DmaRegionPhys(_)) | Some(MethodKind::DmaRegionLen(_)) => {
                for x in args {
                    self.expr(x, Use::Read);
                }
            }
            Some(MethodKind::TaskCurrent(_)) | Some(MethodKind::TaskResume(_)) | Some(MethodKind::TaskEnter(_)) => {
                for x in args {
                    self.expr(x, Use::Read);
                }
            }
            // 切り替えうる呼び出し: すべての実行ルートの効果を持つ呼び出しとして検査する（借用 F2502・生存証明の失効）
            // a call that may switch: checked as a call carrying the effects of every execution root (borrows F2502,
            // expiry of liveness proofs)
            Some(MethodKind::TaskYield(_)) | Some(MethodKind::TaskExit(_)) => {
                for x in args {
                    self.expr(x, Use::Read);
                }
                let eff = self.s.ctx.env_all.clone();
                self.check_effects(&eff, "yield_to", None, e.span, &[]);
                self.st.proven.clear();
            }
            Some(MethodKind::MmioFill(_)) | Some(MethodKind::MmioPut(_)) | Some(MethodKind::MmioFillRect(_)) | Some(MethodKind::MmioLen(_)) | Some(MethodKind::MmioWidth(_)) | Some(MethodKind::MmioHeight(_)) => {
                for x in args {
                    self.expr(x, Use::Read);
                }
            }
            Some(MethodKind::Take) => {
                if let Some(p) = self.place_of(recv) {
                    self.place_base(recv);
                    self.check_access(&p, Acc::Write, recv.span);
                } else {
                    self.expr(recv, Use::Read);
                }
            }
            Some(MethodKind::Unwrap) | Some(MethodKind::UnwrapOr) => {
                // 受け手を消費する / consumes the receiver
                let copy = self.s.t.is_copy(self.ty(recv).strip_ref());
                if crate::sema::is_place_expr(recv) && !copy {
                    if let ExprKind::Path(_) = recv.kind {
                        self.place_value(recv, Use::Move);
                    } else {
                        self.place_value(recv, Use::Move);
                    }
                } else {
                    self.expr(recv, if copy { Use::Read } else { Use::Move });
                }
                for a in args {
                    self.expr(a, Use::Move);
                }
            }
            _ => {
                // clone / is_some / len / byte / index ... は読むだけ / read only
                if crate::sema::is_place_expr(recv) {
                    self.place_value(recv, Use::Read);
                } else {
                    self.expr(recv, Use::Read);
                }
                for a in args {
                    self.expr(a, Use::Read);
                }
            }
        }
        let _ = u;
    }
}

fn proven_alloc_unwrap(s: &Sema, e: &Expr) -> Option<usize> {
    if let ExprKind::MethodCall { recv, .. } = &e.kind {
        if let Some(MethodKind::Unwrap) = s.methods.get(&e.id) {
            if let ExprKind::MethodCall { .. } = &recv.kind {
                if let Some(MethodKind::ArenaAlloc(a)) = s.methods.get(&recv.id) {
                    return Some(*a);
                }
            }
        }
    }
    None
}

fn contains_fact(s: &Sema, cond: &Expr) -> Option<(usize, BindId)> {
    if let ExprKind::MethodCall { args, .. } = &cond.kind {
        if let Some(MethodKind::ArenaContains(a)) = s.methods.get(&cond.id) {
            if let Some(x) = args.first() {
                if let Some(Res::Local(b)) = s.res.get(&x.id) {
                    return Some((*a, *b));
                }
            }
        }
    }
    None
}
