//! ユニバーサル OS 拡張の静的検査（型検査と効果の計算の後に 1 回だけ走る）
//! static checks of the universal-OS extension (run once, after type checking and the effect computation)
//!
//! - Sleep 効果の推移閉包（`schedule()`・`sleep_ticks()`・`rcu_synchronize()`・`#[sleep]`・`#[asm(effects = "sleep")]`、
//!   それに 1.0 の待つ操作 `wait_irq()`・`yield_to`・`exit`・`send`・`recv_wait`）
//!   the transitive Sleep effect (the new sleeping builtins and attributes, plus the blocking operations of 1.0)
//! - F3001: `--target rtos` の再帰（関数ポインタの候補を含む呼び出しグラフの閉路）/ recursion under `--target rtos`
//! - F3202: `rcu_read { }` の中の書き込み・確保・解放（その効果を持つ関数の呼び出しを含む）
//!   writes, allocations and frees inside `rcu_read { }` (including calls of functions with those effects)
//! - F3203: Sleep 効果の呼び出しを `rcu_read` / `atomic` / `#[trap]` の中で、または第二級参照が生きている間に行う
//!   a Sleep call inside `rcu_read` / `atomic` / `#[trap]`, or while a second-class reference is live
//! - F3301〜F3304: `#[secret]` の複製・出力（F3301）、秘密に依存する分岐・比較・番地（F3302）、asm のオペランド（F3303）、
//!   `ct_eq` の型（F3304）/ copies and output of secrets, secret-dependent branches / comparisons / addresses, asm operands,
//!   the operand types of `ct_eq`
//! - F3402: 構造体のフィールドに `dma own T` を直接置いた（`Option<dma own T>` にする）/ a bare `dma own T` struct field
//! - F3501・F3502: `#[on_drop(f)]` の関数の形、`#[stable_abi]` のフィールドの型 / the shape of `#[on_drop(f)]` functions and the
//!   field types of `#[stable_abi]` structs
use crate::ast::*;
use crate::diag::Diag;
use crate::flow::{children, Child};
use crate::sema::{Builtin, CallKind, LocalKind, MethodKind, Res, Sema};
use crate::span::Span;
use crate::target::Profile;
use crate::types::Ty;
use std::collections::{HashMap, HashSet};

/// 解析の結果（C の生成でも使う） / results shared with C generation
#[derive(Default, Clone, Debug)]
pub struct UniversalInfo {
    /// 関数ごとに眠りうるか（推移的。1.0 の待つ操作を含む） / whether each function may sleep (transitively, 1.0 blocking ops included)
    pub sleeps: Vec<bool>,
    /// 新しい Sleep 効果の源（schedule など）から眠りうるか / whether it may sleep through the new Sleep sources
    pub sleeps_new: Vec<bool>,
    /// 再帰する関数（呼び出しグラフの閉路に乗る） / functions on a cycle of the call graph
    pub recursive: Vec<bool>,
    /// gc_arena の確保・gc_collect・promote_to_gc・自動昇格を（推移的に）行う関数 / functions that allocate in a gc_arena,
    /// collect, promote or auto-promote (transitively)
    pub gc_ops: Vec<bool>,
}

