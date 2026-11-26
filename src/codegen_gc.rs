//! fascon-gc のコード生成（gc_arena） / code generation for fascon-gc (gc_arena)
//!
//! gc_arena を使わないプログラムには何も出さない（既存の arena / bump_arena の生成コードは 1 文字も変わらない）。
//! nothing is emitted for programs without gc_arena (the generated code of existing arena / bump_arena is unchanged).
//!
//!   - 型記述子（.rodata）: どのオフセットにどの gc_arena の Id・own があるか / type descriptors (.rodata): where the
//!     Ids of which gc arena and the own pointers are
//!   - ヒープ（gc_arena ごと。共有でなければコアごとの TLS）と、根を回す関数 / heaps (one per gc arena; per-core TLS
//!     unless shared) and the root-walking function
//!   - 関数の枠（シャドウスタック）と根の登録（束縛の有効範囲で外れる） / function frames (the shadow stack) and root
//!     registration (removed when the binding's scope ends)
use super::*;

impl<'a, 'p> Cg<'a, 'p> {
    /// プログラムが gc_arena を使うか / whether the program uses gc_arena
    pub(super) fn gc_any(&self) -> bool {
        self.s.t.arenas.iter().any(|a| a.gc)
    }

    /// gc_arena → ヒープの番号 / gc arena → heap number
    pub(super) fn gc_heap(&self, a: usize) -> Option<usize> {
        if !self.s.t.arenas[a].gc {
            return None;
        }
        Some(self.s.t.arenas[..a].iter().filter(|x| x.gc).count())
    }

    /// 型が gc_arena の Id を含むか（through_own = own の先も見る） / whether a type holds gc arena Ids (through_own:
    /// also behind own pointers)
    pub(super) fn gc_rel(&self, t: &Ty, through_own: bool) -> bool {
        fn go(cg: &Cg, t: &Ty, own: bool, seen: &mut Vec<usize>) -> bool {
            match t {
                Ty::Id(g) => cg.s.t.arenas[*g].gc,
                Ty::Struct(sid) => {
                    if seen.contains(sid) {
                        return false;
                    }
                    seen.push(*sid);
                    let r = cg.s.t.structs[*sid].fields.iter().any(|f| go(cg, &f.ty, own, seen));
                    seen.pop();
                    r
                }
                Ty::Array(e, _) => go(cg, e, own, seen),
                Ty::Option(e) => match &**e {
                    Ty::Own(x) | Ty::DmaOwn(x) => own && go(cg, x, own, seen),
                    other => go(cg, other, own, seen),
                },
                Ty::Own(x) => own && go(cg, x, own, seen),
                _ => false,
            }
        }
        go(self, t, through_own, &mut Vec::new())
    }

    /// 根として登録する型か: gc の Id を直接持つ型に加えて、own / Option<own T>（構造体・配列の中も）を通して gc の Id に
    /// 届く型も根にする（仮引数・ローカル・一時値）。移動・解放・take のたびに C の変数を 0 / NULL にするので、根が古い
    /// own の先を指すことはない
    /// whether a value of this type is registered as a root: besides types holding gc Ids directly, types reaching gc Ids
    /// through own / Option<own T> (inside structs and arrays too) are roots as well (parameters, locals, temporaries).
    /// Every move, drop and take clears the C variable to 0 / NULL, so a root never points at an own that is gone
    pub(super) fn gc_root_ty(&self, t: &Ty) -> bool {
        self.gc_any() && !matches!(t, Ty::Ref(..)) && self.gc_rel(t, true)
    }

    /// own（Option<own T>・dma own も）の先に gc の Id があるか / whether gc Ids sit behind an own (Option<own T>, dma own)
    pub(super) fn gc_own_rel(&self, t: &Ty) -> bool {
        fn go(cg: &Cg, t: &Ty, seen: &mut Vec<usize>) -> bool {
            match t {
                Ty::Own(x) | Ty::DmaOwn(x) => cg.gc_rel(x, true),
                Ty::Option(e) | Ty::Array(e, _) => go(cg, e, seen),
                Ty::Struct(sid) => {
                    if seen.contains(sid) {
                        return false;
                    }
                    seen.push(*sid);
                    let r = cg.s.t.structs[*sid].fields.iter().any(|f| go(cg, &f.ty, seen));
                    seen.pop();
                    r
                }
                _ => false,
            }
        }
        go(self, t, &mut Vec::new())
    }

    /// 根の記述子: own の先に gc の Id が無ければ今までの記述子（own を辿らない r_ 付き。既存のプログラムの C は同じ）、
    /// あれば own の先まで辿る記述子（F20_GC_OWN の行。NULL の own は走査が飛ばす）
    /// the root descriptor: without gc Ids behind an own, the descriptor used so far (r_, not following own; existing
    /// programs keep their C); otherwise the descriptor that follows own (F20_GC_OWN rows; a NULL own is skipped by the walk)
    pub(super) fn gc_root_desc(&mut self, t: &Ty) -> Option<String> {
        if self.gc_own_rel(t) {
            self.gc_desc(t, false)
        } else {
            self.gc_desc(t, true)
        }
    }

    /// own の先まで根にする型か（移動・解放のたびに C の変数を 0 / NULL にする必要がある）/ whether a root type is followed
    /// behind own (its C variable must be cleared at every move and drop)
    pub(super) fn gc_own_root(&self, t: &Ty) -> bool {
        self.gc_root_ty(t) && self.gc_own_rel(t)
    }

