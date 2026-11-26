//! `--target hpc`: 管理コア（0）と計算コア（1..N）の役割の検査と、計算ノードの形の報告
//! `--target hpc`: checks that keep the management core (0) and the compute cores (1..N) apart, and the report of the
//! compute node's shape
//!
//!   F3901（警告）: mmio・port_io・dma・gpu_dispatch など外部の I/O が、計算コアのルート（`spawn_on(k, ..)`、k > 0）から
//!                  届く（計算コアへの OS / I/O の雑音）。その関数かルートに `#[allow(hpc_io)]` を付けると外れる
//!   F3902（エラー）: 計算コアから届く道で、実行中に動的な確保（own・own_alloc・gc_arena の確保・promote_to_gc・
//!                  gc_collect・dma_alloc）をする。または Mirrored-SoA の列の写しを表ごと作り直す（`for id in @a` の中で
//!                  写しの列が読むフィールドへ書く。どの列が写しになるかは C 生成で決まるので、その後で調べる）
//!   F3903（エラー）: spawn_on の定数のコア番号・`#[numa_node(n)]` が計算ノードの形（--topology・`// fascon-topology:`・
//!                  プロファイルの既定）を超える。`fn main` に `#[no_smt]` があるのに、2 つのコアが同じ物理コアの兄弟
//!                  スレッドになる
//!   F3901 (warning): external I/O such as mmio, port_io, dma or gpu_dispatch is reachable from a compute-core root
//!                  (`spawn_on(k, ..)` with k > 0): OS / I/O noise on compute cores. `#[allow(hpc_io)]` on the function or
//!                  the root lifts it
//!   F3902 (error): a path reachable from compute cores allocates dynamically at run time (own, own_alloc, gc_arena
//!                  allocations, promote_to_gc, gc_collect, dma_alloc) or rebuilds a Mirrored-SoA column mirror table-wide
//!                  (a write inside `for id in @a` to a field a mirror column reads; which columns are mirrors is decided
//!                  by code generation, so that part is checked afterwards)
//!   F3903 (error): a constant spawn_on core number or `#[numa_node(n)]` beyond the compute node's shape (--topology,
//!                  `// fascon-topology:` or the profile default), or, with `#[no_smt]` on `fn main`, two cores that are
//!                  sibling threads of one physical core

use std::collections::{HashMap, HashSet, VecDeque};

use crate::ast::*;
use crate::codegen::CgReport;
use crate::diag::Diag;
use crate::sema::{Builtin, CallKind, MethodKind, Sema, TypeCallKind};
use crate::span::Span;
use crate::target::{Profile, Topology};
use crate::types::{RegionRef, Ty};

/// 計算コアのルート / a compute-core root
#[derive(Clone, Debug)]
pub struct HpcRoot {
    /// コア番号（定数なら） / the core number (when constant)
    pub core: Option<u32>,
    pub func: usize,
    pub span: Span,
}

/// 外部 I/O の場所（F3901） / an external I/O site (F3901)
#[derive(Clone, Debug)]
pub struct HpcIo {
    pub func: usize,
    pub span: Span,
    pub what_ja: String,
    pub what_en: String,
    /// `#[allow(hpc_io)]` で外したか / lifted by `#[allow(hpc_io)]`
    pub allowed: bool,
    /// どのルートから届くか（roots の添字） / the root it is reached from (an index into roots)
    pub root: usize,
}

#[derive(Clone, Debug)]
pub struct HpcInfo {
    pub topology: Topology,
    /// 管理コアで走る spawn_on（コア 0 が定数） / spawn_on calls onto the management core (constant core 0)
    pub mgmt_spawns: Vec<(usize, Span)>,
    pub roots: Vec<HpcRoot>,
    /// 計算コアから届く関数 → 最初に見つけたルート / functions reachable from compute cores → the first root found
    pub compute: HashMap<usize, usize>,
    pub io: Vec<HpcIo>,
    /// `#[numa_node(n)]` の表（アリーナ, ノード） / tables with `#[numa_node(n)]` (arena, node)
    pub numa_arenas: Vec<(usize, u32)>,
    pub no_smt: bool,
}

fn has_allow_hpc_io(fd: &FnDef) -> bool {
    fd.attrs.iter().any(|a| a.name == "allow" && (a.ident_arg.as_deref() == Some("hpc_io") || a.kv.iter().any(|(k, v, _)| k == "hpc_io" || matches!(v, AttrVal::Ident(x) if x == "hpc_io"))))
}

