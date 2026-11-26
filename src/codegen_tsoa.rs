//! 完全列化（True SoA）・非時間的ストア・書き込みの多いフィールドの除外
//! True SoA, non-temporal stores and the exclusion of write-heavy fields
//!
//! 完全列化: 集計が値として読む要素のフィールド f を、次の条件がすべて成り立つときスロットから外し、列 `ts_f[]` を唯一の正本に
//! する（書き込みは列への 1 回のストアだけで、写しを作り直す二重書き込みがなくなる）。
//!   - 表のアリーナ（共有・タスク・DMA・バンプ・gc でない）で、要素は #[stable_abi]・#[wire]・#[secret]・#[repr(align)] でない
//!     構造体。要素の型は extern "C" の関数の引数・戻り値に現れない
//!   - 要素が own を持たない（#[on_drop] は持ってよい。その関数が同じアリーナに触れないこと）
//!   - 要素への参照が外へ出ない: `@a.get(id)` / `get_mut(id)` の束縛はフィールドを読み書きするだけ、`&@a[id]`・要素の
//!     メソッド呼び出し・要素を束縛する match / if let がない。要素まるごとの読み書き（コピーできる要素）は組み立て直しで扱う
//!   - フィールドが 32 / 64 ビットの整数で、アドレスを取られず（`&@a[id].f`）、条件に出てこず、1.0.0 の走査に残る集計に
//!     読まれない（atomic・#[no_column] も除く）
//! 条件を満たさないアリーナとフィールドは今までどおりの写しの列（Mirrored）に残り、理由が fascon regions に出る。
//! True SoA: a field f that reductions read as a value leaves the element slot, and the column `ts_f[]` becomes its
//! only copy (a write is one store to the column; the dual write that rebuilt mirrors is gone), when all of these hold:
//!   - a table arena (not shared, task, DMA, bump or gc) whose element is a struct that is not #[stable_abi], #[wire],
//!     #[secret] or #[repr(align)], and whose type never appears in an extern "C" signature
//!   - the element holds no own (#[on_drop] is fine as long as that function does not touch the same arena)
//!   - no reference to an element escapes: bindings of `@a.get(id)` / `get_mut(id)` only read and write fields; no
//!     `&@a[id]`, no method call on an element, no match / if let binding an element. Whole-element reads and writes
//!     (of copyable elements) are rebuilt from and split into the slot and the columns
//!   - the field is a 32 / 64-bit integer, its address is never taken (`&@a[id].f`), it appears in no filter, and no
//!     reduction that keeps the 1.0.0 scan reads it (atomic and #[no_column] fields are excluded too)
//! Arenas and fields that fail stay on the mirrored columns as before; fascon regions prints why.
use super::*;

/// 非時間的ストアを自動で使う一括上書きの、列の大きさの下限（バイト）。これより小さい列は L2 に収まり、普通のストアのほうが
/// 速い（6 万タスクの 480 KB の列で測った: bench/RESULTS.md）
/// the column size from which bulk overwrites use non-temporal stores automatically (bytes). Smaller columns fit in L2,
/// where plain stores are faster (measured on the 480 KB column of 60,000 tasks: bench/RESULTS.md)
pub(super) const NT_AUTO_MIN_BYTES: u64 = 8 << 20;

/// 完全列化したフィールド / a True-SoA field
#[derive(Clone, Debug)]
pub(super) struct TsField {
    /// C のフィールド名 / the C field name
    pub name: String,
    pub it: IntTy,
    /// 浮動小数点の列（float / double。it は同じ幅の符号なし整数） / a float column (float / double; it is the unsigned
    /// integer of the same width)
    pub fty: Option<FloatTy>,
    /// このフィールドを読む集計の行 / source lines of the reductions that read it
    pub lines: Vec<usize>,
}

/// 親の式がこの式をどう使うか / how the parent expression uses this one
#[derive(Clone, Copy, PartialEq)]
enum Par {
    FieldBase,
    RefInner,
    MethodRecv,
    /// `.clone()` の受け手（要素まるごとなら組み立て直した一時値の複製になる） / the receiver of `.clone()` (for a whole
    /// element: a copy of the rebuilt temporary)
    CloneRecv,
    AssignLhs(bool),
    Scrut,
    Other,
}

/// 集計 1 つの読み方（完全列化の判定用） / how one reduction reads the element (for the True-SoA decision)
struct TsRed {
    a: usize,
    line: usize,
    /// 値として読む先頭のフィールド（道筋が 1 段のもの: 名前, 型, 1 段か） / fields read as values (name, type, single-step path)
    terms: Vec<(String, IntTy, bool)>,
    /// 浮動小数点の集計（#[fast_math]）の型と、最小 / 最大か / the float type of a #[fast_math] reduction, and whether it is a
    /// min / max
    fty: Option<FloatTy>,
    pick: bool,
    /// 条件が読むフィールドの先頭 / the head fields the filter reads
    conds: Vec<String>,
    /// 1.0.0 の走査に残る理由 / why it keeps the 1.0.0 scan
    fallback: Option<(String, String)>,
    /// 64 ビットの `<=` / `>=` の選択（鍵にしない） / a 64-bit `<=` / `>=` pick (never keyed)
    nonstrict64: bool,
    /// 走査の重み（ループの深さ d で 4^d） / the scan weight (4^d at loop depth d)
    weight: f64,
}

/// 解析の途中の状態 / the state of the analysis walk
struct TsScan {
    arena_bad: Vec<Option<(String, String)>>,
    field_bad: Vec<BTreeMap<String, (String, String)>>,
    binds: HashMap<BindId, usize>,
    reds: Vec<TsRed>,
    /// 書き込みの重み（表全体の掃引を 1 とする） / write weights (one sweep of the table = 1)
    writes: Vec<BTreeMap<String, f64>>,
    depth: u32,
    loops: Vec<usize>,
}

fn add_bad(m: &mut BTreeMap<String, (String, String)>, f: &str, ja: String, en: String) {
    m.entry(f.to_string()).or_insert((ja, en));
}