    /// 値の解放（#[on_drop] の関数）が回収を起こしうるか（推移的）/ whether dropping a value (its #[on_drop] functions) may
    /// collect (transitively)
    pub(super) fn gc_drop_may_collect(&mut self, t: &Ty) -> bool {
        if !self.gc_any() || !self.s.t.needs_drop(t) {
            return false;
        }
        let table = self.gc_collector_table();
        fn go(cg: &Cg, t: &Ty, table: &[bool], seen: &mut Vec<usize>) -> bool {
            match t {
                Ty::Own(x) | Ty::DmaOwn(x) | Ty::Option(x) | Ty::Array(x, _) | Ty::Result(x, _) => go(cg, x, table, seen),
                Ty::Struct(sid) => {
                    if seen.contains(sid) {
                        return false;
                    }
                    seen.push(*sid);
                    let info = &cg.s.t.structs[*sid];
                    let mut r = info.on_drop.as_ref().and_then(|f| cg.s.t.fn_by_name.get(f)).map_or(false, |fi| table.get(*fi).copied().unwrap_or(false));
                    if !r {
                        r = info.fields.iter().any(|f| go(cg, &f.ty, table, seen));
                    }
                    seen.pop();
                    r
                }
                _ => false,
            }
        }
        go(self, t, &table, &mut Vec::new())
    }

    /// 既にある C の一時変数 `name` を根として登録する（own の先まで根にする型だけ）。番号を返す（外すのは呼び出し側）
    /// register the existing C temporary `name` as a root (only types followed behind own); returns its number (the
    /// caller removes it)
    pub(super) fn gc_root_tmp(&mut self, t: &Ty, name: &str) -> Option<u32> {
        if !self.gc_on() || !self.gc_own_root(t) {
            return None;
        }
        let d = self.gc_root_desc(t)?;
        let k = self.gc_k;
        self.gc_k += 1;
        self.gc_used = true;
        let addr = if matches!(t, Ty::Array(..)) { name.to_string() } else { format!("&{}", name) };
        let c = self.c("fascon-gc: 解放の間も根にしておく（#[on_drop] が回収しうる）", "fascon-gc: kept as a root while it is dropped (an #[on_drop] may collect)");
        self.emit(format!("F20_GC_ROOT({}, {}, &{}); /* {} */", k, addr, d, c));
        Some(k)
    }

    /// 戻り値の入れ物（_rv・return の一時変数）を根として登録する（関数を出るまで。cleanup が外す）
    /// register a return slot (_rv, the temporary of a return) as a root (until the function returns; cleanup removes it)
    pub(super) fn gc_root_slot(&mut self, t: &Ty, name: &str) {
        let Some(d) = self.gc_root_desc(t) else { return };
        let k = self.gc_k;
        self.gc_k += 1;
        self.gc_used = true;
        let c = self.c("fascon-gc: 戻り値の入れ物（own の先まで根）", "fascon-gc: the return slot (a root through own)");
        self.emit(format!("F20_GC_ROOT({}, &{}, &{}); /* {} */", k, name, d, c));
    }

    /// 値 v を解放するコード。解放中に回収が起きうる（#[on_drop]）なら、その間だけ v を根にする（v は C の左辺値）
    /// the code dropping v; when a collection may happen during the drop (#[on_drop]), v is a root for that time (v is a
    /// C lvalue)
    pub(super) fn gc_drop_temp(&mut self, t: &Ty, v: &str) -> String {
        let d = self.drop_code(t, v);
        if d.is_empty() || !self.gc_drop_may_collect(t) || !is_c_lvalue(v) {
            return d;
        }
        match self.gc_root_tmp(t, v) {
            Some(k) => format!("{} _gcp[{}] = NULL;", d, k),
            None => d,
        }
    }

    /// 式の値が、この式の中で作られた（変数でも定数でもない）値か: 根にしないと、後ろの被演算子の回収で漏れる
    /// whether an expression's value was made inside the expression (neither a variable nor a constant): unless it is a
    /// root, a collection in a later operand loses it
    pub(super) fn gc_fresh_value(&self, x: &Expr) -> bool {
        match &x.kind {
            ExprKind::Path(_) => match self.s.res.get(&x.id) {
                // 移動した own は移動元が NULL になり、値は一時変数（_mv）だけが持つ / a moved own leaves its source NULL and
                // only the temporary (_mv) holds the value
                Some(Res::Local(b)) => self.fr.move_exprs.contains(&x.id) && self.fr.moved_locals.contains(b) && self.s.t.needs_drop(&self.local(*b).ty),
                _ => false,
            },
            ExprKind::Int(..) | ExprKind::Float(..) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Unit => false,
            _ => !crate::sema::is_place_expr(x),
        }
    }

    /// 被演算子の並びの終わり: この並びで根にした own の一時値（mark より後）は、消費する式（呼び出し・リテラル）へ渡す
    /// 直前に根から外す（呼び出し先の引数・行き先が持ち主になり、同じ own を 2 つの根が指さない）
    /// the end of an operand list: own temporaries rooted in this list (after mark) are removed from the roots right before
    /// the consuming expression (a call or a literal) takes them (the callee's parameter or the destination becomes the
    /// owner, so no own is reached from two roots)
    pub(super) fn gc_hand_over(&mut self, mark: usize) {
        if self.gc_own_temps.len() <= mark {
            return;
        }
        let ks: Vec<u32> = self.gc_own_temps.drain(mark..).collect();
        self.gc_stmt_temps.retain(|k| !ks.contains(k));
        let c = self.c("own の一時値を受け手へ渡す（根から外す。ここから先は受け手が根）", "the own temporaries go to their receiver (removed from the roots; from here the receiver is the root)");
        let list: Vec<String> = ks.iter().map(|k| format!("_gcp[{}] = NULL;", k)).collect();
        self.emit(format!("{} /* {} */", list.join(" "), c));
    }

