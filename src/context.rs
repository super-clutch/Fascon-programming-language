//! 実行ルートと文脈の解析: 割り込みハンドラ・タスク・閉じた区間（critical）
//! execution roots and contexts: interrupt handlers, tasks and closed sections (critical)
//!
//! 割り込まれうる点（開いた区間）では、ほかの実行ルートの効果（環境効果 E_env）が起きるとみなす。
//!   - 協調モード（どの irq も `resume` を呼ばない）: E_env = 割り込みハンドラの効果の和
//!   - プリエンプティブモード（irq から届く関数が `resume` を呼ぶ）: E_env = 割り込みハンドラ + ほかのタスクの効果の和
//! 開いた区間で E_env と衝突するアリーナに触れば F2601（`critical { .. }` で囲む）。切り替えうる呼び出し
//! （`yield_to`・`exit`）は、すべてのルートの効果を持つ呼び出しとして扱う（借用 F2502 と生存証明の失効は既存の規則のまま）。
//!
//! At every point that can be interrupted (an open section) the effects of the other execution roots (the environment
//! effect E_env) are assumed to happen.
//!   - cooperative mode (no irq calls `resume`): E_env = the union of the interrupt handlers' effects
//!   - preemptive mode (a function reachable from an irq calls `resume`): E_env = interrupt handlers + the other tasks
//! Touching an arena that conflicts with E_env in an open section is F2601 (wrap it in `critical { .. }`). A call that
//! may switch (`yield_to`, `exit`) is treated as a call carrying the effects of every root, so borrows (F2502) and the
//! expiry of liveness proofs follow the existing rules.

use crate::ast::*;
use crate::diag::Diag;
use crate::flow::{children, Child};
use crate::sema::{Builtin, CallKind, MethodKind, Sema};
use crate::span::Span;
use crate::types::{Effects, RegionRef};
use std::collections::HashSet;

#[derive(Clone, Debug, Default)]
pub struct CtxInfo {
    /// 割り込みハンドラかタスクがある（閉じた区間に意味がある） / the program has interrupt handlers or tasks
    pub any_roots: bool,
    /// プリエンプティブモード / preemptive mode
    pub preemptive: bool,
    /// 割り込みハンドラ（関数の番号） / interrupt handlers (function indices)
    pub irq_fns: Vec<usize>,
    /// タスクの入口（関数の番号） / task entries (function indices)
    pub task_entries: Vec<usize>,
    /// すべてのルートの効果の和（切り替えうる呼び出しの効果） / the union of every root's effects (the effect of a call that may switch)
    pub env_all: Effects,
    /// 共有されるアリーナ（報告用: アリーナ・相手のルートの名前） / shared arenas (for reports: arena, the other root's name)
    pub shared: Vec<(usize, String)>,
    /// 閉じた区間の数 / number of closed sections
    pub criticals: u32,
}

#[derive(Clone, Copy)]
struct Access {
    arena: usize,
    write: bool,
    span: Span,
    /// main の起動時（irq_open より前） / in main before irq_open
    boot: bool,
    /// 閉じた区間の中 / inside a closed section
    closed: bool,
    /// このコアの起動時（irq_open / enter より前。spawn_on は数えない）: コアごとの実体には、まだ割り込みもタスクも来ない
    /// boot of this core (before irq_open / enter; spawn_on does not count): nothing interrupts the per-core instances yet
    boot_local: bool,
    /// ほかのコアに対しても守られている（critical・par()・atomic・rcu_read の中。irq の本体だけでは守られない）
    /// protected against other cores too (inside critical, par(), atomic or rcu_read; an irq body alone is not)
    locked: bool,
}

/// 呼び出しの場所の文脈 / the context of a call site
#[derive(Clone, Copy)]
struct CallSite {
    callee: usize,
    boot: bool,
    closed: bool,
    boot_local: bool,
    locked: bool,
}

#[derive(Default)]
struct FnFacts {
    accesses: Vec<Access>,
    /// 呼び出し（呼び出し先・起動時・閉じた区間の中） / calls (callee, at boot, inside a closed section)
    calls: Vec<CallSite>,
    switches: Vec<Span>,
    /// wait_irq() の場所（待っている間だけ割り込みが入る） / wait_irq() sites (interrupts arrive while it waits)
    waits: Vec<Span>,
    resumes: Vec<Span>,
    /// (入口の関数, ループの中か) / (entry function, inside a loop)
    starts: Vec<(Option<usize>, bool)>,
    /// `@t.start(..)` の入口（spawn_on は含まない） / entries of `@t.start(..)` (not spawn_on)
    task_starts: Vec<Option<usize>>,
    opens_irq: bool,
    criticals: u32,
    /// 装置（MMIO・ポート・dma のリング）へのアクセス: (リージョンの鍵, 場所, 起動時, ほかのコアに対して守られているか)
    /// device accesses (MMIO, ports, dma rings): (region key, site, at boot, protected against other cores)
    devs: Vec<(usize, Span, bool, bool)>,
    /// 並列走査の中で書けないもの: (場所, 何か ja, en) / what a parallel scan may not contain
    par_viol: Vec<(Span, String, String)>,
    /// 閉じた区間の中の、待つチャネル操作 / blocking channel operations inside a closed section
    chan_blocks: Vec<Span>,
    /// 装置の完了を待つ `reclaim_wait`: (場所, 閉じた区間の中か) / `reclaim_wait` waiting for the device: (site, inside a
    /// closed section)
    gpu_waits: Vec<(Span, bool)>,
    /// このコアの割り込みを開く（irq_open / enter） / opens the interrupts of this core (irq_open / enter)
    opens_local: bool,
    /// ロックを持ったまま（critical の中）の切り替え・割り込み待ち（マルチコアでは F2603）
    /// switches and interrupt waits while holding the lock (inside critical; F2603 on multi-core)
    locked_switches: Vec<Span>,
}