impl<'a, 'p> Cg<'a, 'p> {
    /// 完全列化するフィールドを決め、書き込みの多いフィールドを調べる（collect_columns の最初に 1 回）
    /// decide the True-SoA fields and find write-heavy fields (once, at the start of collect_columns)
    pub(super) fn plan_tsoa(&mut self) {
        let na = self.s.t.arenas.len();
        self.tsoa = vec![Vec::new(); na];
        self.ts_heavy = vec![BTreeMap::new(); na];
        self.report.tsoa_skips.clear();
        if !self.o.columns {
            return;
        }
        let mut sc = TsScan { arena_bad: vec![None; na], field_bad: vec![BTreeMap::new(); na], binds: HashMap::new(), reds: Vec::new(), writes: vec![BTreeMap::new(); na], depth: 0, loops: Vec::new() };
        let s = self.s;
        let prog = s.prog;
        for f in &s.t.fns {
            if let Item::Fn(fd) = &prog.items[f.item] {
                self.ts_block(&fd.body, &mut sc);
            }
        }
        // 候補: 列で走査できる集計が値として読む 1 段のフィールド / candidates: single-step fields that column-served reductions read as values
        let pb = self.o.ptr_bits;
        let mut cand: Vec<BTreeMap<String, (IntTy, Vec<usize>, Option<FloatTy>)>> = vec![BTreeMap::new(); na];
        let mut blocked: Vec<BTreeMap<String, (String, String)>> = vec![BTreeMap::new(); na];
        for r in &sc.reds {
            for c in &r.conds {
                add_bad(&mut blocked[r.a], c, "条件に出てくる（絞り込みのビット列はスロットと一緒に書く）".to_string(), "appears in a filter (the filter bit column is written with the slot)".to_string());
            }
            for (f, it, single) in &r.terms {
                if let Some((ja, en)) = &r.fallback {
                    add_bad(&mut blocked[r.a], f, format!("1.0.0 の走査に残る集計が読む（{} 行目: {}）", r.line, ja), format!("read by a reduction that keeps the 1.0.0 scan (line {}: {})", r.line, en));
                    continue;
                }
                if r.nonstrict64 {
                    add_bad(&mut blocked[r.a], f, format!("{} 行目の 64 ビットの `<=` / `>=` の選択は 1.0.0 の走査", r.line), format!("the 64-bit `<=` / `>=` pick on line {} keeps the 1.0.0 scan", r.line));
                    continue;
                }
                if !*single {
                    continue;
                }
                // 浮動小数点の最小 / 最大は鍵の列（IEEE-754 の順序の鍵）で走査する。値はスロットに残す
                // a float min / max scans a key column (IEEE-754 order keys); the value stays in the slot
                if r.fty.is_some() && r.pick {
                    add_bad(&mut blocked[r.a], f, format!("{} 行目の浮動小数点の最小 / 最大は鍵の列で走査する（値はスロットに残る）", r.line), format!("the float min / max on line {} scans a key column (the value stays in the slot)", r.line));
                    continue;
                }
                if matches!(it, IntTy::Usize | IntTy::Isize) {
                    add_bad(&mut blocked[r.a], f, "usize / isize（ポインタの幅）は写しの列のまま".to_string(), "usize / isize (pointer-sized) stays a mirrored column".to_string());
                    continue;
                }
                if it.bits(pb) < 32 {
                    add_bad(&mut blocked[r.a], f, "32 ビットより狭い（写しの列のまま）".to_string(), "narrower than 32 bits (stays a mirrored column)".to_string());
                    continue;
                }
                let e = cand[r.a].entry(f.clone()).or_insert((*it, Vec::new(), r.fty));
                if !e.1.contains(&r.line) {
                    e.1.push(r.line);
                }
            }
            if let Some((ja, en)) = &r.fallback {
                for c in &r.conds {
                    add_bad(&mut blocked[r.a], c, format!("1.0.0 の走査に残る集計が読む（{} 行目: {}）", r.line, ja), format!("read by a reduction that keeps the 1.0.0 scan (line {}: {})", r.line, en));
                }
            }
        }
        let mut arena_ok: Vec<Option<(String, String)>> = vec![None; na];
        for a in 0..na {
            if cand[a].is_empty() {
                continue;
            }
            arena_ok[a] = self.ts_arena_veto(a).or_else(|| sc.arena_bad[a].clone());
        }
        // 書き込みの多いフィールド（写しの列に残るもの）を除いていくと、その集計が 1.0.0 の走査に戻り、読むフィールドが完全列化
        // できなくなることがある。変わらなくなるまで繰り返す（完全列化の集合は減る一方）
        // removing write-heavy mirrored fields can send their reductions back to the 1.0.0 scan, which makes the fields
        // they read ineligible for True SoA; repeat until nothing changes (the True-SoA set only shrinks)
        loop {
            let ts_of = |a: usize, f: &str, blocked: &Vec<BTreeMap<String, (String, String)>>| arena_ok[a].is_none() && cand[a].contains_key(f) && !blocked[a].contains_key(f) && !sc.field_bad[a].contains_key(f);
            let mut reads: Vec<BTreeMap<String, f64>> = vec![BTreeMap::new(); na];
            for r in &sc.reds {
                if r.fallback.is_some() {
                    continue;
                }
                for f in r.terms.iter().map(|t| &t.0).chain(r.conds.iter()) {
                    *reads[r.a].entry(f.clone()).or_insert(0.0) += r.weight;
                }
            }
            let mut heavy: Vec<BTreeMap<String, (f64, f64)>> = vec![BTreeMap::new(); na];
            for a in 0..na {
                for (f, w) in &sc.writes[a] {
                    let rd = reads[a].get(f).copied().unwrap_or(0.0);
                    if rd > 0.0 && *w > rd && !ts_of(a, f, &blocked) {
                        heavy[a].insert(f.clone(), (*w, rd));
                    }
                }
            }
            let mut changed = false;
            for r in &sc.reds {
                if r.fallback.is_some() {
                    continue;
                }
                let hit = r.terms.iter().map(|t| &t.0).chain(r.conds.iter()).any(|f| heavy[r.a].contains_key(f));
                if hit {
                    for f in r.terms.iter().map(|t| &t.0).chain(r.conds.iter()) {
                        if ts_of(r.a, f, &blocked) {
                            add_bad(&mut blocked[r.a], f, format!("{} 行目の集計が書き込みの多いフィールドのため 1.0.0 の走査に戻る", r.line), format!("the reduction on line {} returns to the 1.0.0 scan because of a write-heavy field", r.line));
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                self.ts_heavy = heavy;
                break;
            }
        }
        // 決定と報告 / decide and report
        self.report.tsoa_layout.clear();
        for a in 0..na {
            let an = self.s.t.arenas[a].name.clone();
            let order: Vec<(String, String)> = match &self.s.t.arenas[a].elem {
                Ty::Struct(sid) => self.s.t.structs[*sid].fields.iter().map(|f| (sanitize_field(&f.name), f.name.clone())).collect(),
                _ => Vec::new(),
            };
            let mut raw: Vec<String> = Vec::new();
            for (f, src) in order {
                let Some((it, lines, fty)) = cand[a].get(&f).cloned() else { continue };
                let why = arena_ok[a].clone().or_else(|| sc.field_bad[a].get(&f).cloned()).or_else(|| blocked[a].get(&f).cloned());
                match why {
                    Some((ja, en)) => self.report.tsoa_skips.push((an.clone(), f.clone(), ja, en)),
                    None => {
                        self.tsoa[a].push(TsField { name: f.clone(), it, fty, lines });
                        raw.push(src);
                    }
                }
            }
            if !raw.is_empty() {
                self.report.tsoa_layout.push((an, raw));
            }
        }
        // 直後の文が同じ列を読む一括上書きのループ（非時間的ストアにしない） / bulk-overwrite loops whose next statement
        // reads the same column (kept on plain stores)
        let mut rr = HashSet::new();
        for f in &s.t.fns {
            if let Item::Fn(fd) = &prog.items[f.item] {
                self.nt_scan_block(&fd.body, &mut rr);
            }
        }
        self.nt_reread = rr;
    }

    fn nt_scan_block(&self, b: &Block, out: &mut HashSet<ExprId>) {
        let items: Vec<&Expr> = b
            .stmts
            .iter()
            .filter_map(|st| match st {
                Stmt::Expr(e) => Some(e),
                Stmt::Let(l) => l.init.as_ref(),
            })
            .chain(b.tail.iter().map(|t| &**t))
            .collect();
        for (k, x) in items.iter().enumerate() {
            if let ExprKind::For { bind, iter: ForIter::Arena(name, _), body, .. } = &x.kind {
                if let Some(a) = self.s.t.arena_of_name(name) {
                    if let Some(fs) = self.nt_fields(a, *bind, body) {
                        // 後ろの文を、次のループ（を含む文）まで見る / look at the following statements up to the next loop
                        // (or the statement containing it)
                        for next in &items[k + 1..] {
                            if fs.iter().any(|f| self.nt_reads(next, a, f)) {
                                out.insert(x.id);
                                break;
                            }
                            if self.nt_has_loop(next) {
                                break;
                            }
                        }
                    }
                }
            }
            self.nt_scan_expr(x, out);
        }
    }

    /// 式がループを含むか / does the expression contain a loop?
    fn nt_has_loop(&self, e: &Expr) -> bool {
        if matches!(e.kind, ExprKind::For { .. } | ExprKind::While { .. } | ExprKind::Loop { .. }) {
            return true;
        }
        crate::flow::children(e).into_iter().any(|c| match c {
            crate::flow::Child::E(x) => self.nt_has_loop(x),
            crate::flow::Child::B(b) => {
                b.stmts.iter().any(|st| match st {
                    Stmt::Let(l) => l.init.as_ref().map_or(false, |i| self.nt_has_loop(i)),
                    Stmt::Expr(x) => self.nt_has_loop(x),
                }) || b.tail.as_ref().map_or(false, |t| self.nt_has_loop(t))
            }
        })
    }

    fn nt_scan_expr(&self, e: &Expr, out: &mut HashSet<ExprId>) {
        for c in crate::flow::children(e) {
            match c {
                crate::flow::Child::E(x) => self.nt_scan_expr(x, out),
                crate::flow::Child::B(b) => self.nt_scan_block(b, out),
            }
        }
    }

    /// アリーナと要素の型だけで決まる、完全列化できない理由 / reasons against True SoA that depend only on the arena and its element type
    fn ts_arena_veto(&self, a: usize) -> Option<(String, String)> {
        let x = &self.s.t.arenas[a];
        let kind = if x.bump {
            Some(("バンプアリーナ", "a bump arena"))
        } else if x.task_stack.is_some() {
            Some(("task_arena", "a task_arena"))
        } else if x.dma {
            Some(("dma_arena（装置が読み書きする）", "a dma_arena (a device reads and writes it)"))
        } else if x.gpu {
            Some(("gpu_arena（装置がスロットをそのまま読み書きする）", "a gpu_arena (the device reads and writes the slots as they are)"))
        } else if x.shared {
            Some(("shared arena（全コアで 1 つの実体）", "a shared arena (one instance for every core)"))
        } else if x.gc {
            Some(("gc_arena", "a gc_arena"))
        } else if self.s.process_arenas.contains(&a) {
            Some(("プロセスのアリーナ", "a process arena"))
        } else {
            None
        };
        if let Some((ja, en)) = kind {
            return Some((format!("{}は完全列化しない", ja), format!("{}: never True SoA", en)));
        }
        let Ty::Struct(sid) = &x.elem else {
            return Some(("要素が構造体でない".to_string(), "the element is not a struct".to_string()));
        };
        let info = &self.s.t.structs[*sid];
        let sn = info.name.clone();
        if info.stable_abi {
            return Some((format!("要素 `{}` が #[stable_abi]（レイアウト固定）", sn), format!("the element `{}` is #[stable_abi] (fixed layout)", sn)));
        }
        if info.wire.is_some() {
            return Some((format!("要素 `{}` が #[wire]（バイト列のレイアウト）", sn), format!("the element `{}` is #[wire] (a byte layout)", sn)));
        }
        if info.secret {
            return Some((format!("要素 `{}` が #[secret]（破棄で揮発性の 0 埋め）", sn), format!("the element `{}` is #[secret] (volatile zeroing on drop)", sn)));
        }
        if info.align > 0 {
            return Some((format!("要素 `{}` が #[repr(align({}))]", sn, info.align), format!("the element `{}` is #[repr(align({}))]", sn, info.align)));
        }
        if info.fields.iter().any(|f| self.s.t.needs_drop(&f.ty)) {
            return Some((format!("要素 `{}` が own を持つ", sn), format!("the element `{}` holds own", sn)));
        }
        if let Some(od) = &info.on_drop {
            if let Some(fi) = self.s.t.fns.iter().find(|f| &f.name == od) {
                if fi.effects.touches(a) || fi.effects.allocs.contains(&a) {
                    return Some((format!("#[on_drop({})] が同じアリーナ @{} に触れる", od, x.name), format!("#[on_drop({})] touches the same arena @{}", od, x.name)));
                }
            }
        }
        // extern "C" の境界: 要素の型か、この表の Id（C 側が表の添字を持つ）が引数・戻り値に現れる
        // the extern "C" boundary: the element type, or an Id of this table (the C side holds table indices), appears
        // in a parameter or the return value
        for f in &self.s.t.fns {
            if f.extern_c.is_some() && (f.params.iter().any(|p| self.ts_mentions(&p.ty, *sid, a, 0)) || self.ts_mentions(&f.ret, *sid, a, 0)) {
                return Some((format!("要素 `{}` か @{} の Id が extern \"C\" の関数 `{}` の境界を通る", sn, x.name, f.name), format!("the element `{}` or an Id of @{} crosses the extern \"C\" boundary of `{}`", sn, x.name, f.name)));
            }
        }
        None
    }

    fn ts_mentions(&self, t: &Ty, sid: usize, a: usize, d: u32) -> bool {
        if d > 32 {
            return false;
        }
        match t {
            Ty::Id(x) => *x == a,
            Ty::Struct(s) => *s == sid || self.s.t.structs[*s].fields.iter().any(|f| self.ts_mentions(&f.ty, sid, a, d + 1)),
            Ty::Ref(_, i) | Ty::Own(i) | Ty::Option(i) | Ty::Array(i, _) | Ty::Slice(i) => self.ts_mentions(i, sid, a, d + 1),
            Ty::Result(i, e) => self.ts_mentions(i, sid, a, d + 1) || self.ts_mentions(e, sid, a, d + 1),
            _ => false,
        }
    }

    fn ts_block(&self, b: &'p Block, sc: &mut TsScan) {
        for st in &b.stmts {
            match st {
                Stmt::Let(l) => {
                    if let Some(i) = &l.init {
                        self.ts_expr(i, Par::Other, sc);
                    }
                }
                Stmt::Expr(e) => self.ts_expr(e, Par::Other, sc),
            }
        }
        if let Some(t) = &b.tail {
            self.ts_expr(t, Par::Other, sc);
        }
    }

    /// `@a[i]` の要素の場所なら、そのアリーナ（`@a[i]` そのものか、ルックアップの束縛） / the arena when e is an element place
    fn ts_elem_place(&self, e: &Expr, sc: &TsScan) -> Option<usize> {
        match &e.kind {
            ExprKind::Index(..) => self.s.arena_index.get(&e.id).copied(),
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Local(b)) => sc.binds.get(b).copied(),
                _ => None,
            },
            _ => None,
        }
    }

    fn ts_note(&self, sc: &mut TsScan, a: usize, line: usize, ja: &str, en: &str) {
        if sc.arena_bad[a].is_none() {
            sc.arena_bad[a] = Some((format!("{}（{} 行目）", ja, line), format!("{} (line {})", en, line)));
        }
    }

    /// 書き込みの重みを足す（a のループの中なら 1 掃引 × 4^外の深さ、外なら 1 要素 = 1 / 容量） / add a write weight (inside a
    /// loop over a: one sweep × 4^outer depth; outside: one element = 1 / capacity)
    fn ts_write(&self, sc: &mut TsScan, a: usize, path: &[String]) {
        let inside = sc.loops.iter().filter(|x| **x == a).count() > 0;
        let w = if inside { 4f64.powi(sc.depth as i32 - 1) } else { 4f64.powi(sc.depth as i32) / (self.s.t.arenas[a].cap.max(1) as f64) };
        let fields: Vec<String> = match path.first() {
            Some(f) => vec![f.clone()],
            None => match &self.s.t.arenas[a].elem {
                Ty::Struct(sid) => self.s.t.structs[*sid].fields.iter().map(|f| sanitize_field(&f.name)).collect(),
                _ => Vec::new(),
            },
        };
        for f in fields {
            *sc.writes[a].entry(f).or_insert(0.0) += w;
        }
    }

    fn ts_expr(&self, e: &'p Expr, par: Par, sc: &mut TsScan) {
        let line = self.line_of(e.span);
        // 要素の場所の使われ方 / how element places are used
        match &e.kind {
            ExprKind::Index(..) if self.s.arena_index.contains_key(&e.id) => {
                let a = self.s.arena_index[&e.id];
                let copy = self.s.t.is_copy(&self.s.t.arenas[a].elem);
                match par {
                    Par::FieldBase | Par::AssignLhs(false) | Par::CloneRecv => {}
                    Par::Other if copy => {}
                    Par::RefInner => self.ts_note(sc, a, line, "要素への参照 `&@a[id]`", "a reference to the element `&@a[id]`"),
                    Par::MethodRecv => self.ts_note(sc, a, line, "要素のメソッド呼び出し", "a method call on the element"),
                    Par::Scrut => self.ts_note(sc, a, line, "要素を束縛する match / if let", "a match / if let binding the element"),
                    _ => self.ts_note(sc, a, line, "要素まるごとの使い方を組み立て直せない", "a whole-element use that cannot be rebuilt"),
                }
            }
            ExprKind::Path(_) => {
                if let Some(Res::Local(b)) = self.s.res.get(&e.id) {
                    if let Some(&a) = sc.binds.get(b) {
                        if par != Par::FieldBase {
                            self.ts_note(sc, a, line, "get / get_mut の束縛（要素への参照）が外へ出る", "a get / get_mut binding (a reference to the element) escapes");
                        }
                    }
                }
            }
            ExprKind::Field(base, name, _) => {
                if let Some(a) = self.ts_elem_place(base, sc) {
                    let f = sanitize_field(name);
                    match par {
                        Par::RefInner => add_bad(&mut sc.field_bad[a], &f, format!("アドレスを取られる（`&@a[id].{}`、{} 行目）", f, line), format!("its address is taken (`&@a[id].{}`, line {})", f, line)),
                        Par::MethodRecv => add_bad(&mut sc.field_bad[a], &f, format!("フィールドのメソッド呼び出し（{} 行目）", line), format!("a method call on the field (line {})", line)),
                        Par::Scrut => add_bad(&mut sc.field_bad[a], &f, format!("フィールドを束縛する match / if let（{} 行目）", line), format!("a match / if let binding the field (line {})", line)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        // 書き込みの重み / write weights
        if let ExprKind::Assign { lhs, .. } = &e.kind {
            if let Some((a, _, path)) = self.arena_write_root(lhs) {
                self.ts_write(sc, a, &path);
            } else if let Some((b, Some(path))) = self.place_root_path(lhs) {
                if let Some(&a) = sc.binds.get(&b) {
                    self.ts_write(sc, a, &path);
                }
            }
        }
        // 子 / children
        match &e.kind {
            ExprKind::Field(base, _, _) => self.ts_expr(base, Par::FieldBase, sc),
            ExprKind::Ref { inner, .. } => self.ts_expr(inner, Par::RefInner, sc),
            ExprKind::MethodCall { recv, args, .. } => {
                if !matches!(recv.kind, ExprKind::ArenaRef(_)) {
                    // 浮動小数点の値のメソッド（to_bits・is_nan・is_finite・abs）は値を読むだけ / float value methods only read the value
                    let p = match self.s.methods.get(&e.id) {
                        Some(MethodKind::Clone) => Par::CloneRecv,
                        Some(MethodKind::FloatToBits(_) | MethodKind::FloatIsNan(_) | MethodKind::FloatIsFinite(_) | MethodKind::FloatAbs(_)) => Par::Other,
                        _ => Par::MethodRecv,
                    };
                    self.ts_expr(recv, p, sc);
                }
                for x in args {
                    self.ts_expr(x, Par::Other, sc);
                }
            }
            ExprKind::Assign { op, lhs, rhs } => {
                self.ts_expr(lhs, Par::AssignLhs(op.is_some()), sc);
                self.ts_expr(rhs, Par::Other, sc);
            }
            ExprKind::IfLet { pat, scrut, then, els } => {
                self.ts_scrut(scrut, std::slice::from_ref(pat), sc);
                self.ts_block(then, sc);
                if let Some(x) = els {
                    self.ts_expr(x, Par::Other, sc);
                }
            }
            ExprKind::Match { scrut, arms } => {
                let pats: Vec<Pattern> = arms.iter().map(|x| x.pat.clone()).collect();
                self.ts_scrut(scrut, &pats, sc);
                for x in arms {
                    self.ts_expr(&x.body, Par::Other, sc);
                }
            }
            ExprKind::For { bind, iter, body, .. } => {
                let mut arena = None;
                if let ForIter::Arena(name, _) = iter {
                    if let Some(a) = self.s.t.arena_of_name(name) {
                        arena = Some(a);
                        if let Some(plan) = self.reduction_plan(a, *bind, body) {
                            let r = self.ts_red(a, &plan, e, sc.depth);
                            sc.reds.push(r);
                        }
                    }
                }
                for c in crate::flow::children(e) {
                    if let crate::flow::Child::E(x) = c {
                        self.ts_expr(x, Par::Other, sc);
                    }
                }
                sc.depth += 1;
                if let Some(a) = arena {
                    sc.loops.push(a);
                }
                self.ts_block(body, sc);
                if arena.is_some() {
                    sc.loops.pop();
                }
                sc.depth -= 1;
            }
            ExprKind::While { cond, body } => {
                self.ts_expr(cond, Par::Other, sc);
                sc.depth += 1;
                self.ts_block(body, sc);
                sc.depth -= 1;
            }
            ExprKind::Loop { body } => {
                sc.depth += 1;
                self.ts_block(body, sc);
                sc.depth -= 1;
            }
            _ => {
                for c in crate::flow::children(e) {
                    match c {
                        crate::flow::Child::E(x) => self.ts_expr(x, Par::Other, sc),
                        crate::flow::Child::B(b) => self.ts_block(b, sc),
                    }
                }
            }
        }
    }

    /// if let / match の対象: ルックアップなら Some(b) の束縛を覚える（値ごとの束縛はルックアップの結果が外へ出る）
    /// the scrutinee of if let / match: for a lookup, remember the Some(b) bindings (binding the whole Option lets the result escape)
    fn ts_scrut(&self, scrut: &'p Expr, pats: &[Pattern], sc: &mut TsScan) {
        let lk = match self.s.methods.get(&scrut.id) {
            Some(MethodKind::ArenaGet(a)) | Some(MethodKind::ArenaGetMut(a)) if self.s.lookup_scrut.contains(&scrut.id) => Some(*a),
            _ => None,
        };
        match lk {
            Some(a) => {
                for p in pats {
                    match p {
                        Pattern::Some(Some((_, b, _)), _) => {
                            sc.binds.insert(*b, a);
                        }
                        Pattern::Bind(..) => self.ts_note(sc, a, self.line_of(scrut.span), "ルックアップの結果を値ごと束縛する", "a lookup result bound as a whole"),
                        _ => {}
                    }
                }
                if let ExprKind::MethodCall { args, .. } = &scrut.kind {
                    for x in args {
                        self.ts_expr(x, Par::Other, sc);
                    }
                }
            }
            None => self.ts_expr(scrut, Par::Scrut, sc),
        }
    }

    /// 集計 1 つの読み方 / how one reduction reads the element
    fn ts_red(&self, a: usize, plan: &RedPlan, e: &Expr, depth: u32) -> TsRed {
        let pb = self.o.ptr_bits;
        let cx = RedCx { a, var: plan.var, elem: plan.elem_bind, acc: Some(plan.acc), pick: plan.pick };
        let mut caps: Vec<BindId> = Vec::new();
        for c in &plan.conds {
            let _ = self.cond_c(c, &cx, "(*v)", &mut caps);
        }
        let mut deps: Vec<Vec<String>> = Vec::new();
        for c in &plan.conds {
            self.cond_paths(c, &cx, &mut deps);
        }
        let mut all = deps.clone();
        for t in &plan.terms {
            all.push(t.path.clone());
        }
        let fallback = if !caps.is_empty() {
            Some(("条件が外のローカルを読む".to_string(), "the filter reads a local".to_string()))
        } else {
            self.column_veto(a, &all)
        };
        let nonstrict64 = plan.kind == RedKind::MinMax && !plan.strict && plan.acc_it.bits(pb) > 32;
        let mut conds: Vec<String> = Vec::new();
        for d in &deps {
            if let Some(f) = d.first() {
                if !conds.contains(f) {
                    conds.push(f.clone());
                }
            }
        }
        let terms = plan.terms.iter().filter_map(|t| t.path.first().map(|f| (f.clone(), t.it, t.path.len() == 1))).collect();
        let nonstrict64 = nonstrict64 && plan.fty.is_none();
        TsRed { a, line: self.line_of(e.span), terms, conds, fallback, nonstrict64, weight: 4f64.powi(depth as i32), fty: plan.fty, pick: plan.kind == RedKind::MinMax }
    }

    /// 完全列化したフィールドの番号 / the index of a True-SoA field
    pub(super) fn ts_index(&self, a: usize, f: &str) -> Option<usize> {
        self.tsoa.get(a)?.iter().position(|x| x.name == f)
    }

    /// フィールドの読み書きの基になる要素の場所が完全列化のアリーナの要素なら、そのアリーナ
    /// the arena when the base of a field access is an element place of a True-SoA arena
    pub(super) fn ts_base(&self, base: &Expr) -> Option<usize> {
        let a = match &base.kind {
            ExprKind::Index(_, _) => self.s.arena_index.get(&base.id).copied()?,
            ExprKind::Path(_) => match self.s.res.get(&base.id) {
                Some(Res::Local(b)) => self.bind_arena.get(b).copied()?,
                _ => return None,
            },
            _ => return None,
        };
        if self.tsoa.get(a).map_or(true, |v| v.is_empty()) {
            None
        } else {
            Some(a)
        }
    }

    /// `@a[idx]` のスロットの添字（arena_ptr と同じ検査の分け方: 証明済み・その場で止める・読み出しのラッチ・番兵へ逸らす）
    /// the slot index of `@a[idx]` (the same split as arena_ptr: proven, stop on the spot, read latch, divert to the sentinel)
    pub(super) fn arena_ix(&mut self, e: &Expr, a: usize, idx: &Expr) -> String {
        let read = std::mem::replace(&mut self.read_ctx, false);
        let id = self.val(idx);
        let n = self.s.t.arenas[a].name.clone();
        let lix = match &idx.kind {
            ExprKind::Path(_) => match self.s.res.get(&idx.id) {
                Some(Res::Local(b)) => self.loop_ix.get(b).cloned(),
                _ => None,
            },
            _ => None,
        };
        if let (true, Some(ix)) = (self.fr.proven.contains(&e.id), lix) {
            // `for id in @a` の証明済みのループ変数: ループのスロットの添字（gen[] を読まない） / a proven loop variable of
            // `for id in @a`: the loop's slot index (no gen[] read)
            self.report.gen_elided += 1;
            ix
        } else if self.fr.proven.contains(&e.id) {
            self.report.gen_elided += 1;
            format!("F20_ID_INDEX({}, ARENA_{}_K)", id, n)
        } else if self.s.t.needs_drop(&self.s.t.arenas[a].elem) {
            self.report.gen_checked += 1;
            let site = self.site(e.span);
            format!("arena_{}_ix_strict({}, {})", n, id, site)
        } else {
            self.report.gen_latched += 1;
            self.flt_used = true;
            self.at_count += 1;
            if read {
                format!("arena_{}_ix_r({}, &_flt)", n, id)
            } else {
                format!("arena_{}_ix({}, &_flt)", n, id)
            }
        }
    }

    /// 完全列化したフィールドの左辺値（列の 1 要素） / the lvalue of a True-SoA field (one column element)
    pub(super) fn ts_lv(&mut self, base: &Expr, a: usize, f: &str) -> String {
        let n = self.s.t.arenas[a].name.clone();
        let ix = match &base.kind {
            ExprKind::Index(_, idx) => self.arena_ix(base, a, idx),
            _ => {
                self.read_ctx = false;
                let p = match self.s.res.get(&base.id) {
                    Some(Res::Local(b)) => self.names.get(b).cloned().unwrap_or_default(),
                    _ => String::new(),
                };
                format!("arena_{}_ix_p({})", n, p)
            }
        };
        format!("arena_{n}.col.ts_{f}[F20_COL_OF_SLOT({ix}, ARENA_{n}_K)]", n = n, f = f, ix = strip_parens(&ix))
    }

    /// スロットに残る部分の型の名前 / the type name of the part that stays in the slot
    pub(super) fn ts_part(&self, a: usize) -> String {
        format!("f20_part_{}_t", self.s.t.arenas[a].name)
    }

    /// 完全列化のアリーナの型と関数（スロットに残る部分・組み立て直し・分解・添字） / the types and functions of a True-SoA
    /// arena (the part left in the slot, rebuild, split, indices)
    pub(super) fn gen_tsoa_part(&mut self, ai: usize) -> String {
        let mut o = String::new();
        let Ty::Struct(sid) = self.s.t.arenas[ai].elem.clone() else { return o };
        let info = self.s.t.structs[sid].clone();
        let ts: Vec<String> = self.tsoa[ai].iter().map(|f| f.name.clone()).collect();
        let part = self.ts_part(ai);
        let mut body = String::new();
        let mut kept = 0;
        for f in &info.fields {
            if ts.contains(&sanitize_field(&f.name)) {
                continue;
            }
            kept += 1;
            let _ = writeln!(body, "    {};", self.cdecl(&f.ty, &f.name));
        }
        if kept == 0 {
            let _ = writeln!(body, "    uint8_t f20_empty_; /* {} */", self.c("すべてのフィールドが列にある", "every field lives in a column"));
        }
        // 表の外の C（計測ハーネス・BSP）が配置を知るための印 / markers that tell C outside the table (benchmark harnesses,
        // BSPs) about the layout
        let n = self.s.t.arenas[ai].name.clone();
        let _ = writeln!(o, "#define ARENA_{}_TRUE_SOA 1 /* {} */", n, self.c("完全列化の表（フィールド f は arena_<名前>.col.ts_f[i]）", "a True-SoA table (field f is arena_<name>.col.ts_f[i])"));
        for f in &ts {
            let _ = writeln!(o, "#define ARENA_{}_TS_{} 1", n, f);
        }
        let _ = writeln!(
            o,
            "/* {} */\ntypedef struct {{\n{}}} {};",
            self.c(
                &format!("完全列化（True SoA）: スロットに残る {} のフィールド（{} は列 ts_* だけにある）", info.name, ts.join(", ")),
                &format!("True SoA: the fields of {} that stay in the slot ({} live only in the columns ts_*)", info.name, ts.join(", "))
            ),
            body,
            part
        );
        o
    }

    /// 完全列化のアリーナの関数 / the functions of a True-SoA arena
    pub(super) fn gen_tsoa_fns(&mut self, ai: usize) -> String {
        let mut o = String::new();
        let Ty::Struct(sid) = self.s.t.arenas[ai].elem.clone() else { return o };
        let info = self.s.t.structs[sid].clone();
        let n = self.s.t.arenas[ai].name.clone();
        let et = self.cty(&self.s.t.arenas[ai].elem.clone());
        let id = self.id_name(ai);
        let ts: Vec<String> = self.tsoa[ai].iter().map(|f| f.name.clone()).collect();
        let part = self.ts_part(ai);
        let mut put = String::new();
        let mut load = String::new();
        for f in &info.fields {
            let fname = sanitize_field(&f.name);
            if ts.contains(&fname) {
                let _ = writeln!(put, "    arena_{n}.col.ts_{f}[c] = v->{f};", n = n, f = fname);
                let _ = writeln!(load, "    r.{f} = arena_{n}.col.ts_{f}[c];", n = n, f = fname);
            } else {
                let _ = writeln!(put, "    __builtin_memcpy(&p->{f}, &v->{f}, sizeof p->{f});", f = fname);
                let _ = writeln!(load, "    __builtin_memcpy(&r.{f}, &p->{f}, sizeof r.{f});", f = fname);
            }
        }
        let zero: String = ts.iter().map(|f| format!(" arena_{n}.col.ts_{f}[c] = 0;", n = n, f = f)).collect();
        let _ = writeln!(
            o,
            "/* {c} */\n#define arena_{n}_ix_p(p) ((uint32_t)((const f20_slot_{n}_t *)(const void *)(p) - arena_{n}.slot))\n",
            c = self.c("ルックアップの束縛（スロットの要素へのポインタ）からスロットの添字: ポインタの差（v はスロットの先頭のメンバ）", "the slot index from a lookup binding (a pointer to the slot element): the pointer difference (v is the slot's first member)"),
            n = n
        );
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED uint32_t arena_{n}_ix_r({id} id, uint32_t *F20_RESTRICT flt) {{\n    const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K), g = F20_ID_GEN(id, ARENA_{n}_K);\n    *flt |= (uint32_t)(arena_{n}.gen[i] != g) | (uint32_t)(g == 0u); /* F20_FAULT_STALE */\n    return i;\n}}\n",
            c = self.c("生存を証明できない Id の読み出し: 世代の比較はラッチへ流すだけ（arena_{名前}_at_r と同じ）。古い Id で読むのは列の中の同じ型の値か 0", "a read through an unproven Id: the generation compare only feeds the latch (as arena_<name>_at_r); a stale Id reads a value of the same type in the column, or 0").replace("{名前}", &n),
            n = n,
            id = id
        );
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED uint32_t arena_{n}_ix({id} id, uint32_t *F20_RESTRICT flt) {{\n    const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K), g = F20_ID_GEN(id, ARENA_{n}_K);\n    const uint32_t bad = (uint32_t)(arena_{n}.gen[i] != g) | (uint32_t)(g == 0u);\n    *flt |= bad; /* F20_FAULT_STALE */\n    return (i & (bad - 1u)) | (F20_SENTINEL(ARENA_{n}_K) & (0u - bad));\n}}\n",
            c = self.c(
                "生存を証明できない Id の書き込み: 世代が違えば番兵の添字（スロットは F20_SENTINEL、列は F20_COL_OF_SLOT で列の番兵）へ逸らしてラッチ。分岐なし",
                "a write through an unproven Id: a wrong generation is diverted to the sentinel index (the slot sentinel F20_SENTINEL; in columns F20_COL_OF_SLOT gives the column sentinel) and latched, with no branch"
            ),
            n = n,
            id = id
        );
        if self.s.t.needs_drop(&self.s.t.arenas[ai].elem) {
            let _ = writeln!(
                o,
                "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED uint32_t arena_{n}_ix_strict({id} id, const char *site) {{\n    if (FASCON_UNLIKELY(arena_{n}_get(id) == NULL)) fascon_stale_id_panic(id, ARENA_{n}_K, \"{n}\", site);\n    return F20_ID_INDEX(id, ARENA_{n}_K);\n}}\n",
                c = self.c("解放処理のある要素（#[on_drop]）: 古い Id はその場で止める（arena_{名前}_at_strict と同じ）", "an element with drop glue (#[on_drop]): a stale Id stops on the spot (as arena_<name>_at_strict)").replace("{名前}", &n),
                n = n,
                id = id
            );
        }
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED {et} arena_{n}_load(uint32_t i) {{\n    const {part} *p = &arena_{n}.slot[i].v;\n    const uint32_t c = F20_COL_OF_SLOT(i, ARENA_{n}_K);\n    {et} r;\n{load}    return r;\n}}\n",
            c = self.c("要素まるごとの読み出し: スロットに残る部分と列から組み立て直す（i はスロットの添字。番兵なら列の番兵を読む）", "a whole-element read: rebuilt from the part in the slot and the columns (i is the slot index; the sentinel reads the column sentinel)"),
            et = et,
            n = n,
            part = part,
            load = load
        );
        let mirror = if self.arena_cols[ai].is_empty() { String::new() } else { format!("    arena_{n}_mirror(p, c); /* {} */\n", self.c("写しの列（条件のビット列など）", "the mirrored columns (filter bit columns etc.)"), n = n) };
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED void arena_{n}_put(uint32_t i, const {et} *F20_RESTRICT v) {{\n    {part} *p = &arena_{n}.slot[i].v;\n    const uint32_t c = F20_COL_OF_SLOT(i, ARENA_{n}_K);\n{put}{mirror}}}\n",
            c = self.c("要素まるごとの書き込み: スロットに残る部分と列（列が唯一の正本）へ分ける（i はスロットの添字。番兵なら列の番兵へ）", "a whole-element write: split into the part in the slot and the columns (the columns are the only copy; i is the slot index, the sentinel writes the column sentinel)"),
            et = et,
            n = n,
            part = part,
            put = put,
            mirror = mirror
        );
        let _ = writeln!(
            o,
            "/* {c} */\n#define arena_{n}_ts_zero(c) do {{{z} }} while (0)\n",
            c = self.c("死んだスロットの完全列化の値は 0（総和はそのまま列を足せる）", "the True-SoA values of a dead slot are 0 (sums add the column as it is)"),
            n = n,
            z = zero
        );
        o
    }