    /// 型記述子の C の名前（gc の Id を含まなければ None）。root = true は own を辿らない（ローカルの own は移動の後も
    /// 変数が空にならないので、根としては読まない）
    /// the C name of a type descriptor (None without gc Ids). root = true does not follow own (a local own variable is
    /// not cleared after a move, so it is not read as a root)
    pub(super) fn gc_desc(&mut self, t: &Ty, root: bool) -> Option<String> {
        if !self.gc_rel(t, !root) {
            return None;
        }
        let key = format!("fsc_td_{}{}", if root { "r_" } else { "" }, self.mangle(t));
        if self.gc_desc_done.contains(&key) {
            return Some(key);
        }
        self.gc_desc_done.insert(key.clone());
        let _ = writeln!(self.gc_desc_fwd, "static const f20_type_desc_t {};", key);
        let mut rows: Vec<String> = Vec::new();
        self.gc_rows(t, "", root, &mut rows, None);
        let size = format!("sizeof({})", self.cdecl(t, "").trim_end());
        let disp = self.s.t.display(t);
        let mut o = String::new();
        let _ = writeln!(o, "/* {} — {} */", disp, self.c("fascon-gc の型記述子（Id と own の位置。生ポインタは無い）", "fascon-gc type descriptor (where the Ids and owns are; there are no raw pointers)"));
        if rows.is_empty() {
            let _ = writeln!(o, "static const f20_type_desc_t {} = {{ (uint32_t){}, 0u, NULL }};", key, size);
        } else {
            let _ = writeln!(o, "static const f20_gc_field_t {}_f[] = {{\n{}\n}};", key, rows.join(",\n"));
            let _ = writeln!(o, "static const f20_type_desc_t {k} = {{ (uint32_t){s}, {n}u, {k}_f }};", k = key, s = size, n = rows.len());
        }
        self.gc_desc_out.push_str(&o);
        Some(key)
    }

    /// 記述子の行を集める。base = 値の型の C の名前（offsetof の基準）と、その中の道筋 / collect descriptor rows;
    /// `outer` = (C type of the containing value, member path) for offsetof
    fn gc_rows(&mut self, t: &Ty, _path: &str, root: bool, rows: &mut Vec<String>, outer: Option<(&str, &str)>) {
        let off = |o: Option<(&str, &str)>| match o {
            Some((ct, p)) if !p.is_empty() => format!("(uint32_t)__builtin_offsetof({}, {})", ct, p),
            _ => "0u".to_string(),
        };
        match t {
            Ty::Id(g) => {
                if let Some(h) = self.gc_heap(*g) {
                    rows.push(format!("    {{ .off = {}, .kind = F20_GC_ID, .heap = {}u }}", off(outer), h));
                }
            }
            Ty::Option(inner) => match &**inner {
                Ty::Id(_) => self.gc_rows(inner, _path, root, rows, outer),
                Ty::Own(x) | Ty::DmaOwn(x) => {
                    if !root {
                        if let Some(d) = self.gc_desc(x, false) {
                            rows.push(format!("    {{ .off = {}, .kind = F20_GC_OWN, .desc = &{} }}", off(outer), d));
                        }
                    }
                }
                other => {
                    if Self::opt_niche(other) {
                        return;
                    }
                    let other = other.clone();
                    if let Some(d) = self.gc_desc(&other, root) {
                        let on = self.opt_name(&other);
                        let (vo, to) = match outer {
                            Some((ct, p)) if !p.is_empty() => (
                                format!("(uint32_t)(__builtin_offsetof({}, {}) + __builtin_offsetof({}, value))", ct, p, on),
                                format!("(uint32_t)(__builtin_offsetof({}, {}) + __builtin_offsetof({}, is_some))", ct, p, on),
                            ),
                            _ => (format!("(uint32_t)__builtin_offsetof({}, value)", on), format!("(uint32_t)__builtin_offsetof({}, is_some)", on)),
                        };
                        rows.push(format!("    {{ .off = {}, .kind = F20_GC_OPT, .tag = {}, .desc = &{} }}", vo, to, d));
                    }
                }
            },
            Ty::Own(x) => {
                if !root {
                    if let Some(d) = self.gc_desc(x, false) {
                        rows.push(format!("    {{ .off = {}, .kind = F20_GC_OWN, .desc = &{} }}", off(outer), d));
                    }
                }
            }
            Ty::Array(e, n) => {
                if let Some(d) = self.gc_desc(e, root) {
                    let es = format!("sizeof({})", self.cdecl(e, "").trim_end());
                    rows.push(format!("    {{ .off = {}, .kind = F20_GC_ARR, .count = {}u, .stride = (uint32_t){}, .desc = &{} }}", off(outer), n, es, d));
                }
            }
            Ty::Struct(sid) => {
                if outer.map_or(false, |(_, p)| !p.is_empty()) {
                    // 値として埋め込まれた構造体: 要素 1 個の配列として記述子で辿る / a struct embedded by value: walked by
                    // its descriptor as a one-element array
                    if let Some(d) = self.gc_desc(t, root) {
                        let es = format!("sizeof({})", self.struct_name(*sid));
                        rows.push(format!("    {{ .off = {}, .kind = F20_GC_ARR, .count = 1u, .stride = (uint32_t){}, .desc = &{} }}", off(outer), es, d));
                    }
                    return;
                }
                let ct = self.struct_name(*sid);
                let fields: Vec<(String, Ty)> = self.s.t.structs[*sid].fields.iter().map(|f| (f.name.clone(), f.ty.clone())).collect();
                for (fname, fty) in fields {
                    if !self.gc_rel(&fty, !root) {
                        continue;
                    }
                    self.gc_rows(&fty, &fname, root, rows, Some((&ct, &fname)));
                }
            }
            _ => {}
        }
    }