struct Walk<'a, 'p> {
    s: &'a Sema<'p>,
    f: FnFacts,
    boot: bool,
    /// このコアの起動時（spawn_on を数えない） / boot of this core (spawn_on does not count)
    boot_local: bool,
    closed: u32,
    /// ロックを持つ区間の深さ（critical・par()・atomic・rcu_read） / depth of lock-holding sections
    locked: u32,
    loops: u32,
    /// `for id in a.par()` の中 / inside a parallel scan
    par: u32,
    /// atomic・rcu_read の深さ（眠れない区間。Sleep 効果は F3203 で別に調べる） / depth of atomic / rcu_read (sections
    /// that cannot sleep; the Sleep effect is checked separately, F3203)
    sleepless: u32,
}

impl<'a, 'p> Walk<'a, 'p> {
    /// generic-os の共有アリーナの確保・解放はロックフリー / alloc and free of shared arenas are lock-free on generic-os
    fn lockfree(&self, a: usize) -> bool {
        self.s.profile == crate::target::Profile::GenericOs && self.s.t.arenas[a].shared
    }
    fn acc(&mut self, arena: usize, write: bool, span: Span) {
        if self.par > 0 && write && self.s.t.arenas[arena].shared {
            let an = self.s.t.arenas[arena].name.clone();
            self.f.par_viol.push((span, format!("共有アリーナ `{}` への書き込み・確保・解放", an), format!("a write, allocation or free on the shared arena `{}`", an)));
        }
        self.f.accesses.push(Access { arena, write, span, boot: self.boot, closed: self.closed > 0, boot_local: self.boot_local, locked: self.locked > 0 });
    }
    fn call_site(&self, callee: usize) -> CallSite {
        CallSite { callee, boot: self.boot, closed: self.closed > 0, boot_local: self.boot_local, locked: self.locked > 0 }
    }
    /// 装置のリージョンの鍵: MMIO は番号、ポートは 1000 + 番号、番地を直接使う組み込みは usize::MAX
    /// the key of a device region: an MMIO index, 1000 + a port index, usize::MAX for the raw-address builtins
    fn dev(&mut self, key: usize, span: Span) {
        self.f.devs.push((key, span, self.boot, self.locked > 0));
    }
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
            ExprKind::ArenaRef(name) => match self.s.t.regions.get(name) {
                Some(RegionRef::Mmio(m)) => self.dev(*m, e.span),
                Some(RegionRef::Port(p)) => self.dev(1000 + *p, e.span),
                // dma のリング（デバイスと共有する RAM）も装置と同じ扱い: ほかのコアと同時に書くと記述子が混ざる
                // dma rings (RAM shared with a device) are treated like devices: concurrent writes from cores mix descriptors
                Some(RegionRef::Dma(d)) => self.dev(2000 + *d, e.span),
                _ => {}
            },
            // atomic（1 フィールドの不可分な更新）と rcu_read（読み手。書き込みは F3202）は、その区間の中のアクセスを保護する
            // atomic (an indivisible update of one field) and rcu_read (a reader; writes are F3202) protect the accesses inside
            ExprKind::Atomic(b) | ExprKind::RcuRead(b) => {
                self.closed += 1;
                self.locked += 1;
                self.sleepless += 1;
                self.block(b);
                self.sleepless -= 1;
                self.locked -= 1;
                self.closed -= 1;
            }
            ExprKind::Critical(b) => {
                if self.par > 0 {
                    self.f.par_viol.push((e.span, "閉じた区間（`critical`）".to_string(), "a closed section (`critical`)".to_string()));
                }
                self.f.criticals += 1;
                self.closed += 1;
                self.locked += 1;
                self.block(b);
                self.locked -= 1;
                self.closed -= 1;
            }
            ExprKind::Index(base, idx) => {
                if let Some(&a) = self.s.arena_index.get(&e.id) {
                    self.acc(a, write, e.span);
                } else {
                    self.expr(base, write);
                }
                self.expr(idx, false);
            }
            ExprKind::Field(b, _, _) => self.expr(b, write),
            ExprKind::Assign { lhs, rhs, .. } => {
                self.expr(rhs, false);
                self.expr(lhs, true);
            }
            ExprKind::Ref { mutable, inner } => self.expr(inner, *mutable),
            ExprKind::For { iter, body, .. } => {
                // 並列走査: 読み手として入るので、中の読み出しは保護されている / a parallel scan enters as a reader, so reads inside are protected
                let par = self.s.par_fors.contains_key(&e.id);
                if par {
                    self.par += 1;
                    self.closed += 1;
                    self.locked += 1;
                }
                match iter {
                    ForIter::Arena(name, sp) => {
                        if let Some(a) = self.s.t.arena_of_name(name) {
                            self.acc(a, false, *sp);
                        }
                    }
                    ForIter::Range { lo, hi, .. } => {
                        self.expr(lo, false);
                        self.expr(hi, false);
                    }
                }
                self.loops += 1;
                self.block(body);
                self.loops -= 1;
                if par {
                    self.par -= 1;
                    self.closed -= 1;
                    self.locked -= 1;
                }
            }
            ExprKind::While { cond, body } => {
                self.loops += 1;
                self.expr(cond, false);
                self.block(body);
                self.loops -= 1;
            }
            ExprKind::Loop { body } => {
                self.loops += 1;
                self.block(body);
                self.loops -= 1;
            }
            ExprKind::Call { args, .. } | ExprKind::CallIndirect { args, .. } => {
                if let ExprKind::CallIndirect { callee, .. } = &e.kind {
                    self.expr(callee, false);
                }
                for a in args {
                    self.expr(a, false);
                }
                match self.s.calls.get(&e.id) {
                    Some(CallKind::User(g)) => {
                        if self.par > 0 {
                            let eff = &self.s.t.fns[*g].effects;
                            if let Some(&a) = eff.writes.iter().chain(eff.frees.iter()).chain(eff.allocs.iter()).find(|a| self.s.t.arenas[**a].shared) {
                                let (gn, an) = (self.s.t.fns[*g].name.clone(), self.s.t.arenas[a].name.clone());
                                self.f.par_viol.push((e.span, format!("`{}` の呼び出し（共有アリーナ `{}` を書き換える効果を持つ）", gn, an), format!("a call of `{}` (its effects write the shared arena `{}`)", gn, an)));
                            }
                        }
                        let cs = self.call_site(*g);
                        self.f.calls.push(cs)
                    }
                    // 別のコアの起動: ここから先の main は、ほかのコアと同時に動く 1 本のルート
                    // starting another core: from here on main is one root running alongside the other cores
                    Some(CallKind::Builtin(Builtin::SpawnOn)) => {
                        if let Some(fi) = self.s.spawns.iter().find(|x| x.0 == e.id).map(|x| x.1) {
                            self.f.starts.push((Some(fi), self.loops > 0));
                        }
                        self.f.opens_irq = true;
                        self.boot = false;
                    }
                    Some(CallKind::Builtin(Builtin::MmioRead(_))) | Some(CallKind::Builtin(Builtin::MmioWrite(_))) => self.dev(usize::MAX, e.span),
                    Some(CallKind::Indirect) => {
                        for g in self.s.indirect_cands.get(&e.id).cloned().unwrap_or_default() {
                            let cs = self.call_site(g);
                            self.f.calls.push(cs);
                        }
                    }
                    Some(CallKind::Builtin(Builtin::IrqOpen)) => {
                        self.f.opens_irq = true;
                        self.f.opens_local = true;
                        self.boot = false;
                        self.boot_local = false;
                    }
                    Some(CallKind::Builtin(Builtin::WaitIrq)) => {
                        self.f.waits.push(e.span);
                        if self.locked > 0 {
                            self.f.locked_switches.push(e.span);
                        }
                    }
                    _ => {}
                }
            }
            ExprKind::MethodCall { recv, args, .. } => {
                let k = self.s.methods.get(&e.id).copied();
                match k {
                    Some(MethodKind::Take) => self.expr(recv, true),
                    // dma のリングの番地・長さを読むだけ（中身には触れない） / only the address / length of a dma ring
                    Some(MethodKind::DmaRegionPhys(_)) | Some(MethodKind::DmaRegionLen(_)) => {}
                    _ => self.expr(recv, false),
                }
                for a in args {
                    self.expr(a, false);
                }
                match k {
                    Some(MethodKind::ArenaGet(a)) | Some(MethodKind::ArenaContains(a)) | Some(MethodKind::ArenaLen(a)) | Some(MethodKind::ArenaValidate(a)) => self.acc(a, false, e.span),
                    // generic-os の共有アリーナの free は Per-CPU マガジンと RCU の遅延回収でロックフリー（critical は要らない）
                    // on generic-os a free of a shared arena is lock-free (per-CPU magazines + RCU-deferred reclamation; no critical)
                    Some(MethodKind::ArenaFree(a)) | Some(MethodKind::ArenaRemove(a)) if self.lockfree(a) => {}
                    Some(MethodKind::ArenaGetMut(a)) | Some(MethodKind::ArenaFree(a)) | Some(MethodKind::ArenaRemove(a)) | Some(MethodKind::DmaLend(a)) | Some(MethodKind::DmaReclaim(a)) | Some(MethodKind::GpuDispatch(a)) | Some(MethodKind::GpuReclaim(a)) => self.acc(a, true, e.span),
                    // 装置の完了を待つ: 閉じた区間の中と割り込みから届く関数では F2603 / waiting for the device: F2603 inside
                    // closed sections and in functions reachable from interrupts
                    Some(MethodKind::GpuReclaimWait(a)) => {
                        self.acc(a, true, e.span);
                        // atomic / rcu_read だけの中なら Sleep 効果の検査（F3203）に任せる / inside atomic / rcu_read only,
                        // the Sleep-effect check (F3203) reports it
                        self.f.gpu_waits.push((e.span, self.closed > self.sleepless));
                    }
                    Some(MethodKind::ArenaAlloc(a)) if self.lockfree(a) && !self.s.task_starts.contains(&e.id) => {}
                    Some(MethodKind::ChanSend(_)) | Some(MethodKind::ChanRecvWait(_)) if self.closed > 0 => self.f.chan_blocks.push(e.span),
                    Some(MethodKind::ArenaAlloc(a)) => {
                        self.acc(a, true, e.span);
                        if self.s.task_starts.contains(&e.id) && !self.s.user_starts.contains(&e.id) {
                            let entry = match args.get(1).map(|x| &x.kind) {
                                Some(ExprKind::Path(n)) => self.s.t.fn_by_name.get(n).copied(),
                                _ => None,
                            };
                            self.f.starts.push((entry, self.loops > 0));
                            self.f.task_starts.push(entry);
                        }
                    }
                    Some(MethodKind::UserMemOf(u)) => {
                        let a = self.s.t.umems[u].task;
                        if a != usize::MAX {
                            self.acc(a, false, e.span);
                        }
                    }
                    Some(MethodKind::TaskYield(_)) | Some(MethodKind::TaskExit(_)) => {
                        self.f.switches.push(e.span);
                        if self.locked > 0 {
                            self.f.locked_switches.push(e.span);
                        }
                    }
                    Some(MethodKind::TaskResume(_)) => self.f.resumes.push(e.span),
                    Some(MethodKind::TaskEnter(_)) => {
                        if self.locked > 0 {
                            self.f.locked_switches.push(e.span);
                        }
                        self.f.opens_irq = true;
                        self.f.opens_local = true;
                        self.boot = false;
                        self.boot_local = false;
                    }
                    _ => {}
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

fn union(into: &mut Effects, e: &Effects) {
    for a in &e.reads {
        Effects::add(&mut into.reads, *a);
    }
    for a in &e.writes {
        Effects::add(&mut into.writes, *a);
    }
    for a in &e.frees {
        Effects::add(&mut into.frees, *a);
    }
    for a in &e.allocs {
        Effects::add(&mut into.allocs, *a);
    }
}

fn writes_any(e: &Effects, a: usize) -> bool {
    e.writes.contains(&a) || e.frees.contains(&a) || e.allocs.contains(&a)
}

fn touches_any(e: &Effects, a: usize) -> bool {
    writes_any(e, a) || e.reads.contains(&a)
}

/// ルート r から届く関数（f 自身を含む） / functions reachable from f (f included)
fn reach(start: usize, calls: &[Vec<usize>]) -> HashSet<usize> {
    let mut seen = HashSet::new();
    let mut stack = vec![start];
    while let Some(f) = stack.pop() {
        if seen.insert(f) {
            for &g in &calls[f] {
                stack.push(g);
            }
        }
    }
    seen
}

struct Root {
    f: usize,
    name: String,
    irq: bool,
    /// 同じ入口のタスクが 2 つ以上ありうる / more than one task may run this entry
    multi: bool,
    /// マルチコア: spawn_on だけで起動され、そのコアの割り込みを開かない（コアごとの実体を 1 人で使う）
    /// multi-core: started only by spawn_on and never opens the interrupts of its core (uses the per-core instances alone)
    core_only: bool,
    eff: Effects,
    reach: HashSet<usize>,
    span: Span,
}

pub fn analyze(s: &mut Sema) {
    let prog = s.prog;
    let n = s.t.fns.len();
    let mut facts: Vec<FnFacts> = Vec::with_capacity(n);
    let mut main_fn = None;
    // spawn_on で起動される関数: そのコアでは、irq_open / enter までは割り込みが来ない（コアごとの実体の起動時）
    // functions started by spawn_on: on their core no interrupt arrives before irq_open / enter (boot of the per-core instances)
    let spawn_fns: HashSet<usize> = s.spawns.iter().map(|x| x.1).collect();
    for (fi, f) in s.t.fns.iter().enumerate() {
        let local0 = f.is_main || (s.smp && spawn_fns.contains(&fi));
        let mut w = Walk { s, f: FnFacts::default(), boot: f.is_main, boot_local: local0, closed: 0, locked: 0, loops: 0, par: 0, sleepless: 0 };
        if let Item::Fn(fd) = &prog.items[f.item] {
            if f.irq.is_some() {
                w.closed = 1;
            }
            w.block(&fd.body);
        }
        if f.is_main {
            main_fn = Some(fi);
        }
        facts.push(w.f);
    }
    let all_calls: Vec<Vec<usize>> = facts.iter().map(|f| f.calls.iter().map(|c| c.callee).collect()).collect();
    let irq_fns: Vec<usize> = (0..n).filter(|&f| s.t.fns[f].irq.is_some()).collect();
    // タスクの入口: start の 2 番目の引数（関数の名前でなければ、`fn(u64)` として使われた関数すべて）
    // task entries: the second argument of start (every function used as a `fn(u64)` value when it is not a plain name)
    let mut entry_sites: Vec<(usize, bool, usize)> = Vec::new(); // (入口, ループの中, start を書いた関数)
    let mut unknown_entry = false;
    for (fi, ff) in facts.iter().enumerate() {
        for (e, in_loop) in &ff.starts {
            match e {
                Some(g) => entry_sites.push((*g, *in_loop, fi)),
                None => unknown_entry = true,
            }
        }
    }
    if unknown_entry {
        for &(g, _, _) in &s.fn_value_sites {
            let fnty = &s.t.fns[g];
            if fnty.params.len() == 1 && fnty.irq.is_none() {
                entry_sites.push((g, true, usize::MAX));
            }
        }
    }
    // システムコールとトラップは、EL0 のタスクの文脈で動く（何本も同時に途中になりうる）
    // system calls and traps run in the context of EL0 tasks (several may be in progress at once)
    for g in 0..n {
        if s.t.fns[g].trap || (s.t.fns[g].syscall.is_some() && !s.t.fns[g].stub) {
            entry_sites.push((g, true, usize::MAX));
        }
    }
    let mut task_entries: Vec<usize> = Vec::new();
    for (g, _, _) in &entry_sites {
        if !task_entries.contains(g) {
            task_entries.push(*g);
        }
    }
    // `@t.start(..)` でタスクとして起動される関数（入口が名前でなければ全部とみなす） / functions started as tasks by
    // `@t.start(..)` (all of them when an entry is not a plain name)
    let task_started: HashSet<usize> = if facts.iter().any(|f| f.task_starts.iter().any(|e| e.is_none())) {
        (0..n).collect()
    } else {
        facts.iter().flat_map(|f| f.task_starts.iter().flatten().copied()).collect()
    };
    let any_tasks = !task_entries.is_empty();
    let any_roots = !irq_fns.is_empty() || any_tasks;
    let criticals: u32 = facts.iter().map(|f| f.criticals).sum();
    // 閉じた区間の中で装置の完了を待つ reclaim_wait（F2603）: 割り込みやタスクが無いプログラムでも / reclaim_wait waiting
    // for the device inside a closed section (F2603), even in programs without interrupts or tasks
    for ff in facts.iter() {
        for (sp, closed) in &ff.gpu_waits {
            if *closed {
                s.diags.push(
                    Diag::error("F2603", *sp, "閉じた区間（`critical`・並列走査・割り込みハンドラ）の中では、装置の完了を待つ `reclaim_wait` は使えません", "`reclaim_wait`, which waits for the device, cannot be used inside a closed section (`critical`, a parallel scan or an interrupt handler)")
                        .hint("割り込みを止めたまま・ロックを持ったまま装置を待つと、完了の通知も、ほかのコアも止まりえます。区間の外で待つか、待たない `reclaim(f)` を使ってください", "waiting for the device with interrupts masked or the lock held can stall the completion notice and every other core; wait outside the section, or use the non-blocking `reclaim(f)`"),
                );
            }
        }
    }
    if !any_roots {
        s.ctx = CtxInfo { criticals, ..CtxInfo::default() };
        return;
    }
    // 割り込みから届く関数・タスクとして動く関数 / functions reachable from interrupts, functions running as tasks
    let mut in_irq: HashSet<usize> = HashSet::new();
    for &h in &irq_fns {
        in_irq.extend(reach(h, &all_calls));
    }
    let mut diags: Vec<Diag> = Vec::new();
    // このコアの中でのプリエンプション: 割り込みから届く関数が resume を呼ぶ / preemption within a core: a function
    // reachable from an irq calls resume
    let preemptive_local = in_irq.iter().any(|f| !facts[*f].resumes.is_empty());
    // コアをまたぐプログラムは、いつでも本当に同時に動く / a multi-core program is always truly concurrent
    let preemptive = s.smp || preemptive_local;

    // ルートの一覧 / the roots
    let mut roots: Vec<Root> = Vec::new();
    for &h in &irq_fns {
        roots.push(Root { f: h, name: s.t.fns[h].name.clone(), irq: true, multi: false, core_only: false, eff: s.t.fns[h].effects.clone(), reach: reach(h, &all_calls), span: s.t.fns[h].span });
    }
    for &g in &task_entries {
        let sites: Vec<&(usize, bool, usize)> = entry_sites.iter().filter(|x| x.0 == g).collect();
        let multi = sites.len() > 1 || sites.iter().any(|x| x.1 || Some(x.2) != main_fn);
        // spawn_on だけで起動され、そのコアの割り込みを開かない関数は、コアごとの実体を 1 人で使う（タスクに割り込まれない）
        // a function started only by spawn_on that never opens the interrupts of its core uses the per-core instances alone
        let only_spawned = s.smp && spawn_fns.contains(&g) && !task_started.contains(&g) && !s.t.fns[g].trap && !(s.t.fns[g].syscall.is_some() && !s.t.fns[g].stub);
        let core_only = only_spawned && !reach(g, &all_calls).iter().any(|f| facts[*f].opens_local);
        roots.push(Root { f: g, name: s.t.fns[g].name.clone(), irq: false, multi, core_only, eff: s.t.fns[g].effects.clone(), reach: reach(g, &all_calls), span: s.t.fns[g].span });
    }
    // main の irq_open より後の部分は 1 つのタスクとして扱う / the part of main after irq_open counts as one task
    if let Some(m) = main_fn {
        if facts[m].opens_irq {
            let mut eff = Effects::default();
            let mut r: HashSet<usize> = HashSet::new();
            r.insert(m);
            for a in facts[m].accesses.iter().filter(|a| !a.boot) {
                Effects::add(&mut eff.reads, a.arena);
                if a.write {
                    Effects::add(&mut eff.writes, a.arena);
                }
            }
            for c in &facts[m].calls {
                if !c.boot {
                    union(&mut eff, &s.t.fns[c.callee].effects);
                    r.extend(reach(c.callee, &all_calls));
                }
            }
            let core_only = s.smp && !facts[m].opens_local && !r.iter().any(|f| facts[*f].opens_local);
            roots.push(Root { f: m, name: "main".to_string(), irq: false, multi: false, core_only, eff, reach: r, span: s.t.fns[m].span });
        }
    }
    // 開いたまま入られうる関数 / functions that may be entered open
    let mut open = vec![false; n];
    for r in roots.iter().filter(|r| !r.irq) {
        open[r.f] = true;
    }
    loop {
        let mut changed = false;
        for f in 0..n {
            if !open[f] {
                continue;
            }
            for c in &facts[f].calls {
                // マルチコア: コアごとの実体にとっての「開いた」は、そのコアの起動時（boot_local）を除く
                // multi-core: "open" for per-core instances excludes the boot of this core (boot_local)
                let boot = if s.smp { c.boot_local } else { c.boot };
                if !boot && !c.closed && !open[c.callee] {
                    open[c.callee] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // マルチコア: ロックを持たずに入られうる関数（割り込みハンドラも、ほかのコアに対しては開いている）
    // multi-core: functions that may be entered without the lock (interrupt handlers are open to other cores, too)
    let mut unlocked = vec![false; n];
    if s.smp {
        for r in &roots {
            unlocked[r.f] = true;
        }
        loop {
            let mut changed = false;
            for f in 0..n {
                if !unlocked[f] {
                    continue;
                }
                for c in &facts[f].calls {
                    if !c.boot && !c.locked && !unlocked[c.callee] {
                        unlocked[c.callee] = true;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }
    // 全コアで 1 つの実体（共有アリーナ・マルチコアの dma_arena） / regions with one instance for every core (shared arenas,
    // and dma_arena on multi-core)
    let global = |a: usize| s.t.arenas[a].shared || (s.smp && (s.t.arenas[a].dma || s.t.arenas[a].gpu));
    // P1: 保護なしアクセス（F2601） / unprotected access
    let mut shared: Vec<(usize, String)> = Vec::new();
    let mut seen: HashSet<(u32, usize)> = HashSet::new();
    for f in 0..n {
        for a in &facts[f].accesses {
            // どの種類の保護が要るか: 1 コア → 割り込み禁止。マルチコアのコアごとの実体 → 割り込み禁止（そのコアの起動時は不要）。
            // マルチコアの共有の実体 → ロック（critical・par()）。irq の本体の「割り込み禁止」はほかのコアを止めない
            // which protection is needed: single core → interrupts masked. Per-core instances on multi-core → interrupts
            // masked (not during the boot of that core). Shared instances on multi-core → the lock (critical, par()); the
            // "interrupts masked" of an irq body does not stop other cores
            let g = s.smp && global(a.arena);
            let (entered, skip) = if !s.smp {
                (open[f], a.boot || a.closed)
            } else if g {
                (unlocked[f], a.boot || a.locked)
            } else {
                (open[f], a.boot_local || a.closed)
            };
            if !entered || skip {
                continue;
            }
            let mut culprit: Option<&Root> = None;
            for r in &roots {
                let hit = if a.write { touches_any(&r.eff, a.arena) } else { writes_any(&r.eff, a.arena) };
                if !hit {
                    continue;
                }
                if r.irq {
                    culprit = Some(r);
                    break;
                }
                // コアごとの実体: 割り込みを開かない spawn_on の関数は、自分のコアの実体を 1 人で使う。同じコアの中の
                // タスクどうしは、タイマーで切り替わるとき（resume）だけ競合する
                // per-core instances: a spawn_on function that never opens the interrupts of its core uses the instance
                // of its own core alone; tasks within one core race only when the timer switches them (resume)
                let p = if s.smp && !g { preemptive_local } else { preemptive };
                if s.smp && !g && r.core_only {
                    continue;
                }
                if p {
                    // 自分自身だけが触るアリーナは共有ではない（同じ入口のタスクが複数ありうるなら共有）
                    // an arena only this root touches is not shared (unless several tasks may run the same entry)
                    let only_self = roots.iter().filter(|q| !q.irq && q.reach.contains(&f) && !(s.smp && !g && q.core_only)).all(|q| q.f == r.f);
                    if r.multi || !only_self {
                        culprit = Some(r);
                        break;
                    }
                }
            }
            if let Some(r) = culprit {
                if !seen.insert((a.span.lo, a.arena)) {
                    continue;
                }
                let an = s.t.arenas[a.arena].name.clone();
                if !shared.iter().any(|x| x.0 == a.arena) {
                    shared.push((a.arena, r.name.clone()));
                }
                let (kj, ke) = if r.irq { ("割り込みハンドラ", "the interrupt handler") } else if s.smp { ("別のコアで動く関数", "the function running on another core") } else { ("タスク", "the task") };
                let (wj, we) = if a.write { ("書き込み", "write to") } else { ("読み出し", "read of") };
                let mut d = Diag::error(
                    "F2601",
                    a.span,
                    format!("共有リージョンへの保護なしアクセス: ここは割り込まれうる区間で、アリーナ `{}` は{} `{}` も触ります（この{}の途中で割り込まれると、スロットの状態が食い違います）", an, kj, r.name, wj),
                    format!("unprotected access to a shared region: this code can be interrupted, and the arena `{}` is also touched by {} `{}` (an interruption in the middle of this {} the slot would see inconsistent state)", an, ke, r.name, we),
                )
                .note(r.span, format!("{} `{}` はここ", kj, r.name), format!("{} `{}` is here", ke, r.name));
                if g && a.closed && !a.locked {
                    // irq の本体（割り込み禁止）でも、ほかのコアは同時に動く / even inside an irq body other cores run
                    d = d.note(a.span, "ここは割り込みを止めているだけで、ほかのコアは止まっていません（マルチコアの共有の実体にはロック = critical が要ります）", "this only masks the interrupts of this core; other cores keep running (a shared instance on multi-core needs the lock = critical)");
                }
                diags.push(d.hint(
                    "この文を `critical { .. }` で囲むか、関数を `critical fn` にしてください（閉じた区間は割り込まれません）",
                    "wrap the statement in `critical { .. }`, or make the function a `critical fn` (a closed section is not interrupted)",
                ));
            }
        }
    }
    // マルチコア: 並列走査の中身（F2612）、閉じた区間の中で待つチャネル操作（F2603）、装置への保護なしアクセス（F2613）
    // multi-core: the contents of parallel scans (F2612), blocking channel operations inside closed sections (F2603),
    // unprotected device accesses (F2613)
    for ff in facts.iter() {
        for (sp, ja, en) in &ff.par_viol {
            diags.push(
                Diag::error("F2612", *sp, format!("並列走査（`.par()`）の中には書けません: {}", ja), format!("not allowed inside a parallel scan (`.par()`): {}", en))
                    .hint(
                        "並列走査は読み手として入るので、ほかのコアの走査と同時に進めます。その間、共有アリーナは誰も書き換えられません。結果はローカル変数に集めて、ループの後で `critical { .. }` の中から書いてください",
                        "a parallel scan enters as a reader, so it runs alongside the scans of other cores; meanwhile nobody may modify shared arenas. Collect the results in locals and write them inside `critical { .. }` after the loop",
                    ),
            );
        }
        for sp in &ff.chan_blocks {
            diags.push(
                Diag::error("F2603", *sp, "閉じた区間（`critical`・並列走査）の中では、待つチャネル操作（send / recv_wait）は使えません", "blocking channel operations (send / recv_wait) cannot be used inside a closed section (`critical` or a parallel scan)")
                    .hint("相手のコアが同じ区間に入ろうとして待つと、どちらも進めなくなります。区間の外で送受信するか、待たない `recv()` を使ってください", "if the other core is waiting to enter the same section, neither can proceed; send and receive outside the section, or use the non-blocking `recv()`"),
            );
        }
    }
    if s.smp {
        let mut seen_dev: HashSet<u32> = HashSet::new();
        for f in 0..n {
            for &(key, sp, boot, locked) in &facts[f].devs {
                if boot || locked || !seen_dev.insert(sp.lo) {
                    continue;
                }
                // 同じ装置に届くルートが 2 本以上（または、何本も動きうる 1 本）。割り込みハンドラもほかのコアと同時に動く
                // two or more roots (or one that may run several times) reach the device; interrupt handlers run alongside
                // other cores too
                let touching: Vec<&Root> = roots.iter().filter(|r| r.reach.iter().any(|g| facts[*g].devs.iter().any(|d| d.0 == key && !(d.2 && r.f == *g && s.t.fns[*g].is_main)))).collect();
                let me: Vec<&&Root> = touching.iter().filter(|r| r.reach.contains(&f)).collect();
                if me.is_empty() {
                    continue;
                }
                let racy = touching.len() >= 2 || touching.iter().any(|r| r.multi || (r.irq && s.t.fns[r.f].irq.map_or(false, |i| i < 32)));
                if racy {
                    let other = touching.iter().find(|r| !r.reach.contains(&f)).or(touching.first()).map(|r| r.name.clone()).unwrap_or_default();
                    let dn = if key == usize::MAX {
                        "mmio_read / mmio_write".to_string()
                    } else if key >= 2000 {
                        format!("dma {}", s.t.dmas[key - 2000].name)
                    } else if key >= 1000 {
                        s.t.ports[key - 1000].name.clone()
                    } else {
                        s.t.mmios[key].name.clone()
                    };
                    diags.push(
                        Diag::error(
                            "F2613",
                            sp,
                            format!("装置への保護なしアクセス: `{}` は別のコアで動く `{}` からも読み書きされます（レジスタの読み書きの順序が混ざります）", dn, other),
                            format!("unprotected device access: `{}` is also read or written by `{}` running on another core (register accesses would interleave)", dn, other),
                        )
                        .hint("この文を `critical { .. }` で囲むか、関数を `critical fn` にしてください（装置を触る区間を 1 つのコアだけにします）", "wrap the statement in `critical { .. }`, or make the function a `critical fn` (so one core at a time touches the device)"),
                    );
                }
            }
        }
        // F2616: 共有の周辺の割り込み（SPI、INTID 32 以上）は CPU0 だけに届く。そのハンドラがコアごとの実体（ふつうの arena・
        // task_arena・bump_arena）に触ると、ほかのコアで作られた Id を CPU0 の表で引いてしまう（F2611 と同じ理由）
        // F2616: shared peripheral interrupts (SPI, INTID 32 and up) reach CPU0 only. A handler that touches per-core
        // instances (plain arena, task_arena, bump_arena) would look up Ids made on other cores in the table of CPU0 (the
        // reason behind F2611)
        for &h in &irq_fns {
            let intid = s.t.fns[h].irq.unwrap_or(0);
            if intid < 32 {
                continue;
            }
            let e = &s.t.fns[h].effects;
            let mut hit: Option<usize> = None;
            for &a in e.reads.iter().chain(e.writes.iter()).chain(e.frees.iter()).chain(e.allocs.iter()) {
                if !global(a) {
                    hit = Some(a);
                    break;
                }
            }
            if let Some(a) = hit {
                let an = s.t.arenas[a].name.clone();
                let hn = s.t.fns[h].name.clone();
                diags.push(
                    Diag::error(
                        "F2616",
                        s.t.fns[h].span,
                        format!("マルチコアのプログラムで、共有の周辺の割り込み（INTID {}）のハンドラ `{}` がコアごとのアリーナ `{}` に触っています（この割り込みは CPU0 にだけ届くので、ほかのコアで作った Id を CPU0 の表で引いてしまいます）", intid, hn, an),
                        format!("in a multi-core program the handler `{}` of a shared peripheral interrupt (INTID {}) touches the per-core arena `{}` (the interrupt reaches CPU0 only, so it would look up Ids made on other cores in the table of CPU0)", hn, intid, an),
                    )
                    .note(s.t.arenas[a].span, format!("`{}` はコアごとに 1 つの実体です", an), format!("`{}` has one instance per core", an))
                    .hint(
                        format!("`{}` を `shared arena` にして `critical {{ .. }}` の中で触るか、受け取ったデータを `chan` で担当のコアへ渡してください。コアごとの割り込み（タイマーの INTID 27 など 16〜31）のハンドラは、どのコアでも自分の実体に触れます", an),
                        format!("make `{}` a `shared arena` and touch it inside `critical {{ .. }}`, or hand the data to the owning core through a `chan`. Handlers of per-core interrupts (16 to 31, e.g. the timer at INTID 27) may touch the instances of their own core", an),
                    ),
                );
            }
        }
    }
    // P4: 割り込みから届く関数は切り替えられない / P5: resume は割り込みの中だけ
    // P4: nothing reachable from an interrupt may switch / P5: resume only inside interrupts
    let mut task_reach: HashSet<usize> = HashSet::new();
    for r in roots.iter().filter(|r| !r.irq) {
        task_reach.extend(r.reach.iter().copied());
    }
    if let Some(m) = main_fn {
        task_reach.extend(reach(m, &all_calls));
    }
    for f in 0..n {
        if in_irq.contains(&f) {
            for sp in &facts[f].switches {
                diags.push(
                    Diag::error(
                        "F2603",
                        *sp,
                        "割り込みの文脈ではタスクを切り替えられません（この関数は割り込みハンドラから届きます）",
                        "tasks cannot be switched in interrupt context (this function is reachable from an interrupt handler)",
                    )
                    .hint("割り込みの中では `resume(id)` で、割り込みの出口での切り替えを予約します", "inside an interrupt use `resume(id)`, which requests the switch on interrupt exit"),
                );
            }
        }
        if in_irq.contains(&f) {
            for (sp, closed) in &facts[f].gpu_waits {
                if !*closed {
                    diags.push(
                        Diag::error("F2603", *sp, "割り込みの文脈では `reclaim_wait` を呼べません（この関数は割り込みハンドラから届きます。割り込みの中では装置を待てません）", "`reclaim_wait` cannot be called in interrupt context (this function is reachable from an interrupt handler, which cannot wait for the device)")
                            .hint("割り込みの中では待たない `reclaim(f)` で確かめ、終わっていなければタスクに任せてください", "inside an interrupt check with the non-blocking `reclaim(f)` and leave unfinished work to a task"),
                    );
                }
            }
            for sp in &facts[f].waits {
                diags.push(Diag::error(
                    "F2603",
                    *sp,
                    "割り込みの文脈では `wait_irq()` を呼べません（この関数は割り込みハンドラから届きます。割り込みの中では次の割り込みは入りません）",
                    "`wait_irq()` cannot be called in interrupt context (this function is reachable from an interrupt handler, where no further interrupt arrives)",
                ));
            }
        }
        if task_reach.contains(&f) || !in_irq.contains(&f) {
            for sp in &facts[f].resumes {
                diags.push(
                    Diag::error(
                        "F2603",
                        *sp,
                        "`resume` は割り込みハンドラの中（とそこからだけ呼ばれる関数）でしか使えません",
                        "`resume` can only be used inside an interrupt handler (and functions called only from one)",
                    )
                    .hint("タスクの中では `yield_to(id)` を使います", "inside a task use `yield_to(id)`"),
                );
            }
        }
    }
    // 切り替えうる関数は、すべてのルートの効果を持つ / a function that may switch carries the effects of every root
    let mut env_all = Effects::default();
    for r in &roots {
        union(&mut env_all, &r.eff);
    }
    let mut switcher = vec![false; n];
    for f in 0..n {
        switcher[f] = !facts[f].switches.is_empty() || !facts[f].waits.is_empty();
    }
    loop {
        let mut changed = false;
        for f in 0..n {
            if !switcher[f] && all_calls[f].iter().any(|g| switcher[*g]) {
                switcher[f] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // マルチコア: ロック（critical）を持ったままの切り替え・割り込み待ち・タスクへの入場は F2603。ロックを持ったタスクが
    // 眠ると、ほかのコアはそのロックを待って止まり、同じコアの次のタスクはロックを持っているとみなされてしまう
    // multi-core: switching, waiting for an interrupt or entering a task while holding the lock (critical) is F2603. A task
    // that sleeps with the lock stops every other core waiting for it, and the next task on the same core would be taken
    // for the lock holder
    if s.smp {
        let mut seen_sw: HashSet<u32> = HashSet::new();
        for f in 0..n {
            let mut sites: Vec<(Span, Option<String>)> = facts[f].locked_switches.iter().map(|sp| (*sp, None)).collect();
            if let Item::Fn(_) = &prog.items[s.t.fns[f].item] {
                for c in &facts[f].calls {
                    if c.locked && switcher[c.callee] {
                        sites.push((s.t.fns[f].span, Some(s.t.fns[c.callee].name.clone())));
                    }
                }
            }
            for (sp, via) in sites {
                if !seen_sw.insert(sp.lo) {
                    continue;
                }
                let (ja, en) = match &via {
                    Some(g) => (format!("ロック（`critical`）を持ったまま、タスクを切り替えうる `{}` を呼んでいます", g), format!("`{}`, which may switch tasks, is called while holding the lock (`critical`)", g)),
                    None => ("ロック（`critical`）を持ったまま、タスクの切り替え・割り込み待ち（yield_to / exit / enter / wait_irq）はできません".to_string(), "no task switch or interrupt wait (yield_to / exit / enter / wait_irq) while holding the lock (`critical`)".to_string()),
                };
                diags.push(Diag::error("F2603", sp, ja, en).hint(
                    "マルチコアの `critical` はコアの間のロックです。区間を閉じてから切り替えてください（ロックを持ったまま眠ると、ほかのコアがロックを待って止まります）",
                    "on multi-core `critical` is a lock between cores; close the section before switching (sleeping with the lock stops every core that waits for it)",
                ));
            }
        }
    }
    for f in 0..n {
        if switcher[f] {
            let mut e = s.t.fns[f].effects.clone();
            union(&mut e, &env_all);
            s.t.fns[f].effects = e;
        }
    }
    for d in diags {
        s.diags.push(d);
    }
    s.ctx = CtxInfo { any_roots, preemptive, irq_fns, task_entries, env_all, shared, criticals };
}