    /// 完全列化の列の宣言（列の構造体の中） / the True-SoA column declarations (inside the column struct)
    pub(super) fn gen_tsoa_cols(&self, ai: usize) -> String {
        let n = self.s.t.arenas[ai].name.clone();
        let sn = self.s.t.display(&self.s.t.arenas[ai].elem);
        let mut o = String::new();
        for f in &self.tsoa[ai] {
            let ls = f.lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(", ");
            let _ = writeln!(
                o,
                "    F20_ALIGN128 {} ts_{}[F20_COL_LEN(ARENA_{}_K)]; /* {} */",
                f.fty.map_or(f.it.c_name(), |ft| ft.c_name()),
                f.name,
                n,
                self.c(&format!("完全列化（True SoA）: {}.{} の唯一の正本（死んだスロットは 0。{} 行目の集計）", sn, f.name, ls), &format!("True SoA: the only copy of {}.{} (dead slots 0; reductions on line {})", sn, f.name, ls))
            );
        }
        o
    }

    /// 完全列化の列を走査する集計の本体 / the body of a reduction over True-SoA columns
    pub(super) fn ts_red_body(&mut self, a: usize, plan: &RedPlan, u: &ColUse, f: &mut String, body: &mut String) -> String {
        let an = self.s.t.arenas[a].name.clone();
        if let (Some(ft), ColUse::SumT(srcs)) = (plan.fty, u) {
            return self.ts_fsum_body(a, ft, plan, srcs, f, body);
        }
        let act = self.cty(&Ty::Int(plan.acc_it));
        let pb = self.o.ptr_bits;
        let cols: Vec<String> = self.arena_cols[a].iter().map(|c| c.name.clone()).collect();
        let mask_of = |m: &Option<usize>| match m {
            Some(k) => format!("arena->col.{}", cols[*k]),
            None => "arena->live".to_string(),
        };
        match u {
            ColUse::SumT(srcs) => {
                let filtered = !plan.conds.is_empty();
                let desc: Vec<String> = srcs
                    .iter()
                    .map(|s| match s {
                        SumSrc::Col(k) => cols[*k].clone(),
                        SumSrc::Ts { f, mask } => format!("ts_{}{}", self.tsoa[a][*f].name, mask.map(|k| format!(" & {}", cols[k])).unwrap_or_default()),
                    })
                    .collect();
                let _ = writeln!(
                    f,
                    " *   {}",
                    self.c(
                        &format!("完全列化の列（唯一の正本）を読む: {}。{}スロットは読まない。", desc.join(", "), if filtered { "条件は条件のビット列をマスクにして走査の時点で掛ける（写しの列は作らない）。" } else { "" }),
                        &format!("reads the True-SoA columns (the only copy): {}. {}The slots are not read.", desc.join(", "), if filtered { "The filter is applied at scan time with the filter bit column as a mask (no mirror column). " } else { "" })
                    )
                );
                let _ = writeln!(
                    f,
                    " *   {}",
                    self.c(
                        "死んだスロットは列で 0。64 スロットの塊ごとに、マスクの語が 0 なら飛ばし、塊の中は分岐なし（f20_col_sum_u64 / _u64_m / _u32 / _u32_m）。あふれは OR の上界、足りなければ正確な走査をやり直す。",
                        "dead slots are 0 in the column; per 64-slot block a zero mask word skips the block and the inside has no branch (f20_col_sum_u64 / _u64_m / _u32 / _u32_m); overflow uses the OR bound, with an exact rerun when it is not enough."
                    )
                );
                let _ = writeln!(body, "    const uint32_t n = F20_COL_SCAN_END(arena->st.next_unused); /* {} */", self.c("64 の倍数へ切り上げ（next_unused から先は一度も使われていない 0）", "rounded up to a multiple of 64 (slots from next_unused on were never used and are 0)"));
                for (j, s) in srcs.iter().enumerate() {
                    match s {
                        SumSrc::Col(k) => {
                            let wide = matches!(self.arena_cols[a][*k].kind, ColKind::Agg { wide: true });
                            if wide {
                                let _ = writeln!(body, "    const uint64_t s{} = f20_col_sum_u64(arena->col.{}, arena->live, n, flt);", j, cols[*k]);
                            } else {
                                let _ = writeln!(body, "    const uint64_t s{} = f20_col_sum_u32(arena->col.{}, arena->live, n);", j, cols[*k]);
                            }
                        }
                        SumSrc::Ts { f: fi, mask } => {
                            let tf = self.tsoa[a][*fi].clone();
                            let wide = tf.it.bits(pb) > 32;
                            match (wide, mask) {
                                (true, None) => {
                                    let _ = writeln!(body, "    const uint64_t s{} = f20_col_sum_u64(arena->col.ts_{}, arena->live, n, flt);", j, tf.name);
                                }
                                (false, None) => {
                                    let _ = writeln!(body, "    const uint64_t s{} = f20_col_sum_u32(arena->col.ts_{}, arena->live, n);", j, tf.name);
                                }
                                (true, Some(_)) => {
                                    let _ = writeln!(body, "    const uint64_t s{} = f20_col_sum_u64_m(arena->col.ts_{}, {}, n, flt);", j, tf.name, mask_of(mask));
                                }
                                (false, Some(_)) => {
                                    let _ = writeln!(body, "    const uint64_t s{} = f20_col_sum_u32_m(arena->col.ts_{}, {}, n);", j, tf.name, mask_of(mask));
                                }
                            }
                        }
                    }
                }
                let _ = writeln!(body, "    {} r = acc;\n    uint32_t ov = 0u;", act);
                for j in 0..srcs.len() {
                    let _ = writeln!(body, "    ov |= (uint32_t)__builtin_add_overflow(r, s{}, &r);", j);
                }
                let _ = writeln!(body, "    *flt |= ov << 1; /* F20_FAULT_ARITH */\n    return r;");
                if filtered {
                    self.c("条件付き総和（完全列化の列 + 条件のビット列）", "filtered sum (True-SoA column + filter bit column)")
                } else {
                    self.c("総和（完全列化の列）", "sum (True-SoA column)")
                }
            }
            ColUse::PickT { f: fi, mask, last } => {
                let tf = self.tsoa[a][*fi].clone();
                let max = !plan.less;
                let it = plan.acc_it;
                let wide = it.bits(pb) > 32;
                let x = if wide {
                    match (it.signed(), max) {
                        (false, false) => "UINT64_C(0x8000000000000000)",
                        (false, true) => "UINT64_C(0x7FFFFFFFFFFFFFFF)",
                        (true, false) => "UINT64_C(0)",
                        (true, true) => "~UINT64_C(0)",
                    }
                } else if max {
                    "~UINT64_C(0)"
                } else {
                    "UINT64_C(0)"
                };
                let m = match mask {
                    Some(k) => format!("arena->col.{}", cols[*k]),
                    None => "arena->live".to_string(),
                };
                let _ = writeln!(
                    f,
                    " *   {}",
                    self.c(
                        &format!("完全列化の列 ts_{} を {} をマスクにして読む。鍵はその場で作る（key = (int64)(値 ^ x)、x は型と向きの定数）。鍵の列は作らない。", tf.name, m.replace("arena->", "")),
                        &format!("reads the True-SoA column ts_{} masked by {}; keys are made on the fly (key = (int64)(value ^ x), x a constant for the type and direction); no key column is kept.", tf.name, m.replace("arena->", ""))
                    )
                );
                let _ = writeln!(
                    f,
                    " *   {}",
                    self.c(
                        &format!("段 1: 64 スロットの塊ごとの最小（ベクトル、素の SSE2 などは CMOV の鎖か立っているビットだけ）。段 2: 勝った塊で{}（逐次のループと同じ結果）。", if *last { "最後の一致を CLZ で（`<=` / `>=`）" } else { "最初の一致を TZCNT で（`<` / `>`）" }),
                        &format!("stage 1: the minimum per 64-slot block (vectors; on plain SSE2 etc. CMOV chains or only the set bits); stage 2: {} in the winning block (the same result as the sequential loop).", if *last { "the last match via CLZ (`<=` / `>=`)" } else { "the first match via TZCNT (`<` / `>`)" })
                    )
                );
                let enc = self.key_enc(it, max, "*acc");
                let dec = self.key_dec(it, max, "best");
                let _ = writeln!(body, "    int64_t best = {}; /* {} */", enc, self.c("今の値の鍵（これより良いものだけが選ばれる）", "the key of the current value (only better ones are picked)"));
                if wide {
                    let _ = writeln!(body, "    const uint32_t i = f20_col_pick64_m((const uint64_t *)(const void *)arena->col.ts_{}, {}, F20_COL_SCAN_END(arena->st.next_unused), &best, {}, {});", tf.name, m, if *last { 1 } else { 0 }, x);
                } else {
                    let _ = writeln!(body, "    const uint32_t i = f20_col_pick32_m((const uint32_t *)(const void *)arena->col.ts_{}, {}, F20_COL_SCAN_END(arena->st.next_unused), &best, {}, {}, {});", tf.name, m, if *last { 1 } else { 0 }, if it.signed() { 1 } else { 0 }, x);
                }
                if plan.pick.is_some() {
                    let _ = writeln!(body, "    if (i != 0xFFFFFFFFu) {{\n        *acc = {};\n        *pick = F20_ID_MAKE(arena->gen[i], i, ARENA_{}_K);\n    }}", dec, an);
                } else {
                    let _ = writeln!(body, "    if (i != 0xFFFFFFFFu) *acc = {};", dec);
                }
                self.c("最小 / 最大の選択（完全列化の列）", "min / max pick (True-SoA column)")
            }
            _ => String::new(),
        }
    }