    /// gc_arena の要素の Id がすべて単純なフィールド（配列・own・Option<構造体> の中ではない）なら、その道筋と指す
    /// ヒープ。Id 専用の射影列にする
    /// when every Id in a gc arena element is a plain field (not inside an array, own or Option<struct>), its paths and
    /// target heaps; these become the Id-only projection columns
    pub(super) fn gc_id_paths(&self, t: &Ty) -> Option<Vec<(Vec<String>, usize)>> {
        fn go(cg: &Cg, t: &Ty, prefix: &mut Vec<String>, out: &mut Vec<(Vec<String>, usize)>, depth: usize) -> bool {
            if depth > 8 {
                return false;
            }
            let Ty::Struct(sid) = t else { return false };
            for f in &cg.s.t.structs[*sid].fields {
                if !cg.gc_rel(&f.ty, true) {
                    continue;
                }
                let id = match &f.ty {
                    Ty::Id(g) => Some(*g),
                    Ty::Option(i) => match **i {
                        Ty::Id(g) => Some(g),
                        _ => None,
                    },
                    _ => None,
                };
                prefix.push(f.name.clone());
                let ok = match (id, &f.ty) {
                    (Some(g), _) => match cg.gc_heap(g) {
                        Some(h) => {
                            out.push((prefix.clone(), h));
                            true
                        }
                        None => true,
                    },
                    (None, Ty::Struct(_)) => go(cg, &f.ty, prefix, out, depth + 1),
                    _ => false,
                };
                prefix.pop();
                if !ok {
                    return false;
                }
            }
            true
        }
        let mut out = Vec::new();
        if go(self, t, &mut Vec::new(), &mut out, 0) {
            Some(out)
        } else {
            None
        }
    }

    /// collect_columns から: gc_arena の Id のフィールドに「Id 専用の射影列」を作る（既存の列の仕組みがそのまま写しと
    /// unmirror を保つ）
    /// from collect_columns: give the Id fields of gc arenas "Id-only projection columns" (the existing column machinery
    /// keeps the mirrors and the unmirror on free)
    pub(super) fn gc_columns(&mut self) {
        if !self.o.columns {
            return;
        }
        let wide = self.o.profile == crate::target::Profile::GenericOs;
        for ai in 0..self.s.t.arenas.len() {
            if !self.s.t.arenas[ai].gc {
                continue;
            }
            let elem = self.s.t.arenas[ai].elem.clone();
            let Some(paths) = self.gc_id_paths(&elem) else { continue };
            if paths.is_empty() {
                continue;
            }
            let all: Vec<Vec<String>> = paths.iter().map(|(p, _)| p.clone()).collect();
            if self.column_veto(ai, &all).is_some() {
                continue;
            }
            let mut cols = Vec::new();
            for (p, h) in &paths {
                let val = format!("(*v){}", p.iter().map(|x| format!(".{}", x)).collect::<String>());
                let k = self.add_col(ai, ColKind::Agg { wide }, val, String::new(), vec![p.clone()], 0, p);
                cols.push((k, *h));
            }
            self.gc_cols.insert(ai, cols);
        }
    }

    /// 関数の入口に置く枠（N は本体を作り終えてから埋める） / the frame at function entry (N is filled in after the body)
    pub(super) fn gc_prologue(&mut self, fi: usize) {
        self.gc_poll_fn = false;
        if !self.gc_any() || !self.gc_on() {
            return;
        }
        let f = &self.s.t.fns[fi];
        let shared_touch = self.gc_touches_shared(fi);
        self.gc_poll_fn = shared_touch;
        let params: Vec<(BindId, Ty)> = f.params.iter().map(|p| (p.bind, p.ty.clone())).collect();
        let c = self.c(
            "fascon-gc: この関数の根の枠（シャドウスタック。どの出口でも cleanup が外す）",
            "fascon-gc: this function's root frame (the shadow stack; cleanup pops it on every exit)",
        );
        self.emit(format!("F20_GC_FRAME(@GCN@); /* {} */", c));
        if shared_touch {
            self.emit("if (FASCON_UNLIKELY(!fascon__gc_reg)) f20_gc_register_core();");
        }
        for (b, t) in params {
            if self.gc_root_ty(&t) {
                if let Some(d) = self.gc_root_desc(&t) {
                    let n = self.names.get(&b).cloned().unwrap_or_default();
                    let k = self.gc_k;
                    self.gc_k += 1;
                    self.emit(format!("F20_GC_ROOT({}, &{}, &{});", k, n, d));
                    self.gc_used = true;
                }
            }
        }
        if shared_touch {
            let c = self.c("安全点: 共有の gc_arena の回収中ならここで止まる（枠を積んでから柵を置いて読む）", "safepoint: park here while a shared gc_arena is being collected (read after a fence that follows the frame push)");
            self.emit(format!("F20_GC_POLL_ENTRY(); /* {} */", c));
            self.gc_used = true;
        }
    }

    /// ループの頭の安全点（共有の gc_arena に触れる関数だけ。rcu_read の中には置かない: 止まったまま猶予期間を妨げない）
    /// a safepoint at a loop head (only in functions touching a shared gc arena; never inside rcu_read, so a parked core
    /// cannot hold up a grace period)
    pub(super) fn gc_loop_poll(&mut self) {
        if self.gc_poll_fn && self.gc_rcu_depth == 0 && self.gc_on() {
            self.emit("F20_GC_POLL();");
        }
    }

    /// 共有の gc_arena に（推移的に）触れる関数か / whether a function touches a shared gc arena (transitively)
    pub(super) fn gc_touches_shared(&self, fi: usize) -> bool {
        let e = &self.s.t.fns[fi].effects;
        let hit = |v: &Vec<usize>| v.iter().any(|&a| self.s.t.arenas[a].gc && self.s.t.arenas[a].shared);
        hit(&e.reads) || hit(&e.writes) || hit(&e.frees) || hit(&e.allocs)
    }

    /// GC の枠を使うか（1 回目の生成では「使う」とみなす） / whether the GC frame is on (assumed in the first pass)
    pub(super) fn gc_on(&self) -> bool {
        self.gc_known.unwrap_or(true)
    }