/// gc_arena の操作を（推移的に）行う関数の表 / the table of functions that perform gc_arena operations (transitively)
fn compute_gc_ops(s: &Sema) -> Vec<bool> {
    let n = s.t.fns.len();
    let mut t = vec![false; n];
    if !s.t.arenas.iter().any(|a| a.gc) {
        return t;
    }
    fn direct(s: &Sema, e: &Expr) -> bool {
        if s.gc_calls.contains_key(&e.id) || s.auto_promote.contains_key(&e.id) {
            return true;
        }
        if let Some(MethodKind::ArenaAlloc(a)) = s.methods.get(&e.id) {
            if s.t.arenas[*a].gc {
                return true;
            }
        }
        children(e).into_iter().any(|c| match c {
            Child::E(x) => direct(s, x),
            Child::B(b) => block_direct(s, b),
        })
    }
    fn block_direct(s: &Sema, b: &Block) -> bool {
        b.stmts.iter().any(|st| match st {
            Stmt::Let(l) => l.init.as_ref().map_or(false, |i| direct(s, i)),
            Stmt::Expr(e) => direct(s, e),
        }) || b.tail.as_ref().map_or(false, |x| direct(s, x))
    }
    for (fi, f) in s.t.fns.iter().enumerate() {
        if let Item::Fn(fd) = &s.prog.items[f.item] {
            t[fi] = block_direct(s, &fd.body);
        }
    }
    loop {
        let mut changed = false;
        for fi in 0..n {
            if !t[fi] && s.t.fns[fi].callees.iter().any(|g| *g < n && t[*g]) {
                t[fi] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    t
}

/// `--gc=off`: gc_arena を F2623 で拒否する（GC のランタイムを 1 バイトもリンクしない） / reject gc_arena with F2623
/// (not a single byte of the GC runtime is linked)
pub static GC_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--fast-math`: プログラム全体の浮動小数点の集計に再結合を許す（`#[fast_math]` をすべての関数に付けたのと同じ）
/// allow reassociation in every float reduction of the program (as if every function had `#[fast_math]`)
pub static FAST_MATH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--gpu=off`: gpu_arena の dispatch を CPU のスタブで同期に実行する（fascon_plat_gpu_* を呼ばない。F2707 の警告）
/// run gpu_arena dispatches synchronously on the CPU stub (no fascon_plat_gpu_* call; the F2707 warning)
pub static GPU_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 型が（構造体・配列・Option・own・Result を通して）条件に合う型を含むか / whether a type contains (through structs,
/// arrays, Option, own and Result) a type matching the predicate
pub fn ty_contains(s: &Sema, t: &Ty, f: &dyn Fn(&Ty) -> bool) -> bool {
    fn go(s: &Sema, t: &Ty, f: &dyn Fn(&Ty) -> bool, seen: &mut Vec<usize>) -> bool {
        if f(t) {
            return true;
        }
        match t {
            Ty::Struct(sid) => {
                if seen.contains(sid) {
                    return false;
                }
                seen.push(*sid);
                s.t.structs[*sid].fields.iter().any(|fl| go(s, &fl.ty, f, seen))
            }
            Ty::Array(e, _) | Ty::Option(e) | Ty::Own(e) | Ty::DmaOwn(e) => go(s, e, f, seen),
            Ty::Result(a, b) => go(s, a, f, seen) || go(s, b, f, seen),
            _ => false,
        }
    }
    go(s, t, f, &mut Vec::new())
}

/// fascon-gc の宣言の規則 / declaration rules of fascon-gc
///   F2623: `--gc=off` で gc_arena を使った / gc_arena used with `--gc=off`
///   F2624: gc_arena の要素に `dma own T`（転送中に回収されうる） / `dma own T` in a gc_arena element (could be reclaimed mid-transfer)
///   F2625: コアごとのアリーナの要素に共有の gc_arena の Id（回収するコアからは見えない根） / an Id of a shared gc_arena in a
///          per-core arena element (a root the collecting core cannot see)
fn gc_rules(s: &Sema, diags: &mut Vec<Diag>) {
    let off = GC_OFF.load(std::sync::atomic::Ordering::Relaxed);
    for (ai, a) in s.t.arenas.iter().enumerate() {
        if a.gc && off {
            diags.push(
                Diag::error(
                    "F2623",
                    a.span,
                    format!("`--gc=off` では gc_arena `{}` を使えません（GC のランタイムを 1 バイトもリンクしない完全決定論のビルド）", a.name),
                    format!("gc_arena `{}` cannot be used with `--gc=off` (a fully deterministic build that links not a single byte of the GC runtime)", a.name),
                )
                .hint("寿命を手で決めるふつうの `arena` か、関数の出口でまとめて解放する `bump_arena` にしてください", "use a plain `arena` (lifetimes by hand) or a `bump_arena` (bulk free at function exit)"),
            );
        }
        if a.gc && ty_contains(s, &a.elem, &|t| matches!(t, Ty::DmaOwn(_))) {
            diags.push(
                Diag::error(
                    "F2624",
                    a.span,
                    format!("gc_arena `{}` の要素に `dma own T` は置けません（装置が転送している間に回収されうる）", a.name),
                    format!("the elements of gc_arena `{}` cannot hold `dma own T` (it could be reclaimed while a device is transferring)", a.name),
                )
                .hint("DMA バッファはふつうの `arena` か static に置き、gc_arena からはその Id で指してください", "keep DMA buffers in a plain `arena` or a static and point at them by Id from the gc_arena"),
            );
        }
        if !a.gc && !a.shared && !a.bump {
            let hit = ty_contains(s, &a.elem, &|t| matches!(t, Ty::Id(g) if s.t.arenas[*g].gc && s.t.arenas[*g].shared));
            if hit && ai < s.t.arenas.len() {
                diags.push(Diag::error(
                    "F2625",
                    a.span,
                    format!("コアごとのアリーナ `{}` の要素に、共有の gc_arena の Id は置けません（回収するコアからは見えない根になる）", a.name),
                    format!("the elements of the per-core arena `{}` cannot hold Ids of a shared gc_arena (they would be roots the collecting core cannot see)", a.name),
                ));
            }
        }
    }
}

pub fn analyze(s: &mut Sema) {
    let mut info = compute_sleep(s);
    info.gc_ops = compute_gc_ops(s);
    let mut diags: Vec<Diag> = Vec::new();
    struct_rules(s, &mut diags);
    gc_rules(s, &mut diags);
    let recursive = recursion(s, &mut diags);
    for (fi, f) in s.t.fns.iter().enumerate() {
        if !s.fn_ok.get(fi).copied().unwrap_or(false) {
            continue;
        }
        let Item::Fn(fd) = &s.prog.items[f.item] else { continue };
        if f.asm.is_some() || f.extern_c == Some(false) || f.stub {
            continue;
        }
        let mut w = Walk::new(s, &info, fi);
        w.block(&fd.body);
        diags.extend(w.diags);
    }
    for i in 0..s.t.fns.len() {
        s.t.fns[i].sleeps = info.sleeps[i];
    }
    info.recursive = recursive;
    s.universal = info;
    s.diags.extend(diags);
}

// ───────────── Sleep 効果 / the Sleep effect ─────────────

/// 呼び出しが直接眠りうるか: (眠りうる, 新しい源か) / whether a call sleeps directly: (may sleep, through a new source)
fn direct_sleep(s: &Sema, e: &Expr) -> (bool, bool) {
    match s.calls.get(&e.id) {
        Some(CallKind::Builtin(Builtin::Schedule | Builtin::SleepTicks | Builtin::RcuSynchronize)) => return (true, true),
        Some(CallKind::Builtin(Builtin::WaitIrq)) => return (true, false),
        _ => {}
    }
    if let Some(k) = s.methods.get(&e.id) {
        if matches!(k, MethodKind::TaskYield(_) | MethodKind::TaskExit(_) | MethodKind::ChanSend(_) | MethodKind::ChanRecvWait(_) | MethodKind::GpuReclaimWait(_)) {
            return (true, false);
        }
    }
    (false, false)
}

fn compute_sleep(s: &Sema) -> UniversalInfo {
    let n = s.t.fns.len();
    let mut sleeps = vec![false; n];
    let mut sleeps_new = vec![false; n];
    for (fi, f) in s.t.fns.iter().enumerate() {
        // `#[sleep]` と `#[asm(effects = "sleep")]` は登録のときに立っている / set at registration
        if f.sleeps {
            sleeps[fi] = true;
            sleeps_new[fi] = true;
        }
        let Item::Fn(fd) = &s.prog.items[f.item] else { continue };
        crate::sema::walk_block(&fd.body, &mut |x: &Expr| {
            let (a, b) = direct_sleep(s, x);
            sleeps[fi] |= a;
            sleeps_new[fi] |= b;
        });
    }
    loop {
        let mut changed = false;
        for fi in 0..n {
            for &c in &s.t.fns[fi].callees {
                if sleeps[c] && !sleeps[fi] {
                    sleeps[fi] = true;
                    changed = true;
                }
                if sleeps_new[c] && !sleeps_new[fi] {
                    sleeps_new[fi] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    UniversalInfo { sleeps, sleeps_new, recursive: vec![false; n], gc_ops: vec![false; n] }
}

// ───────────── F3001: rtos の再帰 / recursion under rtos ─────────────

fn recursion(s: &Sema, diags: &mut Vec<Diag>) -> Vec<bool> {
    let n = s.t.fns.len();
    // Tarjan の強連結成分 / Tarjan's strongly connected components
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let mut comps: Vec<Vec<usize>> = Vec::new();
    fn strong(v: usize, s: &Sema, index: &mut Vec<usize>, low: &mut Vec<usize>, on: &mut Vec<bool>, stack: &mut Vec<usize>, next: &mut usize, comps: &mut Vec<Vec<usize>>) {
        index[v] = *next;
        low[v] = *next;
        *next += 1;
        stack.push(v);
        on[v] = true;
        for &w in &s.t.fns[v].callees {
            if index[w] == usize::MAX {
                strong(w, s, index, low, on, stack, next, comps);
                low[v] = low[v].min(low[w]);
            } else if on[w] {
                low[v] = low[v].min(index[w]);
            }
        }
        if low[v] == index[v] {
            let mut c = Vec::new();
            while let Some(w) = stack.pop() {
                on[w] = false;
                c.push(w);
                if w == v {
                    break;
                }
            }
            comps.push(c);
        }
    }
    for v in 0..n {
        if index[v] == usize::MAX {
            strong(v, s, &mut index, &mut low, &mut on, &mut stack, &mut next, &mut comps);
        }
    }
    let mut rec = vec![false; n];
    for c in comps {
        let cyc = c.len() > 1 || s.t.fns[c[0]].callees.contains(&c[0]);
        if !cyc {
            continue;
        }
        for &f in &c {
            rec[f] = true;
        }
        if s.profile != Profile::Rtos {
            continue;
        }
        let mut names: Vec<usize> = c.clone();
        names.sort_by_key(|f| s.t.fns[*f].span.lo);
        let path = names.iter().map(|f| s.t.fns[*f].name.clone()).collect::<Vec<_>>();
        let first = names[0];
        let shown = if path.len() == 1 { format!("{} → {}", path[0], path[0]) } else { format!("{} → {}", path.join(" → "), path[0]) };
        let mut d = Diag::error(
            "F3001",
            fn_name_span(s, first),
            format!("`--target rtos` では再帰できません（呼び出しの閉路: {}）。スタックの最大消費量を静的に証明できなくなります", shown),
            format!("recursion is not allowed with `--target rtos` (call cycle: {}); the maximum stack use could not be proven statically", shown),
        );
        for &f in names.iter().skip(1) {
            d = d.note(fn_name_span(s, f), format!("閉路の関数 `{}`", s.t.fns[f].name), format!("`{}` is on the cycle", s.t.fns[f].name));
        }
        d = d.hint(
            "ループと明示的なスタック（固定長の配列か bump_arena）に書き換えてください。関数ポインタの呼び出しは、そこへ渡りうるすべての関数への辺として数えます",
            "rewrite it as a loop with an explicit stack (a fixed-size array or a bump_arena); calls through function pointers count as edges to every function that may reach them",
        );
        diags.push(d);
    }
    rec
}

fn fn_name_span(s: &Sema, fi: usize) -> Span {
    match &s.prog.items[s.t.fns[fi].item] {
        Item::Fn(fd) => fd.name_span,
        _ => s.t.fns[fi].span,
    }
}

// ───────────── 構造体の規則 / struct rules ─────────────

fn struct_rules(s: &Sema, diags: &mut Vec<Diag>) {
    for item in &s.prog.items {
        let Item::Struct(sd) = item else { continue };
        let Some(&sid) = s.t.struct_by_name.get(&sd.name) else { continue };
        let info = &s.t.structs[sid];
        for (k, f) in info.fields.iter().enumerate() {
            let fspan = sd.fields.get(k).map_or(f.span, |x| x.span);
            // F3402: dma own はフィールドに直接置かない（取り外せる Option<dma own T> にする）
            if let Ty::DmaOwn(_) = &f.ty {
                diags.push(
                    Diag::error(
                        "F3402",
                        fspan,
                        format!("フィールド `{}` に `dma own` を直接置くことはできません（構造体へ詰め込まない独立したページなので、取り外せる形にします）", f.name),
                        format!("the field `{}` cannot hold a bare `dma own` (it lives on pages of its own, so it must be detachable)", f.name),
                    )
                    .hint(
                        "`Option<dma own T>` にして、デバイスへ渡すときは `.take()` で取り外してください",
                        "make it `Option<dma own T>` and detach it with `.take()` before handing it to the device",
                    ),
                );
            }
            // F3501: #[stable_abi] の構造体に Id<@a> を直接置けない（rtos は 32 ビット・generic-os は 64 ビットで大きさが変わる）
            // F3501: no Id<@a> directly in a #[stable_abi] struct (32 bits under rtos, 64 bits under generic-os)
            let has_id = {
                fn id_in(t: &Ty, d: u32) -> bool {
                    d < 16 && match t {
                        Ty::Id(_) | Ty::Dma(_) | Ty::GpuBuf(_) => true,
                        Ty::Array(e, _) => id_in(e, d + 1),
                        Ty::Option(e) => matches!(**e, Ty::Id(_) | Ty::Dma(_) | Ty::GpuBuf(_)),
                        _ => false,
                    }
                }
                id_in(&f.ty, 0)
            };
            if info.stable_abi && has_id {
                let d = s.t.display(&f.ty);
                diags.push(
                    Diag::error(
                        "F3501",
                        fspan,
                        format!("`#[stable_abi]` の構造体に `{}: {}` は置けません（Id の幅は --target rtos で 32 ビット・generic-os で 64 ビットと変わり、外部の ABI が壊れる）", f.name, d),
                        format!("`{}: {}` cannot be placed in a `#[stable_abi]` struct (an Id is 32 bits under --target rtos and 64 bits under generic-os, which would break the external ABI)", f.name, d),
                    )
                    .hint(
                        "外部へは幅の決まった整数（u64）で渡し、受け取った側で validate して Id に戻してください",
                        "pass a fixed-width integer (u64) across the boundary and turn it back into an Id with validate on the receiving side",
                    ),
                );
            }
            // F3502: #[stable_abi] のフィールドは C の ABI で意味が固定された型だけ / only ABI-stable field types
            if info.stable_abi && !has_id && !abi_stable(s, &f.ty, 0) {
                let d = s.t.display(&f.ty);
                diags.push(
                    Diag::error(
                        "F3502",
                        fspan,
                        format!("`#[stable_abi]` の構造体のフィールド `{}: {}` は ABI を固定できません", f.name, d),
                        format!("the field `{}: {}` of a `#[stable_abi]` struct cannot have a fixed ABI", f.name, d),
                    )
                    .hint(
                        "使えるのは整数・bool・その配列・ワイヤ構造体・ほかの #[stable_abi] 構造体です（Id は F3501。own・Option・Str・関数ポインタは不可）",
                        "integers, bool, arrays of them, wire structs and other #[stable_abi] structs are allowed (Ids are F3501; no own, Option, Str or function pointers)",
                    ),
                );
            }
        }
        // F3501: #[on_drop(f)] の f は `fn f(x: &mut この構造体)` / f must be `fn f(x: &mut ThisStruct)`
        if let Some(fname) = &info.on_drop {
            let aspan = sd.attrs.iter().find(|a| a.name == "on_drop").map_or(sd.name_span, |a| a.span);
            let ok = match s.t.fn_by_name.get(fname) {
                Some(&fi) => {
                    let g = &s.t.fns[fi];
                    g.params.len() == 1 && matches!(&g.params[0].ty, Ty::Ref(true, inner) if **inner == Ty::Struct(sid)) && matches!(g.ret, Ty::Unit) && !g.sleeps
                }
                None => false,
            };
            if !ok {
                diags.push(
                    Diag::error(
                        "F3505",
                        aspan,
                        format!("`#[on_drop({})]` の関数は `fn {}(x: &mut {})`（戻り値なし・眠らない）でなければなりません", fname, fname, sd.name),
                        format!("the `#[on_drop({})]` function must be `fn {}(x: &mut {})` (no result, never sleeps)", fname, fname, sd.name),
                    )
                    .hint(
                        "破棄の順序は 1. on_drop の関数 → 2. #[secret] の 0 埋め + バリア → 3. 記憶域の解放 です",
                        "the drop order is 1. the on_drop function → 2. #[secret] zeroing + barrier → 3. freeing the storage",
                    ),
                );
            }
        }
    }
}

fn abi_stable(s: &Sema, t: &Ty, depth: u32) -> bool {
    if depth > 16 {
        return false;
    }
    match t {
        Ty::Int(_) | Ty::Bool | Ty::Fault => true,
        Ty::Array(e, _) => abi_stable(s, e, depth + 1),
        Ty::Struct(x) => s.t.structs[*x].stable_abi || s.t.structs[*x].wire.is_some(),
        _ => false,
    }
}

// ───────────── 関数の本体の検査 / checks over function bodies ─────────────

struct Walk<'a, 'p> {
    s: &'a Sema<'p>,
    info: &'a UniversalInfo,
    diags: Vec<Diag>,
    rcu: Vec<Span>,
    atomic: Vec<Span>,
    trap: bool,
    /// `critical { .. }`（と critical fn）の区間 / `critical { .. }` sections (and critical fn)
    critical: Vec<Span>,
    /// 割り込みハンドラ（irq）の本体 / the body of an interrupt handler (irq)
    irq: bool,
    /// 生きている第二級参照（名前, 作られた場所） / live second-class references (name, where made)
    refs: Vec<(String, Span)>,
    /// 秘密の値を持つ束縛 / bindings that hold secrets
    secret: HashSet<BindId>,
    seen: HashSet<(u32, &'static str)>,
    fname: String,
}

impl<'a, 'p> Walk<'a, 'p> {
    fn new(s: &'a Sema<'p>, info: &'a UniversalInfo, fi: usize) -> Walk<'a, 'p> {
        let f = &s.t.fns[fi];
        let mut refs = Vec::new();
        let mut secret: HashSet<BindId> = s.secret_binds.iter().copied().collect();
        for p in &f.params {
            if let Ty::Ref(..) = p.ty {
                refs.push((p.name.clone(), p.span));
            }
            if is_secret_ty(s, &p.ty) {
                secret.insert(p.bind);
            }
        }
        // 秘密の構造体を型に持つローカル（参照を含む） / locals whose type is a secret struct (references included)
        for (b, l) in s.locals.iter().enumerate() {
            if let Some(l) = l {
                if is_secret_ty(s, &l.ty) {
                    secret.insert(b as BindId);
                }
            }
        }
        Walk { s, info, diags: Vec::new(), rcu: Vec::new(), atomic: Vec::new(), trap: f.trap_v2, critical: Vec::new(), irq: f.irq.is_some(), refs, secret, seen: HashSet::new(), fname: f.name.clone() }
    }

    fn err(&mut self, d: Diag) {
        if self.seen.insert((d.span.lo, d.code)) {
            self.diags.push(d);
        }
    }

    fn ty(&self, e: &Expr) -> Ty {
        self.s.expr_ty.get(e.id as usize).cloned().unwrap_or(Ty::Err)
    }

    fn block(&mut self, b: &Block) {
        let nrefs = self.refs.len();
        for st in &b.stmts {
            match st {
                Stmt::Let(l) => {
                    if let Some(i) = &l.init {
                        self.expr(i);
                        self.let_secret(l, i);
                    }
                }
                Stmt::Expr(e) => self.expr(e),
            }
        }
        if let Some(t) = &b.tail {
            self.expr(t);
        }
        self.refs.truncate(nrefs);
    }

    /// パターンの束縛が参照なら、腕の間は生きている参照 / a reference binding of a pattern is a live reference within the arm
    fn pat_refs(&mut self, p: &Pattern) {
        if let Some((name, b, _)) = p.binding() {
            if let Some(Some(l)) = self.s.locals.get(b as usize) {
                if l.kind == LocalKind::PatRef || matches!(l.ty, Ty::Ref(..)) {
                    self.refs.push((name.to_string(), p.span()));
                }
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::RcuRead(b) => {
                self.rcu.push(e.span);
                self.block(b);
                self.rcu.pop();
                return;
            }
            ExprKind::Atomic(b) => {
                if let Some(&r) = self.rcu.last() {
                    self.f3202(e.span, r, "`atomic { .. }` の書き込み", "the write of `atomic { .. }`");
                }
                self.atomic.push(e.span);
                self.block(b);
                self.atomic.pop();
                return;
            }
            ExprKind::Critical(b) => {
                self.critical.push(e.span);
                self.block(b);
                self.critical.pop();
                return;
            }
            ExprKind::IfLet { pat, scrut, then, els } => {
                self.expr(scrut);
                self.branch_on(scrut, "if let");
                let n = self.refs.len();
                self.pat_refs(pat);
                self.pat_secret(pat, scrut);
                self.block(then);
                self.refs.truncate(n);
                if let Some(x) = els {
                    self.expr(x);
                }
                return;
            }
            ExprKind::Match { scrut, arms } => {
                self.expr(scrut);
                // `e?` の脱糖衣は分岐として数えない（失敗の有無は秘密ではない） / the desugared `e?` is not a secret branch
                if !self.s.prog.question_marks.contains(&e.id) {
                    self.branch_on(scrut, "match");
                }
                for a in arms {
                    let n = self.refs.len();
                    self.pat_refs(&a.pat);
                    self.pat_secret(&a.pat, scrut);
                    self.expr(&a.body);
                    self.refs.truncate(n);
                }
                return;
            }
            ExprKind::If { cond, .. } => self.branch_on(cond, "if"),
            ExprKind::While { cond, .. } => self.branch_on(cond, "while"),
            ExprKind::Binary(op, l, r) => {
                let cmp = matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::And | BinOp::Or);
                if cmp && (self.tainted(l) || self.tainted(r)) {
                    let (oj, oe) = if matches!(op, BinOp::And | BinOp::Or) { ("短絡評価（&& / ||）は分岐です", "short-circuit evaluation (&& / ||) is a branch") } else { ("比較の結果で分岐すると、かかる時間から値が漏れます", "branching on the comparison leaks the value through timing") };
                    self.err(
                        Diag::error(
                            "F3302",
                            e.span,
                            format!("#[secret] の値を `{}` で比べています（{}）", op.text(), oj),
                            format!("a #[secret] value is compared with `{}` ({})", op.text(), oe),
                        )
                        .hint("定数時間の比較 `ct_eq(&a, &b)` を使ってください（結果の bool は秘密ではありません）", "use the constant-time comparison `ct_eq(&a, &b)` (its bool result is not secret)"),
                    );
                }
            }
            ExprKind::Index(base, idx) => {
                if self.tainted(idx) {
                    self.err(
                        Diag::error("F3302", idx.span, "#[secret] の値を添字（番地の計算）に使っています（キャッシュの当たり外れから値が漏れます）", "a #[secret] value is used as an index (an address computation); cache timing would leak it")
                            .hint("表を引く代わりに、全要素を定数時間で選ぶ（マスクして足す）書き方にしてください", "instead of a table lookup, select over every element in constant time (mask and add)"),
                    );
                }
                let _ = base;
            }
            ExprKind::Assign { op: _, lhs, rhs } => {
                // rcu_read の中の書き込み（アリーナのスロット） / a write inside rcu_read (to an arena slot)
                if let Some(&r) = self.rcu.last() {
                    if self.arena_root(lhs).is_some() {
                        self.f3202(e.span, r, "アリーナのスロットへの書き込み", "a write to an arena slot");
                    }
                }
                // 秘密の値を秘密でない場所へ写す / copying a secret into a non-secret place
                if self.tainted(rhs) && !self.tainted(lhs) && !self.secret_move(rhs) {
                    self.f3301_copy(rhs.span, "秘密でない場所への代入", "an assignment to a non-secret place");
                }
            }
            ExprKind::Ref { mutable: true, inner } => {
                if let Some(&r) = self.rcu.last() {
                    if self.arena_root(inner).is_some() {
                        self.f3202(e.span, r, "スロットの `&mut` 借用", "a `&mut` borrow of a slot");
                    }
                }
            }
            ExprKind::Return(Some(x)) => {
                if self.tainted(x) && !self.secret_move(x) {
                    self.f3301_copy(x.span, "戻り値として外へ写す", "copying it out as the return value");
                }
            }
            ExprKind::Call { .. } | ExprKind::CallIndirect { .. } => self.call(e),
            ExprKind::MethodCall { .. } => self.method(e),
            _ => {}
        }
        if self.s.auto_promote.contains_key(&e.id) {
            self.gc_site(e.span, "bump_arena から gc_arena への自動昇格（確保）", "an automatic promotion from a bump_arena to a gc_arena (an allocation)");
        }
        for c in children(e) {
            match c {
                Child::E(x) => self.expr(x),
                Child::B(b) => self.block(b),
            }
        }
    }

    // ───── rcu_read / Sleep ─────

    fn f3202(&mut self, sp: Span, rcu: Span, ja: &str, en: &str) {
        self.err(
            Diag::error(
                "F3202",
                sp,
                format!("`rcu_read {{ .. }}` の中では書き込み・確保・解放はできません（ここは {}）", ja),
                format!("no writes, allocations or frees inside `rcu_read {{ .. }}` (this is {})", en),
            )
            .note(rcu, "rcu_read の区間はここから", "the rcu_read section starts here")
            .hint(
                "読み取った値をローカルへ写して区間を閉じ、書き込みは `critical { .. }` か `atomic { .. }` で行ってください（解放は猶予期間の後に回収されます）",
                "copy what you read into locals, close the section, and write inside `critical { .. }` or `atomic { .. }` (frees are reclaimed after a grace period)",
            ),
        );
    }

    /// 眠りうる呼び出しの検査 / checks at a call that may sleep
    fn sleep_site(&mut self, sp: Span, what: &str, new_src: bool) {
        let (ctx_ja, ctx_en, note) = if let Some(&r) = self.rcu.last() {
            ("`rcu_read { .. }` の中（プリエンプション禁止の読み手）", "inside `rcu_read { .. }` (a reader with preemption disabled)", Some(r))
        } else if let Some(&a) = self.atomic.last() {
            ("`atomic { .. }` の中", "inside `atomic { .. }`", Some(a))
        } else if self.trap {
            ("`#[trap]` の関数の中（例外の処理は眠れない）", "inside a `#[trap]` function (exception handling cannot sleep)", None)
        } else {
            ("", "", None)
        };
        if !ctx_ja.is_empty() {
            let mut d = Diag::error(
                "F3203",
                sp,
                format!("Sleep 効果を持つ `{}` を {} で呼んでいます", what, ctx_ja),
                format!("`{}` has the Sleep effect and is called {}", what, ctx_en),
            );
            if let Some(n) = note {
                d = d.note(n, "区間はここから", "the section starts here");
            }
            self.err(d.hint("眠る呼び出し（schedule・sleep_ticks・rcu_synchronize・待つチャネル操作など）は区間の外へ出してください", "move the sleeping call (schedule, sleep_ticks, rcu_synchronize, blocking channel operations ...) out of the section"));
            return;
        }
        if new_src {
            if let Some((rn, rsp)) = self.refs.last().cloned() {
                self.err(
                    Diag::error(
                        "F3203",
                        sp,
                        format!("Sleep 効果を持つ `{}` を、第二級参照 `{}` が生きている間に呼んでいます（眠っている間にほかのタスクが参照先を解放・再利用しうる）", what, rn),
                        format!("`{}` has the Sleep effect and is called while the second-class reference `{}` is live (another task may free or reuse its target while this one sleeps)", what, rn),
                    )
                    .note(rsp, "参照はここで作られました", "the reference was created here")
                    .hint("必要な値をローカルへ写して参照のブロックを閉じてから眠ってください（起きた後は Id で引き直す）", "copy what you need into locals and close the reference's block before sleeping (look it up again by Id after waking)"),
                );
            }
        }
    }

    fn call(&mut self, e: &Expr) {
        if let Some(&(_, b)) = self.s.gc_calls.get(&e.id) {
            let (ja, en) = if b.is_some() { ("promote_to_gc", "promote_to_gc") } else { ("gc_collect", "gc_collect") };
            self.gc_site(e.span, ja, en);
            return;
        }
        // critical・irq の中から、gc_arena を操作する関数を呼ぶ（中で確保・回収すれば同じこと） / calling, inside critical or
        // an irq, a function that operates on a gc_arena (allocating or collecting in there is the same thing)
        if !self.critical.is_empty() || self.irq {
            let cands: Vec<usize> = match self.s.calls.get(&e.id) {
                Some(CallKind::User(g)) => vec![*g],
                Some(CallKind::Indirect) => self.s.indirect_cands.get(&e.id).cloned().unwrap_or_default(),
                _ => Vec::new(),
            };
            if let Some(&g) = cands.iter().find(|g| self.info.gc_ops.get(**g).copied().unwrap_or(false)) {
                let gn = self.s.t.fns[g].name.clone();
                if self.gc_site(e.span, &format!("gc_arena を操作する `{}` の呼び出し", gn), &format!("a call of `{}`, which operates on a gc_arena,", gn)) {
                    return;
                }
            }
        }
        let kind = self.s.calls.get(&e.id).cloned();
        let args: &[Expr] = match &e.kind {
            ExprKind::Call { args, .. } | ExprKind::CallIndirect { args, .. } => args,
            _ => &[],
        };
        let name = match &e.kind {
            ExprKind::Call { callee, .. } => callee.clone(),
            _ => "fn(..)".to_string(),
        };
        let (ds, dn) = direct_sleep(self.s, e);
        if ds {
            self.sleep_site(e.span, &name, dn);
        }
        match kind {
            Some(CallKind::User(g)) => {
                let gf = &self.s.t.fns[g];
                if self.info.sleeps[g] {
                    self.sleep_site(e.span, &gf.name.clone(), self.info.sleeps_new[g]);
                }
                if let Some(&r) = self.rcu.last() {
                    let eff = &gf.effects;
                    if !eff.writes.is_empty() || !eff.frees.is_empty() || !eff.allocs.is_empty() {
                        let an = eff.writes.iter().chain(eff.frees.iter()).chain(eff.allocs.iter()).next().map(|a| self.s.t.arenas[*a].name.clone()).unwrap_or_default();
                        self.f3202(e.span, r, &format!("`{}` の呼び出し（アリーナ `{}` を書き換える効果を持つ）", gf.name, an), &format!("a call of `{}` (its effects modify the arena `{}`)", gf.name, an));
                    }
                }
                // 秘密の値を値渡しで写す / passing a secret by value copies it
                let ps: Vec<Ty> = gf.params.iter().map(|p| p.ty.clone()).collect();
                if gf.asm.is_some() {
                    for a in args {
                        if self.tainted(a) {
                            self.err(
                                Diag::error("F3303", a.span, "#[secret] の値を asm のオペランドに渡しています（命令列がレジスタに残したり時間で漏らしたりしうる）", "a #[secret] value is passed as an asm operand (the instructions may leave it in registers or leak it through timing)")
                                    .hint("秘密の値は Fascon の中の定数時間の処理（ct_eq・マスク）だけで扱ってください", "handle secrets only with constant-time Fascon code (ct_eq, masking)"),
                            );
                        }
                    }
                } else {
                    for (k, a) in args.iter().enumerate() {
                        let by_ref = matches!(ps.get(k), Some(Ty::Ref(..)));
                        if !by_ref && self.tainted(a) && !self.secret_move(a) {
                            self.f3301_copy(a.span, &format!("`{}` への値渡し", gf.name), &format!("passing it by value to `{}`", gf.name));
                        }
                    }
                }
            }
            Some(CallKind::Indirect) => {
                let cands = self.s.indirect_cands.get(&e.id).cloned().unwrap_or_default();
                if let Some(&g) = cands.iter().find(|g| self.info.sleeps[**g]) {
                    let gn = self.s.t.fns[g].name.clone();
                    self.sleep_site(e.span, &format!("{}（{} が入りうる）", name, gn), self.info.sleeps_new[g]);
                }
                if let Some(&r) = self.rcu.last() {
                    if let Some(&g) = cands.iter().find(|g| {
                        let eff = &self.s.t.fns[**g].effects;
                        !eff.writes.is_empty() || !eff.frees.is_empty() || !eff.allocs.is_empty()
                    }) {
                        let gn = self.s.t.fns[g].name.clone();
                        self.f3202(e.span, r, &format!("書き換える効果を持つ `{}` が入りうる関数ポインタの呼び出し", gn), &format!("a call through a function pointer that may be `{}`, which modifies an arena", gn));
                    }
                }
            }
            Some(CallKind::Builtin(b)) => {
                let out = matches!(b, Builtin::PrintInt | Builtin::PrintStr | Builtin::PutInt | Builtin::PutHex | Builtin::PutStr | Builtin::PutBytes | Builtin::Put | Builtin::Panic | Builtin::Exit | Builtin::CopyToUser);
                if out {
                    for a in args {
                        if self.tainted(a) {
                            self.err(
                                Diag::error("F3301", a.span, "#[secret] の値を出力できません（コンソール・ユーザー空間・終了コードへ漏れます）", "a #[secret] value cannot be output (it would leak to the console, user space or the exit code)")
                                    .hint("出すのは定数時間の比較の結果（ct_eq の bool）など、秘密でない値だけにしてください", "output only non-secret values, such as the bool of a constant-time comparison (ct_eq)"),
                            );
                        }
                    }
                }
                if b == Builtin::CtEq && args.len() == 2 {
                    // F3304 は sema が型の組み合わせで出す（ここでは何もしない） / F3304 comes from sema
                }
                if matches!(b, Builtin::SubmitDma) {
                    if let Some(&r) = self.rcu.last() {
                        self.f3202(e.span, r, "デバイスへの貸し出し", "lending to a device");
                    }
                }
            }
            Some(CallKind::Some) | Some(CallKind::Ok) | Some(CallKind::Err) => {
                for a in args {
                    if self.tainted(a) && !self.secret_move(a) {
                        self.f3301_copy(a.span, "Option / Result への詰め込み", "packing it into an Option / Result");
                    }
                }
            }
            None => {}
        }
    }

    /// F2622: gc_arena の確保・gc_collect・promote_to_gc を、#[trap]・rcu_read・atomic の中で使った
    /// F2622: a gc_arena allocation, gc_collect or promote_to_gc inside #[trap], rcu_read or atomic
    fn gc_site(&mut self, sp: Span, what_ja: &str, what_en: &str) -> bool {
        let (ja, en, note) = if let Some(&r) = self.rcu.last() {
            ("`rcu_read { .. }` の中", "inside `rcu_read { .. }`", Some(r))
        } else if let Some(&a) = self.atomic.last() {
            ("`atomic { .. }` の中", "inside `atomic { .. }`", Some(a))
        } else if self.trap {
            ("`#[trap]` の関数の中", "inside a `#[trap]` function", None)
        } else if let Some(&c) = self.critical.last() {
            ("`critical { .. }` の中（割り込み禁止・ロックを持つ区間）", "inside `critical { .. }` (interrupts masked, the lock held)", Some(c))
        } else if self.irq {
            ("割り込みハンドラ（irq）の中", "inside an interrupt handler (irq)", None)
        } else {
            return false;
        };
        let mut d = Diag::error(
            "F2622",
            sp,
            format!("{}で {} はできません（回収の時間が読めず、止めたコアとの待ち合わせで行き詰まりうる）", ja, what_ja),
            format!("{} is not allowed {} (the collection time is unbounded and waiting for parked cores could deadlock)", what_en, en),
        )
        .hint(
            "区間の外（ふつうの関数の中）で確保・回収してください。区間の中では Id を読むだけにします",
            "allocate and collect outside the section (in an ordinary function); only read Ids inside it",
        );
        if let Some(n) = note {
            d = d.note(n, "区間はここから", "the section starts here");
        }
        self.err(d);
        true
    }

    fn method(&mut self, e: &Expr) {
        let Some(k) = self.s.methods.get(&e.id).copied() else { return };
        if let MethodKind::ArenaAlloc(a) = k {
            if self.s.t.arenas[a].gc && self.gc_site(e.span, "gc_arena の確保（alloc）", "a gc_arena allocation (alloc)") {
                return;
            }
        }
        let (ds, dn) = direct_sleep(self.s, e);
        if ds {
            let mname = match &e.kind {
                ExprKind::MethodCall { method, .. } => method.clone(),
                _ => String::new(),
            };
            self.sleep_site(e.span, &mname, dn);
        }
        if let Some(&r) = self.rcu.last() {
            let w = match k {
                MethodKind::ArenaAlloc(_) => Some(("スロットの確保（alloc）", "a slot allocation (alloc)")),
                MethodKind::ArenaFree(_) | MethodKind::ArenaRemove(_) => Some(("スロットの解放（free）", "a slot free (free)")),
                MethodKind::ArenaGetMut(_) => Some(("書き換えのための借用（get_mut）", "a borrow for writing (get_mut)")),
                MethodKind::DmaLend(_) | MethodKind::DmaReclaim(_) => Some(("DMA の貸し出し・回収", "a DMA lend / reclaim")),
                MethodKind::GpuDispatch(_) | MethodKind::GpuReclaim(_) => Some(("GPU への貸し出し・回収", "a GPU dispatch / reclaim")),
                MethodKind::ChanSend(_) | MethodKind::ChanRecv(_) | MethodKind::ChanRecvWait(_) => Some(("チャネルの操作", "a channel operation")),
                _ => None,
            };
            if let Some((ja, en)) = w {
                self.f3202(e.span, r, ja, en);
            }
        }
    }

    // ───── #[secret] ─────

    /// 秘密の値を含む式か / whether an expression carries a secret
    fn tainted(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Local(b)) => self.secret.contains(b),
                _ => false,
            },
            ExprKind::Field(b, _, _) => self.tainted(b) || is_secret_ty(self.s, &self.ty(b)),
            ExprKind::Index(b, _) => self.tainted(b) || is_secret_ty(self.s, &self.ty(b)),
            ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => self.tainted(x),
            ExprKind::Ref { inner, .. } => self.tainted(inner),
            ExprKind::Binary(op, l, r) => {
                // 比較の結果は F3302 で止めるので、ここでは秘密として扱わない（同じ場所でエラーを重ねない）
                // comparisons are stopped by F3302, so their results are not treated as secret here (no duplicate errors)
                if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::And | BinOp::Or) {
                    false
                } else {
                    self.tainted(l) || self.tainted(r)
                }
            }
            ExprKind::MethodCall { recv, args, .. } => match self.s.methods.get(&e.id) {
                Some(MethodKind::IntOp(_)) | Some(MethodKind::Unwrap) | Some(MethodKind::UnwrapOr) | Some(MethodKind::ResUnwrap) | Some(MethodKind::ResUnwrapOr) | Some(MethodKind::Clone) | Some(MethodKind::ReadAt(_)) => self.tainted(recv) || args.iter().any(|a| self.tainted(a)),
                _ => false,
            },
            ExprKind::Call { args, .. } => match self.s.calls.get(&e.id) {
                Some(CallKind::Builtin(Builtin::Min | Builtin::Max)) => args.iter().any(|a| self.tainted(a)),
                _ => false,
            },
            ExprKind::ArrayLit(xs) => xs.iter().any(|x| self.tainted(x)),
            ExprKind::ArrayRepeat(x, _) => self.tainted(x),
            ExprKind::Block(b) => b.tail.as_ref().map_or(false, |t| self.tainted(t)),
            _ => false,
        }
    }

    /// 秘密の構造体を丸ごと移動する（複製ではない）か / whether this moves a whole secret struct (not a copy)
    fn secret_move(&self, e: &Expr) -> bool {
        let t = self.ty(e);
        match &t {
            Ty::Struct(x) => self.s.t.structs[*x].secret,
            Ty::Own(i) => matches!(&**i, Ty::Struct(x) if self.s.t.structs[*x].secret),
            Ty::Ref(..) => is_secret_ty(self.s, &t),
            _ => false,
        }
    }

    fn f3301_copy(&mut self, sp: Span, ja: &str, en: &str) {
        self.err(
            Diag::error("F3301", sp, format!("#[secret] の値を複製しています（{}）。秘密は 1 か所にだけ置き、破棄のときに 0 で埋めます", ja), format!("a #[secret] value is copied ({}); a secret lives in exactly one place and is zeroed when dropped", en))
                .hint("写し先も `#[secret] let` にするか、参照（&）で渡してください", "make the destination a `#[secret] let` too, or pass it by reference (&)"),
        );
    }

    fn let_secret(&mut self, l: &LetStmt, init: &Expr) {
        if self.secret.contains(&l.bind) {
            return; // #[secret] let（または秘密の構造体の型）/ a #[secret] let (or of a secret struct type)
        }
        if self.tainted(init) && !self.secret_move(init) {
            self.f3301_copy(init.span, &format!("`#[secret]` の無い `let {}` への写し", l.name), &format!("a copy into `let {}` without `#[secret]`", l.name));
            // 以降の連鎖的なエラーを避けるため、この束縛も秘密として扱う / treat it as secret from here on to avoid cascades
            self.secret.insert(l.bind);
        }
    }

    fn pat_secret(&mut self, p: &Pattern, scrut: &Expr) {
        if let Some((_, b, _)) = p.binding() {
            if self.tainted(scrut) || is_secret_ty(self.s, &self.ty(scrut)) {
                self.secret.insert(b);
            }
        }
    }

    fn branch_on(&mut self, cond: &Expr, what: &str) {
        if self.tainted(cond) {
            self.err(
                Diag::error(
                    "F3302",
                    cond.span,
                    format!("#[secret] の値で分岐しています（`{}`）。分岐と早期リターンは、かかる時間から値を漏らします", what),
                    format!("branching on a #[secret] value (`{}`); branches and early returns leak the value through timing", what),
                )
                .hint("`ct_eq` と、両方の値をマスクで選ぶ定数時間の書き方にしてください", "use `ct_eq` and constant-time selection with masks over both values"),
            );
        }
        let _ = &self.fname;
    }

    /// 場所の根がアリーナのスロットか / whether a place is rooted at an arena slot
    fn arena_root(&self, e: &Expr) -> Option<usize> {
        let mut cur = e;
        loop {
            match &cur.kind {
                ExprKind::Field(b, _, _) => cur = b,
                ExprKind::Index(b, _) => {
                    if let Some(&a) = self.s.arena_index.get(&cur.id) {
                        return Some(a);
                    }
                    cur = b;
                }
                _ => return None,
            }
        }
    }
}

/// 型が秘密の構造体（参照・own・Option・配列を通しても）か / whether a type is a secret struct (through refs, own, Option, arrays)
pub fn is_secret_ty(s: &Sema, t: &Ty) -> bool {
    match t {
        Ty::Struct(x) => s.t.structs[*x].secret,
        Ty::Ref(_, i) | Ty::Own(i) | Ty::Option(i) | Ty::Array(i, _) => is_secret_ty(s, i),
        _ => false,
    }
}

/// 名前の付いた関数の呼び出しグラフ（rtos のスタックの証明で使う） / the named call graph (used by the rtos stack proof)
pub fn call_graph(s: &Sema) -> HashMap<String, Vec<String>> {
    let mut g = HashMap::new();
    for f in &s.t.fns {
        g.insert(f.name.clone(), f.callees.iter().map(|c| s.t.fns[*c].name.clone()).collect());
    }
    g
}

// ───────────── Spectre v1: 外から来た値の汚染解析 / Spectre v1: taint analysis of outside data ─────────────

/// generic-os で、外から来た値（#[syscall] の引数・copy_from_user の写し・dma own / 窓 / bind_mmio / mmio の読み出し・
/// ワイヤ構造体のビュー・C から返った値）に由来する添字の式を求める（関数の中で流れに依らない不動点）
/// for generic-os, find the index expressions derived from outside data (#[syscall] arguments, copies from copy_from_user,
/// reads of dma own / windows / bind_mmio / mmio, wire-struct views, values returned from C); a flow-insensitive fixpoint
/// within one function
pub fn spectre_taint(s: &Sema, fi: usize) -> HashSet<ExprId> {
    let f = &s.t.fns[fi];
    let Item::Fn(fd) = &s.prog.items[f.item] else { return HashSet::new() };
    let mut bad: HashSet<BindId> = HashSet::new();
    if f.syscall.is_some() || f.trap {
        for p in &f.params {
            bad.insert(p.bind);
        }
    }
    struct T<'a, 'p> {
        s: &'a Sema<'p>,
        bad: HashSet<BindId>,
        changed: bool,
        sinks: HashSet<ExprId>,
    }
    impl<'a, 'p> T<'a, 'p> {
        fn outside_ty(t: &Ty) -> bool {
            match t {
                Ty::DmaOwn(_) | Ty::MmioBind(_) | Ty::Slice(_) | Ty::User(_) | Ty::Dma(_) | Ty::GpuBuf(_) => true,
                Ty::Ref(_, i) => Self::outside_ty(i),
                _ => false,
            }
        }
        fn ty(&self, e: &Expr) -> Ty {
            self.s.expr_ty.get(e.id as usize).cloned().unwrap_or(Ty::Err)
        }
        fn tainted(&self, e: &Expr) -> bool {
            match &e.kind {
                ExprKind::Path(_) => matches!(self.s.res.get(&e.id), Some(Res::Local(b)) if self.bad.contains(b)),
                ExprKind::Field(b, _, _) | ExprKind::Index(b, _) => {
                    self.tainted(b) || Self::outside_ty(&self.ty(b)) || matches!(&b.kind, ExprKind::ArenaRef(n) if matches!(self.s.t.regions.get(n), Some(crate::types::RegionRef::Mmio(_))))
                }
                ExprKind::ArenaRef(_) => self.s.port_reads.contains_key(&e.id),
                ExprKind::Unary(_, x) | ExprKind::Cast(x, _) | ExprKind::Ref { inner: x, .. } => self.tainted(x),
                ExprKind::Binary(_, l, r) => self.tainted(l) || self.tainted(r),
                ExprKind::Call { args, .. } => match self.s.calls.get(&e.id) {
                    Some(CallKind::Builtin(Builtin::CopyFromUser)) => true,
                    Some(CallKind::Builtin(Builtin::MmioRead(_))) => true,
                    Some(CallKind::Builtin(Builtin::Min | Builtin::Max)) => args.iter().any(|a| self.tainted(a)),
                    Some(CallKind::User(g)) => self.s.t.fns[*g].extern_c == Some(false),
                    _ => false,
                },
                ExprKind::MethodCall { recv, args, .. } => match self.s.methods.get(&e.id) {
                    Some(MethodKind::View(..)) | Some(MethodKind::ReadAt(_)) => true,
                    Some(MethodKind::IntOp(_)) | Some(MethodKind::Unwrap) | Some(MethodKind::UnwrapOr) | Some(MethodKind::ResUnwrap) | Some(MethodKind::ResUnwrapOr) | Some(MethodKind::IdIndex) | Some(MethodKind::IdRaw) => {
                        self.tainted(recv) || args.iter().any(|a| self.tainted(a))
                    }
                    _ => false,
                },
                ExprKind::Block(b) => b.tail.as_ref().map_or(false, |t| self.tainted(t)),
                ExprKind::If { then, els, .. } => then.tail.as_ref().map_or(false, |t| self.tainted(t)) || els.as_ref().map_or(false, |x| self.tainted(x)),
                _ => false,
            }
        }
        fn mark(&mut self, b: BindId) {
            if self.bad.insert(b) {
                self.changed = true;
            }
        }
        fn block(&mut self, b: &Block) {
            for st in &b.stmts {
                match st {
                    Stmt::Let(l) => {
                        if let Some(i) = &l.init {
                            if self.tainted(i) {
                                self.mark(l.bind);
                            }
                            self.expr(i);
                        }
                    }
                    Stmt::Expr(e) => self.expr(e),
                }
            }
            if let Some(t) = &b.tail {
                self.expr(t);
            }
        }
        fn expr(&mut self, e: &Expr) {
            match &e.kind {
                ExprKind::Assign { lhs, rhs, .. } => {
                    if self.tainted(rhs) {
                        if let ExprKind::Path(_) = &lhs.kind {
                            if let Some(Res::Local(b)) = self.s.res.get(&lhs.id) {
                                let b = *b;
                                self.mark(b);
                            }
                        }
                    }
                }
                ExprKind::IfLet { pat, scrut, .. } => {
                    if self.tainted(scrut) {
                        if let Some((_, b, _)) = pat.binding() {
                            self.mark(b);
                        }
                    }
                }
                ExprKind::Match { scrut, arms } => {
                    if self.tainted(scrut) {
                        for a in arms {
                            if let Some((_, b, _)) = a.pat.binding() {
                                self.mark(b);
                            }
                        }
                    }
                }
                ExprKind::For { bind, iter: ForIter::Range { lo, hi, .. }, .. } => {
                    if self.tainted(lo) || self.tainted(hi) {
                        let b = *bind;
                        self.mark(b);
                    }
                }
                ExprKind::Index(_, idx) => {
                    if self.tainted(idx) {
                        self.sinks.insert(idx.id);
                    }
                }
                _ => {}
            }
            for c in children(e) {
                match c {
                    Child::E(x) => self.expr(x),
                    Child::B(b) => self.block(b),
                }
            }
        }
    }
    let mut t = T { s, bad, changed: true, sinks: HashSet::new() };
    let mut rounds = 0;
    while t.changed && rounds < 16 {
        t.changed = false;
        t.sinks.clear();
        t.block(&fd.body);
        rounds += 1;
    }
    t.sinks
}