    /// #[fast_math] の浮動小数点の総和の本体（完全列化の float / double の列。死んだスロットは 0.0 なので生存マスクは要らない。
    /// 条件は条件のビット列をマスクにする） / the body of a #[fast_math] float sum (True-SoA float / double columns; dead slots
    /// are 0.0, so no live mask; a filter uses the filter bit column as a mask)
    fn ts_fsum_body(&mut self, a: usize, ft: FloatTy, plan: &RedPlan, srcs: &[SumSrc], f: &mut String, body: &mut String) -> String {
        let cols: Vec<String> = self.arena_cols[a].iter().map(|c| c.name.clone()).collect();
        let filtered = !plan.conds.is_empty();
        let ct = ft.c_name();
        let fname = ft.name();
        let desc: Vec<String> = srcs
            .iter()
            .filter_map(|s| match s {
                SumSrc::Ts { f, mask } => Some(format!("ts_{}{}", self.tsoa[a][*f].name, mask.map(|k| format!(" & {}", cols[k])).unwrap_or_default())),
                SumSrc::Col(_) => None,
            })
            .collect();
        let _ = writeln!(
            f,
            " *   {}",
            self.c(
                &format!("#[fast_math] の浮動小数点の総和: 完全列化の {} の列（唯一の正本）を読む: {}。{}スロットは読まない。", ct, desc.join(", "), if filtered { "条件は条件のビット列をマスクにする。" } else { "" }),
                &format!("a #[fast_math] float sum: reads the True-SoA {} columns (the only copy): {}. {}The slots are not read.", ct, desc.join(", "), if filtered { "The filter is the filter bit column used as a mask. " } else { "" })
            )
        );
        let _ = writeln!(
            f,
            " *   {}",
            self.c(
                &format!("f20_col_sum_{}{}: 4 本の独立なベクトルの累計（レーンごとに足すので、書いた順と結合の順序が違う。#[fast_math] がそれを許す）。死んだスロットは 0.0。", fname, if filtered { "_m" } else { "" }),
                &format!("f20_col_sum_{}{}: four independent vector accumulators (lanes add in parallel, so the association order differs from the written one; #[fast_math] allows it); dead slots are 0.0.", fname, if filtered { "_m" } else { "" })
            )
        );
        let _ = writeln!(body, "    const uint32_t n = F20_COL_SCAN_END(arena->st.next_unused);");
        for (j, s) in srcs.iter().enumerate() {
            if let SumSrc::Ts { f: fi, mask } = s {
                let tn = self.tsoa[a][*fi].name.clone();
                match mask {
                    None => {
                        let _ = writeln!(body, "    const {} s{} = f20_col_sum_{}(arena->col.ts_{}, arena->live, n);", ct, j, fname, tn);
                    }
                    Some(k) => {
                        let _ = writeln!(body, "    const {} s{} = f20_col_sum_{}_m(arena->col.ts_{}, arena->col.{}, n);", ct, j, fname, tn, cols[*k]);
                    }
                }
            }
        }
        let _ = writeln!(body, "    {} r = acc;", ct);
        for j in 0..srcs.len() {
            let _ = writeln!(body, "    r += s{};", j);
        }
        let _ = writeln!(body, "    return r;");
        if filtered {
            self.c("#[fast_math] の浮動小数点の条件付き総和（完全列化の列 + 条件のビット列）", "#[fast_math] filtered float sum (True-SoA column + filter bit column)")
        } else {
            self.c("#[fast_math] の浮動小数点の総和（完全列化の列）", "#[fast_math] float sum (True-SoA column)")
        }
    }