    /// declare_local から: 根の型なら、宣言の行の後に登録を差す / from declare_local: for a root type, register it right
    /// after its declaration line
    pub(super) fn gc_note_local(&mut self, name: &str, t: &Ty) {
        if !self.gc_any() || !self.gc_on() || !self.gc_root_ty(t) {
            return;
        }
        if let Some(d) = self.gc_root_desc(t) {
            self.gc_pending.push((name.to_string(), d));
            self.gc_used = true;
        }
    }

    /// emit から: いま出した行が保留中の根の宣言なら、その直後で登録する / from emit: if the line just emitted declares a
    /// pending root, register it right after
    pub(super) fn gc_after_line(&mut self, line: &str) {
        let t = line.trim_start();
        let mut hit: Option<usize> = None;
        for (i, (n, _)) in self.gc_pending.iter().enumerate() {
            for pat in [format!(" {} =", n), format!(" {};", n), format!(" {}[", n), format!("*{} =", n), format!("*{};", n), format!("*{}[", n)] {
                if let Some(pos) = t.find(&pat) {
                    let pre = &t[..pos + 1];
                    // 行が変数そのもので始まる（代入 `n = ..`）なら宣言ではない。名前が型名の先頭と同じ（`f` と `fsc_..`）でも
                    // 宣言は見分ける / a line starting with the variable itself (an assignment `n = ..`) is not its
                    // declaration; a name that is a prefix of the type name (`f` and `fsc_..`) still matches its declaration
                    let assigns = t.starts_with(n.as_str()) && !t[n.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
                    if !pre.is_empty() && pre.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ' || c == '*') && !assigns {
                        hit = Some(i);
                        break;
                    }
                }
            }
            if hit.is_some() {
                break;
            }
        }
        if let Some(i) = hit {
            let (n, d) = self.gc_pending.remove(i);
            let k = self.gc_k;
            self.gc_k += 1;
            let pad = "    ".repeat(self.ind);
            let addr = if line.contains(&format!(" {}[", n)) || line.contains(&format!("*{}[", n)) { n.clone() } else { format!("&{}", n) };
            self.lines.push(format!("{}F20_GC_ROOT({}, {}, &{});", pad, k, addr, d));
        }
    }

    /// gc_collect(a) / promote_to_gc(a, id) の C / the C of gc_collect(a) / promote_to_gc(a, id)
    pub(super) fn gc_call(&mut self, e: &Expr, args: &[Expr]) -> Option<String> {
        let &(ga, b) = self.s.gc_calls.get(&e.id)?;
        self.gc_used = true;
        let n = self.s.t.arenas[ga].name.clone();
        match b {
            None => Some(format!("f20_gc_collect(fsc_gch_get_{}())", n)),
            Some(b) => {
                let v = self.val(&args[1]);
                let bn = self.s.t.arenas[b].name.clone();
                Some(format!("fsc_gc_promote_{}_{}({})", n, bn, v))
            }
        }
    }