/// 関数の中の、外部 I/O（F3901）と動的な確保（F3902）の場所 / external I/O (F3901) and dynamic allocation (F3902) sites
/// inside a function
fn sites(s: &Sema, body: &Block) -> (Vec<(Span, String, String)>, Vec<(Span, String, String)>) {
    let mut io: Vec<(Span, String, String)> = Vec::new();
    let mut alloc: Vec<(Span, String, String)> = Vec::new();
    crate::sema::walk_block(body, &mut |e: &Expr| {
        match &e.kind {
            ExprKind::ArenaRef(name) => match s.t.regions.get(name) {
                Some(RegionRef::Mmio(_)) => io.push((e.span, format!("MMIO リージョン `{}`", name), format!("the MMIO region `{}`", name))),
                Some(RegionRef::Port(_)) => io.push((e.span, format!("I/O ポート `{}`", name), format!("the I/O port `{}`", name))),
                Some(RegionRef::Dma(_)) => io.push((e.span, format!("dma リージョン `{}`", name), format!("the dma region `{}`", name))),
                _ => {}
            },
            ExprKind::Field(base, _, _) => {
                if matches!(s.expr_ty.get(base.id as usize), Some(Ty::MmioBind(_))) {
                    io.push((e.span, "bind_mmio で結んだ装置のレジスタ".to_string(), "a device register bound with bind_mmio".to_string()));
                }
            }
            ExprKind::Own(_) | ExprKind::OwnAlloc(_) => alloc.push((e.span, "own の確保（ヒープ）".to_string(), "an own allocation (the heap)".to_string())),
            _ => {}
        }
        match s.calls.get(&e.id) {
            Some(CallKind::Builtin(Builtin::MmioRead(_) | Builtin::MmioWrite(_))) => io.push((e.span, "番地を直接使う MMIO の読み書き".to_string(), "a raw-address MMIO read / write".to_string())),
            Some(CallKind::Builtin(Builtin::SubmitDma)) => io.push((e.span, "submit_dma（DMA の発行）".to_string(), "submit_dma (issuing DMA)".to_string())),
            _ => {}
        }
        match s.type_calls.get(&e.id) {
            Some(TypeCallKind::BindMmio(_)) => io.push((e.span, "bind_mmio（装置の窓）".to_string(), "bind_mmio (a device window)".to_string())),
            Some(TypeCallKind::CompleteDma(_)) => io.push((e.span, "complete_dma（DMA の完了）".to_string(), "complete_dma (DMA completion)".to_string())),
            Some(TypeCallKind::DmaAlloc(_)) => alloc.push((e.span, "dma_alloc（DMA プールからの確保）".to_string(), "dma_alloc (an allocation from the DMA pool)".to_string())),
            None => {}
        }
        if s.gc_calls.contains_key(&e.id) {
            alloc.push((e.span, "fascon-gc の操作（promote_to_gc / gc_collect）".to_string(), "a fascon-gc operation (promote_to_gc / gc_collect)".to_string()));
        }
        if let Some(k) = s.methods.get(&e.id) {
            match k {
                MethodKind::DmaLend(a) | MethodKind::DmaReclaim(a) => {
                    let n = &s.t.arenas[*a].name;
                    io.push((e.span, format!("dma_arena `{}` の貸し出し・回収", n), format!("a lend / reclaim on the dma_arena `{}`", n)));
                }
                MethodKind::GpuDispatch(a) | MethodKind::GpuReclaim(a) | MethodKind::GpuReclaimWait(a) => {
                    let n = &s.t.arenas[*a].name;
                    io.push((e.span, format!("gpu_arena `{}` の gpu_dispatch / reclaim", n), format!("gpu_dispatch / reclaim on the gpu_arena `{}`", n)));
                }
                MethodKind::MmioFill(_) | MethodKind::MmioPut(_) | MethodKind::MmioFillRect(_) => io.push((e.span, "MMIO の一括書き込み".to_string(), "a bulk MMIO write".to_string())),
                MethodKind::ArenaAlloc(a) if s.t.arenas[*a].gc => {
                    let n = &s.t.arenas[*a].name;
                    alloc.push((e.span, format!("gc_arena `{}` の確保", n), format!("an allocation in the gc_arena `{}`", n)));
                }
                _ => {}
            }
        }
    });
    (io, alloc)
}

