//! 関数ポインタ（コールバック）の行き先: 関数ポインタを通した呼び出しが、どの関数に届きうるか
//! where function pointers (callbacks) go: which functions a call through a function pointer can reach
//!
//! 関数ポインタの値は「関数の名前を値として使った場所」からしか生まれない（整数からの変換も、関数ポインタを作る別の構文も
//! 無い）。だから呼ばれうる関数は、同じ型で値として使われた関数に必ず含まれる（型による候補 = 健全な上界）。
//! 引数 `f: fn(&mut i32)` については、直接の呼び出しで実際に渡される関数を集めて候補を絞る（文脈を区別しない最小不動点）。
//! この候補は、効果 E(f) = (R, W, F)（F2502）・呼び出しリージョン・エスケープ解析（F0520）がそのまま使う。
//! A function-pointer value only ever comes from a place where a function name is used as a value (there is no cast from
//! integers and no other syntax that makes one). So the functions an indirect call can reach are always among the
//! functions of the same type that are used as values (the type-based candidates: a sound upper bound). For a parameter
//! `f: fn(&mut i32)` the candidates are narrowed to the functions actually passed at direct call sites (a
//! context-insensitive least fixpoint). Effects E(f) = (R, W, F) (F2502), call regions and the escape analysis (F0520)
//! use these candidates as they are.

use crate::ast::*;
use crate::sema::{walk_block, CallKind, LocalKind, Res, Sema};
use crate::types::Ty;
use std::collections::HashMap;

/// 関数 fi の型（関数ポインタとしての型） / the type of function fi as a function pointer
pub fn fn_ptr_ty(s: &Sema, fi: usize) -> Ty {
    let f = &s.t.fns[fi];
    Ty::Fn(f.params.iter().map(|p| p.ty.clone()).collect(), Box::new(f.ret.clone()))
}

/// 値として使われた関数のうち、型が t のもの（型による候補） / the functions used as values whose type is t
pub fn by_type(s: &Sema, t: &Ty) -> Vec<usize> {
    let mut v: Vec<usize> = Vec::new();
    for (fi, _, _) in &s.fn_value_sites {
        if !v.contains(fi) && fn_ptr_ty(s, *fi) == *t {
            v.push(*fi);
        }
    }
    v.sort_unstable();
    v
}

/// ローカル b が関数 fi の何番目の引数か（mut の引数は書き換えられるので数えない） / the parameter index of local b in
/// function fi (mut parameters can be reassigned, so they do not count)
fn param_index(s: &Sema, fi: usize, b: BindId) -> Option<usize> {
    s.t.fns[fi].params.iter().position(|p| p.bind == b && !p.mutable)
}

/// 関数ポインタの型を持つ式から、それが指しうる関数 / the functions an expression of function-pointer type may denote
fn expr_cands(s: &Sema, owner: usize, e: &Expr, pc: &HashMap<(usize, usize), Vec<usize>>) -> Vec<usize> {
    let t = s.expr_ty.get(e.id as usize).cloned().unwrap_or(Ty::Err);
    match &e.kind {
        ExprKind::Path(_) => match s.res.get(&e.id) {
            Some(Res::Fn(g)) => vec![*g],
            Some(Res::Local(b)) => {
                let is_param = s.locals.get(*b as usize).and_then(|x| x.as_ref()).map_or(false, |l| l.kind == LocalKind::Param);
                match (is_param, param_index(s, owner, *b)) {
                    (true, Some(i)) => pc.get(&(owner, i)).cloned().unwrap_or_default(),
                    _ => by_type(s, &t),
                }
            }
            _ => by_type(s, &t),
        },
        _ => by_type(s, &t),
    }
}

fn union_into(dst: &mut Vec<usize>, src: &[usize]) -> bool {
    let mut changed = false;
    for x in src {
        if !dst.contains(x) {
            dst.push(*x);
            changed = true;
        }
    }
    if changed {
        dst.sort_unstable();
    }
    changed
}

/// 候補を求めて Sema に書き込む（indirect_cands / param_cands） / compute the candidates into Sema
pub fn compute_candidates(s: &mut Sema) {
    let prog = s.prog;
    let n = s.t.fns.len();
    // 値として使われた関数（アドレスを取られた関数）は、関数ポインタ経由でどこからでも呼ばれうる → その関数ポインタ型の
    // 引数は型による候補から始める
    // a function used as a value can be called from anywhere through a pointer → its function-pointer parameters start
    // from the type-based candidates
    let mut taken = vec![false; n];
    for (fi, _, _) in &s.fn_value_sites {
        taken[*fi] = true;
    }
    let mut pc: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for fi in 0..n {
        for (i, p) in s.t.fns[fi].params.iter().enumerate() {
            if let Ty::Fn(..) = p.ty {
                let init = if taken[fi] || p.mutable { by_type(s, &p.ty) } else { Vec::new() };
                pc.insert((fi, i), init);
            }
        }
    }
    // 直接の呼び出しで関数ポインタ型の引数へ渡されるもの（最小不動点） / what direct calls pass to function-pointer
    // parameters (least fixpoint)
    let mut sites: Vec<(usize, usize, usize, Expr)> = Vec::new(); // (caller, callee, param index, arg)
    for fi in 0..n {
        if let Item::Fn(fd) = &prog.items[s.t.fns[fi].item] {
            walk_block(&fd.body, &mut |e: &Expr| {
                if let ExprKind::Call { args, .. } = &e.kind {
                    if let Some(CallKind::User(g)) = s.calls.get(&e.id) {
                        for (i, a) in args.iter().enumerate() {
                            if matches!(s.t.fns[*g].params.get(i).map(|p| &p.ty), Some(Ty::Fn(..))) {
                                sites.push((fi, *g, i, a.clone()));
                            }
                        }
                    }
                }
            });
        }
    }
    loop {
        let mut changed = false;
        for (caller, g, i, a) in &sites {
            let c = expr_cands(s, *caller, a, &pc);
            let slot = pc.entry((*g, *i)).or_default();
            changed |= union_into(slot, &c);
        }
        if !changed {
            break;
        }
    }
    // 関数ポインタを通した呼び出しごとの候補 / candidates per indirect call
    let mut ic: HashMap<ExprId, Vec<usize>> = HashMap::new();
    for fi in 0..n {
        if let Item::Fn(fd) = &prog.items[s.t.fns[fi].item] {
            walk_block(&fd.body, &mut |e: &Expr| {
                if s.calls.get(&e.id) != Some(&CallKind::Indirect) {
                    return;
                }
                let c = match &e.kind {
                    ExprKind::Call { .. } => match s.call_targets.get(&e.id) {
                        Some(Res::Local(b)) => {
                            let is_param = s.locals.get(*b as usize).and_then(|x| x.as_ref()).map_or(false, |l| l.kind == LocalKind::Param);
                            let t = s.locals.get(*b as usize).and_then(|x| x.as_ref()).map(|l| l.ty.clone()).unwrap_or(Ty::Err);
                            match (is_param, param_index(s, fi, *b)) {
                                (true, Some(i)) => pc.get(&(fi, i)).cloned().unwrap_or_default(),
                                _ => by_type(s, &t),
                            }
                        }
                        Some(Res::Static(si)) => by_type(s, &s.t.statics[*si].ty.clone()),
                        _ => Vec::new(),
                    },
                    ExprKind::CallIndirect { callee, .. } => expr_cands(s, fi, callee, &pc),
                    _ => Vec::new(),
                };
                ic.insert(e.id, c);
            });
        }
    }
    s.indirect_cands = ic;
    s.param_cands = pc;
}