    /// ヒープ・根の関数・昇格の関数（本体を作り終えた後、組み立ての前に一度だけ） / heaps, the root function and the
    /// promotion helpers (once, after the bodies and before assembly)
    pub(super) fn gc_prepare(&mut self) -> String {
        if !self.gc_any() {
            return String::new();
        }
        let mut o = String::new();
        let mut heaps: Vec<usize> = Vec::new();
        // 要素・根になるアリーナ・静的変数の記述子を先に作る / descriptors of elements, root arenas and statics first
        let mut elem_desc: HashMap<usize, String> = HashMap::new();
        for ai in 0..self.s.t.arenas.len() {
            let a = &self.s.t.arenas[ai];
            if a.dma {
                continue;
            }
            let elem = a.elem.clone();
            if let Some(d) = self.gc_desc(&elem, false) {
                elem_desc.insert(ai, d);
            } else if a.gc {
                let d = self.gc_desc_empty(&elem);
                elem_desc.insert(ai, d);
            }
            if a.gc {
                heaps.push(ai);
            }
        }
        let mut static_roots: Vec<(String, String)> = Vec::new();
        for st in self.s.t.statics.clone() {
            if st.int_value.is_some() {
                continue;
            }
            if let Some(d) = self.gc_desc(&st.ty, false) {
                static_roots.push((format!("fsc_{}", st.name), d));
            }
        }
        // チャネルのリングバッファに滞留している値（own の中まで）にある gc_arena の Id も根（仕様 5.7 の「チャネルで送っている
        // 途中の own」の解消）。共有でない gc_arena の Id はチャネルに乗らない（F2611 / F2625）
        // values waiting in the ring buffer of a channel (inside own, too) hold gc arena Ids that are roots as well (fixes "own in
        // transit through a channel" of spec 5.7). Ids of non-shared gc arenas never ride on channels (F2611 / F2625)
        let mut chan_roots: Vec<(String, u64, String)> = Vec::new();
        for c in self.s.t.chans.clone() {
            if !self.gc_rel(&c.elem, true) {
                continue;
            }
            if let Some(d) = self.gc_desc(&c.elem, false) {
                chan_roots.push((c.name.clone(), c.cap, d));
            }
        }
        let _ = writeln!(o, "/* ───── fascon-gc ───── */");
        o.push_str(&self.gc_desc_fwd);
        o.push_str(&self.gc_desc_out);
        for (h, &ai) in heaps.iter().enumerate() {
            let a = self.s.t.arenas[ai].clone();
            let n = a.name.clone();
            let tls = if a.shared { "static" } else { "static F20_PERCPU" };
            let cols = self.gc_cols.get(&ai).cloned().unwrap_or_default();
            let has_cols = self.arena_cols.get(ai).map_or(false, |c| !c.is_empty());
            let col_names: Vec<String> = cols.iter().map(|(k, _)| self.arena_cols[ai][*k].name.clone()).collect();
            let wide = self.o.profile == crate::target::Profile::GenericOs;
            let ed = elem_desc.get(&ai).cloned().unwrap_or_default();
            let c1 = self.c(
                &format!("gc_arena {}: 印のビット列・作業スタック・Id 専用の射影列の表（共有でなければコアごと）", n),
                &format!("gc_arena {}: mark bits, work stack and the table of Id-only projection columns (per core unless shared)", n),
            );
            let _ = writeln!(o, "/* {} */", c1);
            let _ = writeln!(o, "{tls} f20_gc_heap_t fsc_gch_{n};\n{tls} uint64_t fsc_gcmark_{n}[F20_LIVE_WORDS(ARENA_{n}_K)];\n{tls} uint32_t fsc_gcwork_{n}[F20_SLOTS(ARENA_{n}_K)];\n{tls} const void *fsc_gccols_{n}[{nc}u];", tls = tls, n = n, nc = cols.len() + 1);
            let colh: Vec<String> = cols.iter().map(|(_, h)| format!("{}u", h)).collect();
            let _ = writeln!(o, "static const uint8_t fsc_gccolh_{n}[{nc}u] = {{ {v} }};", n = n, nc = cols.len() + 1, v = if colh.is_empty() { "0u".to_string() } else { colh.join(", ") });
            let c2 = self.c("死んだスロットをアリーナの free へ（own の drop・0 埋め・世代 +1・列の unmirror・空きリスト）", "a dead slot goes to the arena's free (drop of own fields, zero fill, generation + 1, column unmirror, free list)");
            let _ = writeln!(o, "/* {c} */\nstatic bool fsc_gcfree_{n}(uint32_t i) {{ return arena_{n}_free(({id})F20_ID_MAKE(arena_{n}.gen[i], i, ARENA_{n}_K)); }}", c = c2, n = n, id = self.id_name(ai));
            let remirror = if has_cols {
                let _ = writeln!(o, "static void fsc_gcremirror_{n}(uint32_t i) {{ arena_{n}_mirror(&arena_{n}.slot[i].v, F20_COL_OF_SLOT(i, ARENA_{n}_K)); }}", n = n);
                format!("fsc_gcremirror_{}", n)
            } else {
                "NULL".to_string()
            };
            let mut set_cols = String::new();
            for (k, cn) in col_names.iter().enumerate() {
                let _ = writeln!(set_cols, "        fsc_gccols_{n}[{k}] = (const void *)arena_{n}.col.{cn};", n = n, k = k, cn = cn);
            }
            let c3 = self.c("ヒープの記述（最初に使ったときに、このコアのアリーナの番地で埋める）", "the heap record (filled with this core's arena addresses on first use)");
            // 共有の gc_arena: 記述はすべてのコアで 1 つなので、最初の 1 コアだけが埋め、ほかのコアは埋め終わるのを待つ（同時に
            // 埋める競合を作らない） / a shared gc_arena: the record is one for every core, so only the first core fills it and
            // the others wait until it is filled (no race of two cores filling it at once)
            let (open, close) = if a.shared {
                (
                    format!("    static uint32_t once_;\n    if (FASCON_UNLIKELY(__atomic_load_n(&h->st, __ATOMIC_ACQUIRE) == NULL)) {{\n        if (__atomic_exchange_n(&once_, 1u, __ATOMIC_ACQ_REL) != 0u) {{\n            while (__atomic_load_n(&h->st, __ATOMIC_ACQUIRE) == NULL) FASCON_CPU_RELAX();\n            return h;\n        }}\n"),
                    format!("        __atomic_store_n(&h->st, &arena_{n}.st, __ATOMIC_RELEASE);\n", n = n),
                )
            } else {
                ("    if (FASCON_UNLIKELY(h->st == NULL)) {\n".to_string(), format!("        h->st = &arena_{n}.st;\n", n = n))
            };
            let _ = writeln!(
                o,
                "/* {c} */\nstatic f20_gc_heap_t *fsc_gch_get_{n}(void) {{\n    f20_gc_heap_t *h = &fsc_gch_{n};\n{open}        h->name = \"{n}\";\n        h->live = arena_{n}.live;\n        h->mark = fsc_gcmark_{n};\n        h->gen = (void *)arena_{n}.gen;\n        h->link = arena_{n}.link;\n        h->gen_bytes = (uint32_t)sizeof arena_{n}.gen[0];\n        h->k = ARENA_{n}_K;\n        h->idx = {h}u;\n        h->slot0 = (uint8_t *)&arena_{n}.slot[0].v;\n        h->stride = (uint32_t)sizeof arena_{n}.slot[0];\n        h->shared = {sh}u;\n        h->elem = &{ed};\n{sc}        h->ncols = {nc}u;\n        h->col_wide = {w}u;\n        h->cols = fsc_gccols_{n};\n        h->col_heap = fsc_gccolh_{n};\n        h->free_fn = fsc_gcfree_{n};\n        h->remirror = {rm};\n        h->work = fsc_gcwork_{n};\n{close}    }}\n    return h;\n}}",
                c = c3,
                n = n,
                open = open,
                close = close,
                h = h,
                sh = if a.shared { 1 } else { 0 },
                ed = ed,
                sc = set_cols,
                nc = cols.len(),
                w = if wide { 1 } else { 0 },
                rm = remirror
            );
        }
        // 昇格の関数 / promotion helpers
        let mut pairs: Vec<(usize, usize)> = self.s.gc_calls.values().filter_map(|&(g, b)| b.map(|b| (g, b))).collect();
        pairs.extend(self.s.auto_promote.values().copied());
        pairs.sort();
        pairs.dedup();
        for (g, b) in pairs {
            let gn = self.s.t.arenas[g].name.clone();
            let bn = self.s.t.arenas[b].name.clone();
            let et = self.cty(&self.s.t.arenas[g].elem.clone());
            let c = self.c(
                &format!("promote_to_gc: bump_arena {} の生き残りを gc_arena {} へ写す（残りは関数の出口の巻き戻し 1 回で消える）", bn, gn),
                &format!("promote_to_gc: copy a survivor of bump_arena {} into gc_arena {} (the rest vanishes with one rewind at function exit)", bn, gn),
            );
            let _ = writeln!(
                o,
                "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED {gid} fsc_gc_promote_{gn}_{bn}({bid} id) {{\n    const {et} *s = arena_{bn}_get(id);\n    return s != NULL ? arena_{gn}_alloc(s) : ({gid})0u;\n}}",
                c = c,
                gid = self.id_name(g),
                bid = self.id_name(b),
                gn = gn,
                bn = bn,
                et = et
            );
        }
        // ヒープの表 / the heap table
        let _ = writeln!(o, "const uint32_t fascon__gc_nheaps = {}u;", heaps.len());
        let _ = writeln!(o, "f20_gc_heap_t *fascon__gc_heap_at(uint32_t i) {{\n    switch (i) {{");
        for (h, &ai) in heaps.iter().enumerate() {
            let _ = writeln!(o, "    case {}u: return fsc_gch_get_{}();", h, self.s.t.arenas[ai].name);
        }
        let _ = writeln!(o, "    default: return NULL;\n    }}\n}}");
        // 根: 静的変数・ふつうのアリーナ・バンプアリーナ / roots: statics, ordinary arenas, bump arenas
        let c = self.c(
            "根の集合: 静的変数（書き換えでは触れない）と、gc_arena の Id を持つふつうのアリーナ・バンプアリーナの生きているスロット",
            "the root set: statics (never rewritten) and the live slots of ordinary / bump arenas that hold gc_arena Ids",
        );
        let _ = writeln!(o, "/* {} */\nvoid fascon__gc_roots(void (*visit)(void *p, const f20_type_desc_t *d, void *cx), void *cx, int writable) {{", c);
        let _ = writeln!(o, "    (void)visit;\n    (void)cx;\n    (void)writable;");
        if !static_roots.is_empty() {
            let _ = writeln!(o, "    if (!writable) {{");
            for (sn, d) in &static_roots {
                let _ = writeln!(o, "        visit((void *)&{}, &{}, cx);", sn, d);
            }
            let _ = writeln!(o, "    }}");
        }
        for ai in 0..self.s.t.arenas.len() {
            let a = &self.s.t.arenas[ai];
            if a.gc || a.dma {
                continue;
            }
            let Some(d) = elem_desc.get(&ai).cloned() else { continue };
            let n = a.name.clone();
            if a.bump {
                let _ = writeln!(o, "    for (uint32_t i = 0; i < arena_{n}.st.top; i++) visit(&arena_{n}.slot[i].v, &{d}, cx);", n = n, d = d);
            } else {
                let _ = writeln!(
                    o,
                    "    for (uint32_t w = 0; w < F20_LIVE_WORDS_USED(arena_{n}.st.next_unused); w++) {{\n        uint64_t m = arena_{n}.live[w];\n        while (m) {{\n            const uint32_t i = (w << 6) + (uint32_t)__builtin_ctzll(m);\n            m &= m - 1u;\n            visit(&arena_{n}.slot[i].v, &{d}, cx);\n        }}\n    }}",
                    n = n,
                    d = d
                );
            }
        }
        if !chan_roots.is_empty() {
            let c = self.c(
                "チャネルのリングバッファに滞留している値（発行済みのセルだけ）。世代リフレッシュの書き換えもここを通る",
                "values waiting in channel ring buffers (published cells only); generation-refresh rewrites go through here too",
            );
            let _ = writeln!(o, "    /* {} */", c);
            for (cn, cap, d) in &chan_roots {
                let _ = writeln!(o, "    F20_CHAN_GC_WALK({}, {}u, &{}, visit, cx);", cn, cap, d);
            }
        }
        let _ = writeln!(o, "}}\n");
        o
    }