    /// 非時間的ストアにできる一括上書きのループなら、書くフィールド: 本体がすべて `@a[id].f = 値;`（f は完全列化、値は f を
    /// 読まない純粋な式）で、ループ変数で添字を引くもの
    /// the fields of a bulk-overwrite loop that can use non-temporal stores: every statement of the body is `@a[id].f =
    /// value;` (f a True-SoA field, the value a pure expression that does not read f) indexed by the loop variable
    pub(super) fn nt_fields(&self, a: usize, var: BindId, body: &Block) -> Option<Vec<String>> {
        if self.tsoa.get(a).map_or(true, |v| v.is_empty()) {
            return None;
        }
        let mut xs: Vec<&Expr> = Vec::new();
        for st in &body.stmts {
            let Stmt::Expr(x) = st else { return None };
            xs.push(x);
        }
        if let Some(t) = &body.tail {
            if !matches!(t.kind, ExprKind::Unit) {
                xs.push(t);
            }
        }
        let mut out: Vec<String> = Vec::new();
        for x in xs {
            let ExprKind::Assign { op: None, lhs, rhs } = &x.kind else { return None };
            let ExprKind::Field(base, name, _) = &lhs.kind else { return None };
            let ExprKind::Index(_, idx) = &base.kind else { return None };
            if self.s.arena_index.get(&base.id) != Some(&a) || self.local_of(idx) != Some(var) || !self.fr.proven.contains(&base.id) {
                return None;
            }
            let f = sanitize_field(name);
            self.ts_index(a, &f)?;
            if !self.pure(rhs) || self.nt_reads(rhs, a, &f) {
                return None;
            }
            if !out.contains(&f) {
                out.push(f);
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// 式が `@a[..].f` を読むか / does the expression read `@a[..].f`?
    pub(super) fn nt_reads(&self, e: &Expr, a: usize, f: &str) -> bool {
        if let ExprKind::Field(base, name, _) = &e.kind {
            if sanitize_field(name) == f && self.s.arena_index.get(&base.id) == Some(&a) {
                return true;
            }
        }
        if let ExprKind::For { iter: ForIter::Arena(name, _), body, bind, .. } = &e.kind {
            if self.s.t.arena_of_name(name) == Some(a) {
                if let Some(plan) = self.reduction_plan(a, *bind, body) {
                    if plan.terms.iter().any(|t| t.path.first().map(|x| x.as_str()) == Some(f)) {
                        return true;
                    }
                }
            }
        }
        crate::flow::children(e).into_iter().any(|c| match c {
            crate::flow::Child::E(x) => self.nt_reads(x, a, f),
            crate::flow::Child::B(b) => b.stmts.iter().any(|s| match s {
                Stmt::Let(l) => l.init.as_ref().map_or(false, |i| self.nt_reads(i, a, f)),
                Stmt::Expr(x) => self.nt_reads(x, a, f),
            }) || b.tail.as_ref().map_or(false, |t| self.nt_reads(t, a, f)),
        })
    }

    /// 非時間的ストアを使うか（理由つき） / whether to use non-temporal stores (with the reason)
    pub(super) fn nt_decide(&self, a: usize, fields: &[String], next_reads: bool) -> (bool, String, String) {
        let x = &self.s.t.arenas[a];
        let env = self.s.ctx.env_all.touches(a) || self.s.ctx.env_all.allocs.contains(&a) || self.s.ctx.shared.iter().any(|s| s.0 == a);
        let bytes: u64 = fields.iter().filter_map(|f| self.ts_index(a, f)).map(|k| x.cap as u64 * (self.tsoa[a][k].it.bits(self.o.ptr_bits) as u64 / 8)).max().unwrap_or(0);
        match self.o.nontemporal {
            NtMode::Off => (false, "--nontemporal=off".to_string(), "--nontemporal=off".to_string()),
            NtMode::On => (true, "--nontemporal=on（ループの出口に F20_SFENCE）".to_string(), "--nontemporal=on (F20_SFENCE at the loop exit)".to_string()),
            NtMode::Auto => {
                if env {
                    (false, "割り込みハンドラ・タスクと共有するアリーナ（普通のストアのまま）".to_string(), "an arena shared with interrupt handlers or tasks (plain stores)".to_string())
                } else if next_reads {
                    (false, "直後の文が同じ列を読む（キャッシュに残す）".to_string(), "the next statement reads the same column (kept in cache)".to_string())
                } else if bytes < NT_AUTO_MIN_BYTES {
                    (false, format!("列 {} B は {} B 未満（キャッシュに収まる）", group(bytes), group(NT_AUTO_MIN_BYTES)), format!("the column of {} B is below {} B (fits in cache)", group(bytes), group(NT_AUTO_MIN_BYTES)))
                } else {
                    (true, format!("列 {} B ≥ {} B（ループの出口に F20_SFENCE）", group(bytes), group(NT_AUTO_MIN_BYTES)), format!("the column of {} B ≥ {} B (F20_SFENCE at the loop exit)", group(bytes), group(NT_AUTO_MIN_BYTES)))
                }
            }
        }
    }
}