/// --target hpc の検査（ほかのプロファイルでは何もしない） / the --target hpc checks (nothing for other profiles)
pub fn analyze(s: &mut Sema) {
    if s.profile != Profile::Hpc {
        return;
    }
    let topo = s.topology.unwrap_or(Topology::HPC_DEFAULT);
    let prog = s.prog;
    let n = s.t.fns.len();
    let mut diags: Vec<Diag> = Vec::new();
    let src_ja = match topo.source {
        2 => "--topology",
        1 => "`// fascon-topology:`",
        _ => "--target hpc の既定",
    };
    let src_en = match topo.source {
        2 => "--topology",
        1 => "`// fascon-topology:`",
        _ => "the --target hpc default",
    };
    // ルート / roots
    let mut roots: Vec<HpcRoot> = Vec::new();
    let mut mgmt_spawns: Vec<(usize, Span)> = Vec::new();
    for (eid, fi, sp) in s.spawns.clone() {
        let c = s.spawn_cores.get(&eid).copied().flatten();
        // F3903: コア番号が計算ノードの形を超える / F3903: a core number beyond the compute node's shape
        if let Some(v) = c {
            if v < 0 || v >= topo.cores as i128 {
                diags.push(
                    Diag::error("F3903", sp, format!("spawn_on のコア {} は計算ノードにありません（論理コアは 0〜{}。{}: {}）", v, topo.cores - 1, src_ja, topo.spec()), format!("core {} of spawn_on does not exist on the compute node (logical cores are 0-{}; {}: {})", v, topo.cores - 1, src_en, topo.spec()))
                        .hint("計算ノードの形は `--topology cores=N,smt=K,numa=M` か、ファイルの先頭の `// fascon-topology: cores=N smt=K numa=M` で決めます", "set the compute node's shape with `--topology cores=N,smt=K,numa=M` or `// fascon-topology: cores=N smt=K numa=M` at the top of the file"),
                );
                continue;
            }
        }
        match c {
            Some(0) => mgmt_spawns.push((fi, sp)),
            _ => roots.push(HpcRoot { core: c.map(|v| v as u32), func: fi, span: sp }),
        }
    }
    // 届く関数（ルートごとの幅優先） / reachable functions (breadth first per root)
    let allow_fn: Vec<bool> = (0..n).map(|f| matches!(&prog.items[s.t.fns[f].item], Item::Fn(fd) if has_allow_hpc_io(fd))).collect();
    let mut compute: HashMap<usize, usize> = HashMap::new();
    let mut reach_of: Vec<Vec<usize>> = Vec::new();
    for (ri, r) in roots.iter().enumerate() {
        let mut seen: HashSet<usize> = HashSet::new();
        let mut q: VecDeque<usize> = VecDeque::new();
        seen.insert(r.func);
        q.push_back(r.func);
        let mut order = Vec::new();
        while let Some(f) = q.pop_front() {
            order.push(f);
            compute.entry(f).or_insert(ri);
            for &c in &s.t.fns[f].callees {
                if seen.insert(c) {
                    q.push_back(c);
                }
            }
        }
        reach_of.push(order);
    }
    // F3901 / F3902
    let mut io: Vec<HpcIo> = Vec::new();
    let mut fn_sites: HashMap<usize, (Vec<(Span, String, String)>, Vec<(Span, String, String)>)> = HashMap::new();
    let mut seen_io: HashSet<u32> = HashSet::new();
    let mut seen_alloc: HashSet<u32> = HashSet::new();
    for (ri, order) in reach_of.iter().enumerate() {
        let root_allowed = allow_fn[roots[ri].func];
        for &f in order {
            if !fn_sites.contains_key(&f) {
                let st = match &prog.items[s.t.fns[f].item] {
                    Item::Fn(fd) => sites(s, &fd.body),
                    _ => (Vec::new(), Vec::new()),
                };
                fn_sites.insert(f, st);
            }
            let (ios, allocs) = fn_sites[&f].clone();
            let rname = s.t.fns[roots[ri].func].name.clone();
            let core_ja = roots[ri].core.map_or("（実行時に決まるコア）".to_string(), |c| format!("コア {}", c));
            let core_en = roots[ri].core.map_or("(a core chosen at run time)".to_string(), |c| format!("core {}", c));
            for (sp, ja, en) in ios {
                let allowed = root_allowed || allow_fn[f];
                if let Some(x) = io.iter_mut().find(|x| x.span == sp) {
                    // 外していないルートから届くなら、外していない扱い / reached from a root that does not lift it: not lifted
                    if x.allowed && !allowed {
                        x.allowed = false;
                        x.root = ri;
                    }
                } else {
                    io.push(HpcIo { func: f, span: sp, what_ja: ja, what_en: en, allowed, root: ri });
                }
            }
            for (sp, ja, en) in allocs {
                if !seen_alloc.insert(sp.lo) {
                    continue;
                }
                diags.push(
                    Diag::error(
                        "F3902",
                        sp,
                        format!("計算コアで動的な確保はできません: {}（`{}` は {} のルート `{}` から届きます）", ja, s.t.fns[f].name, core_ja, rname),
                        format!("no dynamic allocation on compute cores: {} (`{}` is reached from the {} root `{}`)", en, s.t.fns[f].name, core_en, rname),
                    )
                    .note(roots[ri].span, "計算コアのルートはここ", "the compute-core root is here")
                    .hint(
                        "計算フェーズのページフォールトとジッタを避けるため、確保は管理コア（コア 0）で計算の前に済ませ、計算コアには `own` の引数・shared arena・chan で渡してください",
                        "to avoid page faults and jitter during the compute phase, allocate on the management core (core 0) before computing and hand the data to compute cores as an `own` argument, a shared arena or a chan",
                    ),
                );
            }
        }
    }
    for x in &io {
        if x.allowed || !seen_io.insert(x.span.lo) {
            continue;
        }
        let r = &roots[x.root];
        let rname = s.t.fns[r.func].name.clone();
        let core_ja = r.core.map_or("（実行時に決まるコア）".to_string(), |c| format!("コア {}", c));
        let core_en = r.core.map_or("(a core chosen at run time)".to_string(), |c| format!("core {}", c));
        diags.push(
            Diag::warning(
                "F3901",
                x.span,
                format!("計算コアへ I/O の雑音が入ります: {} を、{} のルート `{}` から届く `{}` が使います", x.what_ja, core_ja, rname, s.t.fns[x.func].name),
                format!("OS / I/O noise on a compute core: {} is used by `{}`, reached from the {} root `{}`", x.what_en, s.t.fns[x.func].name, core_en, rname),
            )
            .note(r.span, "計算コアのルートはここ", "the compute-core root is here")
            .hint(
                "装置・DMA・GPU への出し入れは管理コア（コア 0）に任せ、計算コアとは chan / shared arena でやりとりしてください。わざとなら、その関数かルートに `#[allow(hpc_io)]` を付けます",
                "leave device, DMA and GPU traffic to the management core (core 0) and talk to compute cores through a chan or a shared arena; if it is intended, put `#[allow(hpc_io)]` on the function or the root",
            ),
        );
    }
    // F3903: #[numa_node(n)]
    let mut numa_arenas: Vec<(usize, u32)> = Vec::new();
    for item in &prog.items {
        if let Item::Arena(ad) = item {
            if let Some((v, sp)) = ad.numa_node {
                let Some(ai) = s.t.arena_of_name(&ad.name) else { continue };
                if v == u128::MAX {
                    diags.push(Diag::error("F3903", sp, "`#[numa_node(n)]` にはノードの番号（整数）が要ります", "`#[numa_node(n)]` needs a node number (an integer)"));
                } else if v >= topo.numa as u128 {
                    diags.push(
                        Diag::error("F3903", sp, format!("NUMA ノード {} は計算ノードにありません（ノードは 0〜{}。{}: {}）", v, topo.numa - 1, src_ja, topo.spec()), format!("NUMA node {} does not exist on the compute node (nodes are 0-{}; {}: {})", v, topo.numa - 1, src_en, topo.spec()))
                            .hint("計算ノードの形は `--topology cores=N,smt=K,numa=M` か `// fascon-topology: ..` で決めます", "set the compute node's shape with `--topology cores=N,smt=K,numa=M` or `// fascon-topology: ..`"),
                    );
                } else {
                    numa_arenas.push((ai, v as u32));
                }
            }
        }
    }
    // F3903: #[no_smt] のときの兄弟スレッドへの重複割り当て / sibling threads assigned with #[no_smt]
    let no_smt = s.t.fns.iter().any(|f| f.is_main && matches!(&prog.items[f.item], Item::Fn(fd) if fd.attrs.iter().any(|a| a.name == "no_smt")));
    if no_smt && topo.smt > 1 {
        // コア 0（管理コア）も 1 つの物理コアを使う / core 0 (the management core) uses a physical core too
        let mut used: Vec<(u32, u32, Option<Span>)> = vec![(0, topo.phys(0), None)];
        for r in &roots {
            let Some(c) = r.core else { continue };
            if used.iter().any(|(u, _, _)| *u == c) {
                continue;
            }
            let p = topo.phys(c);
            if let Some((u, _, usp)) = used.iter().find(|(_, q, _)| *q == p).cloned() {
                let mut d = Diag::error(
                    "F3903",
                    r.span,
                    format!("`#[no_smt]` なのに、コア {} はコア {} と同じ物理コア {} の兄弟スレッドです（{}: {}）", c, u, p, src_ja, topo.spec()),
                    format!("with `#[no_smt]`, core {} is a sibling thread of core {} on the same physical core {} ({}: {})", c, u, p, src_en, topo.spec()),
                )
                .hint(
                    format!("兄弟でないコアを選んでください（この形では物理コアは {} 個。論理コア c の物理コアは {}）", topo.cores / topo.smt, if topo.adjacent { "c / smt" } else { "c mod 物理コア数" }),
                    format!("pick cores that are not siblings (this shape has {} physical cores; the physical core of logical core c is {})", topo.cores / topo.smt, if topo.adjacent { "c / smt" } else { "c mod the physical core count" }),
                );
                if let Some(x) = usp {
                    d = d.note(x, "同じ物理コアを使う spawn_on はここ", "the spawn_on using the same physical core is here");
                }
                diags.push(d);
            } else {
                used.push((c, p, Some(r.span)));
            }
        }
    }
    s.diags.extend(diags);
    s.hpc = Some(HpcInfo { topology: topo, mgmt_spawns, roots, compute, io, numa_arenas, no_smt });
}