    /// 回収を起こしうる関数の表（推移的）: gc_collect、共有の gc_arena の安全点（触れる関数の入口・待つチャネル操作）、
    /// タスクの切り替え（止まっている間にほかのタスクが回収しうる）
    /// the table of functions that may collect (transitively): gc_collect, safepoints of a shared gc arena (entries of
    /// functions that touch it, blocking channel operations) and task switches (another task may collect meanwhile)
    fn gc_collector_table(&mut self) -> Vec<bool> {
        if let Some(t) = &self.gc_collectors {
            return t.clone();
        }
        let n = self.s.t.fns.len();
        let shared_gc = self.s.t.arenas.iter().any(|a| a.gc && a.shared);
        let mut direct = vec![false; n];
        let mut calls: Vec<Vec<usize>> = vec![Vec::new(); n];
        for fi in 0..n {
            let f = &self.s.t.fns[fi];
            if shared_gc && self.gc_touches_shared(fi) {
                direct[fi] = true;
            }
            if let Item::Fn(fd) = &self.s.prog.items[f.item] {
                let mut stack: Vec<&Expr> = Vec::new();
                let push_block = |b: &'p Block, st: &mut Vec<&'p Expr>| {
                    for x in &b.stmts {
                        match x {
                            Stmt::Let(l) => {
                                if let Some(i) = &l.init {
                                    st.push(i);
                                }
                            }
                            Stmt::Expr(e) => st.push(e),
                        }
                    }
                    if let Some(t) = &b.tail {
                        st.push(t);
                    }
                };
                push_block(&fd.body, &mut stack);
                while let Some(e) = stack.pop() {
                    if self.gc_direct_collect(e, shared_gc) {
                        direct[fi] = true;
                    }
                    match self.s.calls.get(&e.id) {
                        Some(CallKind::User(g)) => calls[fi].push(*g),
                        Some(CallKind::Indirect) => calls[fi].extend(self.s.indirect_cands.get(&e.id).cloned().unwrap_or_default()),
                        _ => {}
                    }
                    for c in crate::flow::children(e) {
                        match c {
                            crate::flow::Child::E(x) => stack.push(x),
                            crate::flow::Child::B(b) => push_block(b, &mut stack),
                        }
                    }
                }
            }
        }
        let mut t = direct;
        loop {
            let mut changed = false;
            for fi in 0..n {
                if !t[fi] && calls[fi].iter().any(|g| t[*g]) {
                    t[fi] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.gc_collectors = Some(t.clone());
        t
    }

    /// この式そのもの（子を除く）が回収の点か / whether this expression itself (not its children) is a collection point
    fn gc_direct_collect(&self, e: &Expr, shared_gc: bool) -> bool {
        if let Some(&(_, None)) = self.s.gc_calls.get(&e.id) {
            return true;
        }
        if let Some(CallKind::Builtin(b)) = self.s.calls.get(&e.id) {
            if matches!(b, Builtin::WaitIrq | Builtin::SleepTicks) {
                return true;
            }
        }
        match self.s.methods.get(&e.id) {
            Some(MethodKind::TaskYield(_)) | Some(MethodKind::TaskExit(_)) => true,
            Some(MethodKind::ChanSend(_)) | Some(MethodKind::ChanRecvWait(_)) => shared_gc,
            _ => false,
        }
    }

    /// 式が（子を含めて）回収を起こしうるか / whether an expression (children included) may cause a collection
    pub(super) fn gc_may_collect(&mut self, e: &Expr) -> bool {
        let table = self.gc_collector_table();
        let shared_gc = self.s.t.arenas.iter().any(|a| a.gc && a.shared);
        let mut stack: Vec<&Expr> = vec![e];
        while let Some(x) = stack.pop() {
            if self.gc_direct_collect(x, shared_gc) {
                return true;
            }
            match self.s.calls.get(&x.id) {
                Some(CallKind::User(g)) if table[*g] => return true,
                Some(CallKind::Indirect) if self.s.indirect_cands.get(&x.id).map_or(false, |v| v.iter().any(|g| table[*g])) => return true,
                _ => {}
            }
            for c in crate::flow::children(x) {
                match c {
                    crate::flow::Child::E(y) => stack.push(y),
                    crate::flow::Child::B(b) => {
                        for st in &b.stmts {
                            match st {
                                Stmt::Let(l) => {
                                    if let Some(i) = &l.init {
                                        stack.push(i);
                                    }
                                }
                                Stmt::Expr(y) => stack.push(y),
                            }
                        }
                        if let Some(t) = &b.tail {
                            stack.push(t);
                        }
                    }
                }
            }
        }
        false
    }

    /// 被演算子ごとに「回収を起こしうるか」（gc_arena の無いプログラム・枠を使わない関数では全部 false）
    /// per operand, whether it may collect (all false without gc_arena or when the frame is off)
    pub(super) fn gc_collect_flags(&mut self, xs: &[&Expr]) -> Vec<bool> {
        if !self.gc_any() || !self.gc_on() {
            return vec![false; xs.len()];
        }
        xs.iter().map(|x| self.gc_may_collect(x)).collect()
    }

    /// 被演算子を一時変数へ出す。gc_arena の Id を持つ値で、後ろの被演算子が回収を起こしうるなら、一時変数を根として
    /// シャドウスタックへ登録する（式を let に分けなくても、回収で根が漏れない）
    /// hoist an operand into a temporary. When the value holds gc arena Ids and a later operand may collect, the
    /// temporary is registered as a root on the shadow stack (no root is lost even without splitting the expression into lets)
    pub(super) fn hoist_operand(&mut self, t: &Ty, v: &str, gc_later: bool) -> String {
        if !(gc_later && self.gc_root_ty(t)) {
            return self.hoist_force(t, v);
        }
        let Some(d) = self.gc_root_desc(t) else { return self.hoist_force(t, v) };
        let n = self.new_tmp("gt");
        let decl = self.cdecl(t, &n);
        let k = self.gc_k;
        self.gc_k += 1;
        self.gc_used = true;
        let c = self.c(
            "fascon-gc: 式の途中の一時値を根として登録（後ろの被演算子が回収を起こしうる）",
            "fascon-gc: a temporary in the middle of an expression registered as a root (a later operand may collect)",
        );
        self.emit(format!("{} = {}; /* {} */", decl, v, c));
        let addr = if matches!(t, Ty::Array(..)) { n.clone() } else { format!("&{}", n) };
        self.emit(format!("F20_GC_ROOT({}, {}, &{});", k, addr, d));
        self.gc_stmt_temps.push(k);
        // own を持つ一時値は受け手へ渡すときに根から外す（gc_hand_over） / own temporaries leave the roots when handed to
        // their receiver (gc_hand_over)
        if self.gc_own_rel(t) {
            self.gc_own_temps.push(k);
        }
        n
    }

    /// Id を持たない要素の空の記述子 / an empty descriptor for an element without Ids
    fn gc_desc_empty(&mut self, t: &Ty) -> String {
        let key = format!("fsc_td_{}", self.mangle(t));
        if !self.gc_desc_done.contains(&key) {
            self.gc_desc_done.insert(key.clone());
            let size = format!("sizeof({})", self.cdecl(t, "").trim_end());
            let _ = writeln!(self.gc_desc_out, "static const f20_type_desc_t {} = {{ (uint32_t){}, 0u, NULL }};", key, size);
        }
        key
    }
}