/// C 生成のあとの F3902: 計算コアから届く `for id in @a` の中で、Mirrored-SoA の列の写しを作り直す書き込み
/// F3902 after code generation: writes inside `for id in @a` reachable from compute cores that rebuild Mirrored-SoA
/// column mirrors
pub fn mirror_diags(s: &Sema, rep: &CgReport) -> Vec<Diag> {
    let Some(h) = &s.hpc else { return Vec::new() };
    let mut out = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    for (f, sp, arena) in &rep.mirror_loops {
        let Some(&ri) = h.compute.get(f) else { continue };
        if !seen.insert(sp.lo) {
            continue;
        }
        let r = &h.roots[ri];
        let rname = s.t.fns[r.func].name.clone();
        out.push(
            Diag::error(
                "F3902",
                *sp,
                format!("計算コアで Mirrored-SoA の列の写しを表ごと作り直すループです（`{}` の写しの列。`{}` は計算コアのルート `{}` から届きます）", arena, s.t.fns[*f].name, rname),
                format!("this loop rebuilds a Mirrored-SoA column mirror table-wide on a compute core (the mirror columns of `{}`; `{}` is reached from the compute-core root `{}`)", arena, s.t.fns[*f].name, rname),
            )
            .note(r.span, "計算コアのルートはここ", "the compute-core root is here")
            .hint(
                "書き込むフィールドが完全列化（True SoA: 列が正本で写しなし）にならない理由は `fascon regions` に出ます。理由を取り除くか、フィールドに `#[no_column]` を付けて写しを作らせないか、表ごとの更新は管理コア（コア 0）で行ってください",
                "`fascon regions` shows why the written field is not True SoA (the column as the only copy, no mirror); remove that reason, mark the field `#[no_column]` so no mirror is made, or do table-wide updates on the management core (core 0)",
            ),
        );
    }
    // 本体の書き込み集合には呼び出し先の書き込みも入る（data/rules.json の W）: `for id in @a` の本体から（推移的に）呼ぶ関数が
    // a の写しを作り直すなら、呼ぶたびに作り直す = 表ごとの作り直し
    // the write set of the body includes the writes of its callees (W in data/rules.json): when a function the body of
    // `for id in @a` calls (transitively) rebuilds mirrors of a, every call rebuilds = a table-wide rebuild
    let n = s.t.fns.len();
    let mut rebuilds: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for &(f, a) in &rep.mirror_fns {
        if f < n {
            rebuilds[f].insert(a);
        }
    }
    loop {
        let mut changed = false;
        for f in 0..n {
            for &g in &s.t.fns[f].callees {
                for a in rebuilds[g].clone() {
                    if rebuilds[f].insert(a) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut fns: Vec<usize> = h.compute.keys().copied().collect();
    fns.sort();
    for f in fns {
        let Item::Fn(fd) = &s.prog.items[s.t.fns[f].item] else { continue };
        let mut loops: Vec<(usize, Span, &Block)> = Vec::new();
        arena_loops_in(s, &fd.body, &mut loops);
        for (a, sp, body) in loops {
            if seen.contains(&sp.lo) {
                continue;
            }
            let mut hit: Option<(usize, Span)> = None;
            crate::sema::walk_block(body, &mut |e: &Expr| {
                if hit.is_some() {
                    return;
                }
                let cands: Vec<usize> = match s.calls.get(&e.id) {
                    Some(CallKind::User(g)) => vec![*g],
                    Some(CallKind::Indirect) => s.indirect_cands.get(&e.id).cloned().unwrap_or_default(),
                    _ => Vec::new(),
                };
                if let Some(g) = cands.into_iter().find(|g| *g < n && rebuilds[*g].contains(&a)) {
                    hit = Some((g, e.span));
                }
            });
            let Some((g, csp)) = hit else { continue };
            seen.insert(sp.lo);
            let ri = h.compute[&f];
            let r = &h.roots[ri];
            let rname = s.t.fns[r.func].name.clone();
            let arena = s.t.arenas[a].name.clone();
            let gname = s.t.fns[g].name.clone();
            out.push(
                Diag::error(
                    "F3902",
                    sp,
                    format!("計算コアで Mirrored-SoA の列の写しを表ごと作り直すループです（本体から呼ぶ `{}` が `{}` の写しの列を作り直すので、要素ごとに作り直します。`{}` は計算コアのルート `{}` から届きます）", gname, arena, s.t.fns[f].name, rname),
                    format!("this loop rebuilds a Mirrored-SoA column mirror table-wide on a compute core (`{}`, called from its body, rebuilds the mirror columns of `{}`, so they are rebuilt once per element; `{}` is reached from the compute-core root `{}`)", gname, arena, s.t.fns[f].name, rname),
                )
                // 自動修正（replace_in_note）は最初の注のルートを書き換える / the quick fix (replace_in_note) rewrites the root in the first note
                .note(r.span, "計算コアのルートはここ", "the compute-core root is here")
                .note(csp, format!("`{}` の写しを作り直す呼び出しはここ", arena), format!("the call that rebuilds the mirror of `{}` is here", arena))
                .hint(
                    "書き込むフィールドが完全列化（True SoA: 列が正本で写しなし）にならない理由は `fascon regions` に出ます。理由を取り除くか、フィールドに `#[no_column]` を付けて写しを作らせないか、表ごとの更新は管理コア（コア 0）で行ってください",
                    "`fascon regions` shows why the written field is not True SoA (the column as the only copy, no mirror); remove that reason, mark the field `#[no_column]` so no mirror is made, or do table-wide updates on the management core (core 0)",
                ),
            );
        }
    }
    out
}

/// ブロックの中の `for id in @a`（入れ子も）: (アリーナ, ループの場所, 本体) / every `for id in @a` in a block, nested ones
/// included: (arena, loop site, body)
fn arena_loops_in<'a>(s: &Sema, b: &'a Block, out: &mut Vec<(usize, Span, &'a Block)>) {
    fn ex<'a>(s: &Sema, e: &'a Expr, out: &mut Vec<(usize, Span, &'a Block)>) {
        if let ExprKind::For { iter: ForIter::Arena(name, _), body, .. } = &e.kind {
            if let Some(RegionRef::Arena(a)) = s.t.regions.get(name) {
                out.push((*a, e.span, body));
            }
        }
        for c in crate::flow::children(e) {
            match c {
                crate::flow::Child::E(x) => ex(s, x, out),
                crate::flow::Child::B(b) => arena_loops_in(s, b, out),
            }
        }
    }
    for st in &b.stmts {
        match st {
            Stmt::Let(l) => {
                if let Some(i) = &l.init {
                    ex(s, i, out);
                }
            }
            Stmt::Expr(e) => ex(s, e, out),
        }
    }
    if let Some(t) = &b.tail {
        ex(s, t, out);
    }
}
