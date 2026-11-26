//! 意味解析（名前解決・型推論・アリーナの着色・own / MMIO の宣言検査）
//! semantic analysis (name resolution, type inference, arena colouring, own / MMIO declaration checks)
//!
//! 流れに依存する検査（移動・借用・効果・生存証明・範囲）は flow.rs が行う。
//! Flow-dependent checks (moves, borrows, effects, liveness proofs, ranges) live in flow.rs.

use crate::ast::*;
use crate::diag::Diag;
use crate::lexer::{FloatTy, IntTy};
use crate::span::Span;
use crate::types::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Res {
    Local(BindId),
    Static(usize),
    NoneLit,
    /// 関数を値として使う（関数ポインタ `fn(..)`） / a function used as a value (a function pointer `fn(..)`)
    Fn(usize),
    /// `EINVAL` などの失敗の符号の定数（FaultError） / a failure-code constant such as `EINVAL` (FaultError)
    FaultConst(i64),
    /// `f32::NAN`・`f64::INFINITY` などの浮動小数点の定数 / float constants such as `f32::NAN` and `f64::INFINITY`
    FloatConst(FloatTy, &'static str),
}

/// 浮動小数点の型の定数（Rust と同じ名前） / the associated constants of the float types (Rust's names)
pub const FLOAT_CONSTS: &[&str] = &["NAN", "INFINITY", "NEG_INFINITY", "MAX", "MIN", "MIN_POSITIVE", "EPSILON"];

/// FaultError の定数（errno と同じ値。ラッチの異常は ESTALE・EOVERFLOW・EFAULT・EIO に写る）
/// FaultError constants (the errno values; latched faults map to ESTALE, EOVERFLOW, EFAULT and EIO)
pub const FAULT_CONSTS: &[(&str, i64)] = &[
    ("EPERM", 1),
    ("ENOENT", 2),
    ("EIO", 5),
    ("EBADF", 9),
    ("EAGAIN", 11),
    ("ENOMEM", 12),
    ("EFAULT", 14),
    ("EBUSY", 16),
    ("EEXIST", 17),
    ("EINVAL", 22),
    ("ENOSPC", 28),
    ("ENOSYS", 38),
    ("EOVERFLOW", 75),
    ("ETIMEDOUT", 110),
    ("ESTALE", 116),
];

/// `user Str` を写すときの上限（バイト）。ボード定義の `user_str_max` で変えられる / the limit for copying a `user Str`
pub const USER_STR_MAX_DEFAULT: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    /// `f32::from_bits(u32)` / `f64::from_bits(u64)`
    FloatFromBits(FloatTy),
    PrintInt,
    PrintStr,
    PutInt,
    PutHex,
    PutStr,
    /// `put_bytes(窓)`: バイト列をそのままコンソールへ / write the bytes of a window to the console
    PutBytes,
    /// `spawn_on(コア, 関数[, 引数])`: 別のコアで関数を走らせる / run a function on another core
    SpawnOn,
    /// `cpu_count()`: 動いているコアの数 / the number of cores
    CpuCount,
    /// `put(x)`: 整数・Str・bool を型で選んで出力（改行なし。ジェネリクスではなく組み込み）
    /// `put(x)`: prints an integer, Str or bool chosen by its type (no newline; a builtin, not a generic)
    Put,
    Panic,
    Drop,
    CpuRelax,
    Fence,
    CompilerFence,
    CpuId,
    MmioRead(u8),
    MmioWrite(u8),
    HeapLiveBlocks,
    HeapLiveBytes,
    Halt,
    Exit,
    /// カーネル拡張: Generic Timer と割り込み許可 / kernel extensions: the generic timer and interrupt enabling
    TimerFreq,
    TimerNow,
    TimerArm,
    IrqOpen,
    WaitIrq,
    /// 止まらない最小・最大 / non-stopping minimum and maximum
    Min,
    Max,
    /// ── ユニバーサル OS 拡張（同じ名前のユーザー関数があればそちらが優先） / universal-OS extension builtins (a user function
    /// of the same name takes precedence) ──
    /// `copy_from_user(ubuf)`: ユーザー空間から 1 回だけ写す（Result<[u8; N] | own [u8; MAX], FaultError>）
    CopyFromUser,
    /// `copy_to_user(ubuf, &kbuf)`: ユーザー空間へ写す（Result<(), FaultError>）
    CopyToUser,
    /// `submit_dma(buf)`: dma own をデバイスへ貸し出し、IOVA（u64）を返す（buf は消える: 以後の使用は F3401）
    SubmitDma,
    /// `ct_eq(&a, &b)`: 定数時間の比較（#[secret] の値どうしの == の代わり） / constant-time comparison
    CtEq,
    /// `schedule()`・`sleep_ticks(n)`・`rcu_synchronize()`: Sleep 効果を持つ（眠りうる） / builtins with the Sleep effect
    Schedule,
    SleepTicks,
    RcuSynchronize,
    /// `clock_mono_ns()`: 単調時計（ns） / the monotonic clock (ns)
    ClockMonoNs,
    /// `cpu_this()`: 今のコアの番号 / the current core number
    CpuThis,
    /// `fault(code)`: 整数から FaultError を作る / make a FaultError from an integer code
    Fault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    User(usize),
    Some,
    Builtin(Builtin),
    /// `Ok(v)` / `Err(e)`: Result の構築 / building a Result
    Ok,
    Err,
    /// 関数ポインタを通した呼び出し（`f(x)` の f がローカル・引数・static、または `(式)(x)`）。呼ばれうる関数は
    /// `Sema::indirect_cands`、呼ばれる式は `Sema::call_targets`（名前の呼び出し）か CallIndirect の callee
    /// a call through a function pointer (`f(x)` where f is a local, a parameter or a static, or `(expr)(x)`); the possible
    /// callees are in `Sema::indirect_cands`, the callee in `Sema::call_targets` (named calls) or the CallIndirect's callee
    Indirect,
}

/// アリーナの容量の上限（Id の添字 24 ビット） / the largest arena capacity (24 index bits in an Id)
pub const ARENA_CAP_MAX: u64 = 1 << 24;

/// バイト列として扱える型（窓か `[u8; N]`） / types usable as bytes (a window or a `[u8; N]`)
pub fn is_bytes(t: &Ty) -> bool {
    match t {
        Ty::Slice(_) => true,
        Ty::Array(el, _) => **el == Ty::Int(IntTy::U8),
        _ => false,
    }
}

/// 窓の引数の作り方 / how a window argument is made
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SliceFrom {
    /// すでに窓（束縛・引数）をそのまま渡す / already a window (a binding or parameter), passed on
    Slice,
    /// `[u8; N]` の配列（`&a` / `&mut a`、または配列への参照の束縛）から / from a `[u8; N]` array
    Array(u64),
    /// Str（読み出し専用の窓） / from a Str (a read-only window)
    Str,
}

/// 止まらない算術のメソッド名 → 番号 / names of the non-stopping arithmetic methods → codes
pub fn int_op_code(m: &str) -> Option<u8> {
    Some(match m {
        "wrapping_add" => 0,
        "wrapping_sub" => 1,
        "wrapping_mul" => 2,
        "checked_add" => 3,
        "checked_sub" => 4,
        "checked_mul" => 5,
        "saturating_add" => 6,
        "saturating_sub" => 7,
        _ => return None,
    })
}
/// task_arena の容量の上限（プラットフォーム層のタスク表の大きさ） / the capacity limit of a task_arena (the platform task table)
pub const TASK_CAP_MAX: u64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    IsSome,
    IsNone,
    Take,
    Unwrap,
    UnwrapOr,
    Clone,
    StrLen,
    StrByte,
    ArrLen,
    IdIndex,
    IdGeneration,
    IdRaw,
    /// 浮動小数点の値のメソッド: `to_bits()`・`is_nan()`・`is_finite()`・`abs()` / methods of float values
    FloatToBits(FloatTy),
    FloatIsNan(FloatTy),
    FloatIsFinite(FloatTy),
    FloatAbs(FloatTy),
    ArenaAlloc(usize),
    ArenaFree(usize),
    ArenaRemove(usize),
    ArenaContains(usize),
    ArenaLen(usize),
    ArenaCapacity(usize),
    ArenaGet(usize),
    ArenaGetMut(usize),
    ArenaValidate(usize),
    MmioLen(usize),
    MmioWidth(usize),
    MmioHeight(usize),
    MmioFill(usize),
    MmioPut(usize),
    MmioFillRect(usize),
    /// task_arena: `@t.current()` / `yield_to(id)` / `resume(id)` / `enter(id)` / `exit()`（`start` は ArenaAlloc + task_starts）
    TaskCurrent(usize),
    TaskYield(usize),
    TaskResume(usize),
    TaskEnter(usize),
    TaskExit(usize),
    /// `dma` リージョン: `@r.phys()`（番地を整数で）・`@r.len()`
    /// `@u.of(id)`: そのタスクのユーザーメモリの窓（対象の位置だけ） / the window over a task's user memory (scrutinee only)
    UserMemOf(usize),
    /// チャネル `@ch.send(v)` / `recv()` / `recv_wait()`
    ChanSend(usize),
    ChanRecv(usize),
    ChanRecvWait(usize),
    DmaRegionPhys(usize),
    DmaRegionLen(usize),
    /// dma_arena: `@b.lend(h)`（h は消える）・`@b.reclaim(raw)`（検証付き）・`h.phys()`
    DmaLend(usize),
    DmaReclaim(usize),
    DmaPhys(usize),
    /// gpu_arena: `@g.dispatch(kernel, grid, block, h)`（h は消え、GpuFence が返る）・`@g.reclaim(f)`（待たない）・
    /// `@g.reclaim_wait(f)`（完了まで待つ: Sleep 効果）
    /// gpu_arena: `@g.dispatch(kernel, grid, block, h)` (h is consumed, a GpuFence comes back), `@g.reclaim(f)` (does not
    /// wait), `@g.reclaim_wait(f)` (waits for completion: the Sleep effect)
    GpuDispatch(usize),
    GpuReclaim(usize),
    GpuReclaimWait(usize),
    /// 窓とビュー: `s.len()`、`x.window(off, len)` / `window_mut`（bool = 書き込み可）、`x.view(T, off)` / `view_mut`
    /// （構造体の番号）、`x.read(T, off)`（複製）、`d.copy_from(s)`
    /// windows and views
    SliceLen,
    Window(bool),
    View(bool, usize),
    ReadAt(usize),
    CopyFrom,
    /// 止まらない算術: 0..=2 wrapping_add/sub/mul、3..=5 checked_add/sub/mul、6..=7 saturating_add/sub
    /// non-stopping arithmetic: 0..=2 wrapping, 3..=5 checked, 6..=7 saturating
    IntOp(u8),
    /// Result: `is_ok()` / `is_err()` / `unwrap()` / `unwrap_or(v)` / `err()`（Option<E>）
    ResIsOk,
    ResIsErr,
    ResUnwrap,
    ResUnwrapOr,
    /// FaultError: `e.code()`（i64）
    FaultCode,
    /// `user Str`: `u.len()`（レジスタで届いた長さ。ユーザーメモリは読まない） / the length from the register (no user read)
    UserLen,
    /// プロセス表（generic-os）: 隠れ列 cpu_mask とアドレス空間（ページの所有権）を持つアリーナのメソッド
    /// process tables (generic-os): methods of an arena carrying the hidden cpu_mask column and an address space (page ownership)
    ProcOnCpu(usize),
    ProcOffCpu(usize),
    ProcCpuMask(usize),
    ProcMapPage(usize),
    ProcSharePage(usize),
    ProcCowCopy(usize),
    ProcUnmapPage(usize),
    ProcTlbFlush(usize),
    ProcPeek(usize),
    ProcTlbCount(usize, bool),
}

/// 型名を第一引数に取る組み込みの種類 / kinds of builtins that take a type name first
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeCallKind {
    /// `bind_mmio(Regs, 番地)` → `mmio Regs`
    BindMmio(usize),
    /// `dma_alloc(T)` → `dma own T`
    DmaAlloc(Ty),
    /// `complete_dma(T, iova)` → `Option<dma own T>`
    CompleteDma(Ty),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    Let,
    Param,
    ForVar,
    /// パターン束縛（値で受ける） / pattern binding by value
    PatValue,
    /// パターン束縛（場所を借りる・ルックアップ）= 第二級参照 / pattern binding by reference (a second-class reference)
    PatRef,
}

#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    pub span: Span,
    pub kind: LocalKind,
}

pub struct Sema<'p> {
    pub prog: &'p Program,
    pub t: Tables,
    pub diags: Vec<Diag>,
    pub expr_ty: Vec<Ty>,
    pub res: HashMap<ExprId, Res>,
    pub calls: HashMap<ExprId, CallKind>,
    pub methods: HashMap<ExprId, MethodKind>,
    /// Field 式 → (構造体, フィールド番号) / Field expr → (struct, field index)
    pub fields: HashMap<ExprId, (usize, usize)>,
    /// MMIO の添字式 → (リージョン, 何次元目まで添字を付けたか) / MMIO index expr → (region, depth)
    pub mmio_index: HashMap<ExprId, (usize, usize)>,
    /// アリーナの添字式 → アリーナ / arena index expr → arena
    pub arena_index: HashMap<ExprId, usize>,
    /// if let / match の対象になったルックアップ（`@a.get(id)`） / lookups used as if-let / match scrutinees
    pub lookup_scrut: HashSet<ExprId>,
    pub locals: Vec<Option<LocalInfo>>,
    pub alias_items: HashMap<String, usize>,
    pub neg_lits: HashSet<ExprId>,
    /// コピーできる値への参照を自動で辿る Path 式（C では `(*p)`） / Path exprs auto-dereferencing a reference to a copyable value
    pub auto_deref: HashSet<ExprId>,
    /// ポートの読み出し（スカラーのポートを値として使う式） / port reads (a scalar port used as a value)
    pub port_reads: HashMap<ExprId, usize>,
    /// ポートの添字式 → ポート / port index expr → port
    pub port_index: HashMap<ExprId, usize>,
    /// `own_alloc(T)` → T の構造体番号 / `own_alloc(T)` → the struct id of T
    pub own_allocs: HashMap<ExprId, usize>,
    /// 厳格形 `R.get(i).unwrap()`（読み出し専用）から脱糖衣した `R[i]` / `R[i]` desugared from the strict read-only form
    /// `R.get(i).unwrap()`
    pub shared_places: HashSet<ExprId>,
    /// 関数ごとの「呼び出しリージョン」: この関数の出口で先頭を巻き戻すバンプアリーナ / per function: the bump arenas whose top
    /// this function rewinds at its exits (its call regions)
    pub call_regions: Vec<Vec<usize>>,
    /// `own_alloc(T)` の T が構造体でないとき（`own_alloc([u8; 4096])` など）の T / T of `own_alloc(T)` when T is not a struct
    pub own_alloc_tys: HashMap<ExprId, Ty>,
    /// `*e` で own の中身を指す式（式の型は中身の型。C では `(*p)`） / expressions `*e` that denote the contents of an own
    /// (the expression's type is the contents' type; `(*p)` in C)
    pub own_derefs: HashSet<ExprId>,
    /// 名前で呼んだ関数ポインタ（`f(x)` の f がローカル・static） / function pointers called by name (`f(x)` with a local /
    /// static f)
    pub call_targets: HashMap<ExprId, Res>,
    /// 関数ポインタを通した呼び出しで呼ばれうる関数（callback.rs が求める） / the functions an indirect call may reach
    /// (computed by callback.rs)
    pub indirect_cands: HashMap<ExprId, Vec<usize>>,
    /// 関数ポインタ型の引数ごとに、実際に渡される関数（callback.rs が求める。None = 型だけで決めた全候補）
    /// per function-pointer parameter, the functions actually passed (computed by callback.rs; None = every candidate of
    /// the type)
    pub param_cands: HashMap<(usize, usize), Vec<usize>>,
    /// 関数を値として使った場所（関数, 式, 型） / where functions are used as values (function, expression, type)
    pub fn_value_sites: Vec<(usize, ExprId, Span)>,
    int_lits: Vec<(ExprId, u128, Span)>,
    ivar_parent: Vec<u32>,
    ivar_val: Vec<Option<IntTy>>,
    /// 浮動小数点のリテラル（範囲の検査用）と、浮動小数点の型変数 / float literals (for the range check) and float type variables
    float_lits: Vec<(ExprId, String, Span)>,
    fvar_parent: Vec<u32>,
    fvar_val: Vec<Option<FloatTy>>,
    /// 一般の型変数（`let x;` / `None`） / general type variables (`let x;` / `None`)
    var_parent: Vec<u32>,
    var_val: Vec<Option<Ty>>,
    /// 2 周目の型検査に渡す、1 周目で決まった let の型 / let types found by the first type-checking pass, fed to the second
    seeds: HashMap<BindId, Ty>,
    /// この周回で「ここでは型が要る」場所に型変数が残っていたか / whether this pass met an unknown type where one is needed
    deferred_unknown: u32,
    /// 2 周目の型検査中か（2 周目でも決まらなければ F0282） / whether this is the second type-checking pass (F0282 if still unknown)
    second_pass: bool,
    scopes: Vec<HashMap<String, BindId>>,
    cur_ret: Ty,
    loop_breaks: Vec<bool>,
    in_bypass: u32,
    static_state: Vec<u8>,
    alias_depth: u32,
    /// 関数ごとの型付け済み本体の範囲（式番号） / per function: whether it type-checked
    pub fn_ok: Vec<bool>,
    /// `@t.start(値, 入口, 引数)` の式（型と流れの上では alloc と同じ） / `@t.start(..)` expressions (an alloc for typing and flow)
    pub task_starts: HashSet<ExprId>,
    /// 窓の引数 `&[u8]` / `&mut [u8]` に渡した式が何から作られるか / what an argument passed to a window parameter is made from
    pub slice_args: HashMap<ExprId, SliceFrom>,
    /// `@r[i]`（`dma` リージョンの要素）の式 → リージョンの番号 / `@r[i]` expressions of `dma` regions → region index
    pub dma_index: HashMap<ExprId, usize>,
    /// 実行ルートと文脈の解析結果（context.rs） / execution roots and contexts (context.rs)
    pub ctx: crate::context::CtxInfo,
    /// ユーザープログラム（EL0）として翻訳する / compile as a user program (EL0)
    pub user_mode: bool,
    /// `embed("..")` の相対パスの基準（ソースファイルのあるフォルダ） / the base of relative `embed("..")` paths
    pub base_dir: Option<std::path::PathBuf>,
    /// `static X: [u8; N] = embed("..");` の中身（static の番号 → バイト列） / embedded bytes by static index
    pub embeds: HashMap<usize, Vec<u8>>,
    /// `@t.start_user(..)` の式 / `@t.start_user(..)` expressions
    pub user_starts: HashSet<ExprId>,
    /// `spawn_on` を使う（マルチコアのプログラム） / uses `spawn_on` (a multi-core program)
    pub smp: bool,
    /// `spawn_on(コア, 関数, ..)` の式（式・関数・位置） / `spawn_on` expressions (expression, function, span)
    pub spawns: Vec<(ExprId, usize, Span)>,
    /// spawn_on のコア番号（コンパイル時に決まるなら値） / the core number of each spawn_on (its value when known at
    /// compile time)
    pub spawn_cores: HashMap<ExprId, Option<i128>>,
    /// --target hpc の計算ノードの形 / the shape of the --target hpc compute node
    pub topology: Option<crate::target::Topology>,
    /// --target hpc の検査の結果（fascon regions と、C 生成のあとの F3902 に使う） / the --target hpc analysis (used by
    /// fascon regions and by F3902 after code generation)
    pub hpc: Option<crate::hpc::HpcInfo>,
    /// `for id in a.par()` の式 → アリーナ / `for id in a.par()` expressions and their arenas
    pub par_fors: HashMap<ExprId, usize>,
    /// OS のプロファイル（`--target rtos` / `--target generic-os`） / the OS profile
    pub profile: crate::target::Profile,
    /// 型名を取る組み込みの式 → 種類 / type-call expressions → kind
    pub type_calls: HashMap<ExprId, TypeCallKind>,
    /// fascon-gc の組み込み（gc_collect(a) / promote_to_gc(a, bump_id)）: 呼び出し → (gc_arena, 昇格元のバンプアリーナ)
    /// fascon-gc builtins (gc_collect(a) / promote_to_gc(a, bump_id)): call → (gc arena, source bump arena of a promotion)
    pub gc_calls: HashMap<ExprId, (usize, Option<usize>)>,
    /// fascon-gc の自動昇格（エスケープ解析）: bump_arena の Id が gc_arena の Id の場所へ流れ出る式 → (gc_arena, bump_arena)
    /// fascon-gc automatic promotion (escape analysis): an expression whose bump_arena Id escapes into a place typed with a
    /// gc_arena Id → (gc arena, bump arena)
    pub auto_promote: HashMap<ExprId, (usize, usize)>,
    /// `#[secret] let` の束縛 / bindings declared with `#[secret] let`
    pub secret_binds: HashSet<BindId>,
    /// `atomic { .. }` の式 → (アリーナ, フィールドの道筋, 演算) / `atomic` expressions → (arena, field path, op)
    pub atomic_sites: HashMap<ExprId, (usize, Vec<String>)>,
    /// atomic で書かれるフィールド（アリーナ, 道筋） / fields written under atomic (arena, path)
    pub atomic_fields: Vec<(usize, Vec<String>)>,
    /// `user Str` を写す上限 / the limit for copying a `user Str`
    pub user_str_max: u64,
    /// プロセス表として使われたアリーナ（隠れ列 cpu_mask とアドレス空間を足す） / arenas used as process tables (they get the
    /// hidden cpu_mask column and an address space)
    pub process_arenas: HashSet<usize>,
    /// `catch_fault { .. }` の式 / catch_fault expressions
    pub catch_sites: HashSet<ExprId>,
    /// ユニバーサル OS 拡張の解析結果（Sleep 効果・再帰） / results of the universal-OS analysis (Sleep effect, recursion)
    pub universal: crate::universal::UniversalInfo,
}

fn is_struct_name(s: &str) -> bool {
    s.chars().next().map_or(false, |c| c.is_ascii_uppercase())
}

impl<'p> Sema<'p> {
    pub fn new(prog: &'p Program, ptr_bits: u32) -> Sema<'p> {
        let mut t = Tables::default();
        t.ptr_bits = ptr_bits;
        Sema {
            prog,
            t,
            diags: Vec::new(),
            expr_ty: vec![Ty::Err; prog.n_exprs as usize],
            res: HashMap::new(),
            calls: HashMap::new(),
            methods: HashMap::new(),
            fields: HashMap::new(),
            mmio_index: HashMap::new(),
            arena_index: HashMap::new(),
            lookup_scrut: HashSet::new(),
            locals: vec![None; prog.n_binds as usize],
            alias_items: HashMap::new(),
            neg_lits: HashSet::new(),
            auto_deref: HashSet::new(),
            port_reads: HashMap::new(),
            port_index: HashMap::new(),
            own_allocs: HashMap::new(),
            shared_places: HashSet::new(),
            call_regions: Vec::new(),
            own_alloc_tys: HashMap::new(),
            own_derefs: HashSet::new(),
            call_targets: HashMap::new(),
            indirect_cands: HashMap::new(),
            param_cands: HashMap::new(),
            fn_value_sites: Vec::new(),
            int_lits: Vec::new(),
            ivar_parent: Vec::new(),
            ivar_val: Vec::new(),
            float_lits: Vec::new(),
            fvar_parent: Vec::new(),
            fvar_val: Vec::new(),
            var_parent: Vec::new(),
            var_val: Vec::new(),
            seeds: HashMap::new(),
            deferred_unknown: 0,
            second_pass: false,
            scopes: Vec::new(),
            cur_ret: Ty::Unit,
            loop_breaks: Vec::new(),
            in_bypass: 0,
            static_state: Vec::new(),
            alias_depth: 0,
            fn_ok: Vec::new(),
            task_starts: HashSet::new(),
            slice_args: HashMap::new(),
            dma_index: HashMap::new(),
            ctx: crate::context::CtxInfo::default(),
            user_mode: false,
            base_dir: None,
            embeds: HashMap::new(),
            user_starts: HashSet::new(),
            smp: false,
            spawns: Vec::new(),
            spawn_cores: HashMap::new(),
            topology: None,
            hpc: None,
            par_fors: HashMap::new(),
            profile: crate::target::Profile::Classic,
            type_calls: HashMap::new(),
            gc_calls: HashMap::new(),
            auto_promote: HashMap::new(),
            secret_binds: HashSet::new(),
            atomic_sites: HashMap::new(),
            atomic_fields: Vec::new(),
            user_str_max: USER_STR_MAX_DEFAULT,
            process_arenas: HashSet::new(),
            catch_sites: HashSet::new(),
            universal: Default::default(),
        }
    }

    fn err(&mut self, d: Diag) {
        self.diags.push(d);
    }

    // ───────────── 整数型変数 / integer type variables ─────────────

    fn new_ivar(&mut self) -> Ty {
        let id = self.ivar_parent.len() as u32;
        self.ivar_parent.push(id);
        self.ivar_val.push(None);
        Ty::IntVar(id)
    }
    fn new_fvar(&mut self) -> Ty {
        let id = self.fvar_parent.len() as u32;
        self.fvar_parent.push(id);
        self.fvar_val.push(None);
        Ty::FloatVar(id)
    }
    fn ffind(&mut self, v: u32) -> u32 {
        let mut r = v;
        while self.fvar_parent[r as usize] != r {
            r = self.fvar_parent[r as usize];
        }
        let mut c = v;
        while self.fvar_parent[c as usize] != r {
            let n = self.fvar_parent[c as usize];
            self.fvar_parent[c as usize] = r;
            c = n;
        }
        r
    }
    /// 一般の型変数 / a general type variable
    pub fn new_var(&mut self) -> Ty {
        let id = self.var_parent.len() as u32;
        self.var_parent.push(id);
        self.var_val.push(None);
        Ty::Var(id)
    }
    fn vfind(&mut self, v: u32) -> u32 {
        let mut r = v;
        while self.var_parent[r as usize] != r {
            r = self.var_parent[r as usize];
        }
        let mut c = v;
        while self.var_parent[c as usize] != r {
            let n = self.var_parent[c as usize];
            self.var_parent[c as usize] = r;
            c = n;
        }
        r
    }
    /// 出現検査: v が t の中に現れるか（`x = Some(x)` のような無限の型を作らない） / occurs check (no infinite types)
    fn occurs(&mut self, v: u32, t: &Ty) -> bool {
        match self.resolve(t) {
            Ty::Var(w) => self.vfind(w) == v,
            Ty::Option(i) | Ty::Own(i) | Ty::Array(i, _) | Ty::Ref(_, i) | Ty::User(i) | Ty::DmaOwn(i) => self.occurs(v, &i),
            Ty::Fn(ps, r) => ps.iter().any(|p| self.occurs(v, p)) || self.occurs(v, &r),
            Ty::Result(a, b) => self.occurs(v, &a) || self.occurs(v, &b),
            _ => false,
        }
    }
    fn ifind(&mut self, v: u32) -> u32 {
        let mut r = v;
        while self.ivar_parent[r as usize] != r {
            r = self.ivar_parent[r as usize];
        }
        let mut c = v;
        while self.ivar_parent[c as usize] != r {
            let n = self.ivar_parent[c as usize];
            self.ivar_parent[c as usize] = r;
            c = n;
        }
        r
    }
    /// 型変数を可能な限り具体化する / resolve type variables as far as known
    pub fn resolve(&mut self, t: &Ty) -> Ty {
        match t {
            Ty::IntVar(v) => {
                let r = self.ifind(*v);
                match self.ivar_val[r as usize] {
                    Some(it) => Ty::Int(it),
                    None => Ty::IntVar(r),
                }
            }
            Ty::FloatVar(v) => {
                let r = self.ffind(*v);
                match self.fvar_val[r as usize] {
                    Some(ft) => Ty::Float(ft),
                    None => Ty::FloatVar(r),
                }
            }
            Ty::Var(v) => {
                let r = self.vfind(*v);
                match self.var_val[r as usize].clone() {
                    Some(t) => self.resolve(&t),
                    None => Ty::Var(r),
                }
            }
            Ty::Option(i) => Ty::Option(Box::new(self.resolve(i))),
            Ty::Own(i) => Ty::Own(Box::new(self.resolve(i))),
            Ty::Array(e, n) => Ty::Array(Box::new(self.resolve(e)), *n),
            Ty::Ref(m, i) => Ty::Ref(*m, Box::new(self.resolve(i))),
            Ty::Fn(ps, r) => Ty::Fn(ps.iter().map(|p| self.resolve(p)).collect(), Box::new(self.resolve(r))),
            Ty::Result(a, b) => Ty::Result(Box::new(self.resolve(a)), Box::new(self.resolve(b))),
            Ty::User(i) => Ty::User(Box::new(self.resolve(i))),
            Ty::DmaOwn(i) => Ty::DmaOwn(Box::new(self.resolve(i))),
            other => other.clone(),
        }
    }
    /// 最終化: 未決定の整数型は i32、どこからも決まらなかった型変数も i32（値として一度も使われていない）
    /// finalize: undecided integers become i32; a type variable nothing constrained becomes i32 too (it is never used as a value)
    fn finalize(&mut self, t: &Ty) -> Ty {
        match self.resolve(t) {
            Ty::IntVar(_) | Ty::Var(_) => Ty::Int(IntTy::I32),
            // 決まらなかった浮動小数点のリテラルは f64（Rust と同じ） / an undecided float literal is f64 (as in Rust)
            Ty::FloatVar(_) => Ty::Float(FloatTy::F64),
            Ty::Option(i) => Ty::Option(Box::new(self.finalize(&i))),
            Ty::Own(i) => Ty::Own(Box::new(self.finalize(&i))),
            Ty::Array(e, n) => Ty::Array(Box::new(self.finalize(&e)), n),
            Ty::Ref(m, i) => Ty::Ref(m, Box::new(self.finalize(&i))),
            Ty::Fn(ps, r) => Ty::Fn(ps.iter().map(|p| self.finalize(p)).collect(), Box::new(self.finalize(&r))),
            // 失敗の型が決まらなかった Result は FaultError（いちばんよく使う形） / an undetermined error type becomes FaultError
            Ty::Result(a, b) => {
                let bb = match self.resolve(&b) {
                    Ty::Var(_) => Ty::Fault,
                    x => self.finalize(&x),
                };
                Ty::Result(Box::new(self.finalize(&a)), Box::new(bb))
            }
            Ty::User(i) => Ty::User(Box::new(self.finalize(&i))),
            Ty::DmaOwn(i) => Ty::DmaOwn(Box::new(self.finalize(&i))),
            other => other,
        }
    }

    pub fn unify(&mut self, a: &Ty, b: &Ty) -> bool {
        let a = self.resolve(a);
        let b = self.resolve(b);
        match (&a, &b) {
            (Ty::Err, _) | (_, Ty::Err) | (Ty::Never, _) | (_, Ty::Never) => true,
            (Ty::Var(x), Ty::Var(y)) => {
                let (rx, ry) = (self.vfind(*x), self.vfind(*y));
                if rx != ry {
                    self.var_parent[rx as usize] = ry;
                }
                true
            }
            (Ty::Var(x), other) | (other, Ty::Var(x)) => {
                let r = self.vfind(*x);
                if self.occurs(r, other) {
                    return false;
                }
                self.var_val[r as usize] = Some(other.clone());
                true
            }
            (Ty::IntVar(x), Ty::IntVar(y)) => {
                let (rx, ry) = (self.ifind(*x), self.ifind(*y));
                if rx != ry {
                    self.ivar_parent[rx as usize] = ry;
                }
                true
            }
            (Ty::IntVar(x), Ty::Int(it)) | (Ty::Int(it), Ty::IntVar(x)) => {
                let r = self.ifind(*x);
                self.ivar_val[r as usize] = Some(*it);
                true
            }
            (Ty::Int(x), Ty::Int(y)) => x == y,
            (Ty::FloatVar(x), Ty::FloatVar(y)) => {
                let (rx, ry) = (self.ffind(*x), self.ffind(*y));
                if rx != ry {
                    self.fvar_parent[rx as usize] = ry;
                }
                true
            }
            (Ty::FloatVar(x), Ty::Float(ft)) | (Ty::Float(ft), Ty::FloatVar(x)) => {
                let r = self.ffind(*x);
                self.fvar_val[r as usize] = Some(*ft);
                true
            }
            (Ty::Float(x), Ty::Float(y)) => x == y,
            (Ty::Option(x), Ty::Option(y)) => self.unify(x, y),
            (Ty::Own(x), Ty::Own(y)) => self.unify(x, y),
            (Ty::Result(a1, b1), Ty::Result(a2, b2)) => {
                let ok = self.unify(a1, a2);
                ok & self.unify(b1, b2)
            }
            (Ty::DmaOwn(x), Ty::DmaOwn(y)) | (Ty::User(x), Ty::User(y)) => self.unify(x, y),
            (Ty::Array(x, n), Ty::Array(y, m)) => n == m && self.unify(x, y),
            (Ty::Ref(m1, x), Ty::Ref(m2, y)) => m1 == m2 && self.unify(x, y),
            // 関数ポインタ型は構造で比べる（引数の個数・型・参照の可変性・戻り値がすべて同じ）
            // function pointer types are structural (same arity, parameter types, reference mutability and return type)
            (Ty::Fn(p1, r1), Ty::Fn(p2, r2)) => {
                if p1.len() != p2.len() {
                    return false;
                }
                let mut ok = true;
                for (x, y) in p1.iter().zip(p2.iter()) {
                    ok &= self.unify(x, y);
                }
                ok && self.unify(r1, r2)
            }
            _ => a == b,
        }
    }

    fn mismatch(&mut self, span: Span, expected: &Ty, found: &Ty) {
        let e = self.resolve(expected);
        let f = self.resolve(found);
        if e.is_err() || f.is_err() {
            return;
        }
        let (es, fs) = (self.t.display(&e), self.t.display(&f));
        // アリーナの着色違反は専用のコード / arena colour violations get their own code
        if let (Ty::Id(a), Ty::Id(b)) = (&e, &f) {
            let (an, bn) = (self.t.arenas[*a].name.clone(), self.t.arenas[*b].name.clone());
            self.err(
                Diag::error(
                    "F2101",
                    span,
                    format!("アリーナの色が一致しません: `Id<{}>` が必要な場所に `Id<{}>` を使っています", an, bn),
                    format!("arena colour mismatch: `Id<{}>` is required here but `Id<{}>` was given", an, bn),
                )
                .hint(
                    format!("{} の Id は {} のスロットを指しません。{} から取得した Id を使ってください", bn, an, an),
                    format!("an Id of {} never denotes a slot of {}; use an Id obtained from {}", bn, an, an),
                ),
            );
            return;
        }
        if let Ty::Ref(..) = f {
            if !matches!(e, Ty::Ref(..)) {
                self.err(Diag::error(
                    "F0515",
                    span,
                    format!("参照 `{}` を値として保存・返却することはできません（第二級参照）", fs),
                    format!("the reference `{}` cannot be stored or returned as a value (second-class references)", fs),
                ).hint("値を所有させるか、`.clone()` で複製してください", "own the value, or copy it with `.clone()`"));
                return;
            }
        }
        let mut d = Diag::error(
            "F0308",
            span,
            format!("型が一致しません: `{}` が必要ですが `{}` です", es, fs),
            format!("mismatched types: expected `{}`, found `{}`", es, fs),
        );
        // own と中身の取り違え / an own mixed up with its contents
        match (&e, &f) {
            (_, Ty::Own(x)) if **x == e || matches!((&e, &**x), (Ty::Int(_), Ty::Int(_))) => {
                d = d.hint("own の中身は `*` で読みます（例: `*ctx.buffer`）", "read the contents of an own with `*` (e.g. `*ctx.buffer`)");
            }
            (Ty::Own(x), _) if **x == f || matches!((&**x, &f), (Ty::Int(_), Ty::Int(_) | Ty::IntVar(_))) => {
                d = d.hint(
                    "own の中身に書くなら `*場所 = 値;`、新しい own を作るなら `own 値` と書きます（`own` は前置演算子なので、式は `own (a + 1)` と括弧で囲みます）",
                    "write `*place = value;` to store into an own, or `own value` to make a new one (`own` is a prefix operator, so wrap an expression: `own (a + 1)`)",
                );
            }
            (Ty::Fn(..), Ty::Fn(..)) => {
                d = d.hint(
                    "関数ポインタの型は引数の型・参照の可変性・戻り値の型がすべて一致する必要があります",
                    "function pointer types must agree in every parameter type, reference mutability and return type",
                );
            }
            _ => {}
        }
        self.err(d);
    }

    /// ここで型が分かっている必要がある（フィールド・メソッド・比較・束縛の受け方）。型変数のままなら F0282 を出す
    /// （1 周目なら、後ろの代入から推論した型を持って 2 周目をやり直す）。true = 型が分からない
    /// the type must be known here (fields, methods, comparisons, binding modes); a type variable still unresolved is F0282
    /// (in the first pass this triggers a second pass seeded with the types inferred from later assignments). true = unknown
    fn unknown_here(&mut self, t: &Ty, span: Span, strict: bool) -> bool {
        let r = self.resolve(t);
        let top_var = matches!(r, Ty::Var(_)) || matches!(&r, Ty::Option(i) if matches!(**i, Ty::Var(_)));
        if !top_var {
            return false;
        }
        if !self.second_pass {
            // 1 周目: 関数全体から推論した型を持って 2 周目をやり直す / first pass: redo with the types inferred from the whole function
            self.deferred_unknown += 1;
            return true;
        }
        if !strict {
            // 2 周目でも決まらない値は、どこにも型を決める使い方が無い = 最後に i32 になる（比較・キャストはそのまま通る）
            // still unknown in the second pass: nothing determines it, so it ends up i32 (comparisons and casts go through)
            return false;
        }
        self.err(
            Diag::error(
                "F0282",
                span,
                "ここでは型が決まっている必要がありますが、関数全体から推論しても決まりませんでした",
                "the type must be known here, but inferring from the whole function did not determine it",
            )
            .hint(
                "この値をどこかで代入・比較・引数に使うか、`let x: 型 = ..;` と型を書いてください",
                "assign it, compare it or pass it somewhere, or write the type as `let x: Type = ..;`",
            ),
        );
        true
    }

    /// 整数でなければならない場所: まだ決まっていない型は「何かの整数」に決める / a place that must be an integer: an unknown
    /// type becomes "some integer"
    fn want_int(&mut self, t: &Ty) -> Ty {
        let r = self.resolve(t);
        if let Ty::Var(_) = r {
            let iv = self.new_ivar();
            self.unify(&r, &iv);
            return self.resolve(&iv);
        }
        r
    }

    fn expect_ty(&mut self, span: Span, expected: &Ty, found: &Ty) {
        if !self.unify(expected, found) {
            self.mismatch(span, expected, found);
        }
    }

    // ───────────── 宣言の収集 / collecting declarations ─────────────

    pub fn run(&mut self) {
        let prog = self.prog;
        if prog.items.iter().any(|i| matches!(i, Item::Fn(f) if matches!(f.kind, FnKind::SyscallStub(_)))) {
            self.user_mode = true;
        }
        // 1. 名前の登録 / register names
        for (i, item) in prog.items.iter().enumerate() {
            match item {
                // モジュールは字句の段階で展開済み（残っていれば F3701） / modules were expanded at the token level (F3701 if one is left)
                Item::Import(im) => {
                    self.err(Diag::error(
                        "F3701",
                        im.span,
                        format!("`import {};` を展開できませんでした（モジュールの検索パスを確かめてください）", im.path.join("::")),
                        format!("`import {};` could not be expanded (check the module search path)", im.path.join("::")),
                    ));
                }
                Item::Struct(s) => {
                    if self.t.struct_by_name.contains_key(&s.name) || self.alias_items.contains_key(&s.name) {
                        self.dup(&s.name, s.name_span);
                        continue;
                    }
                    self.t.struct_by_name.insert(s.name.clone(), self.t.structs.len());
                    self.t.structs.push(StructInfo { name: s.name.clone(), fields: Vec::new(), span: s.span, needs_drop: false, align: 0, wire: None, secret: false, on_drop: None, stable_abi: false, no_column: Vec::new() });
                }
                Item::Alias(a) => {
                    if self.t.struct_by_name.contains_key(&a.name) || self.alias_items.contains_key(&a.name) || builtin_type_name(&a.name) {
                        self.dup(&a.name, a.name_span);
                        continue;
                    }
                    self.alias_items.insert(a.name.clone(), i);
                }
                Item::Static(s) => {
                    if self.t.static_by_name.contains_key(&s.name) {
                        self.dup(&s.name, s.name_span);
                        continue;
                    }
                    self.t.static_by_name.insert(s.name.clone(), self.t.statics.len());
                    self.t.statics.push(StaticInfo { name: s.name.clone(), ty: Ty::Err, item: i, span: s.span, int_value: None });
                }
                Item::Fn(f) => {
                    // システムコールの名前は組み込み関数と重なってよい（`exit` など。カーネルの中では組み込みのほうが呼ばれる）
                    // a system call may share its name with a builtin (`exit`, ...); inside the kernel the builtin is the one called
                    let sys = matches!(f.kind, FnKind::Syscall(_) | FnKind::SyscallStub(_));
                    if self.t.fn_by_name.contains_key(&f.name) || (builtin_fn_name(&f.name) && !sys) {
                        self.dup(&f.name, f.name_span);
                        continue;
                    }
                    self.t.fn_by_name.insert(f.name.clone(), self.t.fns.len());
                    self.t.fns.push(FnInfo {
                        name: f.name.clone(),
                        params: Vec::new(),
                        ret: Ty::Unit,
                        item: i,
                        span: f.span,
                        is_main: f.name == "main",
                        effects: Effects::default(),
                        callees: Vec::new(),
                        irq: None,
                        syscall: None,
                        stub: matches!(f.kind, FnKind::SyscallStub(_)),
                        trap: false,
                        syscall_v2: f.attrs.iter().any(|a| a.name == "syscall"),
                        trap_v2: f.attrs.iter().any(|a| a.name == "trap"),
                        extern_c: match f.kind {
                            FnKind::ExternC { body } => Some(body),
                            _ => None,
                        },
                        asm: None,
                        sleeps: f.attrs.iter().any(|a| a.name == "sleep"),
                    });
                }
                Item::Arena(a) => {
                    if self.t.regions.contains_key(&a.name) {
                        self.dup(&a.name, a.name_span);
                        continue;
                    }
                    self.t.regions.insert(a.name.clone(), RegionRef::Arena(self.t.arenas.len()));
                    self.t.arenas.push(ArenaInfo { name: a.name.clone(), elem: Ty::Err, cap: 1, span: a.span, bump: a.bump, task_stack: None, dma: a.dma, shared: a.shared, gc: a.gc, gpu: a.gpu });
                }
                Item::Port(pd) => {
                    if self.t.regions.contains_key(&pd.name) {
                        self.dup(&pd.name, pd.name_span);
                        continue;
                    }
                    self.t.regions.insert(pd.name.clone(), RegionRef::Port(self.t.ports.len()));
                    self.t.ports.push(PortInfo { name: pd.name.clone(), elem: IntTy::U8, len: None, port: 0, mmio_map: None, span: pd.span });
                }
                Item::Chan(c) => {
                    if self.t.regions.contains_key(&c.name) {
                        self.dup(&c.name, c.name_span);
                        continue;
                    }
                    self.t.regions.insert(c.name.clone(), RegionRef::Chan(self.t.chans.len()));
                    self.t.chans.push(ChanInfo { name: c.name.clone(), elem: Ty::Err, cap: 2, span: c.span });
                }
                Item::UserMem(u) => {
                    if self.t.regions.contains_key(&u.name) {
                        self.dup(&u.name, u.name_span);
                        continue;
                    }
                    self.t.regions.insert(u.name.clone(), RegionRef::UserMem(self.t.umems.len()));
                    self.t.umems.push(UserMemInfo { name: u.name.clone(), task: usize::MAX, size: 0x20_0000, at: 0x1_0000_0000, span: u.span });
                }
                Item::Dma(d) => {
                    if self.t.regions.contains_key(&d.name) {
                        self.dup(&d.name, d.name_span);
                        continue;
                    }
                    self.t.regions.insert(d.name.clone(), RegionRef::Dma(self.t.dmas.len()));
                    self.t.dmas.push(DmaInfo { name: d.name.clone(), elem: Ty::Err, len: 1, span: d.span });
                }
                Item::Mmio(m) => {
                    if self.t.regions.contains_key(&m.name) {
                        self.dup(&m.name, m.name_span);
                        continue;
                    }
                    self.t.regions.insert(m.name.clone(), RegionRef::Mmio(self.t.mmios.len()));
                    self.t.mmios.push(MmioInfo {
                        name: m.name.clone(),
                        elem: IntTy::U32,
                        dims: vec![1],
                        addr: 0,
                        framebuffer: m.attrs.iter().any(|a| a.name == "framebuffer"),
                        span: m.span,
                    });
                }
            }
        }
        // `@` が無くなったので、リージョンの名前は関数・static・型と同じ名前空間で使われる: 重なれば F0428
        // without `@`, region names share the namespace of functions, statics and types: a clash is F0428
        {
            let mut clashes = Vec::new();
            for item in &prog.items {
                let (n, sp) = match item {
                    Item::Arena(a) => (&a.name, a.name_span),
                    Item::Mmio(m) => (&m.name, m.name_span),
                    Item::Dma(d) => (&d.name, d.name_span),
                    Item::UserMem(u) => (&u.name, u.name_span),
                    Item::Chan(c) => (&c.name, c.name_span),
                    Item::Port(p) => (&p.name, p.name_span),
                    _ => continue,
                };
                if self.t.fn_by_name.contains_key(n) || self.t.static_by_name.contains_key(n) || self.t.struct_by_name.contains_key(n) || self.alias_items.contains_key(n) || builtin_type_name(n) {
                    clashes.push((n.clone(), sp));
                }
            }
            for (n, sp) in clashes {
                self.err(
                    Diag::error(
                        "F0428",
                        sp,
                        format!("リージョン `{}` と同じ名前の関数・static・型があります（リージョンは名前だけで使うので区別できません）", n),
                        format!("a function, static or type is also named `{}` (regions are used by their bare name, so the two cannot be told apart)", n),
                    )
                    .hint("どちらかの名前を変えてください", "rename one of them"),
                );
            }
        }
        self.static_state = vec![0; self.t.statics.len()];
        // 2. 整数 static の定数評価（配列長・容量・番地で使う） / evaluate integer statics (used in lengths, capacities, addresses)
        for si in 0..self.t.statics.len() {
            self.eval_static_int(si);
        }
        // 3a. アリーナの要素型（型の別名の種別境界 `@a: File` の検査に必要） / arena element types (needed by kind bounds)
        for item in &prog.items {
            if let Item::Arena(a) = item {
                self.declare_arena(a);
                if self.user_mode && (a.task_stack.is_some() || a.dma || a.gpu) {
                    self.user_ban(a.span, if a.dma { "dma_arena" } else if a.gpu { "gpu_arena" } else { "task_arena" });
                }
            }
        }
        // 3b. 構造体のフィールド型 / struct field types
        for item in &prog.items {
            if let Item::Struct(s) = item {
                let sid = match self.t.struct_by_name.get(&s.name) {
                    Some(&x) => x,
                    None => continue,
                };
                if !self.t.structs[sid].fields.is_empty() {
                    continue;
                }
                let mut fields: Vec<FieldInfo> = Vec::new();
                for f in &s.fields {
                    if fields.iter().any(|g| g.name == f.name) {
                        self.err(Diag::error("F0124", f.span, format!("フィールド `{}` が重複しています", f.name), format!("field `{}` is declared twice", f.name)));
                        continue;
                    }
                    let ty = self.resolve_type(&f.ty, false, &HashMap::new());
                    if let Ty::Ref(..) = ty {
                        self.err(
                            Diag::error(
                                "F0515",
                                f.ty.span(),
                                "参照型は構造体のフィールドに書けません（第二級参照: 引数と一時的なルックアップだけ）",
                                "reference types cannot be struct fields (second-class references: arguments and temporary lookups only)",
                            )
                            .hint(
                                "相手がアリーナにあるなら `Id<@アリーナ>` を、子として所有するなら `own T` を持たせてください",
                                "store an `Id<@arena>` if the target lives in an arena, or `own T` if this struct owns it",
                            ),
                        );
                    }
                    fields.push(FieldInfo { name: f.name.clone(), ty, span: f.span });
                }
                // フィールドの属性: `#[no_column]`（列を作らない）。`#[secret]` は構造体全体を秘密にする
                // field attributes: `#[no_column]` (no columns); `#[secret]` makes the whole struct secret
                let mut nc = Vec::new();
                for f in &s.fields {
                    nc.push(f.attrs.iter().any(|a| a.name == "no_column"));
                    for a in &f.attrs {
                        match a.name.as_str() {
                            "no_column" => {}
                            "secret" => self.t.structs[sid].secret = true,
                            other => self.err(Diag::warning("F3504", a.span, format!("フィールドの属性 `{}` は無視されます（使えるのは `#[no_column]` と `#[secret]`）", other), format!("field attribute `{}` is ignored (`#[no_column]` and `#[secret]` are accepted)", other))),
                        }
                    }
                }
                self.t.structs[sid].no_column = nc;
                self.t.structs[sid].fields = fields;
                // `#[repr(align(N))]`: N は 1〜4096 の 2 の累乗（スロット・値の整列を N バイト境界に上げる）
                // N must be a power of two from 1 to 4096 (raises the alignment of slots and values to N bytes)
                for a in &s.attrs {
                    match (a.name.as_str(), a.repr_align) {
                        ("repr", Some((n, nsp))) => {
                            if n == 0 || n > 4096 || (n & (n - 1)) != 0 {
                                self.err(Diag::error(
                                    "F2031",
                                    nsp,
                                    format!("`align({})` は使えません（1〜4096 の 2 の累乗）", n),
                                    format!("`align({})` is not allowed (a power of two from 1 to 4096)", n),
                                ));
                            } else {
                                self.t.structs[sid].align = n as u32;
                            }
                        }
                        ("repr", None) => {}
                        // ワイヤ構造体: `#[wire(le)]` / `#[wire(be)]`
                        ("wire", _) => match a.ident_arg.as_deref() {
                            Some("le") => self.t.structs[sid].wire = Some(false),
                            Some("be") => self.t.structs[sid].wire = Some(true),
                            _ => self.err(Diag::error("F2701", a.span, "`#[wire(le)]` か `#[wire(be)]` と書きます（バイト順の指定が要ります）", "write `#[wire(le)]` or `#[wire(be)]` (the byte order is required)")),
                        },
                        // ユニバーサル OS 拡張の構造体の属性 / struct attributes of the universal-OS extension
                        ("secret", _) => self.t.structs[sid].secret = true,
                        ("stable_abi", _) => self.t.structs[sid].stable_abi = true,
                        ("on_drop", _) => match a.ident_arg.clone() {
                            Some(f) => self.t.structs[sid].on_drop = Some(f),
                            None => self.err(Diag::error("F3501", a.span, "`#[on_drop(関数名)]` と書きます", "write `#[on_drop(function_name)]`")),
                        },
                        _ => self.err(Diag::warning("F2031", a.span, format!("属性 `{}` は無視されます（構造体に使えるのは `#[repr(align(N))]`）", a.name), format!("attribute `{}` is ignored (structs accept `#[repr(align(N))]`)", a.name))),
                    }
                }
            }
        }
        self.check_wire_structs();
        self.check_struct_cycles();
        self.compute_needs_drop();
        self.check_bump_rules();
        // 4. MMIO リージョンと I/O ポート / MMIO regions and I/O ports
        for item in &prog.items {
            match item {
                Item::Mmio(m) => {
                    if self.user_mode {
                        self.user_ban(m.span, "mmio");
                    }
                    self.declare_mmio(m)
                }
                Item::Port(pd) if self.user_mode => {
                    self.user_ban(pd.span, "port_io");
                    self.declare_port(pd)
                }
                Item::Dma(d) => {
                    if self.user_mode {
                        self.user_ban(d.span, "dma");
                    }
                    self.declare_dma(d)
                }
                Item::UserMem(u) => self.declare_user_mem(u),
                Item::Chan(c) => self.declare_chan(c),
                Item::Port(pd) => self.declare_port(pd),
                _ => {}
            }
        }
        self.check_mmio_overlap();
        self.check_port_overlap();
        // 5. 関数の型（static の初期値が関数ポインタ `static OPS: [fn(i32) -> i32; 2] = [inc, dbl];` を使えるよう先に）
        // function signatures (first, so that static initializers can use function pointers)
        for item in &prog.items {
            if let Item::Fn(f) = item {
                self.declare_fn(f);
            }
        }
        // 6. static の型と初期値 / static types and initializers
        for item in &prog.items {
            if let Item::Static(s) = item {
                self.check_static(s);
            }
        }
        // 7. 本体の型検査 / bodies
        self.fn_ok = vec![true; self.t.fns.len()];
        for item in &prog.items {
            if let Item::Fn(f) = item {
                self.check_fn(f);
            }
        }
        // 8. 関数ポインタを通した呼び出しで呼ばれうる関数（呼び出しリージョン・効果の計算で使う）
        // the functions reachable through function-pointer calls (used by call regions and effects)
        crate::callback::compute_candidates(self);
        // 9. バンプアリーナの呼び出しリージョン / call regions of bump arenas
        self.compute_call_regions();
        // main の確認 / check main
        match self.t.fn_by_name.get("main") {
            None => self.err(Diag::error(
                "F0601",
                Span::new(0, 0),
                "`fn main()` がありません（実行は main から始まります）",
                "`fn main()` is missing (execution starts at main)",
            )),
            Some(&m) => {
                let f = &self.t.fns[m];
                if !f.params.is_empty() || !(f.ret == Ty::Unit || f.ret == Ty::Never) {
                    let sp = f.span;
                    self.err(Diag::error("F0601", sp, "`main` は引数・戻り値なしの `fn main()` です", "`main` must be `fn main()` with no parameters and no return value"));
                }
            }
        }
    }

    fn dup(&mut self, name: &str, span: Span) {
        self.err(Diag::error("F0428", span, format!("`{}` が重複して定義されています", name), format!("`{}` is defined more than once", name)));
    }

    fn check_struct_cycles(&mut self) {
        // 値として埋め込む依存（own は切れ目）に循環があれば無限サイズ
        // a cycle of by-value embedding (own breaks it) means infinite size
        let n = self.t.structs.len();
        let mut deps: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, s) in self.t.structs.iter().enumerate() {
            for f in &s.fields {
                collect_value_struct_deps(&f.ty, &mut deps[i]);
            }
        }
        let mut state = vec![0u8; n];
        let mut reported = HashSet::new();
        for i in 0..n {
            let mut stack = Vec::new();
            self.dfs_cycle(i, &deps, &mut state, &mut stack, &mut reported);
        }
    }

    fn dfs_cycle(&mut self, i: usize, deps: &[Vec<usize>], state: &mut Vec<u8>, stack: &mut Vec<usize>, reported: &mut HashSet<usize>) {
        if state[i] == 2 {
            return;
        }
        if state[i] == 1 {
            if reported.insert(i) {
                let s = &self.t.structs[i];
                let (sp, name) = (s.span, s.name.clone());
                self.err(
                    Diag::error(
                        "F2030",
                        sp,
                        format!("構造体 `{}` が自分自身を値として含んでいます（無限の大きさ）", name),
                        format!("struct `{}` contains itself by value (infinite size)", name),
                    )
                    .hint(
                        format!("再帰には所有ポインタを使います: `Option<own {}>`（1.0 では値型の構造体はインラインに置かれます）", name),
                        format!("use an owning pointer for recursion: `Option<own {}>` (1.0 stores plain structs inline)", name),
                    ),
                );
            }
            return;
        }
        state[i] = 1;
        stack.push(i);
        for &d in &deps[i] {
            self.dfs_cycle(d, deps, state, stack, reported);
        }
        stack.pop();
        state[i] = 2;
    }

    /// own_alloc(T) が 0 で埋めるとき、Option でない own の子をたどって T へ戻るか（戻るなら無限の連鎖）
    /// whether following non-Option own children from T (as own_alloc(T) does) leads back to a struct already on the path
    /// `own_alloc(T)` が 0 で作る own の子の構造体（Option でない own のフィールド。配列・値の構造体の中も。重複なし）
    /// the structs of the own children `own_alloc(T)` creates zero-filled (non-Option own fields, also inside arrays and
    /// inline structs; no duplicates)
    fn own_alloc_kids(&self, sid: usize, out: &mut Vec<usize>, depth: u32) {
        if depth > 64 {
            return;
        }
        for f in &self.t.structs[sid].fields {
            self.own_alloc_kids_ty(&f.ty, out, depth + 1);
        }
    }

    /// 構造体でない型 t を `own_alloc(t)` で 0 から作るときに 0 で作る own の子の構造体（配列の中・値の構造体の中も）
    /// the structs of the own children created zero-filled by `own_alloc(t)` for a non-struct t (inside arrays and inline
    /// structs too)
    fn own_alloc_kids_ty(&self, t: &Ty, out: &mut Vec<usize>, depth: u32) {
        if depth > 64 {
            return;
        }
        match t {
            Ty::Own(i) => match &**i {
                Ty::Struct(c) => {
                    if !out.contains(c) {
                        out.push(*c);
                    }
                    self.own_alloc_kids(*c, out, depth + 1);
                }
                // `own [own Leaf; 3]` などの中身の own の子も 0 で作る / own children inside a non-struct own are created too
                other => self.own_alloc_kids_ty(other, out, depth + 1),
            },
            Ty::Array(e, _) => self.own_alloc_kids_ty(e, out, depth + 1),
            Ty::Struct(c) => self.own_alloc_kids(*c, out, depth + 1),
            _ => {}
        }
    }

    fn own_chain_cycle(&self, sid: usize) -> Option<String> {
        fn go(t: &Tables, s: usize, path: &mut Vec<usize>) -> bool {
            if path.contains(&s) {
                path.push(s);
                return true;
            }
            path.push(s);
            for f in &t.structs[s].fields {
                // 値として埋め込まれた構造体・配列・構造体でない own の中の own も 0 埋めの対象
                // own inside inline structs, arrays and non-struct owns is filled too
                if go_ty(t, &f.ty, path) {
                    return true;
                }
            }
            path.pop();
            false
        }
        fn go_ty(t: &Tables, ty: &Ty, path: &mut Vec<usize>) -> bool {
            if path.len() > 64 {
                return false;
            }
            match ty {
                Ty::Array(e, _) => go_ty(t, e, path),
                Ty::Own(i) => match &**i {
                    Ty::Struct(c) => go(t, *c, path),
                    other => go_ty(t, other, path),
                },
                Ty::Struct(c) => go(t, *c, path),
                _ => false,
            }
        }
        let mut path = Vec::new();
        if go(&self.t, sid, &mut path) {
            Some(path.iter().map(|x| self.t.structs[*x].name.clone()).collect::<Vec<_>>().join(" → "))
        } else {
            None
        }
    }

    fn compute_needs_drop(&mut self) {
        loop {
            let mut changed = false;
            for i in 0..self.t.structs.len() {
                if self.t.structs[i].needs_drop {
                    continue;
                }
                // #[secret]（破棄で 0 埋め）と #[on_drop(f)]（破棄で f を呼ぶ）も解放処理が要る
                // #[secret] (zeroed on drop) and #[on_drop(f)] (f called on drop) need drop glue too
                let nd = self.t.structs[i].secret || self.t.structs[i].on_drop.is_some() || self.t.structs[i].fields.iter().any(|f| self.t.needs_drop(&f.ty));
                if nd {
                    self.t.structs[i].needs_drop = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    // ───────────── 定数評価 / constant evaluation ─────────────

    fn eval_static_int(&mut self, si: usize) -> Option<i128> {
        match self.static_state.get(si).copied().unwrap_or(2) {
            2 => return self.t.statics[si].int_value,
            1 => {
                let sp = self.t.statics[si].span;
                let name = self.t.statics[si].name.clone();
                self.err(Diag::error("F0391", sp, format!("static `{}` の初期値が循環しています", name), format!("the initializer of static `{}` is cyclic", name)));
                return None;
            }
            _ => {}
        }
        self.static_state[si] = 1;
        let item = self.t.statics[si].item;
        let prog = self.prog;
        let v = if let Item::Static(s) = &prog.items[item] {
            match &s.ty {
                TypeExpr::Path { name, args, .. } if args.is_empty() && IntTy::from_name(name).is_some() => {
                    let it = IntTy::from_name(name).unwrap();
                    let v = self.const_eval(&s.value);
                    if let Some(v) = v {
                        let (lo, hi) = it.min_max(self.t.ptr_bits);
                        if v < lo || v > hi {
                            self.err(Diag::error(
                                "F0308",
                                s.value.span,
                                format!("値 {} は `{}` に収まりません", v, it.name()),
                                format!("the value {} does not fit in `{}`", v, it.name()),
                            ));
                        }
                    }
                    v
                }
                _ => None,
            }
        } else {
            None
        };
        self.t.statics[si].int_value = v;
        self.static_state[si] = 2;
        v
    }

    /// 整数の定数式を評価する（リテラル・四則・シフト・ビット演算・整数 static）
    /// evaluate an integer constant expression (literals, arithmetic, shifts, bit ops, integer statics)
    pub fn const_eval(&mut self, e: &Expr) -> Option<i128> {
        match &e.kind {
            ExprKind::Int(v, _) => Some(*v as i128),
            ExprKind::Path(n) => {
                let si = *self.t.static_by_name.get(n)?;
                self.eval_static_int(si)
            }
            ExprKind::Unary(UnOp::Neg, x) => self.const_eval(x).map(|v| -v),
            ExprKind::Unary(UnOp::Not, x) => self.const_eval(x).map(|v| !v),
            ExprKind::Cast(x, _) => self.const_eval(x),
            ExprKind::Binary(op, a, b) => {
                let x = self.const_eval(a)?;
                let y = self.const_eval(b)?;
                match op {
                    BinOp::Add => x.checked_add(y),
                    BinOp::Sub => x.checked_sub(y),
                    BinOp::Mul => x.checked_mul(y),
                    BinOp::Div => {
                        if y == 0 {
                            None
                        } else {
                            Some(x / y)
                        }
                    }
                    BinOp::Rem => {
                        if y == 0 {
                            None
                        } else {
                            Some(x % y)
                        }
                    }
                    BinOp::Shl => {
                        if (0..64).contains(&y) {
                            x.checked_shl(y as u32)
                        } else {
                            None
                        }
                    }
                    BinOp::Shr => {
                        if (0..128).contains(&y) {
                            Some(x >> y)
                        } else {
                            None
                        }
                    }
                    BinOp::BitAnd => Some(x & y),
                    BinOp::BitOr => Some(x | y),
                    BinOp::BitXor => Some(x ^ y),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn const_u64(&mut self, e: &Expr, what_ja: &str, what_en: &str) -> Option<u64> {
        match self.const_eval(e) {
            Some(v) if v >= 0 && v <= u64::MAX as i128 => Some(v as u64),
            _ => {
                self.err(Diag::error(
                    "F0435",
                    e.span,
                    format!("{}はコンパイル時に決まる 0 以上の整数でなければなりません", what_ja),
                    format!("{} must be a non-negative integer known at compile time", what_en),
                ));
                None
            }
        }
    }

    // ───────────── 型の解決 / resolving type expressions ─────────────

    pub fn resolve_type(&mut self, te: &TypeExpr, allow_ref: bool, env: &HashMap<String, usize>) -> Ty {
        match te {
            TypeExpr::Unit(_) => Ty::Unit,
            // `user [u8; N]` / `user Str`: #[syscall] の引数だけ（引数の位置 = allow_ref のとき） / #[syscall] parameters only
            TypeExpr::User(inner, span) => {
                let it = self.resolve_type(inner, false, env);
                let ok = match &it {
                    Ty::Array(e, _) => **e == Ty::Int(IntTy::U8),
                    Ty::Str => true,
                    Ty::Err => return Ty::Err,
                    _ => false,
                };
                if !ok {
                    let d = self.t.display(&it);
                    self.err(
                        Diag::error("F3102", *span, format!("`user {}` は書けません（ユーザーのバッファは `user [u8; N]` か `user Str`）", d), format!("`user {}` is not allowed (a user buffer is `user [u8; N]` or `user Str`)", d))
                            .hint("構造体を受け取るときは `user [u8; N]` で受けて、写したバイト列を `.read(T, 0)` で読んでください", "to receive a struct, take `user [u8; N]` and read the copied bytes with `.read(T, 0)`"),
                    );
                    return Ty::Err;
                }
                if !allow_ref {
                    self.err(
                        Diag::error("F3102", *span, "`user T` は `#[syscall(N)]` の関数の引数にだけ書けます（保存できないユーザー空間の番地）", "`user T` can only be a parameter of a `#[syscall(N)]` function (a user-space address that cannot be stored)")
                            .hint("中身は `copy_from_user(ubuf)?` でカーネルの値へ 1 回だけ写して使います", "copy the contents once into a kernel value with `copy_from_user(ubuf)?`"),
                    );
                    return Ty::Err;
                }
                Ty::User(Box::new(it))
            }
            // `dma own T`: T は POD（デバイスが書いたどんなビット列も正しい値） / T must be POD (any bit pattern a device writes is valid)
            TypeExpr::DmaOwn(inner, span) => {
                let it = self.resolve_type(inner, false, env);
                if it.is_err() {
                    return Ty::Err;
                }
                if !self.is_pod(&it) {
                    let d = self.t.display(&it);
                    self.err(
                        Diag::error("F3403", *span, format!("`dma own {}` は書けません（デバイスと共有する値は POD: 整数・その配列・ワイヤ構造体）", d), format!("`dma own {}` is not allowed (values shared with a device are POD: integers, arrays of them, wire structs)", d))
                            .hint("bool・Option・Id・own・Str・関数ポインタはデバイスが壊せるので置けません", "bool, Option, Id, own, Str and function pointers can be corrupted by a device and are not allowed"),
                    );
                    return Ty::Err;
                }
                Ty::DmaOwn(Box::new(it))
            }
            // `mmio T`: レジスタ構造体（整数とその配列だけ）のハンドル / the handle of a register struct (integers and arrays only)
            TypeExpr::MmioBind(inner, span) => {
                let it = self.resolve_type(inner, false, env);
                match it {
                    Ty::Struct(sid) if self.is_reg_struct(sid, 0) => Ty::MmioBind(sid),
                    Ty::Err => Ty::Err,
                    other => {
                        let d = self.t.display(&other);
                        self.err(Diag::error("F3404", *span, format!("`mmio {}` は書けません（レジスタ構造体: フィールドが u8〜u64 の整数かその配列だけの構造体）", d), format!("`mmio {}` is not allowed (a register struct has only u8-u64 integer fields or arrays of them)", d)));
                        Ty::Err
                    }
                }
            }
            TypeExpr::Ref { mutable, inner, span } => {
                let it = match &**inner {
                    TypeExpr::Slice { elem, span: ssp } => {
                        let et = self.resolve_type(elem, false, env);
                        if et != Ty::Int(IntTy::U8) {
                            if !et.is_err() {
                                let d = self.t.display(&et);
                                self.err(Diag::error("F2701", *ssp, format!("窓の要素は u8 だけです（`[{}]` は不可）", d), format!("a window holds u8 only (not `[{}]`)", d)));
                            }
                            Ty::Err
                        } else {
                            Ty::Slice(Box::new(et))
                        }
                    }
                    _ => self.resolve_type(inner, false, env),
                };
                if !allow_ref {
                    self.err(
                        Diag::error(
                            "F0515",
                            *span,
                            "参照型は関数の引数にだけ書けます（第二級参照）",
                            "reference types can only be written for function parameters (second-class references)",
                        )
                        .hint("値を所有させるか、関数の引数 `&T` / `&mut T` として受け取ってください", "own the value, or take it as a function parameter of type `&T` / `&mut T`"),
                    );
                    return Ty::Err;
                }
                if let Ty::Ref(..) = it {
                    return Ty::Err;
                }
                Ty::Ref(*mutable, Box::new(it))
            }
            TypeExpr::Own(inner, span) => {
                // own は値の型ならなんでも所有できる（構造体・整数・bool・Str・Id・配列・Option）。参照・()・own の own・
                // 関数ポインタは不可
                // own can own any value type (structs, integers, bool, Str, Id, arrays, Options); references, (), own of own
                // and function pointers cannot be owned
                let it = self.resolve_type(inner, false, env);
                match it {
                    Ty::Err => Ty::Err,
                    ref t if Tables::ownable(t) => Ty::Own(Box::new(it)),
                    other => {
                        let d = self.t.display(&other);
                        self.err(
                            Diag::error(
                                "F2301",
                                *span,
                                format!("`own {}` は書けません（own の中身は値の型: 構造体・整数・bool・Str・Id・配列・Option）", d),
                                format!("`own {}` is not allowed (an own holds a value type: a struct, an integer, bool, Str, an Id, an array or an Option)", d),
                            )
                            .hint(
                                "own は 1 段で足ります（`own own T` は `own T`）。関数ポインタはそのまま値として持てます",
                                "one level of own is enough (`own own T` is `own T`); function pointers are plain values and need no own",
                            ),
                        );
                        Ty::Err
                    }
                }
            }
            TypeExpr::Fn { params, ret, span } => {
                // 関数ポインタ型: 引数は引数の位置なので参照を書ける（第二級参照のまま。呼び出しの間だけ生きる）。戻り値に参照は
                // 書けない（F0515）。配列は値渡し・値返しできない（F2040）
                // a function pointer type: its parameters are parameter positions, so references are allowed (still
                // second-class: they live for the call only); the return type cannot be a reference (F0515); arrays cannot
                // be passed or returned by value (F2040)
                let mut ps = Vec::new();
                for p in params {
                    let pt = self.resolve_type(p, true, env);
                    if let Ty::Array(..) = pt {
                        self.err(
                            Diag::error("F2040", p.span(), "配列は値渡しできません", "arrays cannot be passed by value")
                                .hint("`&[T; N]` か `&mut [T; N]` にしてください", "use `&[T; N]` or `&mut [T; N]` instead"),
                        );
                    }
                    ps.push(pt);
                }
                let rt = match ret {
                    Some(r) => {
                        let rt = self.resolve_type(r, false, env);
                        if let Ty::Array(..) = rt {
                            self.err(Diag::error("F2040", r.span(), "配列は戻り値にできません", "arrays cannot be returned by value"));
                        }
                        rt
                    }
                    None => Ty::Unit,
                };
                if ps.iter().any(|p| p.is_err()) || rt.is_err() {
                    let _ = span;
                    return Ty::Err;
                }
                Ty::Fn(ps, Box::new(rt))
            }
            // 窓は `&[u8]` / `&mut [u8]` の中でだけ解決される（上の Ref の腕）。ここへ来るのは裸の `[u8]`
            // a window is resolved only inside `&[u8]` / `&mut [u8]` (the Ref arm above); a bare `[u8]` ends up here
            TypeExpr::Slice { span, .. } => {
                self.err(
                    Diag::error("F0515", *span, "窓 `[u8]` は `&[u8]` / `&mut [u8]` の形で、引数の型にだけ書けます（保存できない第二級の参照です）", "a window `[u8]` can only be written as `&[u8]` / `&mut [u8]` in a parameter type (it is a second-class reference and cannot be stored)")
                        .hint("保存するなら `[u8; N]` の配列へ `copy_from` で写してください", "to keep the bytes, copy them into a `[u8; N]` array with `copy_from`"),
                );
                Ty::Err
            }
            TypeExpr::Array { elem, len, span } => {
                let et = self.resolve_type(elem, false, env);
                let n = self.const_u64(len, "配列の長さ", "an array length");
                match n {
                    Some(0) => {
                        self.err(Diag::error("F2040", *span, "長さ 0 の配列は使えません", "zero-length arrays are not allowed"));
                        Ty::Err
                    }
                    Some(n) => Ty::Array(Box::new(et), n),
                    None => Ty::Err,
                }
            }
            TypeExpr::Path { name, args, span } => {
                if let Some(it) = IntTy::from_name(name) {
                    self.no_args(args, *span);
                    return Ty::Int(it);
                }
                if let Some(ft) = FloatTy::from_name(name) {
                    self.no_args(args, *span);
                    return Ty::Float(ft);
                }
                match name.as_str() {
                    "bool" => {
                        self.no_args(args, *span);
                        Ty::Bool
                    }
                    // ユニバーサル OS 拡張の型 / types of the universal-OS extension
                    "FaultError" if !self.t.struct_by_name.contains_key("FaultError") => {
                        self.no_args(args, *span);
                        Ty::Fault
                    }
                    // Id の生の値: rtos / 今までの 1.0 は u32、generic-os は u64（移植できる書き方） / the raw value of an Id:
                    // u32 on rtos / plain 1.0, u64 on generic-os (the portable spelling)
                    "IdRaw" if !self.t.struct_by_name.contains_key("IdRaw") && !self.alias_items.contains_key("IdRaw") => {
                        self.no_args(args, *span);
                        Ty::Int(if self.t.id64 { IntTy::U64 } else { IntTy::U32 })
                    }
                    "Result" if !self.t.struct_by_name.contains_key("Result") => {
                        if args.len() != 2 {
                            self.err(Diag::error("F0107", *span, "`Result` には型引数が 2 つ必要です（`Result<T, E>`）", "`Result` takes exactly two type arguments (`Result<T, E>`)"));
                            return Ty::Err;
                        }
                        let mut tys = Vec::new();
                        for a in args {
                            match a {
                                GenericArg::Type(t) => {
                                    let it = self.resolve_type(t, false, env);
                                    if let Ty::Ref(..) = it {
                                        return Ty::Err;
                                    }
                                    tys.push(it);
                                }
                                GenericArg::Arena(_, sp) => {
                                    self.err(Diag::error("F0107", *sp, "`Result` の引数は型です", "the arguments of `Result` must be types"));
                                    return Ty::Err;
                                }
                            }
                        }
                        let e = tys.pop().unwrap();
                        let t = tys.pop().unwrap();
                        if !e.is_err() && !self.t.is_copy(&e) {
                            let d = self.t.display(&e);
                            self.err(
                                Diag::error("F3105", *span, format!("`Result` の失敗の型 `{}` はコピーできる型でなければなりません", d), format!("the error type `{}` of a `Result` must be copyable", d))
                                    .hint("ふつうは `FaultError`（errno と同じ符号）を使います", "`FaultError` (errno-style codes) is the usual choice"),
                            );
                            return Ty::Err;
                        }
                        Ty::Result(Box::new(t), Box::new(e))
                    }
                    "Str" => {
                        self.no_args(args, *span);
                        Ty::Str
                    }
                    "Option" => {
                        if args.len() != 1 {
                            self.err(Diag::error("F0107", *span, "`Option` には型引数が 1 つ必要です", "`Option` takes exactly one type argument"));
                            return Ty::Err;
                        }
                        match &args[0] {
                            GenericArg::Type(t) => {
                                let it = self.resolve_type(t, false, env);
                                if let Ty::Ref(..) = it {
                                    return Ty::Err;
                                }
                                Ty::Option(Box::new(it))
                            }
                            GenericArg::Arena(_, sp) => {
                                self.err(Diag::error("F0107", *sp, "`Option` の引数は型です", "the argument of `Option` must be a type"));
                                Ty::Err
                            }
                        }
                    }
                    "Dma" => {
                        let a = match args.first() {
                            Some(GenericArg::Arena(a, sp)) if args.len() == 1 => self.resolve_arena_arg(a, *sp, env),
                            // `Dma<req>`（`@` なし）/ without the `@`
                            Some(GenericArg::Type(TypeExpr::Path { name: n, args: ta, span: sp })) if args.len() == 1 && ta.is_empty() => self.resolve_arena_arg(n, *sp, env),
                            _ => {
                                self.err(Diag::error("F0107", *span, "`Dma` には dma_arena の名前が 1 つ必要です（`Dma<@名前>`）", "`Dma` takes one dma_arena name (`Dma<@name>`)"));
                                None
                            }
                        };
                        match a {
                            Some(x) if self.t.arenas[x].dma => Ty::Dma(x),
                            Some(x) => {
                                let n = self.t.arenas[x].name.clone();
                                self.err(Diag::error("F2703", *span, format!("`{}` は dma_arena ではありません（`Dma<..>` は dma_arena のハンドルです）", n), format!("`{}` is not a dma_arena (`Dma<..>` is the handle of a dma_arena)", n)));
                                Ty::Err
                            }
                            None => Ty::Err,
                        }
                    }
                    // gpu_arena のハンドルと引換券（文脈依存: 同じ名前の構造体があればそちら） / the handle and the ticket of a
                    // gpu_arena (contextual: a struct of the same name wins)
                    "GpuBuf" | "GpuFence" if !self.t.struct_by_name.contains_key(name.as_str()) => {
                        let a = match args.first() {
                            Some(GenericArg::Arena(a, sp)) if args.len() == 1 => self.resolve_arena_arg(a, *sp, env),
                            Some(GenericArg::Type(TypeExpr::Path { name: n, args: ta, span: sp })) if args.len() == 1 && ta.is_empty() => self.resolve_arena_arg(n, *sp, env),
                            _ => {
                                self.err(Diag::error("F0107", *span, format!("`{}` には gpu_arena の名前が 1 つ必要です（`{}<@名前>`）", name, name), format!("`{}` takes one gpu_arena name (`{}<@name>`)", name, name)));
                                None
                            }
                        };
                        match a {
                            Some(x) if self.t.arenas[x].gpu => {
                                if name == "GpuBuf" {
                                    Ty::GpuBuf(x)
                                } else {
                                    Ty::GpuFence(x)
                                }
                            }
                            Some(x) => {
                                let n = self.t.arenas[x].name.clone();
                                self.err(Diag::error("F2706", *span, format!("`{}` は gpu_arena ではありません（`{}<..>` は gpu_arena のものです）", n, name), format!("`{}` is not a gpu_arena (`{}<..>` belongs to a gpu_arena)", n, name)));
                                Ty::Err
                            }
                            None => Ty::Err,
                        }
                    }
                    "Id" => {
                        if args.len() != 1 {
                            self.err(Diag::error("F0107", *span, "`Id` にはアリーナ引数が 1 つ必要です（`Id<@名前>`）", "`Id` takes exactly one arena argument (`Id<@name>`)"));
                            return Ty::Err;
                        }
                        match &args[0] {
                            GenericArg::Arena(a, sp) => self.resolve_arena_arg(a, *sp, env).map(Ty::Id).unwrap_or(Ty::Err),
                            GenericArg::Type(t) => {
                                self.err(Diag::error(
                                    "F2104",
                                    t.span(),
                                    "`Id` の引数はアリーナの名前です（`Id<名前>`）",
                                    "the argument of `Id` must be an arena name (`Id<name>`)",
                                ));
                                Ty::Err
                            }
                        }
                    }
                    "Chan" => {
                        self.err(Diag::error(
                            "F2900",
                            *span,
                            "`Chan` は 0.1 コアの機能です。1.0 構文（arena / own / mmio）と同じファイルでは使えません",
                            "`Chan` is a 0.1-core feature and cannot be used in a file that uses 1.0 syntax (arena / own / mmio)",
                        ));
                        Ty::Err
                    }
                    _ => {
                        if let Some(&sid) = self.t.struct_by_name.get(name) {
                            self.no_args(args, *span);
                            return Ty::Struct(sid);
                        }
                        if let Some(&item) = self.alias_items.get(name) {
                            return self.instantiate_alias(item, args, *span, env);
                        }
                        self.err(Diag::error("F0412", *span, format!("型 `{}` が見つかりません", name), format!("cannot find type `{}`", name)));
                        Ty::Err
                    }
                }
            }
        }
    }

    fn no_args(&mut self, args: &[GenericArg], span: Span) {
        if !args.is_empty() {
            self.err(Diag::error("F0107", span, "この型は引数を取りません", "this type takes no arguments"));
        }
    }

    fn resolve_arena_arg(&mut self, a: &str, sp: Span, env: &HashMap<String, usize>) -> Option<usize> {
        if let Some(&x) = env.get(a) {
            return Some(x);
        }
        match self.t.regions.get(a) {
            Some(RegionRef::Arena(x)) => Some(*x),
            Some(RegionRef::Mmio(_)) | Some(RegionRef::Port(_)) | Some(RegionRef::Dma(_)) | Some(RegionRef::UserMem(_)) | Some(RegionRef::Chan(_)) => {
                self.err(Diag::error(
                    "F2104",
                    sp,
                    format!("`{}` は MMIO リージョンかポートです。`Id` はアリーナだけを指します", a),
                    format!("`{}` is an MMIO region or a port; `Id` can only refer to arenas", a),
                ));
                None
            }
            None => {
                self.err(Diag::error("F2104", sp, format!("アリーナ `{}` が見つかりません", a), format!("cannot find arena `{}`", a)));
                None
            }
        }
    }

    fn instantiate_alias(&mut self, item: usize, args: &[GenericArg], span: Span, env: &HashMap<String, usize>) -> Ty {
        let prog = self.prog;
        let a = match &prog.items[item] {
            Item::Alias(a) => a,
            _ => return Ty::Err,
        };
        if self.alias_depth > 16 {
            self.err(Diag::error("F0391", span, format!("型の別名 `{}` が循環しています", a.name), format!("type alias `{}` is cyclic", a.name)));
            return Ty::Err;
        }
        if args.len() != a.params.len() {
            self.err(Diag::error(
                "F0107",
                span,
                format!("`{}` にはアリーナ引数が {} 個必要です（{} 個あります）", a.name, a.params.len(), args.len()),
                format!("`{}` takes {} arena argument(s) but {} were given", a.name, a.params.len(), args.len()),
            ));
            return Ty::Err;
        }
        let mut new_env: HashMap<String, usize> = HashMap::new();
        for (p, arg) in a.params.iter().zip(args) {
            match arg {
                GenericArg::Arena(n, sp) => {
                    let ar = match self.resolve_arena_arg(n, *sp, env) {
                        Some(x) => x,
                        None => return Ty::Err,
                    };
                    // 種別の境界 `@a: File` / kind bound `@a: File`
                    if let Some(b) = &p.bound {
                        let bt = self.resolve_type(b, false, &HashMap::new());
                        let elem = self.t.arenas[ar].elem.clone();
                        if !bt.is_err() && !elem.is_err() && bt != elem {
                            let (bs, es) = (self.t.display(&bt), self.t.display(&elem));
                            self.err(Diag::error(
                                "F2102",
                                *sp,
                                format!("`{}<{}>`: アリーナ `{}` の要素は `{}` ですが、境界 `{}: {}` は `{}` を要求します", a.name, n, n, es, p.name, bs, bs),
                                format!("`{}<{}>`: arena `{}` stores `{}`, but the bound `{}: {}` requires `{}`", a.name, n, n, es, p.name, bs, bs),
                            ));
                            return Ty::Err;
                        }
                    }
                    new_env.insert(p.name.clone(), ar);
                }
                GenericArg::Type(t) => {
                    self.err(Diag::error("F0107", t.span(), "ここにはアリーナの名前を書きます", "an arena name is expected here"));
                    return Ty::Err;
                }
            }
        }
        self.alias_depth += 1;
        let r = self.resolve_type(&a.ty, false, &new_env);
        self.alias_depth -= 1;
        r
    }

    // ───────────── アリーナ・MMIO / arenas and MMIO ─────────────

    fn declare_arena(&mut self, a: &ArenaDef) {
        let ai = match self.t.regions.get(&a.name) {
            Some(RegionRef::Arena(x)) => *x,
            _ => return,
        };
        let elem = self.resolve_type(&a.elem, false, &HashMap::new());
        match &elem {
            Ty::Ref(..) => {
                self.err(Diag::error("F0515", a.elem.span(), "アリーナの要素に参照は置けません", "arena elements cannot be references"));
            }
            Ty::Own(_) => {
                self.err(Diag::error(
                    "F2103",
                    a.elem.span(),
                    "アリーナの要素は値型にしてください（`own T` はフィールドに持たせます）",
                    "arena elements must be value types (put `own T` inside a field instead)",
                ));
            }
            Ty::Array(..) => {
                self.err(Diag::error(
                    "F2103",
                    a.elem.span(),
                    "アリーナの要素に配列を直接は置けません（構造体のフィールドに入れてください）",
                    "an array cannot be an arena element directly (put it inside a struct field)",
                ));
            }
            Ty::Fn(..) => {
                self.err(Diag::error(
                    "F2103",
                    a.elem.span(),
                    "アリーナの要素に関数ポインタを直接は置けません（構造体のフィールドに入れてください）",
                    "a function pointer cannot be an arena element directly (put it inside a struct field)",
                ));
            }
            _ => {}
        }
        // Id = [世代 32-K | 添字 K]。K = ceil(log2(容量)) は 24 まで（世代に最低 8 ビット = 255 回を残す）
        // Id = [generation 32-K | index K]; K = ceil(log2(capacity)) is at most 24 (leaving at least 8 generation bits = 255 reuses)
        let cap = self.const_u64(&a.cap, "アリーナの容量", "the arena capacity").unwrap_or(1);
        if cap == 0 || cap > ARENA_CAP_MAX {
            self.err(
                Diag::error(
                    "F2103",
                    a.cap.span,
                    format!("アリーナの容量 {} は 1〜{} の範囲外です（Id の添字は最大 24 ビット）", cap, ARENA_CAP_MAX),
                    format!("arena capacity {} is outside 1..={} (an Id's index has at most 24 bits)", cap, ARENA_CAP_MAX),
                )
                .hint("大きな表は複数のアリーナに分けてください", "split a larger table into several arenas"),
            );
        }
        self.t.arenas[ai].elem = elem;
        self.t.arenas[ai].cap = cap.clamp(1, ARENA_CAP_MAX) as u32;
        // task_arena: スタックの大きさ（割り込みフレーム 800 バイトとハンドラの分を含むので 4 KiB 以上、16 の倍数）と容量
        // task_arena: the stack size (it also holds the 800-byte interrupt frame and the handler, so at least 4 KiB, a
        // multiple of 16) and the capacity
        if let Some(se) = &a.task_stack {
            let sz = self.const_u64(se, "スタックの大きさ", "the stack size").unwrap_or(16384);
            if sz < 4096 || sz > (1 << 20) || sz % 16 != 0 {
                self.err(Diag::error(
                    "F2605",
                    se.span,
                    format!("task_arena のスタック {} バイトは使えません（4096〜1048576 の 16 の倍数。割り込みフレーム 800 バイトとハンドラの分を含みます）", sz),
                    format!("a task_arena stack of {} bytes is not usable (a multiple of 16 from 4096 to 1048576; it also holds the 800-byte interrupt frame and the handler)", sz),
                ));
            }
            if cap > TASK_CAP_MAX {
                self.err(Diag::error(
                    "F2605",
                    a.cap.span,
                    format!("task_arena の容量 {} は 1〜{} の範囲外です", cap, TASK_CAP_MAX),
                    format!("task_arena capacity {} is outside 1..={}", cap, TASK_CAP_MAX),
                ));
            }
            if self.t.arenas.iter().enumerate().any(|(i, x)| i != ai && x.task_stack.is_some()) {
                self.err(Diag::error("F2605", a.name_span, "task_arena は 1 つのプログラムに 1 つです", "a program has at most one task_arena"));
            }
            self.t.arenas[ai].task_stack = Some(sz.clamp(4096, 1 << 20) as u32);
        }
    }

    fn declare_mmio(&mut self, m: &MmioDef) {
        let mi = match self.t.regions.get(&m.name) {
            Some(RegionRef::Mmio(x)) => *x,
            _ => return,
        };
        let ty = self.resolve_type(&m.ty, false, &HashMap::new());
        let mut dims = Vec::new();
        let mut cur = ty.clone();
        let elem = loop {
            match cur {
                Ty::Array(e, n) => {
                    dims.push(n);
                    cur = *e;
                }
                Ty::Int(it) => break Some(it),
                Ty::Err => break None,
                _ => {
                    self.err(Diag::error(
                        "F2202",
                        m.ty.span(),
                        "MMIO リージョンの型は整数の配列（`[u32; N]` や `[[u32; W]; H]`）です",
                        "the type of an MMIO region must be an array of integers (`[u32; N]` or `[[u32; W]; H]`)",
                    ));
                    break None;
                }
            }
        };
        let elem = match elem {
            Some(e) => e,
            None => return,
        };
        if dims.is_empty() {
            self.err(Diag::error("F2202", m.ty.span(), "MMIO リージョンは配列型にしてください", "an MMIO region must have an array type"));
            return;
        }
        if dims.len() > 2 {
            self.err(Diag::error("F2202", m.ty.span(), "MMIO リージョンは 1 次元か 2 次元です", "MMIO regions are one- or two-dimensional"));
            return;
        }
        if matches!(elem, IntTy::Isize | IntTy::Usize) {
            self.err(Diag::error("F2202", m.ty.span(), "MMIO の要素は幅の決まった整数（u8〜u64）にしてください", "MMIO elements must be fixed-width integers (u8 to u64)"));
            return;
        }
        let addr = self.const_u64(&m.addr, "MMIO の番地", "an MMIO address").unwrap_or(0);
        let info = MmioInfo { name: m.name.clone(), elem, dims, addr, framebuffer: m.attrs.iter().any(|a| a.name == "framebuffer"), span: m.span };
        let eb = info.elem_bytes();
        if addr % eb != 0 {
            self.err(Diag::error(
                "F2202",
                m.addr.span,
                format!("番地 {:#x} は要素の大きさ {} バイトに整列していません", addr, eb),
                format!("address {:#x} is not aligned to the element size ({} bytes)", addr, eb),
            ));
        }
        let size = info.size_bytes() as u128;
        let limit: u128 = if self.t.ptr_bits == 32 { 1u128 << 32 } else { 1u128 << 64 };
        if addr as u128 + size > limit || addr == 0 {
            self.err(Diag::error(
                "F2202",
                m.addr.span,
                format!("リージョン [{:#x}, +{:#x}) がアドレス空間に収まらないか、番地 0 です", addr, size),
                format!("the region [{:#x}, +{:#x}) does not fit the address space, or its address is 0", addr, size),
            ));
        }
        for a in &m.attrs {
            if a.name != "framebuffer" {
                self.err(Diag::warning("F2202", a.span, format!("属性 `{}` は無視されます", a.name), format!("attribute `{}` is ignored", a.name)));
            }
        }
        self.t.mmios[mi] = info;
    }

    fn check_mmio_overlap(&mut self) {
        let n = self.t.mmios.len();
        for i in 0..n {
            for j in (i + 1)..n {
                let (a, b) = (&self.t.mmios[i], &self.t.mmios[j]);
                let (a0, a1) = (a.addr as u128, a.addr as u128 + a.size_bytes() as u128);
                let (b0, b1) = (b.addr as u128, b.addr as u128 + b.size_bytes() as u128);
                if a0 < b1 && b0 < a1 && a1 > a0 && b1 > b0 {
                    let (sp, an, bn) = (b.span, a.name.clone(), b.name.clone());
                    self.err(Diag::error(
                        "F2203",
                        sp,
                        format!("MMIO リージョン `{}` と `{}` の番地範囲が重なっています（同じ物理メモリを 2 つの名前で書けてしまう）", bn, an),
                        format!("MMIO regions `{}` and `{}` overlap (the same physical memory would be writable under two names)", bn, an),
                    ));
                }
            }
        }
    }

    // ───────────── I/O ポート / I/O ports ─────────────

    fn declare_port(&mut self, pd: &PortDef) {
        let pi = match self.t.regions.get(&pd.name) {
            Some(RegionRef::Port(x)) => *x,
            _ => return,
        };
        let (elem, len) = match &pd.ty {
            None => (IntTy::U8, None),
            Some(te) => match self.resolve_type(te, false, &HashMap::new()) {
                Ty::Int(it) if matches!(it, IntTy::U8 | IntTy::U16 | IntTy::U32) => (it, None),
                Ty::Array(e, n) => match *e {
                    Ty::Int(it) if matches!(it, IntTy::U8 | IntTy::U16 | IntTy::U32) => (it, Some(n)),
                    _ => {
                        self.err(Diag::error("F2205", te.span(), "ポートの要素は u8 / u16 / u32 です", "port elements are u8 / u16 / u32"));
                        return;
                    }
                },
                Ty::Err => return,
                _ => {
                    self.err(Diag::error(
                        "F2205",
                        te.span(),
                        "ポートの型は u8 / u16 / u32 か、その配列 `[u8; N]` です（x86 の in / out は 8・16・32 ビット）",
                        "a port has type u8 / u16 / u32 or an array `[u8; N]` of them (x86 in / out are 8, 16 or 32 bits)",
                    ));
                    return;
                }
            },
        };
        let port = self.const_u64(&pd.port, "ポート番号", "a port number").unwrap_or(0);
        let wb = (elem.bits(64) / 8) as u64;
        let end = port + len.unwrap_or(1) * wb;
        if end > 0x1_0000 {
            self.err(Diag::error(
                "F2205",
                pd.port.span,
                format!("ポート [{:#x}, {:#x}) は x86 の I/O 空間（0〜0xffff）に収まりません", port, end),
                format!("ports [{:#x}, {:#x}) do not fit the x86 I/O space (0 to 0xffff)", port, end),
            ));
        }
        let mut mmio_map = None;
        for a in &pd.attrs {
            match (a.name.as_str(), a.int_arg) {
                ("mmio_map", Some((v, _))) => mmio_map = Some(v as u64),
                _ => self.err(Diag::warning(
                    "F2205",
                    a.span,
                    format!("属性 `{}` は無視されます（ポートに使えるのは `#[mmio_map(番地)]`）", a.name),
                    format!("attribute `{}` is ignored (ports accept `#[mmio_map(address)]`)", a.name),
                )),
            }
        }
        self.t.ports[pi] = PortInfo { name: pd.name.clone(), elem, len, port, mmio_map, span: pd.span };
    }

    fn check_port_overlap(&mut self) {
        let n = self.t.ports.len();
        for i in 0..n {
            for j in (i + 1)..n {
                let (a, b) = (&self.t.ports[i], &self.t.ports[j]);
                let (a0, a1) = (a.port, a.port + a.len.unwrap_or(1) * a.width_bytes());
                let (b0, b1) = (b.port, b.port + b.len.unwrap_or(1) * b.width_bytes());
                if a0 < b1 && b0 < a1 {
                    let (sp, an, bn) = (b.span, a.name.clone(), b.name.clone());
                    self.err(Diag::error(
                        "F2203",
                        sp,
                        format!("ポート `{}` と `{}` の範囲が重なっています（同じレジスタを 2 つの名前で書けてしまう）", bn, an),
                        format!("ports `{}` and `{}` overlap (the same register would be writable under two names)", bn, an),
                    ));
                }
            }
        }
    }

    // ───────────── バンプアリーナの規則 / rules for bump arenas ─────────────

    /// バンプアリーナの Id は呼び出しリージョンが戻ると無効になるので、それより長く生きる場所（表のアリーナの要素・static・
    /// 別のバンプアリーナの要素）には置けない（F2402）。ローカル・引数・戻り値・own の木・自分のスロットには置ける（戻り値と
    /// &mut 引数に乗る Id の確保は呼び出し元のリージョンへ昇格する。どこに置いても実行時は世代で検査する）。
    /// 要素は own を持てない（一括解放は解放処理を走らせない: F2103）
    /// Ids of a bump arena die when their call region returns, so they cannot be stored where they would outlive it (elements
    /// of table arenas, statics, elements of another bump arena): F2402. Locals, parameters, return values, own trees and the
    /// arena's own slots are fine (allocations whose Ids travel through the return value or a &mut parameter are promoted to
    /// the caller's region; wherever they are kept, they are checked by generation at run time). Elements cannot hold own
    /// (bulk release runs no drop glue): F2103
    fn check_bump_rules(&mut self) {
        let prog = self.prog;
        for item in &prog.items {
            let Item::Arena(a) = item else { continue };
            let Some(ai) = self.t.arena_of_name(&a.name) else { continue };
            let elem = self.t.arenas[ai].elem.clone();
            if a.bump && self.t.needs_drop(&elem) {
                self.err(
                    Diag::error(
                        "F2103",
                        a.elem.span(),
                        format!("バンプアリーナ `{}` の要素は own を持てません（呼び出しリージョンの一括解放は own の解放処理を走らせない）", a.name),
                        format!("elements of the bump arena `{}` cannot hold own (the bulk release of a call region runs no drop glue)", a.name),
                    )
                    .hint("own を持つ値は `arena`（個別に free）か own の木に置いてください", "keep values with own in an `arena` (freed one by one) or in an own tree"),
                );
            }
            // 長く生きる表（ふつうのアリーナ・別のバンプアリーナ）の要素にはバンプの Id を置けない。own の箱の中も辿る
            // elements of long-lived tables (plain arenas, other bump arenas) cannot hold bump Ids, also inside own boxes
            let mut ids = Vec::new();
            self.t.bump_ids_in(&elem, &mut ids);
            if a.bump {
                ids.retain(|x| *x != ai);
            }
            if let Some(&b) = ids.first() {
                let bn = self.t.arenas[b].name.clone();
                self.err(
                    Diag::error(
                        "F2402",
                        a.elem.span(),
                        format!("`{}` の要素はバンプアリーナ `{}` の Id を含みますが、`{}` の Id は呼び出しリージョンが戻ると無効になります", a.name, bn, bn),
                        format!("elements of `{}` contain Ids of the bump arena `{}`, which die when their call region returns", a.name, bn),
                    )
                    .hint(format!("長く保存する相手は `arena {}: ..` にしてください", bn), format!("make `{}` an `arena` when its Ids are stored for long", bn)),
                );
            }
        }
    }

    /// 呼び出しリージョン: 関数 f がバンプアリーナ B を（直接、または B の Id を返す関数を通して）確保し、f 自身は B の Id を
    /// 返さないなら、f の出口で B の先頭を巻き戻す（1 回の格納 = O(1) の一括解放）。B の Id を返す関数の確保は呼び出し元の
    /// リージョンへ昇格する（エスケープする値は親リージョンへ）
    /// call regions: when f allocates from bump arena B (directly, or through functions returning B's Ids) and f itself does
    /// not return B's Ids, f rewinds B's top at its exits (one store = an O(1) bulk free). Allocations of functions returning
    /// B's Ids are promoted to the caller's region (escaping values go to the parent region)
    fn compute_call_regions(&mut self) {
        let n = self.t.fns.len();
        let prog = self.prog;
        let mut direct: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut callees: Vec<Vec<usize>> = vec![Vec::new(); n];
        for fi in 0..n {
            if let Item::Fn(fd) = &prog.items[self.t.fns[fi].item] {
                let mut d = Vec::new();
                let mut c = Vec::new();
                walk_block(&fd.body, &mut |e: &Expr| {
                    match &e.kind {
                        ExprKind::MethodCall { .. } => {
                            if let Some(MethodKind::ArenaAlloc(a)) = self.methods.get(&e.id) {
                                if self.t.arenas[*a].bump && !d.contains(a) {
                                    d.push(*a);
                                }
                            }
                        }
                        ExprKind::Call { .. } | ExprKind::CallIndirect { .. } => {
                            match self.calls.get(&e.id) {
                                Some(CallKind::User(g)) => {
                                    if !c.contains(g) {
                                        c.push(*g);
                                    }
                                }
                                // 関数ポインタを通した呼び出し: 呼ばれうる関数すべて / a call through a function pointer: every
                                // function it may reach
                                Some(CallKind::Indirect) => {
                                    for g in self.indirect_cands.get(&e.id).cloned().unwrap_or_default() {
                                        if !c.contains(&g) {
                                            c.push(g);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                });
                direct[fi] = d;
                callees[fi] = c;
            }
        }
        // 外へ出る道: 戻り値と &mut 引数（呼び出し元の場所へ書ける）。そこにバンプの Id が乗るなら、その確保は呼び出し元へ昇格
        // the ways out: the return value and &mut parameters (they write into the caller's places); allocations whose Ids
        // can travel that way are promoted to the caller
        let escapes = |s: &Sema, f: usize| -> Vec<usize> {
            let mut v = Vec::new();
            s.t.bump_ids_in(&s.t.fns[f].ret, &mut v);
            for p in &s.t.fns[f].params {
                if let Ty::Ref(true, inner) = &p.ty {
                    s.t.bump_ids_in(inner, &mut v);
                }
            }
            v
        };
        let esc: Vec<Vec<usize>> = (0..n).map(|f| escapes(self, f)).collect();
        let mut pending = direct.clone();
        loop {
            let mut changed = false;
            for f in 0..n {
                for &g in &callees[f].clone() {
                    for b in pending[g].clone() {
                        if esc[g].contains(&b) && !pending[f].contains(&b) {
                            pending[f].push(b);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        self.call_regions = (0..n).map(|f| pending[f].iter().copied().filter(|b| !esc[f].contains(b)).collect()).collect();
    }

    // ───────────── static ─────────────

    fn check_static(&mut self, s: &StaticDef) {
        let si = match self.t.static_by_name.get(&s.name) {
            Some(&x) => x,
            None => return,
        };
        let ty = self.resolve_type(&s.ty, false, &HashMap::new());
        if self.t.needs_drop(&ty) {
            self.err(Diag::error(
                "F2302",
                s.ty.span(),
                "static（Kernel リージョン・不変）には own を含む型を置けません",
                "a static (Kernel region, immutable) cannot hold a type that contains own",
            ));
        }
        let mut ids = Vec::new();
        self.t.bump_ids_in(&ty, &mut ids);
        if let Some(&b) = ids.first() {
            let bn = self.t.arenas[b].name.clone();
            self.err(Diag::error(
                "F2402",
                s.ty.span(),
                format!("static はプログラムの最後まで生きるので、バンプアリーナ `{}` の Id を置けません", bn),
                format!("a static lives until the program ends, so it cannot hold Ids of the bump arena `{}`", bn),
            ));
        }
        self.t.statics[si].ty = ty.clone();
        self.scopes.clear();
        self.cur_ret = Ty::Unit;
        // `static X: [u8; N] = embed("ファイル");`: ファイルの中身を埋め込む（足りない分は 0。相対パスはソースのフォルダから）
        // embed the bytes of a file (zero-padded; relative paths start at the folder of the source file)
        if let ExprKind::Call { callee, args, callee_span, .. } = &s.value.kind {
            if callee == "embed" && !self.t.fn_by_name.contains_key("embed") {
                let n = match &ty {
                    Ty::Array(el, n) if **el == Ty::Int(IntTy::U8) => Some(*n),
                    _ => None,
                };
                let file = match args.first().map(|a| &a.kind) {
                    Some(ExprKind::Str(p)) if args.len() == 1 => Some(p.clone()),
                    _ => None,
                };
                self.expr_ty[s.value.id as usize] = ty.clone();
                for a in args {
                    self.expr_ty[a.id as usize] = Ty::Str;
                }
                match (n, file) {
                    (Some(n), Some(file)) => {
                        let path = match &self.base_dir {
                            Some(d) if !std::path::Path::new(&file).is_absolute() => d.join(&file),
                            _ => std::path::PathBuf::from(&file),
                        };
                        match std::fs::read(&path) {
                            Ok(bytes) if bytes.len() as u64 <= n => {
                                self.embeds.insert(si, bytes);
                            }
                            Ok(bytes) => {
                                self.err(
                                    Diag::error("F2804", s.value.span, format!("`{}` は {} バイトで、`[u8; {}]` に収まりません", file, bytes.len(), n), format!("`{}` is {} bytes and does not fit in `[u8; {}]`", file, bytes.len(), n))
                                        .hint(format!("配列を `[u8; {}]` 以上にしてください", bytes.len().next_power_of_two()), format!("make the array at least `[u8; {}]`", bytes.len().next_power_of_two())),
                                );
                            }
                            Err(_) => {
                                self.err(
                                    Diag::error("F2804", s.value.span, format!("埋め込むファイル `{}` が読めません", path.display()), format!("cannot read the file to embed, `{}`", path.display()))
                                        .hint("ユーザープログラムの像は `fascon build --user app.fsc --abi kernel.fsc` で作ります（相対パスはこのソースのフォルダから）", "build a user program image with `fascon build --user app.fsc --abi kernel.fsc` (relative paths start at this source file's folder)"),
                                );
                            }
                        }
                    }
                    _ => {
                        self.err(
                            Diag::error("F2804", *callee_span, "`embed` は `static 名前: [u8; N] = embed(\"ファイル名\");` の形でだけ使えます", "`embed` is only used as `static NAME: [u8; N] = embed(\"file name\");`")
                                .hint("型は `[u8; N]`、引数は文字列リテラル 1 つです", "the type is `[u8; N]` and the argument is one string literal"),
                        );
                    }
                }
                self.finish_types();
                return;
            }
        }
        let vt = self.check_expr(&s.value, Some(&ty));
        self.expect_ty(s.value.span, &ty, &vt);
        if !is_const_expr(&s.value, &self.t) {
            self.err(Diag::error(
                "F0015",
                s.value.span,
                "static の初期値はコンパイル時の定数（リテラル・構造体・配列・整数 static の式）にしてください",
                "a static initializer must be a compile-time constant (literals, structs, arrays, integer-static expressions)",
            ));
        }
        self.finish_types();
    }

    // ───────────── 関数 / functions ─────────────

    fn declare_fn(&mut self, f: &FnDef) {
        let fi = match self.t.fn_by_name.get(&f.name) {
            Some(&x) => x,
            None => return,
        };
        if self.t.fns[fi].item != self.fn_item_index(f) {
            return;
        }
        if f.is_async {
            self.err(Diag::error(
                "F2900",
                f.span,
                "`async fn` は 0.1 コアの機能です。1.0 構文と同じファイルでは使えません",
                "`async fn` is a 0.1-core feature and cannot be used in a file that uses 1.0 syntax",
            ));
        }
        for a in &f.attrs {
            if a.name == "isr" {
                self.err(Diag::error(
                    "F2900",
                    a.span,
                    "`#[isr]` は 0.1 コアの機能です。1.0 エンジン（arena / own / mmio を使うファイル）ではまだ使えません",
                    "`#[isr]` is a 0.1-core feature and is not yet available in the 1.0 engine (files using arena / own / mmio)",
                ));
            } else if matches!(a.name.as_str(), "syscall" | "trap" | "asm" | "sleep" | "inline" | "cold" | "fast_math" | "allow" | "no_smt") {
                // ユニバーサル OS 拡張の関数の属性（種類は構文解析で決まっている） / function attributes of the universal-OS
                // extension (the kind was decided by the parser)
            } else {
                self.err(Diag::warning("F0001", a.span, format!("属性 `{}` は無視されます", a.name), format!("attribute `{}` is ignored", a.name)));
            }
        }
        let mut params = Vec::new();
        let mut names = HashSet::new();
        for p in &f.params {
            let ty = self.resolve_type(&p.ty, true, &HashMap::new());
            if let Ty::Array(..) = ty {
                self.err(
                    Diag::error("F2040", p.ty.span(), "配列は値渡しできません", "arrays cannot be passed by value")
                        .hint("`&[T; N]` か `&mut [T; N]` で渡してください", "pass `&[T; N]` or `&mut [T; N]` instead"),
                );
            }
            if !names.insert(p.name.clone()) {
                self.err(Diag::error("F0415", p.span, format!("引数 `{}` が重複しています", p.name), format!("parameter `{}` is declared twice", p.name)));
            }
            // `user T` は `#[syscall(N)] fn` の引数だけ（1.0 の `syscall .. at N` は F3103 で別に報告する）
            // `user T` is only a parameter of `#[syscall(N)] fn` (the 1.0 `syscall .. at N` form is reported as F3103)
            if let Ty::User(_) = ty {
                if !f.attrs.iter().any(|a| a.name == "syscall") && !matches!(f.kind, FnKind::Syscall(_)) {
                    self.err(
                        Diag::error("F3102", p.ty.span(), "`user T` は `#[syscall(N)]` の関数の引数にだけ書けます（保存できないユーザー空間の番地）", "`user T` can only be a parameter of a `#[syscall(N)]` function (a user-space address that cannot be stored)")
                            .hint("中身は `copy_from_user(ubuf)?` でカーネルの値へ 1 回だけ写して使います", "copy the contents once into a kernel value with `copy_from_user(ubuf)?`"),
                    );
                }
            }
            params.push(ParamInfo { name: p.name.clone(), bind: p.bind, ty, mutable: p.mutable, span: p.span });
        }
        let ret = match &f.ret {
            Some(t) => {
                let r = self.resolve_type(t, false, &HashMap::new());
                if let Ty::Array(..) = r {
                    self.err(
                        Diag::error("F2040", t.span(), "配列は戻り値にできません", "arrays cannot be returned by value")
                            .hint("配列を構造体のフィールドに入れて返すか、`&mut [T; N]` 引数に書き込んでください", "wrap the array in a struct, or write into a `&mut [T; N]` parameter"),
                    );
                }
                r
            }
            None => Ty::Unit,
        };
        self.t.fns[fi].params = params;
        self.t.fns[fi].ret = ret;
        self.check_boundary_fn(f, fi);
        // `irq 名前 at INTID { .. }`: INTID は 16〜1019 の定数で、重ならない（F2602）
        // `irq name at INTID { .. }`: the INTID is a constant from 16 to 1019 and distinct (F2602)
        if let FnKind::Irq(ie) = &f.kind {
            let n = self.const_u64(ie, "割り込み番号（INTID）", "the interrupt number (INTID)").unwrap_or(16);
            if !(16..=1019).contains(&n) {
                self.err(Diag::error(
                    "F2602",
                    ie.span,
                    format!("INTID {} は 16〜1019 の範囲外です（16〜31 は PPI、32 以上は SPI）", n),
                    format!("INTID {} is outside 16..=1019 (16 to 31 are PPIs, 32 and above SPIs)", n),
                ));
            } else if let Some(other) = self.t.fns.iter().find(|g| g.irq == Some(n as u32)) {
                let on = other.name.clone();
                self.err(Diag::error(
                    "F2602",
                    ie.span,
                    format!("INTID {} のハンドラは `{}` がすでにあります", n, on),
                    format!("INTID {} already has the handler `{}`", n, on),
                ));
            }
            self.t.fns[fi].irq = Some(n.clamp(16, 1019) as u32);
        }
    }

    /// どんなビット列でも正しい値になる型か（整数・その配列・ワイヤ構造体） / whether every bit pattern is a valid value
    /// レジスタ構造体か（フィールドが u8〜u64 の整数・その配列・レジスタ構造体だけ） / whether a struct is a register struct
    /// (u8-u64 integer fields, arrays of them, nested register structs)
    pub fn is_reg_struct(&self, sid: usize, depth: u32) -> bool {
        if depth > 16 {
            return false;
        }
        fn reg_ty(s: &Sema, t: &Ty, depth: u32) -> bool {
            match t {
                Ty::Int(i) => !matches!(i, IntTy::Isize | IntTy::Usize),
                Ty::Array(e, _) => reg_ty(s, e, depth),
                Ty::Struct(x) => s.is_reg_struct(*x, depth + 1),
                _ => false,
            }
        }
        !self.t.structs[sid].fields.is_empty() && self.t.structs[sid].fields.iter().all(|f| reg_ty(self, &f.ty, depth))
    }

    pub fn is_pod(&self, t: &Ty) -> bool {
        match t {
            Ty::Int(_) => true,
            Ty::Array(e, _) => self.is_pod(e),
            Ty::Struct(s) => self.t.structs[*s].wire.is_some(),
            _ => false,
        }
    }

    /// gpu_arena の要素に置ける型（F2705）: 整数・浮動小数点数と、その配列・構造体（ワイヤ構造体でなくてよい）。装置が書いた
    /// どんなビット列でも正しい値になり、ほかの表やヒープを指さない型だけ（own・Id・Str・参照・関数ポインタ・bool・Option は不可）
    /// types allowed as gpu_arena elements (F2705): integers, floats and arrays / structs of them (not necessarily wire
    /// structs); only types for which any bits a device writes are a valid value and that point at no other table or heap
    /// (own, Id, Str, references, function pointers, bool and Option are not)
    pub fn is_gpu_pod(&self, t: &Ty) -> bool {
        self.is_gpu_pod_d(t, 0)
    }
    fn is_gpu_pod_d(&self, t: &Ty, d: u32) -> bool {
        d < 32
            && match t {
                Ty::Int(_) | Ty::Float(_) => true,
                Ty::Array(e, _) => self.is_gpu_pod_d(e, d + 1),
                Ty::Struct(s) => self.t.structs[*s].fields.iter().all(|f| self.is_gpu_pod_d(&f.ty, d + 1)),
                _ => false,
            }
    }

    /// `Dma<..>` を（推移的に）含む型か / whether a type contains a `Dma<..>` (transitively)
    pub fn has_dma(&self, t: &Ty) -> bool {
        match t {
            Ty::Dma(_) | Ty::GpuBuf(_) => true,
            Ty::Option(i) | Ty::Own(i) | Ty::Array(i, _) => self.has_dma(i),
            Ty::Struct(s) => self.t.structs[*s].fields.iter().any(|f| self.has_dma(&f.ty)),
            _ => false,
        }
    }

    /// ユーザープログラム（EL0）では使えない項目（F2803） / items not available to user programs (EL0)
    fn user_ban(&mut self, sp: Span, what: &str) {
        self.err(
            Diag::error("F2803", sp, format!("`{}` はユーザープログラム（EL0）では使えません", what), format!("`{}` is not available in a user program (EL0)", what))
                .hint("装置・割り込み・タスクの切り替えはカーネルの仕事です。カーネルの `syscall` を通して頼んでください", "devices, interrupts and task switching belong to the kernel; ask for them through a kernel `syscall`"),
        );
    }

    /// `user_mem @u for @t: バイト数 at 仮想番地;`（F2803）: 4 KiB の倍数、恒等マップ（0〜2 GiB）の外、1 GiB の区画の中。
    /// ユーザーメモリはタスクごとのアドレス空間（ASID）に 4 KiB のページで要求時に写す
    /// (F2803): multiples of 4 KiB, outside the identity map (0 to 2 GiB), inside one 1 GiB slot. User memory is mapped
    /// on demand with 4 KiB pages into per-task address spaces (ASIDs)
    fn declare_user_mem(&mut self, u: &UserMemDef) {
        let Some(RegionRef::UserMem(ui)) = self.t.regions.get(&u.name).copied() else { return };
        if self.user_mode {
            self.user_ban(u.span, "user_mem");
        }
        if ui > 0 {
            self.err(Diag::error("F2803", u.span, "`user_mem` は 1 つのプログラムに 1 つだけです", "a program can have only one `user_mem`"));
        }
        match self.t.regions.get(&u.task).copied() {
            Some(RegionRef::Arena(a)) if self.t.arenas[a].task_stack.is_some() => self.t.umems[ui].task = a,
            _ => {
                self.err(
                    Diag::error("F2803", u.task_span, format!("`{}` は task_arena ではありません（ユーザーメモリはタスクごとに 1 つ付きます）", u.task), format!("`{}` is not a task_arena (user memory is attached to each task)", u.task))
                        .hint("`task_arena task: Task[8] stack 16384;` のように宣言したタスク表の名前を書いてください", "name a task arena declared like `task_arena task: Task[8] stack 16384;`"),
                );
            }
        }
        const PAGE: u64 = 0x1000;
        let size = self.const_u64(&u.size, "ユーザーメモリの大きさ", "the user memory size").unwrap_or(0x20_0000);
        let at = self.const_u64(&u.at, "ユーザーメモリの仮想番地", "the user memory virtual address").unwrap_or(0x1_0000_0000);
        if size == 0 || size % PAGE != 0 || size > 0x4000_0000 {
            self.err(Diag::error(
                "F2803",
                u.size.span,
                format!("ユーザーメモリの大きさ {:#x} は 4 KiB（0x1000）の倍数で、1 GiB 以下でなければなりません", size),
                format!("the user memory size {:#x} must be a multiple of 4 KiB (0x1000) and at most 1 GiB", size),
            ));
        } else if at % PAGE != 0 || at < 0x8000_0000 || at >= (1u64 << 39) || (at >> 30) != ((at + size - 1) >> 30) {
            self.err(
                Diag::error(
                    "F2803",
                    u.at.span,
                    format!("ユーザーメモリの仮想番地 {:#x} が使えません", at),
                    format!("the user memory virtual address {:#x} cannot be used", at),
                )
                .hint(
                    "4 KiB の倍数で、カーネルの恒等マップ（0〜0x8000_0000）の外、0x80_0000_0000 未満、全体が 1 GiB の区画 1 つに収まる番地にしてください（例: 0x1_0000_0000）",
                    "use a multiple of 4 KiB outside the kernel identity map (0 to 0x8000_0000), below 0x80_0000_0000, with the whole range inside one 1 GiB slot (e.g. 0x1_0000_0000)",
                ),
            );
        } else {
            self.t.umems[ui].size = size;
            self.t.umems[ui].at = at;
        }
    }

    /// システムコールの引数が使うレジスタの数。境界を渡れない型なら None（F2801）
    /// the number of registers a system call parameter takes; None for types that cannot cross the boundary (F2801)
    pub fn syscall_regs(&self, t: &Ty) -> Option<u32> {
        match t {
            Ty::Int(_) => Some(1),
            // 属性の形 `#[syscall(N)]` だけ: bool（0 / 1 を検査）・Id（validate を通す）・`user [u8; N]`（番地）・`user Str`（番地と長さ）
            // the attribute form only: bool (0 / 1 checked), Id (validated), `user [u8; N]` (an address), `user Str` (address + length)
            Ty::Bool | Ty::Id(_) => Some(1),
            Ty::User(i) => Some(if matches!(**i, Ty::Str) { 2 } else { 1 }),
            Ty::Ref(_, inner) => match &**inner {
                Ty::Slice(e) if **e == Ty::Int(IntTy::U8) => Some(2),
                Ty::Struct(s) if self.t.structs[*s].wire.is_some() => Some(1),
                _ => None,
            },
            _ => None,
        }
    }

    /// `syscall` / `trap` の形の検査（F2801・F2802・F2803） / the shape of `syscall` and `trap` items
    fn check_boundary_fn(&mut self, f: &FnDef, fi: usize) {
        match &f.kind {
            FnKind::Syscall(nr) | FnKind::SyscallStub(nr) => {
                let stub = matches!(f.kind, FnKind::SyscallStub(_));
                if self.user_mode && !stub {
                    self.err(
                        Diag::error("F2803", f.name_span, format!("本体のある `syscall {}` はカーネルの項目です。ユーザープログラム（EL0）には書けません", f.name), format!("`syscall {}` with a body is a kernel item and cannot appear in a user program (EL0)", f.name))
                            .hint("ユーザー側では本体の無い宣言（`syscall 名前 at 番号 (引数) -> i64;`）だけを書きます", "a user program only declares it without a body (`syscall name at number (params) -> i64;`)"),
                    );
                }
                let n = self.const_u64(nr, "システムコールの番号", "the system call number").unwrap_or(0);
                if n > 255 {
                    self.err(Diag::error("F2802", nr.span, format!("システムコールの番号 {} は 0〜255 の範囲外です", n), format!("the system call number {} is outside 0..=255", n)));
                } else if let Some(other) = self.t.fns.iter().find(|g| g.syscall == Some(n as u32)) {
                    let on = other.name.clone();
                    self.err(Diag::error("F2802", nr.span, format!("番号 {} のシステムコールは `{}` がすでにあります", n, on), format!("system call number {} is already taken by `{}`", n, on)));
                }
                self.t.fns[fi].syscall = Some(n.min(255) as u32);
                let ret = self.t.fns[fi].ret.clone();
                let v2 = self.t.fns[fi].syscall_v2;
                // 属性の形: `-> Result<T, FaultError>`（T は整数か ()）。Ok(v) は x0 = v、Err(e) は x0 = -e.code()
                // the attribute form: `-> Result<T, FaultError>` (T an integer or ()); Ok(v) gives x0 = v, Err(e) x0 = -e.code()
                let v2_ok = match &ret {
                    Ty::Result(t, e) => matches!(**e, Ty::Fault) && matches!(**t, Ty::Int(_) | Ty::Unit),
                    _ => false,
                };
                if v2 && !v2_ok && ret != Ty::Int(IntTy::I64) && !ret.is_err() {
                    let sp = f.ret.as_ref().map_or(f.name_span, |t| t.span());
                    self.err(
                        Diag::error("F3104", sp, "`#[syscall(N)]` の関数は `Result<usize, FaultError>`（成功値は整数か ()）か `i64` を返します", "a `#[syscall(N)]` function returns `Result<usize, FaultError>` (an integer or () on success) or `i64`")
                            .hint("成功は `Ok(n)`、失敗は `Err(EINVAL)` のように書きます（x0 には n か -22 が入る）", "write `Ok(n)` for success and `Err(EINVAL)` for failure (x0 receives n or -22)"),
                    );
                } else if !v2 && ret != Ty::Int(IntTy::I64) && !ret.is_err() {
                    let sp = f.ret.as_ref().map_or(f.name_span, |t| t.span());
                    self.err(
                        Diag::error("F2801", sp, "システムコールの戻り値は `i64` です（x0 で返る）", "a system call returns `i64` (in x0)")
                            .hint("`-> i64` と書き、失敗は負の数で返してください（例: -9）", "write `-> i64` and report failures as negative numbers (e.g. -9)"),
                    );
                }
                let mut regs = 0u32;
                let params = self.t.fns[fi].params.clone();
                for p in &params {
                    if !v2 && matches!(p.ty, Ty::Id(_)) {
                        // 今までの形の Id は F2801（u32 で受けて validate を通す） / an Id in the older form stays F2801
                        let d = self.t.display(&p.ty);
                        self.err(
                            Diag::error("F2801", p.span, format!("システムコールの引数 `{}: {}` は EL0 との境界を渡れません", p.name, d), format!("the system call parameter `{}: {}` cannot cross the EL0 boundary", p.name, d))
                                .hint("Id は `u32` で受けて `validate` を通すか、属性の形 `#[syscall(N)] fn` で受けてください（入口で自動的に validate される）", "take the Id as `u32` and pass it through `validate`, or use the attribute form `#[syscall(N)] fn` (validated automatically at entry)"),
                        );
                        continue;
                    }
                    if !v2 && matches!(p.ty, Ty::Bool | Ty::User(_)) {
                        let d = self.t.display(&p.ty);
                        self.err(
                            Diag::error("F3103", p.span, format!("引数 `{}: {}` は属性の形 `#[syscall(N)] fn` だけで受けられます", p.name, d), format!("the parameter `{}: {}` is accepted only by the attribute form `#[syscall(N)] fn`", p.name, d))
                                .hint("`#[syscall(N)] fn 名前(..) -> Result<usize, FaultError> { .. }` と書いてください（レジスタの値は入口で型ごとに検証される）", "write `#[syscall(N)] fn name(..) -> Result<usize, FaultError> { .. }` (register values are validated per type at entry)"),
                        );
                        continue;
                    }
                    match self.syscall_regs(&p.ty) {
                        Some(r) => regs += r,
                        None if p.ty.is_err() => {}
                        None => {
                            let d = self.t.display(&p.ty);
                            self.err(
                                Diag::error(
                                    "F2801",
                                    p.span,
                                    format!("システムコールの引数 `{}: {}` は EL0 との境界を渡れません", p.name, d),
                                    format!("the system call parameter `{}: {}` cannot cross the EL0 boundary", p.name, d),
                                )
                                .hint(
                                    "渡せるのは整数、`&[u8]` / `&mut [u8]`（番地と長さ。入口で検証される）、ワイヤ構造体への参照だけです。Id は `u32` で受けて `validate` を通してください",
                                    "only integers, `&[u8]` / `&mut [u8]` (an address and a length, validated at entry) and references to wire structs can cross; take an Id as `u32` and pass it through `validate`",
                                ),
                            );
                        }
                    }
                }
                if regs > 8 {
                    self.err(Diag::error(
                        "F2802",
                        f.name_span,
                        format!("システムコール `{}` の引数はレジスタ {} 本ぶんです（x0〜x7 の 8 本まで。窓は 2 本使います）", f.name, regs),
                        format!("the parameters of system call `{}` take {} registers (at most 8, x0 to x7; a window takes 2)", f.name, regs),
                    ));
                }
            }
            FnKind::Trap => {
                if self.user_mode {
                    self.user_ban(f.name_span, "trap");
                }
                let ps = self.t.fns[fi].params.clone();
                let legacy = f.name == "user_fault" && ps.len() == 3 && ps.iter().all(|p| p.ty == Ty::Int(IntTy::U64));
                // 属性の形: `#[trap] fn 名前(pid: Id<表>, addr: u64, is_write: bool)`（ページフォールト。CoW などを本体で行う）
                // the attribute form: `#[trap] fn name(pid: Id<table>, addr: u64, is_write: bool)` (a page fault; the body does CoW etc.)
                let v2 = self.t.fns[fi].trap_v2 && ps.len() == 3 && matches!(ps[0].ty, Ty::Id(_)) && ps[1].ty == Ty::Int(IntTy::U64) && ps[2].ty == Ty::Bool && self.t.fns[fi].ret == Ty::Unit;
                if self.t.fns[fi].trap_v2 && !v2 && !legacy {
                    self.err(
                        Diag::error("F3204", f.name_span, "`#[trap]` の関数の形は `fn 名前(pid: Id<表>, addr: u64, is_write: bool)` です（戻り値なし）", "a `#[trap]` function has the form `fn name(pid: Id<table>, addr: u64, is_write: bool)` (no result)")
                            .hint("EL0 のページフォールトで呼ばれます。Sleep 効果のある関数は呼べません（F3203）", "it runs on EL0 page faults; functions with the Sleep effect cannot be called (F3203)"),
                    );
                }
                let ok = legacy || v2;
                if !ok && !self.t.fns[fi].trap_v2 {
                    self.err(
                        Diag::error("F2801", f.name_span, "`trap` の形は `trap user_fault(esr: u64, far: u64, pc: u64) { .. }` です", "a `trap` item has the form `trap user_fault(esr: u64, far: u64, pc: u64) { .. }`")
                            .hint("EL0 の例外（不正アクセス・未定義命令）で呼ばれます。戻ると、そのタスクは終了します", "it runs on EL0 exceptions (bad accesses, undefined instructions); when it returns, that task is ended"),
                    );
                }
                self.t.fns[fi].trap = true;
            }
            FnKind::Irq(_) => {
                if self.user_mode {
                    self.user_ban(f.name_span, "irq");
                }
            }
            FnKind::Normal => {}
            FnKind::ExternC { .. } => {
                // C との境界を渡れる型: 整数・bool・Id・Option<Id>（生の整数。入ってきた値は validate を通す）・FaultError
                // types that cross the C boundary: integers, bool, Id, Option<Id> (raw integers; incoming ones are validated), FaultError
                let ok_ty = |t: &Ty| matches!(t, Ty::Int(_) | Ty::Bool | Ty::Id(_) | Ty::Fault | Ty::Err) || matches!(t, Ty::Option(i) if matches!(**i, Ty::Id(_)));
                let ps = self.t.fns[fi].params.clone();
                for p in &ps {
                    if !ok_ty(&p.ty) {
                        let d = self.t.display(&p.ty);
                        self.err(
                            Diag::error("F3601", p.span, format!("`extern \"C\"` の引数 `{}: {}` は C との境界を渡れません", p.name, d), format!("the `extern \"C\"` parameter `{}: {}` cannot cross the C boundary", p.name, d))
                                .hint("渡せるのは整数・bool・Id・Option<Id>・FaultError です（Id は C 側では生の整数。入ってきた値はコンパイラが validate を差し込む）", "integers, bool, Id, Option<Id> and FaultError can cross (an Id is a raw integer on the C side; the compiler inserts validate for incoming values)"),
                        );
                    }
                }
                let r = self.t.fns[fi].ret.clone();
                if !(ok_ty(&r) || r == Ty::Unit) {
                    let d = self.t.display(&r);
                    let sp = f.ret.as_ref().map_or(f.name_span, |t| t.span());
                    self.err(Diag::error("F3601", sp, format!("`extern \"C\"` の戻り値 `{}` は C との境界を渡れません", d), format!("the `extern \"C\"` result `{}` cannot cross the C boundary", d)));
                }
                if self.t.fns[fi].is_main {
                    self.err(Diag::error("F3601", f.name_span, "`main` は `extern \"C\"` にできません", "`main` cannot be `extern \"C\"`"));
                }
            }
            FnKind::Asm => {
                let a = f.attrs.iter().find(|a| a.name == "asm").cloned();
                let mut spec = crate::types::AsmSpec::default();
                if let Some(a) = &a {
                    spec.aarch64 = a.kv_str("aarch64").map(|s| s.to_string());
                    spec.x86_64 = a.kv_str("x86_64").map(|s| s.to_string());
                    spec.host = a.kv_str("host").map(|s| s.to_string());
                    spec.effects = a.kv_str("effects").unwrap_or("none").to_string();
                    spec.clobbers = a.kv_str("clobbers").unwrap_or("").to_string();
                    for (k, _, ksp) in &a.kv {
                        if !matches!(k.as_str(), "aarch64" | "x86_64" | "host" | "effects" | "clobbers") {
                            self.err(Diag::error("F3503", *ksp, format!("`#[asm(..)]` の `{}` は使えません（aarch64 / x86_64 / host / effects / clobbers）", k), format!("`{}` is not a `#[asm(..)]` key (aarch64 / x86_64 / host / effects / clobbers)", k)));
                        }
                    }
                    for e in spec.effects.split(|c| c == ',' || c == ' ' || c == '|').filter(|x| !x.is_empty()) {
                        if !matches!(e, "none" | "sleep" | "barrier" | "irq" | "mmio") {
                            self.err(Diag::error("F3503", a.span, format!("`effects = \"{}\"` の `{}` は不明です（none / sleep / barrier / irq / mmio）", spec.effects, e), format!("unknown effect `{}` in `effects = \"{}\"` (none / sleep / barrier / irq / mmio)", e, spec.effects)));
                        }
                    }
                }
                if spec.aarch64.is_none() && spec.x86_64.is_none() && spec.host.is_none() {
                    self.err(
                        Diag::error("F3503", f.name_span, "`#[asm(..)]` の関数には少なくとも 1 つのアーキテクチャの命令列（aarch64 = \"..\" / x86_64 = \"..\"）か host = \"C の式\" が要ります", "a `#[asm(..)]` function needs at least one instruction template (aarch64 = \"..\" / x86_64 = \"..\") or host = \"a C expression\"")
                            .hint("例: `#[asm(aarch64 = \"mrs {r}, cntvct_el0\", effects = \"none\")] fn read_cntvct() -> u64;`", "e.g. `#[asm(aarch64 = \"mrs {r}, cntvct_el0\", effects = \"none\")] fn read_cntvct() -> u64;`"),
                    );
                }
                let ps = self.t.fns[fi].params.clone();
                for p in &ps {
                    if !matches!(p.ty, Ty::Int(_) | Ty::Bool | Ty::Err) {
                        let d = self.t.display(&p.ty);
                        self.err(Diag::error("F3503", p.span, format!("asm のオペランド `{}: {}` は整数か bool だけです（メモリを指す値は渡せない）", p.name, d), format!("the asm operand `{}: {}` must be an integer or bool (nothing that points to memory)", p.name, d)));
                    }
                }
                let r = self.t.fns[fi].ret.clone();
                if !matches!(r, Ty::Int(_) | Ty::Bool | Ty::Unit | Ty::Err) {
                    let d = self.t.display(&r);
                    self.err(Diag::error("F3503", f.name_span, format!("asm の結果 `{}` は整数・bool・() だけです", d), format!("the asm result `{}` must be an integer, bool or ()", d)));
                }
                if spec.effects.contains("sleep") {
                    self.t.fns[fi].sleeps = true;
                }
                self.t.fns[fi].asm = Some(spec);
            }
        }
    }

    /// マルチコアの規則のうち、本体の検査の後で分かるもの（F2611・F2614・F2803）
    /// multi-core rules that are known only after the bodies are checked (F2611, F2614, F2803)
    pub fn check_smp(&mut self) {
        let mut errs: Vec<Diag> = Vec::new();
        for a in &self.t.arenas {
            if !a.shared {
                continue;
            }
            if self.user_mode {
                errs.push(Diag::error("F2803", a.span, "`shared arena` はユーザープログラム（EL0）では使えません", "`shared arena` is not available in a user program (EL0)"));
            }
            if let Some(p) = self.percpu_id_in(&a.elem) {
                let pn = self.t.arenas[p].name.clone();
                errs.push(
                    Diag::error(
                        "F2611",
                        a.span,
                        format!("共有アリーナ `{}` の要素が、コアごとのアリーナ `{}` の Id を含んでいます（ほかのコアでは別の表を指します）", a.name, pn),
                        format!("the elements of the shared arena `{}` contain an Id of the per-core arena `{}` (on another core it would refer to a different table)", a.name, pn),
                    )
                    .hint(format!("`{}` も `shared arena` にするか、Id を置かない形にしてください", pn), format!("make `{}` a `shared arena` too, or keep its Ids out of shared data", pn)),
                );
            }
        }
        // F2614（spawn_on と irq・task_arena・dma_arena・syscall・trap の併用の禁止）は解除した: 割り込みはコアごとに GIC で
        // 配送され、タスクの表と実行キューはコアごと（task_arena はコアごとの実体）、dma_arena は全コアで 1 つの実体になる。
        // 残る制限は context.rs の F2616（SPI のハンドラがコアごとの実体に触る）と F2601 / F2603 / F2613 で検査する
        // F2614 (no irq, task_arena, dma_arena, syscall or trap together with spawn_on) is lifted: interrupts are routed per
        // core by the GIC, the task table and run queue are per core (task_arena has one instance per core) and dma_arena
        // has one instance for every core. What remains is checked in context.rs: F2616 (an SPI handler touching per-core
        // instances) and F2601 / F2603 / F2613
        for d in errs {
            self.err(d);
        }
    }

    /// コアごとのアリーナ（ふつうの arena・bump_arena）の Id を（推移的に）含むなら、そのアリーナ
    /// the per-core arena (a plain arena or bump_arena) whose Id a type contains (transitively), if any
    pub fn percpu_id_in(&self, t: &Ty) -> Option<usize> {
        match t {
            Ty::Id(a) | Ty::Dma(a) | Ty::GpuBuf(a) => (!self.t.arenas[*a].shared).then_some(*a),
            Ty::Option(i) | Ty::Own(i) | Ty::Array(i, _) | Ty::Ref(_, i) | Ty::Slice(i) => self.percpu_id_in(i),
            Ty::Struct(s) => self.t.structs[*s].fields.iter().find_map(|f| self.percpu_id_in(&f.ty)),
            _ => None,
        }
    }

    /// コアの間を渡る型の検査（F2611）: コアごとのアリーナの Id は、ほかのコアでは別の表を指してしまう
    /// types that cross between cores (F2611): an Id of a per-core arena would refer to a different table on another core
    fn check_cross_core(&mut self, t: &Ty, sp: Span, what_ja: &str, what_en: &str) {
        if let Some(a) = self.percpu_id_in(t) {
            let an = self.t.arenas[a].name.clone();
            self.err(
                Diag::error(
                    "F2611",
                    sp,
                    format!("{}が、コアごとのアリーナ `{}` の Id を含んでいます（ほかのコアでは別の表を指します）", what_ja, an),
                    format!("{} contains an Id of the per-core arena `{}` (on another core it would refer to a different table)", what_en, an),
                )
                .hint(
                    format!("コアの間で共有する表は `shared arena {}: ..;` と宣言してください。コアごとのデータは `own T` に入れて所有権ごと送れます", an),
                    format!("declare a table shared between cores as `shared arena {}: ..;`; per-core data can be sent with its ownership inside an `own T`", an),
                ),
            );
        }
    }

    /// `chan @ch: own T[CAP];`: 容量は 2 の累乗、要素は `own T` か解放の要らない値（参照は F0515、コアごとの Id は F2611）
    /// the capacity is a power of two; elements are `own T` or values that need no drop (references F0515, per-core Ids F2611)
    fn declare_chan(&mut self, c: &ChanDef) {
        let Some(RegionRef::Chan(ci)) = self.t.regions.get(&c.name).copied() else { return };
        if self.user_mode {
            self.user_ban(c.span, "chan");
        }
        let elem = self.resolve_type(&c.elem, true, &HashMap::new());
        let cap = self.const_u64(&c.cap, "チャネルの容量", "the channel capacity").unwrap_or(2);
        if !cap.is_power_of_two() || cap < 2 || cap > 65536 {
            self.err(
                Diag::error("F2615", c.cap.span, format!("チャネルの容量 {} は 2 の累乗（2〜65536）でなければなりません", cap), format!("the channel capacity {} must be a power of two (2 to 65536)", cap))
                    .hint(format!("`[{}]` にしてください（添字をマスクだけで回すため）", cap.max(2).next_power_of_two().min(65536)), format!("use `[{}]` (so indices wrap with a mask alone)", cap.max(2).next_power_of_two().min(65536))),
            );
        } else {
            self.t.chans[ci].cap = cap;
        }
        fn has_ref(s: &Sema, t: &Ty) -> bool {
            match t {
                Ty::Ref(..) | Ty::Slice(_) | Ty::Fn(..) => true,
                Ty::Option(i) | Ty::Own(i) | Ty::Array(i, _) => has_ref(s, i),
                Ty::Struct(x) => s.t.structs[*x].fields.iter().any(|f| has_ref(s, &f.ty)),
                _ => false,
            }
        }
        if has_ref(self, &elem) {
            self.err(
                Diag::error("F0515", c.elem.span(), "チャネルの要素に参照（と関数ポインタ）は置けません（第二級の参照: 別のコアへは渡せない）", "channel elements cannot be references or function pointers (second-class references never cross to another core)")
                    .hint("`own T` で所有権ごと送ってください", "send ownership with `own T`"),
            );
        } else if !matches!(elem, Ty::Own(_) | Ty::Err) && (self.t.needs_drop(&elem) || self.has_dma(&elem)) {
            let d = self.t.display(&elem);
            self.err(
                Diag::error("F2615", c.elem.span(), format!("チャネルの要素 `{}` は、`own T` か、解放の要らない値でなければなりません", d), format!("the channel element `{}` must be `own T` or a value that needs no drop", d))
                    .hint(format!("`chan {}: own {}[..];` と書いてください", c.name, d), format!("write `chan {}: own {}[..];`", c.name, d)),
            );
        } else {
            self.check_cross_core(&elem, c.elem.span(), "チャネルの要素", "the channel element");
        }
        self.t.chans[ci].elem = elem;
    }

    /// `dma @r: [T; N];`: 要素は POD（F2702） / elements are plain old data (F2702)
    fn declare_dma(&mut self, d: &DmaDef) {
        let Some(RegionRef::Dma(di)) = self.t.regions.get(&d.name).copied() else { return };
        let t = self.resolve_type(&d.ty, false, &HashMap::new());
        match t {
            Ty::Array(el, n) => {
                if !self.is_pod(&el) && !el.is_err() {
                    let dn = self.t.display(&el);
                    self.err(
                        Diag::error("F2702", d.ty.span(), format!("`dma` リージョンの要素 `{}` は POD ではありません（整数・整数の配列・ワイヤ構造体だけ）", dn), format!("the `dma` region element `{}` is not plain old data (integers, arrays of integers and wire structs only)", dn))
                            .hint("デバイスが書いたどんなビット列でも正しい値になる型だけを置けます。構造体には `#[wire(le)]` を付けてください", "only types for which any bits written by the device are a valid value are allowed; mark the struct `#[wire(le)]`"),
                    );
                }
                self.t.dmas[di].elem = *el;
                self.t.dmas[di].len = n;
            }
            Ty::Err => {}
            other => {
                let dn = self.t.display(&other);
                self.err(Diag::error("F2702", d.ty.span(), format!("`dma` リージョンの型は配列 `[T; N]` です（`{}` は不可）", dn), format!("the type of a `dma` region is an array `[T; N]` (not `{}`)", dn)));
            }
        }
    }

    /// ワイヤ構造体の形（F2701）: フィールドは整数・整数の配列・ワイヤ構造体（とその配列）だけ。big-endian の構造体の
    /// 配列は 1 バイトの整数だけ（要素ごとのバイト順の変換はしない）。`#[repr(align(N))]` とは併用できない
    /// the shape of wire structs (F2701): fields are integers, arrays of integers and wire structs (and arrays of them)
    /// only; arrays in a big-endian struct hold 1-byte integers only (no per-element byte swapping); not combinable with
    /// `#[repr(align(N))]`
    fn check_wire_structs(&mut self) {
        let mut errs = Vec::new();
        // dma_arena の要素は POD（F2702）。構造体の属性が決まってから調べる / dma_arena elements are plain old data,
        // checked once the struct attributes are known
        for a in &self.t.arenas {
            // gpu_arena の要素（F2705）と `--gpu=off`（F2707: 装置へは出さず、呼んだコアでその場で走らせる）
            // gpu_arena elements (F2705) and `--gpu=off` (F2707: nothing goes to a device; kernels run in place on the
            // calling core)
            if a.gpu && !a.elem.is_err() && !self.is_gpu_pod(&a.elem) {
                let d = self.t.display(&a.elem);
                errs.push(
                    Diag::error("F2705", a.span, format!("gpu_arena `{}` の要素 `{}` は POD ではありません（整数・浮動小数点数と、その配列・構造体だけ）", a.name, d), format!("the element `{}` of the gpu_arena `{}` is not plain old data (integers, floats and arrays / structs of them only)", d, a.name))
                        .hint("装置はスロットのバイト列をそのまま読み書きします。own・Id・Str・参照・関数ポインタ・bool・Option はほかの表やヒープを指すか、装置が書いたビット列では壊れた値になります。CPU 側の情報は別の arena に置き、添字（`h.index()`）で結んでください", "the device reads and writes the slot bytes as they are; own, Id, Str, references, function pointers, bool and Option point into other tables or heaps, or become broken values from device-written bits. Keep CPU-side data in a separate arena and link it by index (`h.index()`)"),
                );
            }
            if a.gpu && crate::universal::GPU_OFF.load(std::sync::atomic::Ordering::Relaxed) {
                errs.push(
                    Diag::warning("F2707", a.span, format!("`--gpu=off` なので gpu_arena `{}` は装置へ出しません（dispatch は呼んだコアでその場で走り、すぐ完了します）", a.name), format!("with `--gpu=off` the gpu_arena `{}` is not offloaded (dispatch runs in place on the calling core and completes at once)", a.name))
                        .hint("CPU で走らせるカーネルは FASCON_GPU_KERNEL(番号) で登録したものです（登録が無い番号は何もしない）。装置へ出すときは `--gpu=off` を外してください", "the kernels run on the CPU are the ones registered with FASCON_GPU_KERNEL(id) (an unregistered id does nothing); drop `--gpu=off` to offload"),
                );
            }
            if a.dma && !self.is_pod(&a.elem) && !a.elem.is_err() {
                let d = self.t.display(&a.elem);
                errs.push(
                    Diag::error("F2702", a.span, format!("dma_arena `{}` の要素 `{}` は POD ではありません（整数・整数の配列・ワイヤ構造体だけ）", a.name, d), format!("the element `{}` of the dma_arena `{}` is not plain old data (integers, arrays of integers and wire structs only)", d, a.name))
                        .hint("デバイスが書いたどんなビット列でも正しい値になる型だけを置けます。構造体には `#[wire(le)]` を付けてください", "only types for which any bits written by the device are a valid value are allowed; mark the struct `#[wire(le)]`"),
                );
            }
        }
        for s in &self.t.structs {
            let Some(be) = s.wire else { continue };
            if s.align > 0 {
                errs.push(Diag::error("F2701", s.span, format!("ワイヤ構造体 `{}` に `#[repr(align(N))]` は付けられません（整列は 1 です）", s.name), format!("the wire struct `{}` cannot take `#[repr(align(N))]` (its alignment is 1)", s.name)));
            }
            for f in &s.fields {
                let ok = match &f.ty {
                    Ty::Int(_) | Ty::Err => true,
                    Ty::Array(e, _) => match &**e {
                        Ty::Int(i) => !be || i.bits(64) == 8,
                        Ty::Struct(x) => self.t.structs[*x].wire.is_some(),
                        _ => false,
                    },
                    Ty::Struct(x) => self.t.structs[*x].wire.is_some(),
                    _ => false,
                };
                if !ok {
                    let d = self.t.display(&f.ty);
                    errs.push(
                        Diag::error(
                            "F2701",
                            f.span,
                            format!("ワイヤ構造体 `{}` のフィールド `{}: {}` は置けません（整数・整数の配列・ワイヤ構造体だけ。big-endian の配列は u8 / i8 だけ）", s.name, f.name, d),
                            format!("the field `{}: {}` cannot be in the wire struct `{}` (integers, arrays of integers and wire structs only; arrays in a big-endian struct hold u8 / i8 only)", f.name, d, s.name),
                        )
                        .hint("どんなビット列でも正しい値になる型だけを置けます（bool・Option・Id・own・Str・関数ポインタは不可）", "only types for which every bit pattern is a valid value are allowed (no bool, Option, Id, own, Str or function pointers)"),
                    );
                }
            }
        }
        for d in errs {
            self.err(d);
        }
    }

    /// 式がワイヤ構造体の 2 バイト以上の整数フィールドなら、(整数型, big-endian か)
    /// (integer type, big-endian?) when the expression is an integer field of 2 bytes or more of a wire struct
    pub fn wire_int_field(&self, e: &Expr) -> Option<(IntTy, bool)> {
        if let ExprKind::Field(base, name, _) = &e.kind {
            if let Ty::Struct(sid) = self.expr_ty.get(base.id as usize)?.deref_all() {
                let st = &self.t.structs[*sid];
                let be = st.wire?;
                if let Some(Ty::Int(i)) = st.fields.iter().find(|f| &f.name == name).map(|f| &f.ty) {
                    if i.bits(64) > 8 {
                        return Some((*i, be));
                    }
                }
            }
        }
        None
    }

    fn fn_item_index(&self, f: &FnDef) -> usize {
        self.prog.items.iter().position(|it| matches!(it, Item::Fn(g) if std::ptr::eq(g, f))).unwrap_or(usize::MAX)
    }

    /// 関数の本体の型検査。1 周目で「ここでは型が決まっている必要がある」（F0282）が出たら、1 周目で決まった let の型を
    /// 種にして 2 周目をやり直す（`let mut best = None;` の中身の型が、後ろの `best = Some(id);` から決まる）
    /// type-check a function body. If the first pass reports "the type must be known here" (F0282), redo it with the let
    /// types found by the first pass as seeds (so the payload of `let mut best = None;` follows from a later `best = Some(id);`)
    fn check_fn(&mut self, f: &FnDef) {
        let fi = match self.t.fn_by_name.get(&f.name) {
            Some(&x) => x,
            None => return,
        };
        if self.t.fns[fi].item != self.fn_item_index(f) {
            return;
        }
        if self.t.fns[fi].stub {
            return; // 本体の無い宣言（ユーザー側の svc） / a bodiless declaration (an svc on the user side)
        }
        // 本体の無い境界の関数（`#[asm(..)] fn ..;`・C からの取り込み） / bodiless boundary functions (asm, imports from C)
        if self.t.fns[fi].asm.is_some() || self.t.fns[fi].extern_c == Some(false) {
            return;
        }
        let before = self.diags.iter().filter(|d| d.is_error()).count();
        let diag_len = self.diags.len();
        let lit_len = self.int_lits.len();
        let flit_len = self.float_lits.len();
        self.deferred_unknown = 0;
        self.seeds.clear();
        self.check_fn_once(f, fi);
        if self.deferred_unknown > 0 {
            let mut seeds = HashMap::new();
            collect_let_binds(&f.body, &mut |b| {
                if let Some(l) = &self.locals[b as usize] {
                    let t = l.ty.clone();
                    seeds.insert(b, t);
                }
            });
            let mut ok_seeds = HashMap::new();
            for (b, t) in seeds {
                let r = self.resolve(&t);
                if !r.has_var() && !contains_ivar(&r) && !r.is_err() {
                    ok_seeds.insert(b, r);
                }
            }
            // 1 周目の結果を捨てる（式の番号はこの関数の範囲だけ） / discard the first pass (only this function's expression ids)
            let (lo, hi) = fn_expr_range(f);
            let inr = |k: &ExprId| *k >= lo && *k < hi;
            self.diags.truncate(diag_len);
            self.int_lits.truncate(lit_len);
            self.float_lits.truncate(flit_len);
            self.res.retain(|k, _| !inr(k));
            self.calls.retain(|k, _| !inr(k));
            self.methods.retain(|k, _| !inr(k));
            self.fields.retain(|k, _| !inr(k));
            self.mmio_index.retain(|k, _| !inr(k));
            self.arena_index.retain(|k, _| !inr(k));
            self.port_index.retain(|k, _| !inr(k));
            self.port_reads.retain(|k, _| !inr(k));
            self.own_allocs.retain(|k, _| !inr(k));
            self.own_alloc_tys.retain(|k, _| !inr(k));
            self.own_derefs.retain(|k| !inr(k));
            self.call_targets.retain(|k, _| !inr(k));
            self.fn_value_sites.retain(|(_, k, _)| !inr(k));
            self.lookup_scrut.retain(|k| !inr(k));
            self.auto_deref.retain(|k| !inr(k));
            self.neg_lits.retain(|k| !inr(k));
            self.seeds = ok_seeds;
            self.deferred_unknown = 0;
            self.second_pass = true;
            self.check_fn_once(f, fi);
            self.second_pass = false;
            self.seeds.clear();
        }
        self.finish_types();
        let after = self.diags.iter().filter(|d| d.is_error()).count();
        self.fn_ok[fi] = after == before;
    }

    fn check_fn_once(&mut self, f: &FnDef, fi: usize) {
        self.scopes.clear();
        self.scopes.push(HashMap::new());
        let params = self.t.fns[fi].params.clone();
        for p in &params {
            self.locals[p.bind as usize] = Some(LocalInfo { name: p.name.clone(), ty: p.ty.clone(), mutable: p.mutable, span: p.span, kind: LocalKind::Param });
            self.scopes.last_mut().unwrap().insert(p.name.clone(), p.bind);
        }
        self.cur_ret = self.t.fns[fi].ret.clone();
        self.loop_breaks.clear();
        let ret = self.cur_ret.clone();
        let bt = self.check_block(&f.body, Some(&ret));
        if !self.unify(&ret, &bt) {
            let sp = f.body.tail.as_ref().map(|t| t.span).unwrap_or(f.body.span);
            if self.resolve(&bt) == Ty::Unit {
                let rs = self.t.display(&ret);
                self.err(Diag::error(
                    "F0308",
                    sp,
                    format!("関数 `{}` は `{}` を返す必要がありますが、値を返していません", f.name, rs),
                    format!("function `{}` must return `{}` but returns nothing", f.name, rs),
                ));
            } else {
                self.mismatch(sp, &ret, &bt);
            }
        }
        self.scopes.clear();
    }

    /// 型変数を確定させ、整数リテラルの範囲を検査する / finalize type variables and check integer literal ranges
    fn finish_types(&mut self) {
        // 浮動小数点のリテラルが型の範囲を超えて無限大になるなら誤り（Rust の overflowing_literals と同じ）
        // a float literal that rounds to infinity in its type is an error (as Rust's overflowing_literals)
        let flits = std::mem::take(&mut self.float_lits);
        for (id, text, sp) in flits {
            let t = self.finalize(&self.expr_ty[id as usize].clone());
            let inf = match t {
                Ty::Float(FloatTy::F32) => text.parse::<f32>().map_or(false, |v| v.is_infinite()),
                Ty::Float(FloatTy::F64) => text.parse::<f64>().map_or(false, |v| v.is_infinite()),
                _ => false,
            };
            if inf {
                let tn = self.t.display(&t);
                self.err(Diag::error("F0308", sp, format!("浮動小数点のリテラル {} は `{}` の範囲外です（無限大になります）", text, tn), format!("float literal {} is out of range for `{}` (it would be infinite)", text, tn)));
            }
        }
        let lits = std::mem::take(&mut self.int_lits);
        for (id, v, sp) in lits {
            let t = self.finalize(&self.expr_ty[id as usize].clone());
            if let Ty::Int(it) = t {
                let (lo, hi) = it.min_max(self.t.ptr_bits);
                let val = if self.neg_lits.contains(&id) { -(v as i128) } else { v as i128 };
                if val < lo || val > hi {
                    self.err(Diag::error(
                        "F0308",
                        sp,
                        format!("整数リテラル {} は `{}` の範囲（{}〜{}）外です", val, it.name(), lo, hi),
                        format!("integer literal {} is out of range for `{}` ({} to {})", val, it.name(), lo, hi),
                    ));
                }
            }
        }
        for i in 0..self.expr_ty.len() {
            if contains_ivar(&self.expr_ty[i]) {
                let t = self.expr_ty[i].clone();
                self.expr_ty[i] = self.finalize(&t);
            }
        }
        for i in 0..self.locals.len() {
            if let Some(l) = &self.locals[i] {
                if contains_ivar(&l.ty) {
                    let t = l.ty.clone();
                    let ft = self.finalize(&t);
                    self.locals[i].as_mut().unwrap().ty = ft;
                }
            }
        }
    }

    fn lookup_local(&self, name: &str) -> Option<BindId> {
        for s in self.scopes.iter().rev() {
            if let Some(&b) = s.get(name) {
                return Some(b);
            }
        }
        None
    }

    fn declare_local(&mut self, name: &str, bind: BindId, info: LocalInfo) {
        self.locals[bind as usize] = Some(info);
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), bind);
        }
    }

    pub fn check_block(&mut self, b: &Block, exp: Option<&Ty>) -> Ty {
        self.scopes.push(HashMap::new());
        let mut diverges = false;
        for s in &b.stmts {
            match s {
                Stmt::Let(l) => {
                    for a in &l.attrs {
                        if a.name == "secret" {
                            self.secret_binds.insert(l.bind);
                        } else {
                            self.err(Diag::warning("F3504", a.span, format!("let の属性 `{}` は無視されます（使えるのは `#[secret]`）", a.name), format!("let attribute `{}` is ignored (`#[secret]` is accepted)", a.name)));
                        }
                    }
                    let declared = l.ty.as_ref().map(|t| self.resolve_type(t, false, &HashMap::new()));
                    let ty = match (&l.init, &declared) {
                        (Some(init), Some(dt)) => {
                            let it = self.check_expr(init, Some(dt));
                            self.expect_ty(init.span, dt, &it);
                            if it == Ty::Never {
                                diverges = true;
                            }
                            dt.clone()
                        }
                        (Some(init), None) => {
                            let it = self.check_expr(init, None);
                            if it == Ty::Never {
                                diverges = true;
                            }
                            if let Ty::Ref(..) = it {
                                self.err(
                                    Diag::error(
                                        "F0515",
                                        l.span,
                                        format!("参照 `{}` を変数に束縛することはできません（第二級参照）", l.name),
                                        format!("a reference cannot be bound to the variable `{}` (second-class references)", l.name),
                                    )
                                    .hint("参照は関数の引数として直接渡してください（`f(&x)`）", "pass the reference directly as a call argument (`f(&x)`)"),
                                );
                                Ty::Err
                            } else {
                                it
                            }
                        }
                        (None, Some(dt)) => dt.clone(),
                        // `let x;`: 型は後ろの代入・比較・引数から推論する（F0282 は出さない）
                        // `let x;`: the type is inferred from later assignments, comparisons and arguments (no F0282)
                        (None, None) => self.new_var(),
                    };
                    // 2 周目: 1 周目で関数全体から決まった型を先に与える / second pass: give the type found from the whole function first
                    if let Some(sd) = self.seeds.get(&l.bind).cloned() {
                        self.unify(&ty, &sd);
                    }
                    self.declare_local(&l.name, l.bind, LocalInfo { name: l.name.clone(), ty, mutable: l.mutable, span: l.name_span, kind: LocalKind::Let });
                }
                Stmt::Expr(e) => {
                    let t = self.check_expr(e, None);
                    if t == Ty::Never {
                        diverges = true;
                    }
                }
            }
        }
        let r = match &b.tail {
            Some(t) => {
                let tt = self.check_expr(t, exp);
                if diverges && tt == Ty::Unit {
                    Ty::Never
                } else {
                    tt
                }
            }
            None => {
                if diverges {
                    Ty::Never
                } else {
                    Ty::Unit
                }
            }
        };
        self.scopes.pop();
        r
    }

    pub fn check_expr(&mut self, e: &Expr, exp: Option<&Ty>) -> Ty {
        let mut t = self.check_expr_inner(e, exp);
        // `*e`: 参照には何もしない（自動で辿られる）。own（own への参照を含む）には own の中身
        // `*e`: nothing for references (they are auto-dereferenced); the contents for an own (or a reference to an own)
        if self.prog.derefs.contains(&e.id) {
            let r = self.resolve(&t);
            let inner = match &r {
                Ty::Own(i) => Some((**i).clone()),
                Ty::Ref(_, i) => match &**i {
                    Ty::Own(x) => Some((**x).clone()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(i) = inner {
                self.own_derefs.insert(e.id);
                t = i;
            }
        }
        // エスケープ解析による自動昇格: 期待が gc_arena の Id（Option も）で、実際が同じ要素の bump_arena の Id なら、
        // 出口で巻き戻される前に gc_arena へ写す（promote_to_gc と同じ）
        // automatic promotion by escape analysis: when a gc_arena Id (or Option of it) is expected and the value is a
        // bump_arena Id of the same element type, it is copied into the gc_arena before the exit rewind (as promote_to_gc)
        if let Some(x) = exp {
            let want = match x {
                Ty::Id(g) => Some((*g, false)),
                Ty::Option(i) => match **i {
                    Ty::Id(g) => Some((g, true)),
                    _ => None,
                },
                _ => None,
            };
            if let Some((g, opt)) = want.filter(|(g, _)| self.t.arenas[*g].gc) {
                let r = self.resolve(&t);
                let got = match &r {
                    Ty::Id(b) if opt => Some((*b, true)),
                    Ty::Option(i) if opt => match **i {
                        Ty::Id(b) => Some((b, false)),
                        _ => None,
                    },
                    Ty::Id(b) => Some((*b, false)),
                    _ => None,
                };
                if let Some((b, wrap_some)) = got {
                    if self.t.arenas[b].bump && self.t.arenas[b].elem == self.t.arenas[g].elem && !matches!(r, Ty::Err) {
                        // Id<bump> → Option<Id<gc>> は Some(..) の形（失敗すると None） / Id<bump> → Option<Id<gc>> (None on failure)
                        let _ = wrap_some;
                        self.auto_promote.insert(e.id, (g, b));
                        t = x.clone();
                    }
                }
            }
        }
        self.expr_ty[e.id as usize] = t.clone();
        t
    }

    fn check_expr_inner(&mut self, e: &Expr, exp: Option<&Ty>) -> Ty {
        let exp_r = exp.map(|t| self.resolve(t));
        match &e.kind {
            ExprKind::Int(v, suf) => {
                let t = match (suf, &exp_r) {
                    (Some(s), Some(x)) if x.is_int() => {
                        let st = Ty::Int(*s);
                        self.expect_ty(e.span, x, &st);
                        st
                    }
                    (Some(s), _) => Ty::Int(*s),
                    (None, Some(Ty::Int(it))) => Ty::Int(*it),
                    (None, Some(Ty::IntVar(v))) => Ty::IntVar(*v),
                    _ => self.new_ivar(),
                };
                self.int_lits.push((e.id, *v, e.span));
                t
            }
            ExprKind::Float(text, suf) => {
                let t = match (suf, &exp_r) {
                    (Some(s), Some(x)) if x.is_float() => {
                        let st = Ty::Float(*s);
                        self.expect_ty(e.span, x, &st);
                        st
                    }
                    (Some(s), _) => Ty::Float(*s),
                    (None, Some(Ty::Float(ft))) => Ty::Float(*ft),
                    (None, Some(Ty::FloatVar(v))) => Ty::FloatVar(*v),
                    _ => self.new_fvar(),
                };
                self.float_lits.push((e.id, text.clone(), e.span));
                t
            }
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Str(_) => Ty::Str,
            ExprKind::Unit => Ty::Unit,
            ExprKind::Path(name) => {
                if let Some(b) = self.lookup_local(name) {
                    self.res.insert(e.id, Res::Local(b));
                    let lt = self.locals[b as usize].as_ref().map(|l| l.ty.clone()).unwrap_or(Ty::Err);
                    if let Ty::Ref(_, inner) = &lt {
                        let ir = self.resolve(inner);
                        if self.t.is_copy(&ir) {
                            // `p: &i64` は値の位置では `*p` として読む / read `p: &i64` as `*p` in value positions
                            self.auto_deref.insert(e.id);
                            return ir;
                        }
                    }
                    return lt;
                }
                if let Some(&si) = self.t.static_by_name.get(name) {
                    self.res.insert(e.id, Res::Static(si));
                    return self.t.statics[si].ty.clone();
                }
                if name == "None" {
                    self.res.insert(e.id, Res::NoneLit);
                    return match &exp_r {
                        Some(Ty::Option(i)) => Ty::Option(i.clone()),
                        // 中身の型は後で決まる（`best = Some(id);` などから） / the payload is inferred later (from `best = Some(id);` ...)
                        _ => {
                            let v = self.new_var();
                            Ty::Option(Box::new(v))
                        }
                    };
                }
                // 関数の名前を値として使う = 関数ポインタ `fn(..) -> R`（コールバックとして渡す・フィールドや static に置く）
                // a function name used as a value = a function pointer `fn(..) -> R` (passed as a callback, stored in a field
                // or a static)
                if let Some(&fi) = self.t.fn_by_name.get(name) {
                    if self.t.fns[fi].is_main {
                        self.err(Diag::error(
                            "F0308",
                            e.span,
                            "`main` は値として使えません（実行の入口です）",
                            "`main` cannot be used as a value (it is the entry point)",
                        ));
                        return Ty::Err;
                    }
                    self.res.insert(e.id, Res::Fn(fi));
                    self.fn_value_sites.push((fi, e.id, e.span));
                    let f = &self.t.fns[fi];
                    return Ty::Fn(f.params.iter().map(|p| p.ty.clone()).collect(), Box::new(f.ret.clone()));
                }
                // `f32::NAN` などの浮動小数点の定数 / float constants such as `f32::NAN`
                if let Some((tn, cn)) = name.split_once("::") {
                    if let (Some(ft), Some(c)) = (FloatTy::from_name(tn), FLOAT_CONSTS.iter().find(|c| **c == cn)) {
                        self.res.insert(e.id, Res::FloatConst(ft, c));
                        return Ty::Float(ft);
                    }
                }
                // `EINVAL` などの失敗の符号（FaultError）。同じ名前のローカル・static・関数があればそちらが優先
                // failure codes such as `EINVAL` (FaultError); a local, static or function of the same name takes precedence
                if let Some(&(_, c)) = FAULT_CONSTS.iter().find(|(n, _)| *n == name.as_str()) {
                    self.res.insert(e.id, Res::FaultConst(c));
                    return Ty::Fault;
                }
                if builtin_fn_name(name) {
                    self.err(Diag::error("F0308", e.span, format!("組み込みの `{}` は値として使えません（呼び出してください）", name), format!("the builtin `{}` cannot be used as a value (call it)", name)));
                    return Ty::Err;
                }
                if is_struct_name(name) && self.t.struct_by_name.contains_key(name) {
                    self.err(Diag::error("F0423", e.span, format!("`{}` は型です（構造体リテラル `{} {{ .. }}` を書いてください）", name, name), format!("`{}` is a type (write a struct literal `{} {{ .. }}`)", name, name)));
                    return Ty::Err;
                }
                self.err(Diag::error("F0425", e.span, format!("`{}` が見つかりません", name), format!("cannot find `{}` in this scope", name)));
                Ty::Err
            }
            ExprKind::ArenaRef(name) => {
                match self.t.regions.get(name).copied() {
                    // スカラーのポートを値として使う = 読み出し（`in al, dx`） / a scalar port used as a value = a read (`in al, dx`)
                    Some(RegionRef::Port(p)) if self.t.ports[p].len.is_none() => {
                        self.port_reads.insert(e.id, p);
                        return Ty::Int(self.t.ports[p].elem);
                    }
                    Some(RegionRef::Port(p)) => {
                        let n = self.t.ports[p].name.clone();
                        self.err(Diag::error(
                            "F2204",
                            e.span,
                            format!("`{}` はポートの配列です。`{}[i]` のように添字を付けて読み書きしてください", n, n),
                            format!("`{}` is an array of ports; read and write it with an index, as in `{}[i]`", n, n),
                        ));
                    }
                    None => {
                        self.err(Diag::error("F2104", e.span, format!("`{}` が見つかりません", name), format!("cannot find `{}`", name)));
                    }
                    Some(_) => {
                        self.err(Diag::error(
                            "F2204",
                            e.span,
                            format!("`{}` はリージョンの名前で、値ではありません（`{}[id]` や `{}.alloc(..)` のように使います）", name, name, name),
                            format!("`{}` names a region and is not a value (use it as `{}[id]` or `{}.alloc(..)`)", name, name, name),
                        ));
                    }
                }
                Ty::Err
            }
            ExprKind::OwnAlloc(te) => {
                if self.in_bypass > 0 {
                    self.err(Diag::error("F0811", e.span, "volatile_bypass の中では own の割り当てはできません", "own allocations are not allowed inside volatile_bypass"));
                }
                let t = self.resolve_type(te, false, &HashMap::new());
                match t {
                    // 構造体でない値（`own_alloc(i32)` / `own_alloc([u8; 4096])` / `own_alloc(Option<own Node>)`）:
                    // 0 で埋めた値を新しい所有リージョンに作る。配列の中の own の子も 0 で作って詰める
                    // a non-struct value (`own_alloc(i32)` / `own_alloc([u8; 4096])` / `own_alloc(Option<own Node>)`): a
                    // zero-filled value in a new own region; own children inside arrays are created zero-filled and packed
                    ref other if !matches!(other, Ty::Struct(_) | Ty::Err) && Tables::ownable(other) => {
                        let mut kids = Vec::new();
                        self.own_alloc_kids_ty(other, &mut kids, 0);
                        for k in kids.clone() {
                            if let Some(path) = self.own_chain_cycle(k) {
                                self.err(Diag::error(
                                    "F2304",
                                    e.span,
                                    format!("`own_alloc` は own の子を無限にたどることになります（{}）", path),
                                    format!("`own_alloc` would need an endless chain of own children ({})", path),
                                ));
                                return Ty::Err;
                            }
                        }
                        let al = self.t.type_align_attr(other);
                        if al > 16 || kids.iter().any(|k| self.t.type_align_attr(&Ty::Struct(*k)) > 16) {
                            let d = self.t.display(other);
                            self.err(Diag::error(
                                "F2032",
                                e.span,
                                format!("`{}` は 16 バイトより強い整列が要るので own の所有リージョン（16 バイト境界）には置けません", d),
                                format!("`{}` needs more than 16-byte alignment and cannot live in an own region (16-byte aligned)", d),
                            ));
                        }
                        self.own_alloc_tys.insert(e.id, other.clone());
                        Ty::Own(Box::new(t))
                    }
                    Ty::Struct(sid) => {
                        if let Some(path) = self.own_chain_cycle(sid) {
                            self.err(
                                Diag::error(
                                    "F2304",
                                    e.span,
                                    format!("`own_alloc({})` は own の子を無限にたどることになります（{}）", self.t.structs[sid].name, path),
                                    format!("`own_alloc({})` would need an endless chain of own children ({})", self.t.structs[sid].name, path),
                                )
                                .hint("自分へ戻る own のフィールドは `Option<own T>` にしてください（None で初期化される）", "make the own field that leads back `Option<own T>` (it starts as None)"),
                            );
                            return Ty::Err;
                        }
                        // 根と、同じブロックに 0 で詰める own の子のどれもが 16 バイト境界で足りること
                        // the root and every own child zero-filled into the same block must be fine with 16-byte alignment
                        let mut kids = vec![sid];
                        self.own_alloc_kids(sid, &mut kids, 0);
                        for k in kids {
                            let kt = Ty::Struct(k);
                            let al = self.t.type_align_attr(&kt);
                            if al > 16 {
                                let d = self.t.display(&kt);
                                self.err(Diag::error(
                                    "F2032",
                                    e.span,
                                    format!("`{}` は {} バイト境界が要るので own の所有リージョン（16 バイト境界）には置けません", d, al),
                                    format!("`{}` needs {}-byte alignment and cannot live in an own region (16-byte aligned)", d, al),
                                ));
                                break;
                            }
                        }
                        self.own_allocs.insert(e.id, sid);
                        Ty::Own(Box::new(t))
                    }
                    Ty::Err => Ty::Err,
                    other => {
                        let d = self.t.display(&other);
                        self.err(Diag::error(
                            "F2301",
                            te.span(),
                            format!("`own_alloc({})` は書けません（own の中身は値の型: 構造体・整数・bool・Str・Id・配列・Option）", d),
                            format!("`own_alloc({})` is not allowed (an own holds a value type: a struct, an integer, bool, Str, an Id, an array or an Option)", d),
                        ));
                        Ty::Err
                    }
                }
            }
            ExprKind::Call { callee, callee_span, args, turbofish } => self.check_call(e, callee, *callee_span, args, turbofish.as_ref(), exp_r.as_ref()),
            ExprKind::CallIndirect { callee, args } => self.check_call_indirect(e, callee, args),
            ExprKind::MethodCall { recv, method, method_span, args } => self.check_method(e, recv, method, *method_span, args, exp_r.as_ref(), false),
            ExprKind::Field(base, name, nsp) => {
                if let ExprKind::ArenaRef(a) = &base.kind {
                    self.err(Diag::error(
                        "F2204",
                        base.span,
                        format!("`{}` のフィールドではなく、スロット `{}[id].{}` を指定してください", a, a, name),
                        format!("access a slot field as `{}[id].{}` instead of a field of `{}`", a, name, a),
                    ));
                    return Ty::Err;
                }
                let bt = self.check_expr(base, None);
                let bt = self.resolve(&bt);
                if self.unknown_here(bt.deref_all(), base.span, true) {
                    return Ty::Err;
                }
                match bt.deref_all() {
                    Ty::Struct(s) | Ty::MmioBind(s) => {
                        let s = *s;
                        match self.t.structs[s].field(name) {
                            Some((fi, f)) => {
                                let ft = f.ty.clone();
                                self.fields.insert(e.id, (s, fi));
                                ft
                            }
                            None => {
                                let sn = self.t.structs[s].name.clone();
                                self.err(Diag::error("F0609", *nsp, format!("構造体 `{}` にフィールド `{}` はありません", sn, name), format!("struct `{}` has no field `{}`", sn, name)));
                                Ty::Err
                            }
                        }
                    }
                    Ty::Err => Ty::Err,
                    other => {
                        let d = self.t.display(other);
                        self.err(Diag::error("F0609", *nsp, format!("`{}` 型にはフィールド `{}` がありません", d, name), format!("type `{}` has no field `{}`", d, name)));
                        Ty::Err
                    }
                }
            }
            ExprKind::Index(base, idx) => self.check_index(e, base, idx),
            ExprKind::StructLit { name, name_span, fields } => {
                let sid = match self.t.struct_by_name.get(name) {
                    Some(&s) => s,
                    None => {
                        self.err(Diag::error("F0422", *name_span, format!("構造体 `{}` が見つかりません", name), format!("cannot find struct `{}`", name)));
                        for (_, _, v) in fields {
                            self.check_expr(v, None);
                        }
                        return Ty::Err;
                    }
                };
                let mut seen = HashSet::new();
                for (fname, fsp, v) in fields {
                    if !seen.insert(fname.clone()) {
                        self.err(Diag::error("F0062", *fsp, format!("フィールド `{}` を 2 回指定しています", fname), format!("field `{}` specified more than once", fname)));
                    }
                    match self.t.structs[sid].field(fname) {
                        Some((_, f)) => {
                            let ft = f.ty.clone();
                            let vt = self.check_expr(v, Some(&ft));
                            self.expect_ty(v.span, &ft, &vt);
                        }
                        None => {
                            let sn = self.t.structs[sid].name.clone();
                            self.err(Diag::error("F0560", *fsp, format!("構造体 `{}` にフィールド `{}` はありません", sn, fname), format!("struct `{}` has no field named `{}`", sn, fname)));
                            self.check_expr(v, None);
                        }
                    }
                }
                let missing: Vec<String> = self.t.structs[sid].fields.iter().filter(|f| !seen.contains(&f.name)).map(|f| f.name.clone()).collect();
                if !missing.is_empty() {
                    self.err(Diag::error(
                        "F0063",
                        e.span,
                        format!("フィールド {} の初期値がありません", missing.iter().map(|m| format!("`{}`", m)).collect::<Vec<_>>().join(", ")),
                        format!("missing field(s) {}", missing.iter().map(|m| format!("`{}`", m)).collect::<Vec<_>>().join(", ")),
                    ));
                }
                Ty::Struct(sid)
            }
            ExprKind::ArrayLit(elems) => {
                let et = match &exp_r {
                    Some(Ty::Array(et, _)) => Some((**et).clone()),
                    _ => None,
                };
                let mut first: Option<Ty> = et;
                for el in elems {
                    let t = self.check_expr(el, first.as_ref());
                    match &first {
                        Some(f) => {
                            let f = f.clone();
                            self.expect_ty(el.span, &f, &t);
                        }
                        None => first = Some(t),
                    }
                }
                Ty::Array(Box::new(first.unwrap_or(Ty::Err)), elems.len() as u64)
            }
            ExprKind::ArrayRepeat(v, n) => {
                let et = match &exp_r {
                    Some(Ty::Array(et, _)) => Some((**et).clone()),
                    _ => None,
                };
                let vt = self.check_expr(v, et.as_ref());
                if let Some(et) = &et {
                    self.expect_ty(v.span, et, &vt);
                }
                let cnt = self.const_u64(n, "繰り返しの個数", "the repeat count").unwrap_or(1);
                let elem = et.unwrap_or(vt);
                let is_none_lit = matches!(&v.kind, ExprKind::Path(p) if p == "None");
                let elem_r = self.resolve(&elem);
                if !self.t.is_copy(&elem_r) && !is_none_lit && cnt > 1 {
                    self.err(Diag::error(
                        "F0277",
                        v.span,
                        "`[値; N]` の値はコピーできる型（整数・bool・Id・Str など）か `None` にしてください",
                        "the value in `[value; N]` must be copyable (integers, bool, Id, Str ...) or `None`",
                    ));
                }
                if cnt == 0 {
                    self.err(Diag::error("F2040", n.span, "長さ 0 の配列は使えません", "zero-length arrays are not allowed"));
                }
                Ty::Array(Box::new(elem), cnt.max(1))
            }
            ExprKind::Own(inner) => {
                if self.in_bypass > 0 {
                    self.err(Diag::error("F0811", e.span, "volatile_bypass の中では own の割り当てはできません", "own allocations are not allowed inside volatile_bypass"));
                }
                let iexp = match &exp_r {
                    Some(Ty::Own(t)) => Some((**t).clone()),
                    _ => None,
                };
                let it = self.check_expr(inner, iexp.as_ref());
                let it = self.resolve(&it);
                if let Some(ie) = &iexp {
                    // `own 5` が `own u8` の場所にあるなら中身は u8 / `own 5` where an `own u8` is expected holds a u8
                    if !matches!(it, Ty::Ref(..)) {
                        self.unify(ie, &it);
                    }
                }
                let it = self.resolve(&it);
                // 所有リージョンは 16 バイト境界なので、それより強い整列の型は置けない
                // own regions are 16-byte aligned, so a type that needs more cannot live in one
                if !matches!(it, Ty::Err | Ty::Ref(..)) {
                    let al = self.t.type_align_attr(&it);
                    if al > 16 {
                        let d = self.t.display(&it);
                        self.err(
                            Diag::error(
                                "F2032",
                                e.span,
                                format!("`{}` は {} バイト境界が要るので own の所有リージョン（16 バイト境界）には置けません", d, al),
                                format!("`{}` needs {}-byte alignment and cannot live in an own region (16-byte aligned)", d, al),
                            )
                            .hint(
                                "アリーナの要素か、ふつうの値（ローカル変数・構造体のフィールド）として使ってください",
                                "use it as an arena element or as a plain value (a local or a struct field)",
                            ),
                        );
                    }
                }
                match it {
                    Ty::Err => Ty::Err,
                    // 参照を own に入れると、借りただけの場所を呼び出しより長く生きる所有リージョンから指すことになる
                    // boxing a reference would let an own region (which can outlive the call) point at a borrowed place
                    Ty::Ref(..) => {
                        let d = self.t.display(&it);
                        self.err(
                            Diag::error(
                                "F0515",
                                inner.span,
                                format!("参照 `{}` を own に入れることはできません（第二級参照）", d),
                                format!("the reference `{}` cannot be put into an own (second-class references)", d),
                            )
                            .hint("中身をコピーして入れる（`own *x`）か、`.clone()` で複製してください", "copy the contents in (`own *x`), or duplicate it with `.clone()`"),
                        );
                        Ty::Err
                    }
                    ref t if Tables::ownable(t) => Ty::Own(Box::new(it)),
                    other => {
                        let d = self.t.display(&other);
                        self.err(Diag::error(
                            "F2301",
                            e.span,
                            format!("`own` は `{}` を所有できません（own の中身は値の型: 構造体・整数・bool・Str・Id・配列・Option）", d),
                            format!("`own` cannot own `{}` (an own holds a value type: a struct, an integer, bool, Str, an Id, an array or an Option)", d),
                        ));
                        Ty::Err
                    }
                }
            }
            ExprKind::Unary(op, x) => match op {
                UnOp::Neg => {
                    if let ExprKind::Int(..) = &x.kind {
                        self.neg_lits.insert(x.id);
                    }
                    let xt = self.check_expr(x, exp_r.as_ref().filter(|t| t.is_int() || t.is_float()));
                    let r = self.want_int(&xt);
                    match r {
                        Ty::Float(_) | Ty::FloatVar(_) => r,
                        Ty::Int(it) if !it.signed() && !matches!(x.kind, ExprKind::Int(..)) => {
                            self.err(Diag::error("F0600", e.span, format!("符号なし整数 `{}` に単項 `-` は使えません", it.name()), format!("unary `-` cannot be applied to the unsigned type `{}`", it.name())));
                            Ty::Err
                        }
                        Ty::Int(_) | Ty::IntVar(_) | Ty::Err => r,
                        other => {
                            let d = self.t.display(&other);
                            self.err(Diag::error("F0600", e.span, format!("`{}` に単項 `-` は使えません", d), format!("unary `-` cannot be applied to `{}`", d)));
                            Ty::Err
                        }
                    }
                }
                UnOp::Not => {
                    let xt = self.check_expr(x, exp_r.as_ref().filter(|t| t.is_int() || **t == Ty::Bool));
                    let r = self.resolve(&xt);
                    if self.unknown_here(&r, x.span, false) {
                        return Ty::Err;
                    }
                    match r {
                        Ty::Bool | Ty::Int(_) | Ty::IntVar(_) | Ty::Err => r,
                        other => {
                            let d = self.t.display(&other);
                            self.err(Diag::error("F0600", e.span, format!("`{}` に `!` は使えません", d), format!("`!` cannot be applied to `{}`", d)));
                            Ty::Err
                        }
                    }
                }
            },
            ExprKind::Binary(op, a, b) => self.check_binary(e, *op, a, b, exp_r.as_ref()),
            ExprKind::Cast(x, te) => {
                let target = self.resolve_type(te, false, &HashMap::new());
                let xt = self.check_expr(x, None);
                let xr = self.resolve(&xt);
                if self.unknown_here(&xr, x.span, false) {
                    return target;
                }
                let ok = match (&xr, &target) {
                    (Ty::Err, _) | (_, Ty::Err) => true,
                    (Ty::Int(_) | Ty::IntVar(_), Ty::Int(_)) => true,
                    // 整数 ↔ 浮動小数点・f32 ↔ f64（浮動小数点から整数へは飽和、NaN は 0。Rust と同じ）
                    // integer <-> float and f32 <-> f64 (float to integer saturates, NaN becomes 0, as in Rust)
                    (Ty::Int(_) | Ty::IntVar(_), Ty::Float(_)) => true,
                    (Ty::Float(_) | Ty::FloatVar(_), Ty::Int(_) | Ty::Float(_)) => true,
                    (Ty::Bool, Ty::Int(_)) => true,
                    (Ty::Id(a), Ty::Int(it)) => *it == if self.t.arenas[*a].bump { IntTy::U64 } else { IntTy::U32 },
                    _ => false,
                };
                if !ok {
                    let (a, b) = (self.t.display(&xr), self.t.display(&target));
                    let mut d = Diag::error("F0605", e.span, format!("`{}` を `as {}` で変換できません", a, b), format!("`{}` cannot be cast with `as {}`", a, b));
                    if let Ty::Own(_) = xr {
                        d = d.hint("own の中身は `*` で読みます（例: `*ctx.buffer as i64`）", "read the contents of an own with `*` (e.g. `*ctx.buffer as i64`)");
                    }
                    self.err(d);
                }
                target
            }
            ExprKind::Ref { mutable, inner } => {
                self.err(
                    Diag::error(
                        "F0515",
                        e.span,
                        "参照 `&` は関数呼び出しの引数としてだけ作れます（第二級参照）",
                        "a reference `&` can only be created as a call argument (second-class references)",
                    )
                    .hint("`f(&x)` のように呼び出しの中で直接渡してください", "pass it directly inside the call, as in `f(&x)`"),
                );
                let it = self.check_expr(inner, None);
                Ty::Ref(*mutable, Box::new(it))
            }
            ExprKind::Assign { op, lhs, rhs } => {
                let lt = self.check_expr(lhs, None);
                let lt = self.resolve(&lt);
                // `p = v`（`*p = v`）: 参照の束縛への代入は参照先への書き込み / assigning to a reference binding writes through it
                let lt = match (&lhs.kind, lt) {
                    (ExprKind::Path(_), Ty::Ref(_, inner)) => *inner,
                    (_, t) => t,
                };
                match op {
                    None => {
                        let rt = self.check_expr(rhs, Some(&lt));
                        self.expect_ty(rhs.span, &lt, &rt);
                    }
                    Some(bop) => {
                        // 複合代入の左辺は整数（ビット演算なら bool も）: まだ決まっていなければ整数に決める
                        // the left side of a compound assignment is an integer (or bool for bit ops); an unknown type becomes an integer
                        let lt = if bop.is_bit() { lt } else { self.want_int(&lt) };
                        if self.unknown_here(&lt, lhs.span, false) {
                            return Ty::Unit;
                        }
                        if bop.is_shift() {
                            let rt = self.check_expr(rhs, None);
                            let rt = self.want_int(&rt);
                            if !rt.is_int() && !rt.is_err() {
                                self.err(Diag::error("F0308", rhs.span, "シフト量は整数です", "the shift amount must be an integer"));
                            }
                            if !lt.is_int() && !lt.is_err() {
                                let d = self.t.display(&lt);
                                self.err(Diag::error("F0368", e.span, format!("`{}` に `{}=` は使えません", d, bop.text()), format!("`{}=` cannot be applied to `{}`", bop.text(), d)));
                            }
                        } else {
                            let rt = self.check_expr(rhs, Some(&lt));
                            self.expect_ty(rhs.span, &lt, &rt);
                            let lt = self.resolve(&lt);
                            let float_ok = lt.is_float() && matches!(bop, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
                            let ok = lt.is_int() || lt.is_err() || (lt == Ty::Bool && bop.is_bit()) || float_ok;
                            if !ok {
                                let d = self.t.display(&lt);
                                self.err(Diag::error("F0368", e.span, format!("`{}` に `{}=` は使えません", d, bop.text()), format!("`{}=` cannot be applied to `{}`", bop.text(), d)));
                            }
                        }
                    }
                }
                Ty::Unit
            }
            ExprKind::If { cond, then, els } => {
                let ct = self.check_expr(cond, Some(&Ty::Bool));
                self.expect_ty(cond.span, &Ty::Bool, &ct);
                self.check_branches(e, then, els.as_deref(), exp_r.as_ref())
            }
            ExprKind::IfLet { pat, scrut, then, els } => {
                let st = self.check_scrutinee(scrut);
                self.scopes.push(HashMap::new());
                self.check_pattern(pat, &st, scrut);
                let r = self.check_branches(e, then, els.as_deref(), exp_r.as_ref());
                self.scopes.pop();
                if matches!(pat, Pattern::Bind(..) | Pattern::Wild(_)) {
                    self.err(Diag::warning("F0001", pat.span(), "このパターンは必ず一致します（`let` を使ってください）", "this pattern always matches (use `let`)"));
                }
                r
            }
            ExprKind::Match { scrut, arms } => {
                let st = self.check_scrutinee(scrut);
                // `e?`（パーサが match に脱糖衣した）: e は Result、関数は Result を返す / `e?` (desugared into a match by the
                // parser): e must be a Result and the function must return a Result
                if self.prog.question_marks.contains(&e.id) {
                    let sr = self.resolve(&st);
                    let cr = self.resolve(&self.cur_ret.clone());
                    if !matches!(sr, Ty::Result(..) | Ty::Err | Ty::Var(_)) {
                        let d = self.t.display(&sr);
                        self.err(
                            Diag::error("F3105", e.span, format!("`?` は Result にだけ使えます（`{}` は不可）", d), format!("`?` only applies to a Result (not `{}`)", d))
                                .hint("Option は `if let Some(x) = .. { .. } else { return Err(EINVAL); }` のように書いてください", "for an Option write `if let Some(x) = .. { .. } else { return Err(EINVAL); }`"),
                        );
                        return Ty::Err;
                    }
                    if !matches!(cr, Ty::Result(..) | Ty::Err) {
                        let d = self.t.display(&cr);
                        self.err(
                            Diag::error("F3105", e.span, format!("`?` は Result を返す関数の中でだけ使えます（この関数は `{}` を返す）", d), format!("`?` can only be used in a function that returns a Result (this one returns `{}`)", d))
                                .hint("失敗を自分で扱うなら `match e { Ok(v) => .., Err(x) => .. }` と書きます", "to handle the failure here, write `match e { Ok(v) => .., Err(x) => .. }`"),
                        );
                        return Ty::Err;
                    }
                }
                let mut result: Option<Ty> = exp_r.clone();
                let mut all_never = true;
                for arm in arms {
                    self.scopes.push(HashMap::new());
                    self.check_pattern(&arm.pat, &st, scrut);
                    let at = self.check_expr(&arm.body, result.as_ref());
                    self.scopes.pop();
                    if at != Ty::Never {
                        all_never = false;
                    }
                    match &result {
                        Some(r) => {
                            let r = r.clone();
                            if !self.unify(&r, &at) {
                                self.mismatch(arm.body.span, &r, &at);
                            }
                            if r == Ty::Never {
                                result = Some(at);
                            }
                        }
                        None => {
                            if at != Ty::Never {
                                result = Some(at);
                            }
                        }
                    }
                }
                self.check_exhaustive(e.span, &st, arms);
                if arms.is_empty() || all_never {
                    if arms.is_empty() {
                        Ty::Unit
                    } else {
                        Ty::Never
                    }
                } else {
                    result.unwrap_or(Ty::Unit)
                }
            }
            ExprKind::While { cond, body } => {
                let ct = self.check_expr(cond, Some(&Ty::Bool));
                self.expect_ty(cond.span, &Ty::Bool, &ct);
                self.loop_breaks.push(false);
                let bt = self.check_block(body, Some(&Ty::Unit));
                self.loop_breaks.pop();
                self.expect_unit_body(body, &bt);
                Ty::Unit
            }
            ExprKind::Loop { body } => {
                self.loop_breaks.push(false);
                let bt = self.check_block(body, Some(&Ty::Unit));
                let broke = self.loop_breaks.pop().unwrap_or(false);
                self.expect_unit_body(body, &bt);
                if broke {
                    Ty::Unit
                } else {
                    Ty::Never
                }
            }
            ExprKind::For { var, var_span, bind, iter, body } => {
                self.scopes.push(HashMap::new());
                let vt = match iter {
                    ForIter::Range { lo, hi, .. } => {
                        let lt = self.check_expr(lo, None);
                        let ht = self.check_expr(hi, Some(&lt));
                        self.expect_ty(hi.span, &lt, &ht);
                        let r = self.want_int(&lt);
                        if !r.is_int() && !r.is_err() {
                            let d = self.t.display(&r);
                            self.err(Diag::error("F0308", lo.span, format!("範囲は整数です（`{}` は不可）", d), format!("a range must be of integers (not `{}`)", d)));
                        }
                        r
                    }
                    ForIter::Arena(a, sp) => match self.t.regions.get(a) {
                        Some(RegionRef::Arena(x)) if self.t.arenas[*x].gpu => {
                            self.err(
                                Diag::error("F2706", *sp, format!("gpu_arena `{}` は for で回せません（装置へ貸し出し中のスロットがあり、スロットに届くのは移動専用のハンドル `GpuBuf<{}>` だけです）", a, a), format!("the gpu_arena `{}` cannot be iterated (some slots may be lent to the device; slots are reached only through the move-only handle `GpuBuf<{}>`)", a, a))
                                    .hint("ハンドルを自分の表（アリーナや配列）に持ち、そこを回してください", "keep the handles in a table of your own (an arena or an array) and iterate that"),
                            );
                            Ty::Err
                        }
                        Some(RegionRef::Arena(x)) if self.t.arenas[*x].dma => {
                            self.err(
                                Diag::error("F2703", *sp, format!("dma_arena `{}` は for で回せません（スロットに届くのは移動専用のハンドル `Dma<{}>` だけです）", a, a), format!("the dma_arena `{}` cannot be iterated (its slots are reached only through the move-only handle `Dma<{}>`)", a, a))
                                    .hint("ハンドルを自分の表（アリーナや配列）に持ち、そこを回してください", "keep the handles in a table of your own (an arena or an array) and iterate that"),
                            );
                            Ty::Err
                        }
                        Some(RegionRef::Arena(x)) => {
                            let x = *x;
                            if self.prog.par_fors.contains(&e.id) {
                                // 並列走査は共有アリーナだけ（コアごとのアリーナは自分のコアしか見えない）
                                // parallel scans are for shared arenas (a per-core arena is visible to its own core only)
                                if !self.t.arenas[x].shared {
                                    self.err(
                                        Diag::error("F2612", *sp, format!("`{}.par()` は共有アリーナにだけ使えます（`{}` はコアごとのアリーナで、ほかのコアからは見えません）", a, a), format!("`{}.par()` is for shared arenas only (`{}` is a per-core arena that no other core can see)", a, a))
                                            .hint(format!("`shared arena {}: ..;` と宣言するか、ふつうの `for id in {}` で回してください", a, a), format!("declare `shared arena {}: ..;`, or iterate with a plain `for id in {}`", a, a)),
                                    );
                                } else {
                                    self.par_fors.insert(e.id, x);
                                }
                            }
                            Ty::Id(x)
                        }
                        Some(RegionRef::Mmio(_)) | Some(RegionRef::Port(_)) | Some(RegionRef::Dma(_)) | Some(RegionRef::UserMem(_)) | Some(RegionRef::Chan(_)) => {
                            self.err(Diag::error("F2204", *sp, format!("`{}` は for で回せません（範囲 `0..N` で添字を回してください）", a), format!("`{}` cannot be iterated with for (iterate an index range `0..N`)", a)));
                            Ty::Err
                        }
                        None => {
                            self.err(
                                Diag::error("F2104", *sp, format!("アリーナ `{}` が見つかりません", a), format!("cannot find arena `{}`", a))
                                    .hint("`for` は範囲（`a..b` / `a..=b`）かアリーナを回します", "`for` iterates over a range (`a..b` / `a..=b`) or an arena"),
                            );
                            Ty::Err
                        }
                    },
                };
                self.declare_local(var, *bind, LocalInfo { name: var.clone(), ty: vt, mutable: false, span: *var_span, kind: LocalKind::ForVar });
                self.loop_breaks.push(false);
                let bt = self.check_block(body, Some(&Ty::Unit));
                self.loop_breaks.pop();
                self.scopes.pop();
                self.expect_unit_body(body, &bt);
                Ty::Unit
            }
            ExprKind::Break | ExprKind::Continue => {
                if self.loop_breaks.is_empty() {
                    let w = if matches!(e.kind, ExprKind::Break) { "break" } else { "continue" };
                    self.err(Diag::error("F0268", e.span, format!("`{}` はループの中でだけ使えます", w), format!("`{}` can only be used inside a loop", w)));
                } else if matches!(e.kind, ExprKind::Break) {
                    *self.loop_breaks.last_mut().unwrap() = true;
                }
                Ty::Never
            }
            ExprKind::Return(v) => {
                let ret = self.cur_ret.clone();
                match v {
                    Some(v) => {
                        let vt = self.check_expr(v, Some(&ret));
                        self.expect_ty(v.span, &ret, &vt);
                    }
                    None => {
                        if !self.unify(&ret, &Ty::Unit) {
                            let d = self.t.display(&ret);
                            self.err(Diag::error("F0069", e.span, format!("`return` に値（`{}`）が必要です", d), format!("`return` needs a value of type `{}`", d)));
                        }
                    }
                }
                Ty::Never
            }
            ExprKind::Critical(b) => {
                if self.user_mode {
                    self.user_ban(e.span, "critical");
                }
                self.check_block(b, exp_r.as_ref())
            }
            ExprKind::Block(b) => self.check_block(b, exp_r.as_ref()),
            // `atomic { 場所 op= 値; }`: 形は F3201 で検査（1 つのスロットの整数フィールドへの RMW 1 回だけ）
            // the shape is checked by F3201 (exactly one RMW on an integer field of one slot)
            // 値の形（`;` の無い 1 式）: 更新前の値を返す（fetch_or / fetch_add / exchange）。場所だけなら不可分な読み出し
            // the value form (one expression without `;`): yields the value before the update (fetch_or / fetch_add /
            // exchange); a bare place is an indivisible load
            ExprKind::Atomic(b) => {
                let t = self.check_block(b, None);
                self.check_atomic_shape(e, b);
                match (&b.tail, b.stmts.is_empty()) {
                    (Some(x), true) => match &x.kind {
                        ExprKind::Assign { lhs, .. } => self.resolve(&self.expr_ty[lhs.id as usize].clone()),
                        _ => t,
                    },
                    _ => t,
                }
            }
            // `rcu_read { .. }`: 書き込み・解放の禁止（F3202）は効果が求まった後で検査する / writes and frees (F3202) are
            // checked once the effects are known
            ExprKind::RcuRead(b) => self.check_block(b, exp_r.as_ref()),
            // `catch_fault { .. }` → Result<T, FaultError>
            ExprKind::CatchFault(b) => {
                self.catch_sites.insert(e.id);
                let iexp = match &exp_r {
                    Some(Ty::Result(t, _)) => Some((**t).clone()),
                    _ => None,
                };
                let t = self.check_block(b, iexp.as_ref());
                let t = if t == Ty::Never { iexp.unwrap_or(Ty::Unit) } else { t };
                Ty::Result(Box::new(t), Box::new(Ty::Fault))
            }
            ExprKind::TypeCall { name, name_span, ty, args } => self.check_type_call(e, name, *name_span, ty, args),
            ExprKind::VolatileBypass(b) => {
                self.in_bypass += 1;
                let t = self.check_block(b, exp_r.as_ref());
                self.in_bypass -= 1;
                t
            }
            ExprKind::Unsupported(what) => {
                self.err(Diag::error(
                    "F2900",
                    e.span,
                    format!("`{}` は 0.1 コアの機能です。1.0 構文（arena / own / mmio）を使うファイルではまだ使えません（スレッドはアリーナと併用できる次版で統合予定）", what),
                    format!("`{}` is a 0.1-core feature and is not yet available in files that use 1.0 syntax (arena / own / mmio)", what),
                ));
                Ty::Err
            }
        }
    }

    fn expect_unit_body(&mut self, body: &Block, bt: &Ty) {
        let r = self.resolve(bt);
        if r != Ty::Unit && r != Ty::Never && !r.is_err() {
            let sp = body.tail.as_ref().map(|t| t.span).unwrap_or(body.span);
            self.mismatch(sp, &Ty::Unit, &r);
        }
    }

    fn check_branches(&mut self, e: &Expr, then: &Block, els: Option<&Expr>, exp: Option<&Ty>) -> Ty {
        let tt = self.check_block(then, exp);
        match els {
            Some(el) => {
                let hint = if tt == Ty::Never { exp.cloned() } else { Some(tt.clone()) };
                let et = self.check_expr(el, hint.as_ref());
                if tt == Ty::Never {
                    return et;
                }
                if et == Ty::Never {
                    return tt;
                }
                if !self.unify(&tt, &et) {
                    self.mismatch(el.span, &tt, &et);
                }
                tt
            }
            None => {
                let r = self.resolve(&tt);
                if r != Ty::Unit && r != Ty::Never && !r.is_err() {
                    let d = self.t.display(&r);
                    self.err(Diag::error(
                        "F0317",
                        e.span,
                        format!("`else` の無い if は値（`{}`）を返せません", d),
                        format!("an `if` without `else` cannot produce a value (`{}`)", d),
                    ));
                }
                Ty::Unit
            }
        }
    }

    /// if let / match の対象。アリーナのルックアップ（`@a.get(id)`）はここでだけ許される
    /// the scrutinee of if let / match: arena lookups (`@a.get(id)`) are allowed only here
    fn check_scrutinee(&mut self, scrut: &Expr) -> Ty {
        if let ExprKind::MethodCall { recv, method, method_span, args } = &scrut.kind {
            if let ExprKind::ArenaRef(_) = &recv.kind {
                if method == "get" || method == "get_mut" {
                    let t = self.check_method(scrut, recv, method, *method_span, args, None, true);
                    self.expr_ty[scrut.id as usize] = t.clone();
                    self.lookup_scrut.insert(scrut.id);
                    return t;
                }
            }
        }
        // 窓とビュー: `if let Some(v) = x.view(T, off)` / `x.window(off, len)`
        if let ExprKind::MethodCall { recv, method, method_span, args } = &scrut.kind {
            if method == "of" && matches!(&recv.kind, ExprKind::ArenaRef(n) if matches!(self.t.regions.get(n), Some(RegionRef::UserMem(_)))) {
                let t = self.check_method(scrut, recv, method, *method_span, args, None, true);
                self.expr_ty[scrut.id as usize] = t.clone();
                if matches!(self.methods.get(&scrut.id), Some(MethodKind::UserMemOf(_))) {
                    self.lookup_scrut.insert(scrut.id);
                }
                return t;
            }
            if matches!(method.as_str(), "view" | "view_mut" | "window" | "window_mut") && !matches!(recv.kind, ExprKind::ArenaRef(_)) {
                let t = self.check_method(scrut, recv, method, *method_span, args, None, true);
                self.expr_ty[scrut.id as usize] = t.clone();
                if matches!(self.methods.get(&scrut.id), Some(MethodKind::View(..)) | Some(MethodKind::Window(_))) {
                    self.lookup_scrut.insert(scrut.id);
                }
                return t;
            }
        }
        let t = self.check_expr(scrut, None);
        let r = self.resolve(&t);
        // `match b { .. }` / `if let Some(x) = b` の b が構造体でない own なら中身を照合する（`*b` と同じ）
        // a non-struct own scrutinee is matched by its contents (the same as `*b`)
        let inner = match r.strip_ref() {
            Ty::Own(x) if !matches!(**x, Ty::Struct(_)) => Some((**x).clone()),
            _ => None,
        };
        if let Some(x) = inner {
            self.own_derefs.insert(scrut.id);
            self.expr_ty[scrut.id as usize] = x.clone();
            return x;
        }
        r
    }

    /// パターンの型付けと束縛 / type a pattern and declare its bindings
    fn check_pattern(&mut self, pat: &Pattern, st: &Ty, scrut: &Expr) {
        let st = self.resolve(st);
        let mut inner_st = st.strip_ref().clone();
        // まだ決まっていない対象: パターンの形から決める（整数 / bool / Option） / an unknown scrutinee takes the pattern's shape
        if let Ty::Var(_) = inner_st {
            let shape = match pat {
                Pattern::Int(..) => Some(self.new_ivar()),
                Pattern::Bool(..) => Some(Ty::Bool),
                Pattern::None(_) | Pattern::Some(..) => {
                    let v = self.new_var();
                    Some(Ty::Option(Box::new(v)))
                }
                Pattern::Ok(..) | Pattern::Err(..) => {
                    let (a, b) = (self.new_var(), self.new_var());
                    Some(Ty::Result(Box::new(a), Box::new(b)))
                }
                _ => None,
            };
            if let Some(sh) = shape {
                self.unify(&inner_st, &sh);
                inner_st = self.resolve(&sh);
            }
        }
        match pat {
            Pattern::Wild(_) => {}
            Pattern::Int(v, sp) => {
                if !inner_st.is_int() && !inner_st.is_err() {
                    let d = self.t.display(&inner_st);
                    self.err(Diag::error("F0308", *sp, format!("整数のパターンは `{}` に一致しません", d), format!("an integer pattern cannot match `{}`", d)));
                } else if let Ty::Int(it) = self.resolve(&inner_st) {
                    let (lo, hi) = it.min_max(self.t.ptr_bits);
                    if *v < lo || *v > hi {
                        self.err(Diag::error("F0308", *sp, format!("パターン {} は `{}` の範囲外です", v, it.name()), format!("pattern {} is out of range for `{}`", v, it.name())));
                    }
                }
            }
            Pattern::Bool(_, sp) => {
                if inner_st != Ty::Bool && !inner_st.is_err() {
                    let d = self.t.display(&inner_st);
                    self.err(Diag::error("F0308", *sp, format!("bool のパターンは `{}` に一致しません", d), format!("a bool pattern cannot match `{}`", d)));
                }
            }
            Pattern::None(sp) => {
                if !matches!(inner_st, Ty::Option(_) | Ty::Err) {
                    let d = self.t.display(&inner_st);
                    self.err(Diag::error("F0308", *sp, format!("`None` は Option 型にだけ一致します（`{}` は不可）", d), format!("`None` only matches an Option (not `{}`)", d)));
                }
            }
            Pattern::Some(b, sp) => {
                let payload = match &inner_st {
                    Ty::Option(i) => (**i).clone(),
                    Ty::Err => Ty::Err,
                    other => {
                        let d = self.t.display(other);
                        self.err(Diag::error("F0308", *sp, format!("`Some(..)` は Option 型にだけ一致します（`{}` は不可）", d), format!("`Some(..)` only matches an Option (not `{}`)", d)));
                        Ty::Err
                    }
                };
                if let Some((name, bind, mutable)) = b {
                    let (ty, kind) = self.binding_mode(&payload, scrut, *mutable);
                    self.declare_local(name, *bind, LocalInfo { name: name.clone(), ty, mutable: *mutable, span: *sp, kind });
                }
            }
            Pattern::Bind(name, bind, sp) => {
                let (ty, kind) = self.binding_mode(&inner_st, scrut, false);
                self.declare_local(name, *bind, LocalInfo { name: name.clone(), ty, mutable: false, span: *sp, kind });
            }
            // `Ok(x)`: 成功値（Some と同じ受け方）/ `Err(e)`: 失敗値（コピーできる型なのでいつも値で受ける）
            // `Ok(x)`: the success value (bound like Some) / `Err(e)`: the failure value (copyable, always by value)
            Pattern::Ok(b, sp) | Pattern::Err(b, sp) => {
                let is_ok = matches!(pat, Pattern::Ok(..));
                let payload = match &inner_st {
                    Ty::Result(t, e) => {
                        if is_ok {
                            (**t).clone()
                        } else {
                            (**e).clone()
                        }
                    }
                    Ty::Err => Ty::Err,
                    other => {
                        let d = self.t.display(other);
                        self.err(Diag::error("F0308", *sp, format!("`{}(..)` は Result 型にだけ一致します（`{}` は不可）", if is_ok { "Ok" } else { "Err" }, d), format!("`{}(..)` only matches a Result (not `{}`)", if is_ok { "Ok" } else { "Err" }, d)));
                        Ty::Err
                    }
                };
                if let Some((name, bind, mutable)) = b {
                    let (ty, kind) = if is_ok { self.binding_mode(&payload, scrut, *mutable) } else { (self.resolve(&payload), LocalKind::PatValue) };
                    self.declare_local(name, *bind, LocalInfo { name: name.clone(), ty, mutable: *mutable, span: *sp, kind });
                }
            }
        }
    }

    /// 束縛の受け方: ルックアップ → 参照、場所 + コピー不可 → 参照、それ以外 → 値
    /// binding mode: lookup → reference; place + non-copy → reference; otherwise → by value
    fn binding_mode(&mut self, payload: &Ty, scrut: &Expr, mutable_bind: bool) -> (Ty, LocalKind) {
        let payload = self.resolve(payload);
        // 値で受けるか参照で受けるかは型で決まるので、ここでは型が要る / by value or by reference depends on the type
        if matches!(payload, Ty::Var(_)) && is_place_expr(scrut) && self.unknown_here(&payload, scrut.span, false) {
            return (Ty::Err, LocalKind::PatValue);
        }
        if let Ty::Ref(m, inner) = &payload {
            // ルックアップの結果 / the result of a lookup
            return (Ty::Ref(*m, inner.clone()), LocalKind::PatRef);
        }
        if self.t.is_copy(&payload) {
            return (payload, LocalKind::PatValue);
        }
        if is_place_expr(scrut) {
            let m = self.place_is_mutable(scrut) || mutable_bind;
            return (Ty::Ref(m, Box::new(payload)), LocalKind::PatRef);
        }
        (payload, LocalKind::PatValue)
    }

    /// 場所が書き換え可能か（mut 変数・&mut 引数・アリーナのスロット） / is a place writable?
    pub fn place_is_mutable(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Path(_) => match self.res.get(&e.id) {
                Some(Res::Local(b)) => match &self.locals[*b as usize] {
                    Some(l) => match &l.ty {
                        Ty::Ref(m, _) => *m,
                        _ => l.mutable,
                    },
                    None => false,
                },
                _ => false,
            },
            ExprKind::Field(b, _, _) => self.place_is_mutable(b),
            ExprKind::Index(b, _) => match &b.kind {
                ExprKind::ArenaRef(_) => true,
                _ => self.place_is_mutable(b),
            },
            _ => false,
        }
    }

    fn check_exhaustive(&mut self, span: Span, st: &Ty, arms: &[Arm]) {
        let st = self.resolve(st);
        let inner = st.strip_ref().clone();
        let has_catch = arms.iter().any(|a| matches!(a.pat, Pattern::Wild(_) | Pattern::Bind(..)));
        if has_catch || inner.is_err() {
            return;
        }
        let ok = match &inner {
            Ty::Bool => {
                let t = arms.iter().any(|a| matches!(a.pat, Pattern::Bool(true, _)));
                let f = arms.iter().any(|a| matches!(a.pat, Pattern::Bool(false, _)));
                t && f
            }
            Ty::Option(_) => {
                let s = arms.iter().any(|a| matches!(a.pat, Pattern::Some(..)));
                let n = arms.iter().any(|a| matches!(a.pat, Pattern::None(_)));
                s && n
            }
            Ty::Result(..) => {
                let o = arms.iter().any(|a| matches!(a.pat, Pattern::Ok(..)));
                let r = arms.iter().any(|a| matches!(a.pat, Pattern::Err(..)));
                o && r
            }
            Ty::Int(_) | Ty::IntVar(_) => false,
            _ => true,
        };
        if !ok {
            self.err(
                Diag::error("F0004", span, "match が網羅的ではありません（すべての場合を扱ってください）", "non-exhaustive match (all cases must be handled)")
                    .hint("`_ => ..` のアームを追加してください", "add a `_ => ..` arm"),
            );
        }
    }

    fn check_binary(&mut self, e: &Expr, op: BinOp, a: &Expr, b: &Expr, exp: Option<&Ty>) -> Ty {
        match op {
            BinOp::And | BinOp::Or => {
                let at = self.check_expr(a, Some(&Ty::Bool));
                self.expect_ty(a.span, &Ty::Bool, &at);
                let bt = self.check_expr(b, Some(&Ty::Bool));
                self.expect_ty(b.span, &Ty::Bool, &bt);
                Ty::Bool
            }
            _ if op.is_cmp() => {
                let at = self.check_expr(a, None);
                let bt = self.check_expr(b, Some(&at));
                // None との比較などで左辺の型が後から決まる場合 / the left type may only be known from the right
                if !self.unify(&at, &bt) {
                    self.mismatch(b.span, &at, &bt);
                    return Ty::Bool;
                }
                // 大小の比較は整数だけ / ordering comparisons are for integers only
                let t = if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) { self.want_int(&at) } else { self.resolve(&at) };
                if self.unknown_here(&t, e.span, false) {
                    return Ty::Bool;
                }
                let ok = match &t {
                    Ty::Int(_) | Ty::IntVar(_) | Ty::Err | Ty::Never => true,
                    // 浮動小数点は IEEE-754 の比較（NaN はどれとも等しくなく、大小もない） / IEEE-754 comparisons (NaN
                    // equals nothing and is unordered)
                    Ty::Float(_) | Ty::FloatVar(_) => true,
                    Ty::Bool | Ty::Id(_) | Ty::Str => matches!(op, BinOp::Eq | BinOp::Ne),
                    // 整数の配列の一致（MAC アドレスなど） / equality of integer arrays (MAC addresses and the like)
                    Ty::Array(el, _) => matches!(op, BinOp::Eq | BinOp::Ne) && el.is_int(),
                    // 窓の一致（長さと中身） / equality of windows (length and contents)
                    Ty::Slice(_) => matches!(op, BinOp::Eq | BinOp::Ne),
                    Ty::Ref(_, i) if matches!(**i, Ty::Slice(_)) => matches!(op, BinOp::Eq | BinOp::Ne),
                    Ty::Option(i) => matches!(op, BinOp::Eq | BinOp::Ne) && matches!(**i, Ty::Id(_) | Ty::Int(_) | Ty::Bool | Ty::Err),
                    _ => false,
                };
                if !ok {
                    let d = self.t.display(&t);
                    self.err(Diag::error("F0369", e.span, format!("`{}` 同士を `{}` で比較できません", d, op.text()), format!("`{}` values cannot be compared with `{}`", d, op.text())));
                }
                Ty::Bool
            }
            _ if op.is_shift() => {
                let at = self.check_expr(a, exp.filter(|t| t.is_int()));
                let bt = self.check_expr(b, None);
                let ar = self.want_int(&at);
                let bt = self.want_int(&bt);
                if !ar.is_int() && !ar.is_err() {
                    let d = self.t.display(&ar);
                    self.err(Diag::error("F0369", a.span, format!("`{}` はシフトできません", d), format!("`{}` cannot be shifted", d)));
                    return Ty::Err;
                }
                let br = self.resolve(&bt);
                if !br.is_int() && !br.is_err() {
                    self.err(Diag::error("F0308", b.span, "シフト量は整数です", "the shift amount must be an integer"));
                }
                ar
            }
            _ => {
                let at = self.check_expr(a, exp.filter(|t| t.is_int() || (t.is_float() && !op.is_bit()) || (**t == Ty::Bool && op.is_bit())));
                let bt = self.check_expr(b, Some(&at));
                if !self.unify(&at, &bt) {
                    self.mismatch(b.span, &at, &bt);
                    return Ty::Err;
                }
                // 四則演算は整数と浮動小数点（ビット演算は bool もあるので、決まっていなければ待つ）
                // arithmetic is for integers and floats (bit operations also take bool, so an unknown type waits)
                let t = if op.is_bit() { self.resolve(&at) } else { self.want_int(&at) };
                if self.unknown_here(&t, e.span, false) {
                    return Ty::Err;
                }
                // 浮動小数点は + - * / だけ（% は C の fmod が要るので使わない。ビット演算は to_bits で整数にしてから）
                // floats take + - * / only (% would need C fmod; bit operations go through to_bits)
                let float_ok = t.is_float() && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
                let ok = t.is_int() || t.is_err() || (t == Ty::Bool && op.is_bit()) || float_ok;
                if !ok {
                    let d = self.t.display(&t);
                    let mut dg = Diag::error("F0369", e.span, format!("`{}` に `{}` は使えません", d, op.text()), format!("`{}` cannot be applied to `{}`", op.text(), d));
                    if t.is_float() {
                        dg = dg.hint("浮動小数点に使える演算は + - * / と比較です。ビットを扱うなら `x.to_bits()` で整数にします", "floats take + - * / and comparisons; to work on the bits, turn the value into an integer with `x.to_bits()`");
                    }
                    self.err(dg);
                    return Ty::Err;
                }
                t
            }
        }
    }

    fn check_index(&mut self, e: &Expr, base: &Expr, idx: &Expr) -> Ty {
        // アリーナ `@a[id]` と MMIO `@m[i]` / arena `@a[id]` and MMIO `@m[i]`
        if let ExprKind::ArenaRef(name) = &base.kind {
            match self.t.regions.get(name).copied() {
                // dma_arena: 添字はハンドル `Dma<名前>`（CPU が持っている間だけスロットに届く）
                // a dma_arena is indexed by its handle `Dma<name>` (the slot is reachable only while the CPU owns it)
                // gpu_arena: 添字はハンドル `GpuBuf<名前>`（CPU が持っている間だけ。dispatch で消え、reclaim で戻る）
                // a gpu_arena is indexed by its handle `GpuBuf<name>` (only while the CPU owns it: dispatch consumes it,
                // reclaim gives it back)
                Some(RegionRef::Arena(a)) if self.t.arenas[a].gpu => {
                    let it = self.check_expr(idx, Some(&Ty::GpuBuf(a)));
                    let ir = self.resolve(&it);
                    self.gpu_handle_ok(idx.span, a, &ir, false);
                    self.arena_index.insert(e.id, a);
                    return self.t.arenas[a].elem.clone();
                }
                Some(RegionRef::Arena(a)) if self.t.arenas[a].dma => {
                    let it = self.check_expr(idx, Some(&Ty::Dma(a)));
                    let ir = self.resolve(&it);
                    match ir.strip_ref() {
                        Ty::Dma(b) if *b == a => {}
                        Ty::Err => {}
                        other => {
                            let d = self.t.display(other);
                            self.err(
                                Diag::error("F2703", idx.span, format!("dma_arena `{}` の添字はハンドル `Dma<{}>` だけです（`{}` は使えません）", name, name, d), format!("the dma_arena `{}` can only be indexed by its handle `Dma<{}>` (not `{}`)", name, name, d))
                                    .hint("ハンドルは `alloc` と `reclaim` が返します。デバイスが返した添字は `reclaim(raw)` で検証してください", "handles come from `alloc` and `reclaim`; validate an index returned by the device with `reclaim(raw)`"),
                            );
                        }
                    }
                    self.arena_index.insert(e.id, a);
                    return self.t.arenas[a].elem.clone();
                }
                // `dma` リージョン: 整数の添字（境界は検査する） / a `dma` region: an integer index (bounds-checked)
                Some(RegionRef::Chan(_)) => {
                    self.check_expr(idx, None);
                    self.err(Diag::error("F0599", base.span, format!("チャネル `{}` は添字で読めません（send / recv / recv_wait）", name), format!("the channel `{}` cannot be indexed (send / recv / recv_wait)", name)));
                    return Ty::Err;
                }
                Some(RegionRef::UserMem(_)) => {
                    self.check_expr(idx, None);
                    self.err(
                        Diag::error("F2803", base.span, format!("ユーザーメモリ `{}` は添字で読み書きできません", name), format!("the user memory `{}` cannot be indexed", name))
                            .hint(format!("`if let Some(m) = {}.of(タスクの Id) {{ .. }}` で窓（`&mut [u8]`）として受け取ってください", name), format!("take a window (`&mut [u8]`) with `if let Some(m) = {}.of(task_id) {{ .. }}`", name)),
                    );
                    return Ty::Err;
                }
                Some(RegionRef::Dma(d)) => {
                    let it = self.check_expr(idx, None);
                    if !self.resolve(&it).is_int() && !it.is_err() {
                        self.err(Diag::error("F0308", idx.span, "添字は整数です", "the index must be an integer"));
                    }
                    self.dma_index.insert(e.id, d);
                    return self.t.dmas[d].elem.clone();
                }
                Some(RegionRef::Arena(a)) => {
                    let it = self.check_expr(idx, Some(&Ty::Id(a)));
                    let mut ir = self.resolve(&it);
                    // まだ決まっていない型（`let id;` など）はここで `Id<名前>` に決まる / an unknown type becomes `Id<name>` here
                    if let Ty::Var(_) = ir {
                        self.unify(&ir, &Ty::Id(a));
                        ir = Ty::Id(a);
                    }
                    match &ir {
                        Ty::Id(b) if *b == a => {}
                        Ty::Id(_) => self.mismatch(idx.span, &Ty::Id(a), &ir),
                        Ty::Err => {}
                        other => {
                            let d = self.t.display(other);
                            self.err(
                                Diag::error(
                                    "F2101",
                                    idx.span,
                                    format!("アリーナ `{}` の添字は `Id<{}>` だけです（`{}` は使えません）", name, name, d),
                                    format!("arena `{}` can only be indexed by `Id<{}>` (not `{}`)", name, name, d),
                                )
                                .hint("整数から Id は作れません。外部から来た値は `名前.validate(raw)` で検証してください", "an Id cannot be made from an integer; validate external values with `name.validate(raw)`"),
                            );
                        }
                    }
                    self.arena_index.insert(e.id, a);
                    return self.t.arenas[a].elem.clone();
                }
                Some(RegionRef::Mmio(m)) => {
                    return self.mmio_index_step(e, m, 1, idx);
                }
                Some(RegionRef::Port(p)) => {
                    let it = self.check_expr(idx, None);
                    let ir = self.want_int(&it);
                    if !ir.is_int() && !ir.is_err() {
                        let d = self.t.display(&ir);
                        self.err(Diag::error("F0308", idx.span, format!("ポートの添字は整数です（`{}` は不可）", d), format!("a port index must be an integer (not `{}`)", d)));
                    }
                    let info = self.t.ports[p].clone();
                    if info.len.is_none() {
                        self.err(Diag::error(
                            "F0608",
                            e.span,
                            format!("ポート `{}` は配列ではありません（`{}` だけで読み書きします）", info.name, info.name),
                            format!("port `{}` is not an array (read and write it as `{}`)", info.name, info.name),
                        ));
                        return Ty::Err;
                    }
                    self.port_index.insert(e.id, p);
                    return Ty::Int(info.elem);
                }
                None => {
                    self.err(Diag::error("F2104", base.span, format!("`{}` が見つかりません", name), format!("cannot find `{}`", name)));
                    self.check_expr(idx, None);
                    return Ty::Err;
                }
            }
        }
        // 2 次元 MMIO の 2 つ目の添字 / the second index of a 2-D MMIO region
        if let ExprKind::Index(b2, _) = &base.kind {
            if let ExprKind::ArenaRef(name) = &b2.kind {
                if let Some(RegionRef::Mmio(m)) = self.t.regions.get(name).copied() {
                    self.check_expr(base, None);
                    let depth = self.mmio_index.get(&base.id).map(|x| x.1).unwrap_or(1);
                    return self.mmio_index_step(e, m, depth + 1, idx);
                }
            }
        }
        let bt = self.check_expr(base, None);
        let it = self.check_expr(idx, None);
        let ir = self.want_int(&it);
        if !ir.is_int() && !ir.is_err() {
            let d = self.t.display(&ir);
            self.err(Diag::error("F0308", idx.span, format!("配列の添字は整数です（`{}` は不可）", d), format!("an array index must be an integer (not `{}`)", d)));
        }
        let br = self.resolve(&bt);
        if self.unknown_here(br.deref_all(), base.span, true) {
            return Ty::Err;
        }
        if let Ty::User(_) = &br {
            self.err(
                Diag::error("F3101", e.span, "`user T`（ユーザー空間のバッファ）は直接読めません（TOCTOU を防ぐため、中身は 1 回だけ写す）", "a `user T` (a user-space buffer) cannot be read directly (its contents are copied exactly once to prevent TOCTOU)")
                    .hint("`let k = copy_from_user(ubuf)?;` で写したカーネル側の値を読んでください", "read the kernel-side copy from `let k = copy_from_user(ubuf)?;`"),
            );
            return Ty::Err;
        }
        match br.deref_all() {
            Ty::Array(el, _) => (**el).clone(),
            Ty::Slice(el) => (**el).clone(),
            Ty::Err => Ty::Err,
            Ty::Str => {
                self.err(Diag::error("F0608", e.span, "Str は添字で読めません（`s.byte(i)` を使ってください）", "a Str cannot be indexed (use `s.byte(i)`)"));
                Ty::Err
            }
            other => {
                let d = self.t.display(other);
                self.err(Diag::error("F0608", e.span, format!("`{}` は添字で読めません", d), format!("`{}` cannot be indexed", d)));
                Ty::Err
            }
        }
    }

    fn mmio_index_step(&mut self, e: &Expr, m: usize, depth: usize, idx: &Expr) -> Ty {
        let it = self.check_expr(idx, None);
        let ir = self.want_int(&it);
        if !ir.is_int() && !ir.is_err() {
            let d = self.t.display(&ir);
            self.err(Diag::error("F0308", idx.span, format!("MMIO の添字は整数です（`{}` は不可）", d), format!("an MMIO index must be an integer (not `{}`)", d)));
        }
        let info = self.t.mmios[m].clone();
        if depth > info.dims.len() {
            self.err(Diag::error("F0608", e.span, format!("`{}` は {} 次元です", info.name, info.dims.len()), format!("`{}` has {} dimension(s)", info.name, info.dims.len())));
            return Ty::Err;
        }
        self.mmio_index.insert(e.id, (m, depth));
        if depth == info.dims.len() {
            Ty::Int(info.elem)
        } else {
            // 行（値としては使えない。flow.rs が検査） / a row (not usable as a value; checked in flow.rs)
            Ty::Array(Box::new(Ty::Int(info.elem)), info.dims[depth])
        }
    }

    fn check_call(&mut self, e: &Expr, callee: &str, csp: Span, args: &[Expr], turbofish: Option<&TypeExpr>, exp: Option<&Ty>) -> Ty {
        if (callee == "gc_collect" || callee == "promote_to_gc") && turbofish.is_none() && !self.t.fn_by_name.contains_key(callee) {
            return self.check_gc_call(e, callee, csp, args);
        }
        if turbofish.is_some() || callee == "chan" || callee == "block_on" {
            self.err(Diag::error(
                "F2900",
                e.span,
                format!("`{}` は 0.1 コアの機能です。1.0 構文（arena / own / mmio）を使うファイルではまだ使えません", callee),
                format!("`{}` is a 0.1-core feature and is not yet available in files that use 1.0 syntax (arena / own / mmio)", callee),
            ));
            for a in args {
                self.check_expr(a, None);
            }
            return Ty::Err;
        }
        // `Ok(v)` / `Err(e)`: Result の構築（期待される型があればそれに合わせる） / building a Result (following the expected type)
        if (callee == "Ok" || callee == "Err") && self.lookup_local(callee).is_none() && !self.t.fn_by_name.contains_key(callee) {
            let is_ok = callee == "Ok";
            self.calls.insert(e.id, if is_ok { CallKind::Ok } else { CallKind::Err });
            if args.len() != 1 {
                self.err(Diag::error("F0061", e.span, format!("`{}` の引数は 1 つです", callee), format!("`{}` takes exactly one argument", callee)));
                for a in args {
                    self.check_expr(a, None);
                }
                return Ty::Err;
            }
            let (te, ee) = match exp {
                Some(Ty::Result(t, x)) => (Some((**t).clone()), Some((**x).clone())),
                _ => (None, None),
            };
            let want = if is_ok { te.clone() } else { ee.clone() };
            let it = self.check_expr(&args[0], want.as_ref());
            if let Some(w) = &want {
                self.expect_ty(args[0].span, w, &it);
            }
            if let Ty::Ref(..) = self.resolve(&it) {
                self.err(Diag::error("F0515", args[0].span, "参照を Result に入れることはできません（第二級参照）", "a reference cannot be put into a Result (second-class references)"));
                return Ty::Err;
            }
            let other = if is_ok { ee } else { te };
            let other = match other {
                Some(o) => o,
                None => self.new_var(),
            };
            let it = want.unwrap_or(it);
            return if is_ok { Ty::Result(Box::new(it), Box::new(other)) } else { Ty::Result(Box::new(other), Box::new(it)) };
        }
        if callee == "Some" {
            self.calls.insert(e.id, CallKind::Some);
            if args.len() != 1 {
                self.err(Diag::error("F0061", e.span, "`Some` の引数は 1 つです", "`Some` takes exactly one argument"));
                for a in args {
                    self.check_expr(a, None);
                }
                return Ty::Err;
            }
            let iexp = match exp {
                Some(Ty::Option(i)) => Some((**i).clone()),
                _ => None,
            };
            let it = self.check_expr(&args[0], iexp.as_ref());
            if let Some(ie) = &iexp {
                self.expect_ty(args[0].span, ie, &it);
                return Ty::Option(Box::new(ie.clone()));
            }
            if let Ty::Ref(..) = self.resolve(&it) {
                self.err(Diag::error("F0515", args[0].span, "参照を Option に入れることはできません（第二級参照）", "a reference cannot be put into an Option (second-class references)"));
                return Ty::Err;
            }
            return Ty::Option(Box::new(it));
        }
        // 関数ポインタのローカル・引数・static を名前で呼ぶ（ローカルは同じ名前の関数より優先）
        // calling a function-pointer local, parameter or static by name (a local hides a function of the same name)
        let target = match self.lookup_local(callee) {
            Some(b) => Some(Res::Local(b)),
            None => self.t.static_by_name.get(callee).map(|&si| Res::Static(si)),
        };
        let target = target.filter(|r| {
            // 関数ポインタでない同名のローカルがあっても、同じ名前の関数・組み込みがあれば今までどおりそれを呼ぶ
            // a non-function-pointer local does not hide a function or builtin of the same name (as before)
            let t = match r {
                Res::Local(b) => self.locals[*b as usize].as_ref().map(|l| l.ty.clone()).unwrap_or(Ty::Err),
                Res::Static(si) => self.t.statics[*si].ty.clone(),
                _ => Ty::Err,
            };
            let t = self.resolve(&t);
            matches!(t, Ty::Fn(..) | Ty::Var(_)) || !(self.t.fn_by_name.contains_key(callee) || builtin_fn_name(callee))
        });
        if let Some(r) = target {
            let t = match r {
                Res::Local(b) => self.locals[b as usize].as_ref().map(|l| l.ty.clone()).unwrap_or(Ty::Err),
                Res::Static(si) => self.t.statics[si].ty.clone(),
                _ => Ty::Err,
            };
            let t = self.resolve(&t);
            if self.unknown_here(&t, csp, true) {
                for a in args {
                    self.check_expr_noref(a);
                }
                return Ty::Err;
            }
            match t {
                Ty::Fn(ps, ret) => {
                    self.calls.insert(e.id, CallKind::Indirect);
                    self.call_targets.insert(e.id, r);
                    if self.in_bypass > 0 {
                        self.err(Diag::error("F0811", e.span, "volatile_bypass の中では関数を呼び出せません", "functions cannot be called inside volatile_bypass"));
                    }
                    self.check_call_args(e, callee, &ps, args);
                    return *ret;
                }
                Ty::Err => {
                    for a in args {
                        self.check_expr_noref(a);
                    }
                    return Ty::Err;
                }
                other => {
                    let d = self.t.display(&other);
                    self.err(
                        Diag::error(
                            "F0618",
                            csp,
                            format!("`{}` は関数ではありません（型は `{}`）", callee, d),
                            format!("`{}` is not a function (its type is `{}`)", callee, d),
                        )
                        .hint("呼び出せるのは関数と関数ポインタ `fn(..)` です", "only functions and function pointers `fn(..)` can be called"),
                    );
                    for a in args {
                        self.check_expr_noref(a);
                    }
                    return Ty::Err;
                }
            }
        }
        let kernel_sys_builtin = |s: &Sema, fi: usize| builtin_fn_name(callee) && s.t.fns[fi].syscall.is_some() && !s.t.fns[fi].stub;
        if let Some(fi) = self.t.fn_by_name.get(callee).copied().filter(|&fi| !kernel_sys_builtin(self, fi)) {
            self.calls.insert(e.id, CallKind::User(fi));
            if self.t.fns[fi].trap || (self.t.fns[fi].syscall.is_some() && !self.t.fns[fi].stub) {
                self.err(
                    Diag::error(
                        "F2801",
                        csp,
                        format!("`{}` はカーネルの中からは呼び出せません（EL0 からの svc・例外でだけ動きます）", callee),
                        format!("`{}` cannot be called from inside the kernel (it runs only from an svc or an exception at EL0)", callee),
                    )
                    .hint("共通の処理はふつうの `fn` に出して、両方から呼んでください", "move the shared work into an ordinary `fn` and call that from both"),
                );
            }
            if self.t.fns[fi].irq.is_some() {
                self.err(Diag::error(
                    "F2602",
                    csp,
                    format!("割り込みハンドラ `{}` は呼び出せません（割り込みからだけ動きます）", callee),
                    format!("the interrupt handler `{}` cannot be called (it runs from its interrupt only)", callee),
                ));
            }
            if self.in_bypass > 0 {
                self.err(Diag::error("F0811", e.span, "volatile_bypass の中では関数を呼び出せません", "functions cannot be called inside volatile_bypass"));
            }
            let params: Vec<Ty> = self.t.fns[fi].params.iter().map(|p| p.ty.clone()).collect();
            let ret = self.t.fns[fi].ret.clone();
            self.check_call_args(e, callee, &params, args);
            return ret;
        }
        let b = match callee {
            "f32::from_bits" => Builtin::FloatFromBits(FloatTy::F32),
            "f64::from_bits" => Builtin::FloatFromBits(FloatTy::F64),
            "print_int" => Builtin::PrintInt,
            "print_str" => Builtin::PrintStr,
            "put_int" => Builtin::PutInt,
            "put_hex" => Builtin::PutHex,
            "put_str" => Builtin::PutStr,
            "put" => Builtin::Put,
            "panic" => Builtin::Panic,
            "drop" => Builtin::Drop,
            "cpu_relax" => Builtin::CpuRelax,
            "fence" => Builtin::Fence,
            "compiler_fence" => Builtin::CompilerFence,
            "cpu_id" => Builtin::CpuId,
            "mmio_read8" => Builtin::MmioRead(8),
            "mmio_read16" => Builtin::MmioRead(16),
            "mmio_read32" => Builtin::MmioRead(32),
            "mmio_read64" => Builtin::MmioRead(64),
            "mmio_write8" => Builtin::MmioWrite(8),
            "mmio_write16" => Builtin::MmioWrite(16),
            "mmio_write32" => Builtin::MmioWrite(32),
            "mmio_write64" => Builtin::MmioWrite(64),
            "heap_live_blocks" => Builtin::HeapLiveBlocks,
            "heap_live_bytes" => Builtin::HeapLiveBytes,
            "halt" => Builtin::Halt,
            "exit" => Builtin::Exit,
            "timer_freq" => Builtin::TimerFreq,
            "timer_now" => Builtin::TimerNow,
            "timer_arm" => Builtin::TimerArm,
            "irq_open" => Builtin::IrqOpen,
            "wait_irq" => Builtin::WaitIrq,
            "min" => Builtin::Min,
            "put_bytes" => Builtin::PutBytes,
            "spawn_on" => Builtin::SpawnOn,
            "cpu_count" => Builtin::CpuCount,
            "max" => Builtin::Max,
            "copy_from_user" => Builtin::CopyFromUser,
            "copy_to_user" => Builtin::CopyToUser,
            "submit_dma" => Builtin::SubmitDma,
            "ct_eq" => Builtin::CtEq,
            "schedule" => Builtin::Schedule,
            "sleep_ticks" => Builtin::SleepTicks,
            "rcu_synchronize" => Builtin::RcuSynchronize,
            "clock_mono_ns" => Builtin::ClockMonoNs,
            "cpu_this" => Builtin::CpuThis,
            "fault" => Builtin::Fault,
            _ => {
                let hint = if self.t.struct_by_name.contains_key(callee) {
                    (format!("構造体は `{} {{ フィールド: 値 }}` で作ります", callee), format!("build a struct with `{} {{ field: value }}`", callee))
                } else {
                    (String::new(), String::new())
                };
                let mut d = Diag::error("F0425", csp, format!("関数 `{}` が見つかりません", callee), format!("cannot find function `{}`", callee));
                if !hint.0.is_empty() {
                    d = d.hint(hint.0, hint.1);
                }
                self.err(d);
                for a in args {
                    self.check_expr_noref(a);
                }
                return Ty::Err;
            }
        };
        self.calls.insert(e.id, CallKind::Builtin(b));
        let want = |n: usize, s: &mut Sema| {
            if args.len() != n {
                s.err(Diag::error(
                    "F0061",
                    e.span,
                    format!("`{}` の引数は {} 個です", callee, n),
                    format!("`{}` takes {} argument(s)", callee, n),
                ));
            }
        };
        match b {
            Builtin::PrintInt | Builtin::PutInt | Builtin::PutHex => {
                want(1, self);
                if let Some(a) = args.first() {
                    let t = self.check_expr(a, None);
                    let r = self.resolve(&t);
                    if !r.is_int() && !r.is_err() {
                        let d = self.t.display(&r);
                        self.err(Diag::error("F0308", a.span, format!("print_int には整数を渡します（`{}` は不可）", d), format!("print_int takes an integer (not `{}`)", d)));
                    }
                }
                Ty::Unit
            }
            Builtin::Put => {
                want(1, self);
                if let Some(a) = args.first() {
                    let t = self.check_expr(a, None);
                    let r = self.resolve(&t);
                    match r {
                        Ty::Int(_) | Ty::IntVar(_) | Ty::Str | Ty::Bool | Ty::Err => {}
                        // 浮動小数点は往復できる最短の 10 進（`1.0`・`0.1`・`1e16`・`NaN`・`inf`） / floats print the shortest
                        // decimal that reads back to the same value (`1.0`, `0.1`, `1e16`, `NaN`, `inf`)
                        Ty::Float(_) | Ty::FloatVar(_) => {}
                        Ty::Var(_) => {
                            self.unknown_here(&r, a.span, true);
                        }
                        other => {
                            let d = self.t.display(&other);
                            self.err(Diag::error("F0308", a.span, format!("put には整数・浮動小数点・Str・bool を渡します（`{}` は不可）", d), format!("put takes an integer, a float, a Str or a bool (not `{}`)", d)));
                        }
                    }
                }
                Ty::Unit
            }
            Builtin::FloatFromBits(ft) => {
                want(1, self);
                if let Some(a) = args.first() {
                    let bt = Ty::Int(ft.bits_int());
                    let t = self.check_expr(a, Some(&bt));
                    self.expect_ty(a.span, &bt, &t);
                }
                Ty::Float(ft)
            }
            Builtin::CpuCount => {
                want(0, self);
                Ty::Int(IntTy::U32)
            }
            Builtin::SpawnOn => {
                if self.user_mode {
                    self.user_ban(e.span, "spawn_on");
                }
                self.smp = true;
                if args.len() != 2 && args.len() != 3 {
                    self.err(Diag::error("F0061", e.span, "`spawn_on` の引数は 2 個か 3 個です（コア番号, 関数[, 引数]）", "`spawn_on` takes 2 or 3 arguments (core, function[, argument])"));
                    for a in args {
                        self.check_expr_noref(a);
                    }
                    return Ty::Unit;
                }
                let ct = self.check_expr(&args[0], Some(&Ty::Int(IntTy::U32)));
                self.expect_ty(args[0].span, &Ty::Int(IntTy::U32), &ct);
                let target = match &args[1].kind {
                    ExprKind::Path(n) => self.t.fn_by_name.get(n).copied(),
                    _ => None,
                };
                let Some(fi) = target.filter(|&fi| !self.t.fns[fi].is_main && self.t.fns[fi].irq.is_none() && self.t.fns[fi].syscall.is_none() && !self.t.fns[fi].trap) else {
                    self.err(
                        Diag::error("F2615", args[1].span, "`spawn_on` の 2 番目の引数は関数の名前です", "the second argument of `spawn_on` is the name of a function")
                            .hint("`fn worker() { .. }` か `fn worker(job: own Job) { .. }` を定義して、その名前を書いてください", "define `fn worker() { .. }` or `fn worker(job: own Job) { .. }` and write its name"),
                    );
                    if let Some(a) = args.get(2) {
                        self.check_expr_noref(a);
                    }
                    return Ty::Unit;
                };
                self.expr_ty[args[1].id as usize] = Ty::Unit;
                let f = self.t.fns[fi].clone();
                let want_params = args.len() - 2;
                if f.params.len() != want_params || f.ret != Ty::Unit {
                    self.err(
                        Diag::error(
                            "F2615",
                            args[1].span,
                            format!("`{}` は `spawn_on` で起動できる形ではありません（引数 {} 個・戻り値なしの関数が要ります）", f.name, want_params),
                            format!("`{}` does not have the shape `spawn_on` needs (a function with {} parameter(s) and no result)", f.name, want_params),
                        )
                        .hint("コアへ渡せる引数は 1 個までです。複数の値は `own` の構造体にまとめて渡してください", "at most one argument crosses to the core; bundle several values in an `own` struct"),
                    );
                }
                if let (Some(a), Some(p)) = (args.get(2), f.params.first()) {
                    let pt = p.ty.clone();
                    let at = self.check_expr(a, Some(&pt));
                    self.expect_ty(a.span, &pt, &at);
                    // コアへ渡せる型: 整数・bool・共有アリーナの Id・own T（レジスタ 1 本で渡る）。参照は F0515、コアごとの Id は F2611
                    // types that cross to a core: integers, bool, Ids of shared arenas, own T (one register); references are
                    // F0515, per-core Ids F2611
                    match &pt {
                        Ty::Ref(..) => {
                            self.err(
                                Diag::error("F0515", a.span, "参照は別のコアへ渡せません（第二級の参照: 呼び出しの間だけ貸せる）", "a reference cannot cross to another core (second-class references are lent for one call only)")
                                    .hint("`own T` で所有権ごと渡すか、`shared arena` に置いて Id を渡してください", "hand over ownership with `own T`, or put the data in a `shared arena` and pass its Id"),
                            );
                        }
                        Ty::Int(_) | Ty::Bool | Ty::Id(_) | Ty::Own(_) | Ty::Err => {
                            self.check_cross_core(&pt, a.span, "spawn_on の引数", "the argument of spawn_on");
                        }
                        other => {
                            let d = self.t.display(other);
                            self.err(
                                Diag::error("F2615", a.span, format!("`{}` は `spawn_on` の引数にできません（整数・bool・共有アリーナの Id・`own T` だけ）", d), format!("`{}` cannot be a `spawn_on` argument (integers, bool, Ids of shared arenas and `own T` only)", d))
                                    .hint("構造体は `own` にして渡してください（所有権ごと移る）", "pass a struct as `own` (ownership moves with it)"),
                            );
                        }
                    }
                }
                let core = self.const_eval(&args[0]);
                self.spawn_cores.insert(e.id, core);
                self.spawns.push((e.id, fi, e.span));
                Ty::Unit
            }
            Builtin::PutBytes => {
                want(1, self);
                if let Some(a) = args.first() {
                    self.check_slice_arg(a, false);
                }
                Ty::Unit
            }
            Builtin::PrintStr | Builtin::PutStr | Builtin::Panic => {
                want(1, self);
                if let Some(a) = args.first() {
                    let t = self.check_expr(a, Some(&Ty::Str));
                    self.expect_ty(a.span, &Ty::Str, &t);
                }
                if b == Builtin::Panic {
                    Ty::Never
                } else {
                    Ty::Unit
                }
            }
            Builtin::Drop => {
                want(1, self);
                if let Some(a) = args.first() {
                    self.check_expr_noref(a);
                }
                Ty::Unit
            }
            Builtin::CpuRelax | Builtin::Fence | Builtin::CompilerFence => {
                want(0, self);
                Ty::Unit
            }
            Builtin::CpuId => {
                want(0, self);
                Ty::Int(IntTy::U32)
            }
            Builtin::HeapLiveBlocks | Builtin::HeapLiveBytes => {
                want(0, self);
                Ty::Int(IntTy::Usize)
            }
            Builtin::Halt => {
                want(0, self);
                Ty::Never
            }
            Builtin::TimerFreq | Builtin::TimerNow => {
                want(0, self);
                Ty::Int(IntTy::U64)
            }
            Builtin::TimerArm => {
                if self.user_mode {
                    self.user_ban(e.span, "timer_arm");
                }
                want(1, self);
                if let Some(a) = args.first() {
                    let t = self.check_expr(a, Some(&Ty::Int(IntTy::U64)));
                    self.expect_ty(a.span, &Ty::Int(IntTy::U64), &t);
                }
                Ty::Unit
            }
            Builtin::IrqOpen | Builtin::WaitIrq => {
                if self.user_mode {
                    self.user_ban(e.span, if b == Builtin::IrqOpen { "irq_open" } else { "wait_irq" });
                }
                want(0, self);
                Ty::Unit
            }
            Builtin::Min | Builtin::Max => {
                want(2, self);
                let mut t = Ty::Err;
                if let Some(a) = args.first() {
                    let at = self.check_expr(a, exp.filter(|x| x.is_int()));
                    t = self.want_int(&at);
                }
                if let Some(b) = args.get(1) {
                    let bt = self.check_expr(b, Some(&t));
                    self.expect_ty(b.span, &t, &bt);
                }
                t
            }
            Builtin::Exit => {
                want(1, self);
                if let Some(a) = args.first() {
                    let t = self.check_expr(a, Some(&Ty::Int(IntTy::I32)));
                    self.expect_ty(a.span, &Ty::Int(IntTy::I32), &t);
                }
                Ty::Never
            }
            Builtin::CopyFromUser | Builtin::CopyToUser => self.check_user_copy(e, b, args),
            Builtin::SubmitDma => {
                want(1, self);
                if let Some(a) = args.first() {
                    let t = self.check_expr(a, None);
                    let r = self.resolve(&t);
                    if !matches!(r, Ty::DmaOwn(_) | Ty::Err) {
                        let d = self.t.display(&r);
                        self.err(
                            Diag::error("F3401", a.span, format!("`submit_dma` に渡せるのは `dma own T` だけです（`{}` は不可）", d), format!("only a `dma own T` can be passed to `submit_dma` (not `{}`)", d))
                                .hint("デバイスと共有するバッファは `dma_alloc(T)` で作ります（4 KiB 境界の独立したページ）", "make device-shared buffers with `dma_alloc(T)` (their own 4 KiB-aligned pages)"),
                        );
                    }
                }
                Ty::Int(IntTy::U64)
            }
            Builtin::CtEq => {
                want(2, self);
                let mut ts: Vec<Ty> = Vec::new();
                for (k, a) in args.iter().take(2).enumerate() {
                    let t = self.check_expr_noref(a);
                    if k == 1 {
                        // 2 つ目は 1 つ目の型に合わせる（整数リテラルの配列を u8 に決める） / the second follows the first
                        // (an array of integer literals becomes u8)
                        let first = ts[0].clone();
                        self.unify(&first, &t);
                    }
                    ts.push(self.resolve(&t));
                }
                if ts.len() == 2 {
                    let ok = |t: &Ty, s: &Sema| match t.strip_ref() {
                        Ty::Array(e, _) => e.is_int(),
                        Ty::Int(_) => true,
                        Ty::Struct(x) => s.t.structs[*x].secret || s.t.structs[*x].wire.is_some(),
                        Ty::Err => true,
                        _ => false,
                    };
                    if !ok(&ts[0], self) || ts[0].strip_ref() != ts[1].strip_ref() {
                        let (a, b2) = (self.t.display(&ts[0]), self.t.display(&ts[1]));
                        self.err(Diag::error("F3304", e.span, format!("`ct_eq` は同じ型の整数・整数の配列・#[secret] / ワイヤ構造体を比べます（`{}` と `{}`）", a, b2), format!("`ct_eq` compares two values of the same integer, integer-array, #[secret] or wire-struct type (`{}` and `{}`)", a, b2)));
                    }
                }
                Ty::Bool
            }
            Builtin::Schedule | Builtin::RcuSynchronize => {
                want(0, self);
                Ty::Unit
            }
            Builtin::SleepTicks => {
                want(1, self);
                if let Some(a) = args.first() {
                    let t = self.check_expr(a, Some(&Ty::Int(IntTy::U64)));
                    self.expect_ty(a.span, &Ty::Int(IntTy::U64), &t);
                }
                Ty::Unit
            }
            Builtin::ClockMonoNs => {
                want(0, self);
                Ty::Int(IntTy::U64)
            }
            Builtin::CpuThis => {
                want(0, self);
                Ty::Int(IntTy::U32)
            }
            Builtin::Fault => {
                want(1, self);
                if let Some(a) = args.first() {
                    let t = self.check_expr(a, Some(&Ty::Int(IntTy::I64)));
                    self.expect_ty(a.span, &Ty::Int(IntTy::I64), &t);
                }
                Ty::Fault
            }
            Builtin::MmioRead(bits) | Builtin::MmioWrite(bits) => {
                if self.in_bypass == 0 {
                    self.err(
                        Diag::error("F0811", e.span, format!("`{}` は volatile_bypass の中でだけ使えます", callee), format!("`{}` can only be used inside volatile_bypass", callee))
                            .hint("型安全な固定番地リージョン `mmio @名前: [u32; N] at 番地;` を使うと境界検査付きで書けます", "a typed fixed-address region `mmio @name: [u32; N] at addr;` gives bounds-checked access instead"),
                    );
                }
                let vt = Ty::Int(match bits {
                    8 => IntTy::U8,
                    16 => IntTy::U16,
                    32 => IntTy::U32,
                    _ => IntTy::U64,
                });
                let is_write = matches!(b, Builtin::MmioWrite(_));
                want(if is_write { 2 } else { 1 }, self);
                if let Some(a) = args.first() {
                    let t = self.check_expr(a, Some(&Ty::Int(IntTy::Usize)));
                    self.expect_ty(a.span, &Ty::Int(IntTy::Usize), &t);
                }
                if is_write {
                    if let Some(v) = args.get(1) {
                        let t = self.check_expr(v, Some(&vt));
                        self.expect_ty(v.span, &vt, &t);
                    }
                    Ty::Unit
                } else {
                    vt
                }
            }
        }
    }

    /// 呼び出しの引数（関数の直接呼び出しと関数ポインタの呼び出しで共通） / call arguments (shared by direct calls and calls
    /// through function pointers)
    fn check_call_args(&mut self, e: &Expr, callee: &str, params: &[Ty], args: &[Expr]) {
        if params.len() != args.len() {
            self.err(Diag::error(
                "F0061",
                e.span,
                format!("`{}` の引数は {} 個ですが {} 個渡しています", callee, params.len(), args.len()),
                format!("`{}` takes {} argument(s) but {} were supplied", callee, params.len(), args.len()),
            ));
        }
        for (i, a) in args.iter().enumerate() {
            match params.get(i).cloned() {
                Some(Ty::Ref(pm, pt)) => self.check_ref_arg(a, pm, &pt),
                Some(pt) => {
                    if let ExprKind::Ref { .. } = a.kind {
                        let pd = self.t.display(&pt);
                        self.err(Diag::error("F0308", a.span, format!("この引数は値渡し（`{}`）です。`&` は不要です", pd), format!("this parameter is passed by value (`{}`); remove the `&`", pd)));
                        self.check_expr_noref(a);
                        continue;
                    }
                    let at = self.check_expr(a, Some(&pt));
                    self.expect_ty(a.span, &pt, &at);
                }
                None => {
                    self.check_expr_noref(a);
                }
            }
        }
    }

    /// 式を呼ぶ `(h.on_click)(&mut b)` / `ops[i](x)`: 呼ばれる式は関数ポインタ
    /// calling an expression `(h.on_click)(&mut b)` / `ops[i](x)`: the callee must be a function pointer
    fn check_call_indirect(&mut self, e: &Expr, callee: &Expr, args: &[Expr]) -> Ty {
        let ct = self.check_expr(callee, None);
        let ct = self.resolve(&ct);
        if self.unknown_here(&ct, callee.span, true) {
            for a in args {
                self.check_expr_noref(a);
            }
            return Ty::Err;
        }
        match ct {
            Ty::Fn(ps, ret) => {
                self.calls.insert(e.id, CallKind::Indirect);
                if self.in_bypass > 0 {
                    self.err(Diag::error("F0811", e.span, "volatile_bypass の中では関数を呼び出せません", "functions cannot be called inside volatile_bypass"));
                }
                self.check_call_args(e, "fn(..)", &ps, args);
                *ret
            }
            Ty::Err => {
                for a in args {
                    self.check_expr_noref(a);
                }
                Ty::Err
            }
            other => {
                let d = self.t.display(&other);
                self.err(
                    Diag::error("F0618", callee.span, format!("この式は関数ではありません（型は `{}`）", d), format!("this expression is not a function (its type is `{}`)", d))
                        .hint("呼び出せるのは関数と関数ポインタ `fn(..)` です", "only functions and function pointers `fn(..)` can be called"),
                );
                for a in args {
                    self.check_expr_noref(a);
                }
                Ty::Err
            }
        }
    }

    /// 参照引数: `&x` / `&mut x`、または参照の束縛をそのまま渡す
    /// a reference argument: `&x` / `&mut x`, or an existing reference binding passed on
    /// 窓・ビューを取る式の検査: 対象の位置にあるか、書き込める窓か
    /// checks for an expression taking a window or view: scrutinee position, and a writable window when needed
    fn bytes_recv_ok(&mut self, recv: &Expr, rr: &Ty, mutable: bool, scrut_ok: bool, span: Span, method: &str) {
        if !scrut_ok {
            self.err(
                Diag::error(
                    "F0515",
                    span,
                    format!("`.{}(..)` は `if let Some(x) = ..` / `match` の対象にだけ書けます（結果は保存できない第二級の参照です）", method),
                    format!("`.{}(..)` can only be the scrutinee of `if let Some(x) = ..` / `match` (its result is a second-class reference and cannot be stored)", method),
                )
                .hint("値として持ち出すなら `.read(T, off)`（複製）を使ってください", "to take a value out, use `.read(T, off)` (a copy)"),
            );
        }
        if mutable {
            let ro = match rr {
                Ty::Ref(m, _) => !*m,
                _ => !self.place_is_mutable(recv),
            };
            if ro {
                self.err(Diag::error("F0596", recv.span, format!("読み出し専用のバイト列に `.{}(..)` は使えません（mut 変数・`&mut` の引数・`&mut [u8]` の窓が要ります）", method), format!("`.{}(..)` cannot be used on read-only bytes (a mut variable, a `&mut` parameter or a `&mut [u8]` window is needed)", method)));
            }
        }
    }

    /// 窓の引数（`&[u8]` / `&mut [u8]`）: 窓そのもの・`[u8; N]` の配列への参照・Str（読み出しだけ）を受ける
    /// a window argument (`&[u8]` / `&mut [u8]`): takes a window, a reference to a `[u8; N]` array, or a Str (read-only)
    fn check_slice_arg(&mut self, a: &Expr, pm: bool) {
        let u8t = Ty::Int(IntTy::U8);
        let (from, mutable): (Option<SliceFrom>, bool) = match &a.kind {
            ExprKind::Ref { mutable, inner } => {
                let it = self.check_expr(inner, None);
                let ir = self.resolve(&it);
                let target = match ir.strip_ref() {
                    Ty::Own(x) => (**x).clone(),
                    t => t.clone(),
                };
                match target {
                    Ty::Array(el, n) if *el == u8t => (Some(SliceFrom::Array(n)), *mutable),
                    Ty::Err => return,
                    _ => (None, *mutable),
                }
            }
            ExprKind::Path(_) => {
                let t = self.check_expr(a, None);
                let declared = match self.res.get(&a.id) {
                    Some(Res::Local(b)) => self.locals[*b as usize].as_ref().map(|l| l.ty.clone()),
                    _ => None,
                };
                match declared {
                    Some(Ty::Ref(m, x)) => {
                        self.auto_deref.remove(&a.id);
                        self.own_derefs.remove(&a.id);
                        match &*x {
                            Ty::Slice(_) => (Some(SliceFrom::Slice), m),
                            Ty::Array(el, n) if **el == u8t => (Some(SliceFrom::Array(*n)), m),
                            _ => (None, m),
                        }
                    }
                    _ => match self.resolve(&t) {
                        Ty::Str => (Some(SliceFrom::Str), false),
                        Ty::Err => return,
                        _ => (None, false),
                    },
                }
            }
            _ => {
                let t = self.check_expr(a, None);
                match self.resolve(&t) {
                    Ty::Str => (Some(SliceFrom::Str), false),
                    Ty::Err => return,
                    _ => (None, false),
                }
            }
        };
        match from {
            Some(f) => {
                if pm && !mutable {
                    self.err(Diag::error("F0596", a.span, "この引数は `&mut [u8]` です。書き込める窓か `&mut 配列` を渡してください", "this parameter is `&mut [u8]`; pass a writable window or `&mut array`"));
                }
                self.slice_args.insert(a.id, f);
                self.expr_ty[a.id as usize] = Ty::Ref(pm, Box::new(Ty::Slice(Box::new(u8t))));
            }
            None => {
                self.err(
                    Diag::error("F0308", a.span, "この引数は窓 `&[u8]` です", "this parameter is a window `&[u8]`")
                        .hint("窓（束縛・引数）、`&配列`（`[u8; N]`）、文字列を渡せます", "pass a window (a binding or parameter), `&array` (a `[u8; N]`), or a string"),
                );
            }
        }
    }

    fn check_ref_arg(&mut self, a: &Expr, pm: bool, pt: &Ty) {
        if let Ty::Slice(_) = pt {
            return self.check_slice_arg(a, pm);
        }
        match &a.kind {
            ExprKind::Ref { mutable, inner } => {
                let it = self.check_expr(inner, None);
                if self.wire_int_field(inner).is_some() {
                    self.err(
                        Diag::error("F2701", a.span, "ワイヤ構造体の 2 バイト以上の整数フィールドは参照で渡せません（整列 1 で、バイト順の変換も要ります）", "an integer field of 2 bytes or more of a wire struct cannot be passed by reference (its alignment is 1 and it may need byte swapping)")
                            .hint("値を読んでローカル変数に入れ、呼び出しの後で書き戻してください", "read the value into a local variable and write it back after the call"),
                    );
                }
                let ir = self.resolve(&it);
                let target = match &ir {
                    Ty::Ref(_, x) => (**x).clone(),
                    t => t.clone(),
                };
                let target = match target {
                    Ty::Own(x) => *x,
                    t => t,
                };
                if !self.unify(pt, &target) {
                    self.mismatch(inner.span, pt, &target);
                }
                if pm && !*mutable {
                    let d = self.t.display(pt);
                    self.err(Diag::error(
                        "F0596",
                        a.span,
                        format!("この引数は `&mut {}` です。`&mut` で渡してください", d),
                        format!("this parameter is `&mut {}`; pass it with `&mut`", d),
                    ));
                }
                self.expr_ty[a.id as usize] = Ty::Ref(*mutable, Box::new(pt.clone()));
            }
            ExprKind::Path(name) => {
                let t = self.check_expr(a, None);
                let declared = match self.res.get(&a.id) {
                    Some(Res::Local(b)) => self.locals[*b as usize].as_ref().map(|l| l.ty.clone()),
                    _ => None,
                };
                let _ = name;
                let r = match declared {
                    Some(d @ Ty::Ref(..)) => {
                        // 参照の束縛をそのまま渡す（自動の `*` は付けない） / pass the reference on (no auto `*`)
                        self.auto_deref.remove(&a.id);
                        self.own_derefs.remove(&a.id);
                        self.expr_ty[a.id as usize] = d.clone();
                        self.resolve(&d)
                    }
                    _ => self.resolve(&t),
                };
                match &r {
                    Ty::Ref(m, x) => {
                        let target = match &**x {
                            Ty::Own(y) => (**y).clone(),
                            y => y.clone(),
                        };
                        if !self.unify(pt, &target) {
                            self.mismatch(a.span, pt, &target);
                        }
                        if pm && !*m {
                            self.err(Diag::error("F0596", a.span, "共有参照 `&T` を `&mut T` 引数へは渡せません", "a shared reference `&T` cannot be passed to a `&mut T` parameter"));
                        }
                    }
                    Ty::Err => {}
                    _ => {
                        let d = self.t.display(pt);
                        self.err(Diag::error(
                            "F0308",
                            a.span,
                            format!("この引数は参照 `&{}{}` です。`&` を付けて渡してください", if pm { "mut " } else { "" }, d),
                            format!("this parameter is a reference `&{}{}`; pass it with `&`", if pm { "mut " } else { "" }, d),
                        ));
                    }
                }
            }
            _ => {
                self.check_expr_noref(a);
                let d = self.t.display(pt);
                self.err(Diag::error(
                    "F0308",
                    a.span,
                    format!("この引数は参照 `&{}{}` です。場所（変数・フィールド・`a[id]`）に `&` を付けて渡してください", if pm { "mut " } else { "" }, d),
                    format!("this parameter is a reference `&{}{}`; pass a place (variable, field, `a[id]`) with `&`", if pm { "mut " } else { "" }, d),
                ));
            }
        }
    }

    fn check_expr_noref(&mut self, a: &Expr) -> Ty {
        if let ExprKind::Ref { inner, mutable } = &a.kind {
            let t = self.check_expr(inner, None);
            let r = Ty::Ref(*mutable, Box::new(t));
            self.expr_ty[a.id as usize] = r.clone();
            return r;
        }
        self.check_expr(a, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn check_method(&mut self, e: &Expr, recv: &Expr, method: &str, msp: Span, args: &[Expr], exp: Option<&Ty>, scrut_ok: bool) -> Ty {
        // リージョンのメソッド / region methods
        if let ExprKind::ArenaRef(name) = &recv.kind {
            return match self.t.regions.get(name).copied() {
                Some(RegionRef::Arena(a)) => self.check_arena_method(e, a, method, msp, args, scrut_ok),
                Some(RegionRef::Mmio(m)) => self.check_mmio_method(e, m, method, msp, args),
                Some(RegionRef::Dma(d)) => {
                    for a in args {
                        self.check_expr_noref(a);
                    }
                    match (method, args.len()) {
                        ("phys", 0) => {
                            self.methods.insert(e.id, MethodKind::DmaRegionPhys(d));
                            Ty::Int(IntTy::U64)
                        }
                        ("len", 0) => {
                            self.methods.insert(e.id, MethodKind::DmaRegionLen(d));
                            Ty::Int(IntTy::U64)
                        }
                        _ => {
                            let n = self.t.dmas[d].name.clone();
                            self.err(Diag::error("F0599", msp, format!("`dma` リージョン `{}` にメソッド `{}` はありません（phys / len）", n, method), format!("the `dma` region `{}` has no method `{}` (phys / len)", n, method)));
                            Ty::Err
                        }
                    }
                }
                // チャネル: send は値を移動する（送った変数はもう使えない: F0382）。recv は無ければ None、recv_wait は届くまで待つ
                // channels: send moves the value (the sent variable is gone: F0382); recv gives None when empty, recv_wait waits
                Some(RegionRef::Chan(c)) => {
                    let elem = self.t.chans[c].elem.clone();
                    let n = self.t.chans[c].name.clone();
                    match (method, args.len()) {
                        ("send", 1) => {
                            let at = self.check_expr(&args[0], Some(&elem));
                            self.expect_ty(args[0].span, &elem, &at);
                            self.methods.insert(e.id, MethodKind::ChanSend(c));
                            Ty::Unit
                        }
                        ("recv", 0) => {
                            self.methods.insert(e.id, MethodKind::ChanRecv(c));
                            Ty::Option(Box::new(elem))
                        }
                        ("recv_wait", 0) => {
                            self.methods.insert(e.id, MethodKind::ChanRecvWait(c));
                            elem
                        }
                        _ => {
                            for a in args {
                                self.check_expr_noref(a);
                            }
                            self.err(Diag::error("F0599", msp, format!("チャネル `{}` にメソッド `{}` はありません（send(値) / recv() / recv_wait()）", n, method), format!("the channel `{}` has no method `{}` (send(value) / recv() / recv_wait())", n, method)));
                            Ty::Err
                        }
                    }
                }
                Some(RegionRef::UserMem(u)) => {
                    let n = self.t.umems[u].name.clone();
                    let task = self.t.umems[u].task;
                    if method == "of" && args.len() == 1 {
                        let want = if task == usize::MAX { Ty::Err } else { Ty::Id(task) };
                        let at = self.check_expr(&args[0], Some(&want));
                        self.expect_ty(args[0].span, &want, &at);
                        if !scrut_ok {
                            self.err(
                                Diag::error("F0515", e.span, format!("`{}.of(..)` の窓は `if let` / `match` の対象にだけ書けます（保存できません）", n), format!("the window of `{}.of(..)` can only be an `if let` / `match` scrutinee (it cannot be stored)", n))
                                    .hint(format!("`if let Some(m) = {}.of(id) {{ .. }}` と書いてください", n), format!("write `if let Some(m) = {}.of(id) {{ .. }}`", n)),
                            );
                        }
                        self.methods.insert(e.id, MethodKind::UserMemOf(u));
                        return Ty::Option(Box::new(Ty::Ref(true, Box::new(Ty::Slice(Box::new(Ty::Int(IntTy::U8)))))));
                    }
                    for a in args {
                        self.check_expr_noref(a);
                    }
                    self.err(Diag::error("F0599", msp, format!("ユーザーメモリ `{}` にメソッド `{}` はありません（`of(タスクの Id)` だけ）", n, method), format!("the user memory `{}` has no method `{}` (only `of(task_id)`)", n, method)));
                    Ty::Err
                }
                Some(RegionRef::Port(p)) => {
                    let n = self.t.ports[p].name.clone();
                    self.err(Diag::error(
                        "F0599",
                        msp,
                        format!("ポート `{}` にメソッド `{}` はありません（read / write / read_at / write_at、または `{}` への代入と読み出し）", n, method, n),
                        format!("port `{}` has no method `{}` (read / write / read_at / write_at, or assign to and read `{}`)", n, method, n),
                    ));
                    for a in args {
                        self.check_expr_noref(a);
                    }
                    Ty::Err
                }
                None => {
                    self.err(Diag::error("F2104", recv.span, format!("`{}` が見つかりません", name), format!("cannot find `{}`", name)));
                    for a in args {
                        self.check_expr_noref(a);
                    }
                    Ty::Err
                }
            };
        }
        let rt = self.check_expr(recv, None);
        let mut rr = self.resolve(&rt);
        // 構造体でない own のメソッドは中身に対して呼ぶ（`b.take()` は `(*b).take()`、`buf.len()` は配列の長さ）。
        // `.clone()` は own ごと複製する
        // methods on a non-struct own act on its contents (`b.take()` is `(*b).take()`, `buf.len()` is the array's
        // length); `.clone()` duplicates the whole own
        if method != "clone" {
            let inner = match rr.strip_ref() {
                Ty::Own(x) if !matches!(**x, Ty::Struct(_)) => Some((**x).clone()),
                _ => None,
            };
            if let Some(x) = inner {
                self.own_derefs.insert(recv.id);
                self.expr_ty[recv.id as usize] = x.clone();
                rr = x;
            }
        }
        let base = rr.strip_ref().clone();
        let base_val = match &base {
            Ty::Own(x) | Ty::DmaOwn(x) => (**x).clone(),
            t => t.clone(),
        };
        if let Ty::Var(_) = base_val {
            if method != "clone" {
                self.unknown_here(&base_val, recv.span, true);
                for a in args {
                    self.check_expr_noref(a);
                }
                return Ty::Err;
            }
        }
        let nargs = |n: usize, s: &mut Sema| {
            if args.len() != n {
                s.err(Diag::error("F0061", e.span, format!("`.{}()` の引数は {} 個です", method, n), format!("`.{}()` takes {} argument(s)", method, n)));
            }
        };
        let kind_ty: Option<(MethodKind, Ty)> = match (method, &base_val) {
            ("clone", t) if !t.is_err() => {
                nargs(0, self);
                let out = match &rr {
                    Ty::Ref(_, x) => (**x).clone(),
                    t => t.clone(),
                };
                Some((MethodKind::Clone, out))
            }
            ("is_some", Ty::Option(_)) => {
                nargs(0, self);
                Some((MethodKind::IsSome, Ty::Bool))
            }
            // 浮動小数点の値: ビットの並び・NaN・有限か・絶対値（型の決まらないリテラルは f64） / float values: the bits, NaN,
            // finiteness, the absolute value (a literal of undecided type is f64)
            ("to_bits" | "is_nan" | "is_finite" | "abs", Ty::Float(_) | Ty::FloatVar(_)) => {
                nargs(0, self);
                if let Ty::FloatVar(_) = &base_val {
                    self.unify(&base_val, &Ty::Float(FloatTy::F64));
                }
                let ft = match self.resolve(&base_val) {
                    Ty::Float(f) => f,
                    _ => FloatTy::F64,
                };
                Some(match method {
                    "to_bits" => (MethodKind::FloatToBits(ft), Ty::Int(ft.bits_int())),
                    "is_nan" => (MethodKind::FloatIsNan(ft), Ty::Bool),
                    "is_finite" => (MethodKind::FloatIsFinite(ft), Ty::Bool),
                    _ => (MethodKind::FloatAbs(ft), Ty::Float(ft)),
                })
            }
            ("is_none", Ty::Option(_)) => {
                nargs(0, self);
                Some((MethodKind::IsNone, Ty::Bool))
            }
            ("take", Ty::Option(_)) => {
                nargs(0, self);
                Some((MethodKind::Take, base_val.clone()))
            }
            ("unwrap", Ty::Option(i)) => {
                nargs(0, self);
                Some((MethodKind::Unwrap, (**i).clone()))
            }
            ("unwrap_or", Ty::Option(i)) => {
                nargs(1, self);
                let it = (**i).clone();
                if let Some(a) = args.first() {
                    let at = self.check_expr(a, Some(&it));
                    self.expect_ty(a.span, &it, &at);
                }
                Some((MethodKind::UnwrapOr, it))
            }
            ("len", Ty::Str) => {
                nargs(0, self);
                Some((MethodKind::StrLen, Ty::Int(IntTy::Usize)))
            }
            ("byte", Ty::Str) => {
                nargs(1, self);
                if let Some(a) = args.first() {
                    let at = self.check_expr(a, None);
                    if !self.resolve(&at).is_int() && !at.is_err() {
                        self.err(Diag::error("F0308", a.span, "添字は整数です", "the index must be an integer"));
                    }
                }
                Some((MethodKind::StrByte, Ty::Int(IntTy::U8)))
            }
            ("len", Ty::Array(..)) => {
                nargs(0, self);
                Some((MethodKind::ArrLen, Ty::Int(IntTy::Usize)))
            }
            ("index", Ty::Id(_)) | ("index", Ty::Dma(_)) | ("index", Ty::GpuBuf(_)) => {
                nargs(0, self);
                Some((MethodKind::IdIndex, Ty::Int(IntTy::U32)))
            }
            // ハンドルが指すスロットの番地（整数。Fascon には整数からポインタを作る式が無いので、知っても安全性は変わらない）
            // the address of the slot a handle refers to (an integer; Fascon has no way to make a pointer from an integer)
            ("phys", Ty::Dma(a)) => {
                nargs(0, self);
                Some((MethodKind::DmaPhys(*a), Ty::Int(IntTy::U64)))
            }
            ("len", Ty::Slice(_)) => {
                nargs(0, self);
                Some((MethodKind::SliceLen, Ty::Int(IntTy::U64)))
            }
            // 窓 `x.window(off, len)` / `x.window_mut(off, len)`: 収まらなければ None（止まらない）。第二級の参照なので
            // `if let` / `match` の対象にだけ書ける
            // windows: None when out of range (never stops); second-class references, so only as an `if let` / `match`
            // scrutinee
            ("window" | "window_mut", t) if is_bytes(t) => {
                let m = method == "window_mut";
                nargs(2, self);
                for a in args.iter().take(2) {
                    let at = self.check_expr(a, Some(&Ty::Int(IntTy::U64)));
                    self.expect_ty(a.span, &Ty::Int(IntTy::U64), &at);
                }
                self.bytes_recv_ok(recv, &rr, m, scrut_ok, e.span, method);
                Some((MethodKind::Window(m), Ty::Option(Box::new(Ty::Ref(m, Box::new(Ty::Slice(Box::new(Ty::Int(IntTy::U8)))))))))
            }
            // ビュー `x.view(T, off)` / `x.view_mut(T, off)`、複製 `x.read(T, off)`: T はワイヤ構造体
            // views and copies: T is a wire struct
            ("view" | "view_mut" | "read", t) if is_bytes(t) => {
                let m = method == "view_mut";
                nargs(2, self);
                let sid = match args.first().map(|x| &x.kind) {
                    Some(ExprKind::Path(n)) => self.t.struct_by_name.get(n).copied().filter(|s| self.t.structs[*s].wire.is_some()),
                    _ => None,
                };
                if sid.is_none() {
                    if let Some(x) = args.first() {
                        self.err(
                            Diag::error("F2701", x.span, format!("`.{}(T, off)` の T はワイヤ構造体の名前です", method), format!("the T of `.{}(T, off)` is the name of a wire struct", method))
                                .hint("`#[wire(le)]` か `#[wire(be)]` を付けた構造体だけを、バイト列の上に重ねられます", "only a struct marked `#[wire(le)]` or `#[wire(be)]` can be laid over bytes"),
                        );
                    }
                }
                if let Some(a) = args.get(1) {
                    let at = self.check_expr(a, Some(&Ty::Int(IntTy::U64)));
                    self.expect_ty(a.span, &Ty::Int(IntTy::U64), &at);
                }
                match sid {
                    Some(s) if method == "read" => Some((MethodKind::ReadAt(s), Ty::Option(Box::new(Ty::Struct(s))))),
                    Some(s) => {
                        self.bytes_recv_ok(recv, &rr, m, scrut_ok, e.span, method);
                        Some((MethodKind::View(m, s), Ty::Option(Box::new(Ty::Ref(m, Box::new(Ty::Struct(s)))))))
                    }
                    None => return Ty::Err,
                }
            }
            ("copy_from", t) if is_bytes(t) => {
                nargs(1, self);
                if let Some(a) = args.first() {
                    self.check_slice_arg(a, false);
                }
                self.bytes_recv_ok(recv, &rr, true, true, e.span, method);
                Some((MethodKind::CopyFrom, Ty::Unit))
            }
            // 止まらない算術（あふれてもパニックしない）。信頼できない値（デバイス・回線・ユーザー）の計算に使う
            // non-stopping arithmetic (no panic on overflow), for values that cannot be trusted (devices, the wire, users)
            (m, Ty::Int(it)) if int_op_code(m).is_some() => {
                let code = int_op_code(m).unwrap_or(0);
                let t = Ty::Int(*it);
                nargs(1, self);
                if let Some(a) = args.first() {
                    let at = self.check_expr(a, Some(&t));
                    self.expect_ty(a.span, &t, &at);
                }
                let rt = if (3..=5).contains(&code) { Ty::Option(Box::new(t)) } else { t };
                Some((MethodKind::IntOp(code), rt))
            }
            ("generation", Ty::Id(a)) => {
                nargs(0, self);
                let w = if self.t.arenas[*a].bump { IntTy::U64 } else { IntTy::U32 };
                Some((MethodKind::IdGeneration, Ty::Int(w)))
            }
            ("raw", Ty::Id(a)) => {
                nargs(0, self);
                // generic-os では Id は 64 ビット（`IdRaw` 型で受ければ移植できる） / Ids are 64 bits on generic-os (an `IdRaw`
                // binding stays portable)
                let w = if self.t.arenas[*a].bump || self.t.id64 { IntTy::U64 } else { IntTy::U32 };
                Some((MethodKind::IdRaw, Ty::Int(w)))
            }
            ("is_ok", Ty::Result(..)) => {
                nargs(0, self);
                Some((MethodKind::ResIsOk, Ty::Bool))
            }
            ("is_err", Ty::Result(..)) => {
                nargs(0, self);
                Some((MethodKind::ResIsErr, Ty::Bool))
            }
            ("unwrap", Ty::Result(t, _)) => {
                nargs(0, self);
                Some((MethodKind::ResUnwrap, (**t).clone()))
            }
            ("unwrap_or", Ty::Result(t, _)) => {
                nargs(1, self);
                let t = (**t).clone();
                if let Some(a) = args.first() {
                    let at = self.check_expr(a, Some(&t));
                    self.expect_ty(a.span, &t, &at);
                }
                Some((MethodKind::ResUnwrapOr, t))
            }
            ("code", Ty::Fault) => {
                nargs(0, self);
                Some((MethodKind::FaultCode, Ty::Int(IntTy::I64)))
            }
            ("len", Ty::User(i)) if matches!(**i, Ty::Str) => {
                nargs(0, self);
                Some((MethodKind::UserLen, Ty::Int(IntTy::U64)))
            }
            _ => None,
        };
        let _ = exp;
        match kind_ty {
            Some((k, t)) => {
                self.methods.insert(e.id, k);
                if k == MethodKind::Take && !self.place_is_mutable(recv) {
                    self.err(Diag::error(
                        "F0596",
                        recv.span,
                        "`.take()` は書き換え可能な場所（mut 変数・&mut 引数・アリーナのスロット）にだけ使えます",
                        "`.take()` needs a writable place (a mut variable, a &mut parameter or an arena slot)",
                    ));
                }
                // 引数を自分で検査した種類は、ここで検査し直さない / kinds that checked their own arguments are not rechecked here
                let own_args = matches!(k, MethodKind::UnwrapOr | MethodKind::ResUnwrapOr | MethodKind::SliceLen | MethodKind::Window(_) | MethodKind::View(..) | MethodKind::ReadAt(_) | MethodKind::CopyFrom | MethodKind::IntOp(_));
                if !own_args {
                    for a in args.iter().skip(if matches!(k, MethodKind::StrByte) { 1 } else { 0 }) {
                        if !matches!(k, MethodKind::StrByte) {
                            self.check_expr_noref(a);
                        }
                    }
                }
                t
            }
            None => {
                if !base_val.is_err() {
                    let d = self.t.display(&base_val);
                    let mut diag = Diag::error(
                        "F0599",
                        msp,
                        format!("`{}` にメソッド `{}` はありません（Fascon にユーザー定義メソッドはありません。自由関数を使ってください）", d, method),
                        format!("no method `{}` on `{}` (Fascon has no user-defined methods; use a free function)", method, d),
                    );
                    // 関数ポインタのフィールドを呼びたい場合（Rust と同じく括弧が要る） / calling a function-pointer field
                    // (parentheses are needed, as in Rust)
                    if let Ty::Struct(s) = base_val {
                        if let Some((_, f)) = self.t.structs[s].field(method) {
                            if let Ty::Fn(..) = f.ty {
                                diag = diag.hint(
                                    format!("フィールド `{}` の関数ポインタを呼ぶときは括弧で囲みます: `(x.{})(..)`", method, method),
                                    format!("to call the function pointer in field `{}`, wrap the field in parentheses: `(x.{})(..)`", method, method),
                                );
                            }
                        }
                    }
                    self.err(diag);
                }
                for a in args {
                    self.check_expr_noref(a);
                }
                Ty::Err
            }
        }
    }

    /// gpu_arena の引数の検査（F2706）: ハンドル `GpuBuf<@a>`（fence = false）か引換券 `GpuFence<@a>`（fence = true）で、
    /// 同じ gpu_arena のものでなければならない
    /// checks a gpu_arena argument (F2706): it must be the handle `GpuBuf<@a>` (fence = false) or the ticket
    /// `GpuFence<@a>` (fence = true) of this very gpu_arena
    fn gpu_handle_ok(&mut self, sp: Span, a: usize, got: &Ty, fence: bool) {
        let ok = match got.strip_ref() {
            Ty::GpuBuf(b) => !fence && *b == a,
            Ty::GpuFence(b) => fence && *b == a,
            Ty::Err => true,
            _ => false,
        };
        if ok {
            return;
        }
        let an = self.t.arenas[a].name.clone();
        let want = if fence { format!("GpuFence<{}>", an) } else { format!("GpuBuf<{}>", an) };
        let d = self.t.display(got.strip_ref());
        let (ja, en) = match got.strip_ref() {
            Ty::GpuBuf(b) | Ty::GpuFence(b) if *b != a => (format!("別の gpu_arena `{}` のもの", self.t.arenas[*b].name), format!("one of another gpu_arena, `{}`", self.t.arenas[*b].name)),
            _ => ("型が違う".to_string(), "of a different type".to_string()),
        };
        self.err(
            Diag::error("F2706", sp, format!("`{}` が要るところに `{}`（{}）が渡されています", want, d, ja), format!("`{}` is expected here, but `{}` ({}) is given", want, d, en))
                .hint(
                    "ハンドル `GpuBuf<..>` は alloc と reclaim が、引換券 `GpuFence<..>` は dispatch が返します。どちらも出てきた gpu_arena にだけ使えます",
                    "handles `GpuBuf<..>` come from alloc and reclaim, tickets `GpuFence<..>` from dispatch; each works only with the gpu_arena it came from",
                ),
        );
    }

    fn check_arena_method(&mut self, e: &Expr, a: usize, method: &str, msp: Span, args: &[Expr], scrut_ok: bool) -> Ty {
        if let Some(t) = self.check_process_method(e, a, method, msp, args) {
            return t;
        }
        let elem = self.t.arenas[a].elem.clone();
        let an = self.t.arenas[a].name.clone();
        let bump = self.t.arenas[a].bump;
        let task = self.t.arenas[a].task_stack.is_some();
        let dma = self.t.arenas[a].dma;
        let gpu = self.t.arenas[a].gpu;
        let dma_arg = |s: &mut Sema| {
            if let Some(x) = args.first() {
                let t = s.check_expr(x, Some(&Ty::Dma(a)));
                let r = s.resolve(&t);
                match r.strip_ref() {
                    Ty::Dma(b) if *b == a => {}
                    Ty::Err => {}
                    _ => s.mismatch(x.span, &Ty::Dma(a), &r),
                }
            }
        };
        let nargs = |n: usize, s: &mut Sema| {
            if args.len() != n {
                s.err(Diag::error("F0061", e.span, format!("`{}.{}()` の引数は {} 個です", an, method, n), format!("`{}.{}()` takes {} argument(s)", an, method, n)));
            }
        };
        let id_arg = |s: &mut Sema| {
            if let Some(x) = args.first() {
                let t = s.check_expr(x, Some(&Ty::Id(a)));
                let r = s.resolve(&t);
                match &r {
                    Ty::Id(b) if *b == a => {}
                    Ty::Err => {}
                    Ty::Var(_) => {
                        s.unify(&r, &Ty::Id(a));
                    }
                    _ => s.mismatch(x.span, &Ty::Id(a), &r),
                }
            }
        };
        if bump && matches!(method, "free" | "remove") {
            self.err(
                Diag::error(
                    "F2401",
                    msp,
                    format!("バンプアリーナ `{}` のスロットは 1 つずつは解放できません（呼び出しリージョンが戻るとまとめて O(1) で解放される）", an),
                    format!("slots of the bump arena `{}` are not freed one by one (they are released together, in O(1), when the call region returns)", an),
                )
                .hint("1 つずつ解放したい値は `arena` に置いてください", "keep values that are freed one by one in an `arena`"),
            );
            for x in args {
                self.check_expr_noref(x);
            }
            return Ty::Err;
        }
        if self.in_bypass > 0 {
            self.err(Diag::error("F0811", e.span, "volatile_bypass の中ではアリーナを操作できません", "arenas cannot be used inside volatile_bypass"));
        }
        let (k, t) = match method {
            "alloc" => {
                nargs(1, self);
                if let Some(x) = args.first() {
                    let t = self.check_expr(x, Some(&elem));
                    self.expect_ty(x.span, &elem, &t);
                }
                // dma_arena・gpu_arena はハンドル（移動専用）を返す / a dma_arena / gpu_arena returns the move-only handle
                let key = if dma {
                    Ty::Dma(a)
                } else if gpu {
                    Ty::GpuBuf(a)
                } else {
                    Ty::Id(a)
                };
                (MethodKind::ArenaAlloc(a), Ty::Option(Box::new(key)))
            }
            "free" if gpu => {
                nargs(1, self);
                if let Some(x) = args.first() {
                    let t = self.check_expr(x, Some(&Ty::GpuBuf(a)));
                    let r = self.resolve(&t);
                    self.gpu_handle_ok(x.span, a, &r, false);
                }
                (MethodKind::ArenaFree(a), Ty::Bool)
            }
            // 装置へ出す: ハンドルはここで消え（以後の使用は F0382）、完了を確かめる引換券 GpuFence が返る
            // offloading: the handle is consumed here (later use is F0382) and a GpuFence, the ticket to check completion,
            // comes back
            "dispatch" if gpu => {
                nargs(4, self);
                for (i, want) in [Ty::Int(IntTy::U32), Ty::Int(IntTy::U64), Ty::Int(IntTy::U64)].iter().enumerate() {
                    if let Some(x) = args.get(i) {
                        let t = self.check_expr(x, Some(want));
                        self.expect_ty(x.span, want, &t);
                    }
                }
                if let Some(x) = args.get(3) {
                    let t = self.check_expr(x, Some(&Ty::GpuBuf(a)));
                    let r = self.resolve(&t);
                    self.gpu_handle_ok(x.span, a, &r, false);
                }
                (MethodKind::GpuDispatch(a), Ty::GpuFence(a))
            }
            // 回収: 引換券の仕事が終わっていればハンドルが戻る（reclaim は待たない。reclaim_wait は終わるまで待つ）。同じ
            // 引換券で 2 度目は None（ハンドルは 1 つだけ）
            // reclaiming: when the ticket's work is done the handle comes back (reclaim does not wait; reclaim_wait waits
            // until it is done). A second reclaim with the same ticket gives None (there is only one handle)
            "reclaim" | "reclaim_wait" if gpu => {
                nargs(1, self);
                if let Some(x) = args.first() {
                    let t = self.check_expr(x, Some(&Ty::GpuFence(a)));
                    let r = self.resolve(&t);
                    self.gpu_handle_ok(x.span, a, &r, true);
                }
                let k = if method == "reclaim" { MethodKind::GpuReclaim(a) } else { MethodKind::GpuReclaimWait(a) };
                (k, Ty::Option(Box::new(Ty::GpuBuf(a))))
            }
            m if gpu && !matches!(m, "len" | "capacity") => {
                for x in args {
                    self.check_expr_noref(x);
                }
                self.err(
                    Diag::error("F2706", msp, format!("gpu_arena `{}` にメソッド `{}` はありません（alloc / free / dispatch / reclaim / reclaim_wait / len / capacity）", an, method), format!("the gpu_arena `{}` has no method `{}` (alloc / free / dispatch / reclaim / reclaim_wait / len / capacity)", an, method))
                        .hint("スロットには `名前[h]`（h は `GpuBuf<名前>`）で届きます。Id を使う操作（get・contains・validate など）はありません", "slots are reached with `name[h]` (h is a `GpuBuf<name>`); there are no Id-based operations (get, contains, validate, ...)"),
                );
                return Ty::Err;
            }
            "free" if dma => {
                nargs(1, self);
                dma_arg(self);
                (MethodKind::ArenaFree(a), Ty::Bool)
            }
            // 貸し出し: 所有権がデバイスへ移る（ハンドルはここで消える）。戻す道は reclaim だけ
            // lending: ownership moves to the device (the handle is consumed here); reclaim is the only way back
            "lend" if dma => {
                nargs(1, self);
                dma_arg(self);
                (MethodKind::DmaLend(a), Ty::Unit)
            }
            // 回収: デバイスが返した添字（信頼できない）を検証する。範囲内かつ貸し出し中のときだけハンドルが戻る
            // reclaiming: validates an index returned by the device (untrusted); a handle comes back only when it is in
            // range and lent
            "reclaim" if dma => {
                nargs(1, self);
                if let Some(x) = args.first() {
                    let t = self.check_expr(x, Some(&Ty::Int(IntTy::U32)));
                    if !self.resolve(&t).is_int() && !t.is_err() {
                        self.err(Diag::error("F0308", x.span, "`reclaim` の引数は整数（デバイスが返した添字）です", "the argument of `reclaim` is an integer (the index returned by the device)"));
                    }
                }
                (MethodKind::DmaReclaim(a), Ty::Option(Box::new(Ty::Dma(a))))
            }
            m if dma && !matches!(m, "len" | "capacity") => {
                for x in args {
                    self.check_expr_noref(x);
                }
                self.err(
                    Diag::error("F2703", msp, format!("dma_arena `{}` にメソッド `{}` はありません（alloc / free / lend / reclaim / len / capacity）", an, method), format!("the dma_arena `{}` has no method `{}` (alloc / free / lend / reclaim / len / capacity)", an, method))
                        .hint("スロットには `名前[h]`（h は `Dma<名前>`）で届きます。Id を使う操作（get・validate など）はありません", "slots are reached with `name[h]` (h is a `Dma<name>`); there are no Id-based operations (get, validate, ...)"),
                );
                return Ty::Err;
            }
            "free" | "remove" if self.t.arenas[a].gc => {
                nargs(1, self);
                id_arg(self);
                let an = self.t.arenas[a].name.clone();
                self.err(
                    Diag::error(
                        "F2621",
                        e.span,
                        format!("gc_arena `{}` には `{}` を呼べません（解放は fascon-gc の仕事です）", an, method),
                        format!("`{}` cannot be called on the gc_arena `{}` (freeing is fascon-gc's job)", method, an),
                    )
                    .hint(
                        "参照を外す（Id を持つフィールド・変数を None にする）と、次の `gc_collect(..)` で回収されます。手で寿命を決めたいならふつうの `arena` にしてください",
                        "drop the references (set the Id fields / variables to None) and the next `gc_collect(..)` reclaims it; use a plain `arena` to manage lifetimes by hand",
                    ),
                );
                return Ty::Err;
            }
            "free" => {
                nargs(1, self);
                id_arg(self);
                (MethodKind::ArenaFree(a), Ty::Bool)
            }
            "remove" => {
                nargs(1, self);
                id_arg(self);
                (MethodKind::ArenaRemove(a), Ty::Option(Box::new(elem.clone())))
            }
            "contains" => {
                nargs(1, self);
                id_arg(self);
                (MethodKind::ArenaContains(a), Ty::Bool)
            }
            "len" => {
                nargs(0, self);
                (MethodKind::ArenaLen(a), Ty::Int(IntTy::Usize))
            }
            // task_arena: 生成 = alloc + 文脈の用意。入口は `fn(u64)`
            // task_arena: creation = alloc + preparing the context; the entry is a `fn(u64)`
            "start" if task => {
                nargs(3, self);
                if let Some(x) = args.first() {
                    let t = self.check_expr(x, Some(&elem));
                    self.expect_ty(x.span, &elem, &t);
                }
                let ft = Ty::Fn(vec![Ty::Int(IntTy::U64)], Box::new(Ty::Unit));
                if let Some(x) = args.get(1) {
                    let t = self.check_expr(x, Some(&ft));
                    let r = self.resolve(&t);
                    if r != ft && !r.is_err() {
                        let d = self.t.display(&r);
                        self.err(Diag::error(
                            "F2605",
                            x.span,
                            format!("タスクの入口は `fn(u64)` です（`{}` は不可）", d),
                            format!("a task entry is a `fn(u64)` (not `{}`)", d),
                        ));
                    }
                }
                if let Some(x) = args.get(2) {
                    let t = self.check_expr(x, Some(&Ty::Int(IntTy::U64)));
                    self.expect_ty(x.span, &Ty::Int(IntTy::U64), &t);
                }
                self.task_starts.insert(e.id);
                (MethodKind::ArenaAlloc(a), Ty::Option(Box::new(Ty::Id(a))))
            }
            // EL0 のタスク: 入口の番地とスタックの番地（どちらもユーザーの仮想番地）
            // an EL0 task: its entry address and stack address (both user virtual addresses)
            "start_user" if task => {
                nargs(3, self);
                if let Some(x) = args.first() {
                    let t = self.check_expr(x, Some(&elem));
                    self.expect_ty(x.span, &elem, &t);
                }
                for x in args.iter().skip(1).take(2) {
                    let t = self.check_expr(x, Some(&Ty::Int(IntTy::U64)));
                    self.expect_ty(x.span, &Ty::Int(IntTy::U64), &t);
                }
                if !self.t.umems.iter().any(|u| u.task == a) {
                    let an = self.t.arenas[a].name.clone();
                    self.err(
                        Diag::error("F2803", msp, format!("`{}.start_user(..)` には、このタスク表のユーザーメモリの宣言が要ります", an), format!("`{}.start_user(..)` needs a user memory declared for this task arena", an))
                            .hint(format!("`user_mem umem for {}: 0x20_0000 at 0x1_0000_0000;` を足してください", an), format!("add `user_mem umem for {}: 0x20_0000 at 0x1_0000_0000;`", an)),
                    );
                }
                self.task_starts.insert(e.id);
                self.user_starts.insert(e.id);
                (MethodKind::ArenaAlloc(a), Ty::Option(Box::new(Ty::Id(a))))
            }
            "current" if task => {
                nargs(0, self);
                (MethodKind::TaskCurrent(a), Ty::Id(a))
            }
            "yield_to" if task => {
                nargs(1, self);
                id_arg(self);
                (MethodKind::TaskYield(a), Ty::Unit)
            }
            "resume" if task => {
                nargs(1, self);
                id_arg(self);
                (MethodKind::TaskResume(a), Ty::Unit)
            }
            "enter" if task => {
                nargs(1, self);
                id_arg(self);
                (MethodKind::TaskEnter(a), Ty::Never)
            }
            "exit" if task => {
                nargs(0, self);
                (MethodKind::TaskExit(a), Ty::Never)
            }
            "capacity" => {
                nargs(0, self);
                (MethodKind::ArenaCapacity(a), Ty::Int(IntTy::Usize))
            }
            "validate" => {
                nargs(1, self);
                // バンプアリーナの Id は 64 ビット / Ids of bump arenas are 64 bits
                let raw = Ty::Int(if bump { IntTy::U64 } else { IntTy::U32 });
                if let Some(x) = args.first() {
                    if self.t.id64 && !bump {
                        // generic-os: 生の値は 64 ビット。ソースを変えずに通すため、符号なしの整数ならどの幅でも受ける（64 ビット
                        // 未満は世代が入らないのでいつも None になる: F3010 の警告）
                        // generic-os: raw values are 64 bits; to keep sources unchanged any unsigned width is accepted (below 64
                        // bits there is no room for the generation, so the result is always None: warning F3010)
                        let t = self.check_expr(x, None);
                        let r = self.want_int(&t);
                        match r {
                            Ty::Int(it) if !it.signed() => {
                                if it.bits(self.t.ptr_bits) < 64 {
                                    self.err(
                                        Diag::warning("F3010", x.span, format!("--target generic-os では Id の生の値は 64 ビットです。`{}` の値は世代を持てないので validate はいつも None を返します", it.name()), format!("with --target generic-os a raw Id is 64 bits; a `{}` value cannot hold the generation, so validate always returns None", it.name()))
                                            .hint("`IdRaw` 型（rtos では u32、generic-os では u64）で受け渡すと、どちらのターゲットでも同じソースで動きます", "pass raw Ids as `IdRaw` (u32 on rtos, u64 on generic-os) and the same source works on both targets"),
                                    );
                                }
                            }
                            Ty::IntVar(_) => {
                                let u = Ty::Int(IntTy::U64);
                                self.unify(&r, &u);
                            }
                            Ty::Err => {}
                            other => self.mismatch(x.span, &Ty::Int(IntTy::U64), &other),
                        }
                    } else {
                        let t = self.check_expr(x, Some(&raw));
                        self.expect_ty(x.span, &raw, &t);
                    }
                }
                (MethodKind::ArenaValidate(a), Ty::Option(Box::new(Ty::Id(a))))
            }
            "get" | "get_mut" => {
                nargs(1, self);
                id_arg(self);
                let m = method == "get_mut";
                if !scrut_ok {
                    self.err(
                        Diag::error(
                            "F0515",
                            e.span,
                            format!("`{}.{}(id)` の結果は参照なので、`if let` / `match` の対象か、`.unwrap()` を付けた場所としてだけ使えます（第二級参照）", an, method),
                            format!("`{}.{}(id)` yields a reference, so it can only be the scrutinee of `if let` / `match`, or a place followed by `.unwrap()` (second-class references)", an, method),
                        )
                        .hint(
                            format!("`if let Some(p) = {}.{}(id) {{ .. }}` か `{}[id]`（= `{}.get_mut(id).unwrap()`）を使ってください", an, method, an, an),
                            format!("write `if let Some(p) = {}.{}(id) {{ .. }}` or `{}[id]` (= `{}.get_mut(id).unwrap()`)", an, method, an, an),
                        ),
                    );
                }
                (if m { MethodKind::ArenaGetMut(a) } else { MethodKind::ArenaGet(a) }, Ty::Option(Box::new(Ty::Ref(m, Box::new(elem.clone())))))
            }
            _ => {
                let list = if bump { "alloc / contains / get / get_mut / validate / len / capacity" } else { "alloc / free / remove / contains / get / get_mut / validate / len / capacity" };
                self.err(Diag::error(
                    "F0599",
                    msp,
                    format!("アリーナ `{}` にメソッド `{}` はありません（{}）", an, method, list),
                    format!("arena `{}` has no method `{}` ({})", an, method, list),
                ));
                for x in args {
                    self.check_expr_noref(x);
                }
                return Ty::Err;
            }
        };
        self.methods.insert(e.id, k);
        t
    }

    fn check_mmio_method(&mut self, e: &Expr, m: usize, method: &str, msp: Span, args: &[Expr]) -> Ty {
        let info = self.t.mmios[m].clone();
        let et = Ty::Int(info.elem);
        let n = info.dims.len();
        let nargs = |k: usize, s: &mut Sema| {
            if args.len() != k {
                s.err(Diag::error("F0061", e.span, format!("`{}.{}()` の引数は {} 個です", info.name, method, k), format!("`{}.{}()` takes {} argument(s)", info.name, method, k)));
            }
        };
        let int_args = |s: &mut Sema, from: usize, to: usize, exp: Option<Ty>| {
            for x in args.iter().take(to).skip(from) {
                let t = s.check_expr(x, exp.as_ref());
                if let Some(ex) = &exp {
                    s.expect_ty(x.span, ex, &t);
                } else if !s.resolve(&t).is_int() && !t.is_err() {
                    s.err(Diag::error("F0308", x.span, "整数が必要です", "an integer is expected"));
                }
            }
        };
        let (k, t) = match (method, n) {
            ("len", 1) => {
                nargs(0, self);
                (MethodKind::MmioLen(m), Ty::Int(IntTy::Usize))
            }
            ("width", 2) => {
                nargs(0, self);
                (MethodKind::MmioWidth(m), Ty::Int(IntTy::Usize))
            }
            ("height", 2) => {
                nargs(0, self);
                (MethodKind::MmioHeight(m), Ty::Int(IntTy::Usize))
            }
            ("fill", _) => {
                nargs(1, self);
                if let Some(x) = args.first() {
                    let t = self.check_expr(x, Some(&et));
                    self.expect_ty(x.span, &et, &t);
                }
                (MethodKind::MmioFill(m), Ty::Unit)
            }
            ("put", 2) => {
                nargs(3, self);
                int_args(self, 0, 2, None);
                if let Some(x) = args.get(2) {
                    let t = self.check_expr(x, Some(&et));
                    self.expect_ty(x.span, &et, &t);
                }
                (MethodKind::MmioPut(m), Ty::Bool)
            }
            ("fill_rect", 2) => {
                nargs(5, self);
                int_args(self, 0, 4, Some(Ty::Int(IntTy::I32)));
                if let Some(x) = args.get(4) {
                    let t = self.check_expr(x, Some(&et));
                    self.expect_ty(x.span, &et, &t);
                }
                (MethodKind::MmioFillRect(m), Ty::Unit)
            }
            _ => {
                let list = if n == 2 { "width / height / fill / put / fill_rect" } else { "len / fill" };
                self.err(Diag::error(
                    "F0599",
                    msp,
                    format!("MMIO リージョン `{}` にメソッド `{}` はありません（{}）", info.name, method, list),
                    format!("MMIO region `{}` has no method `{}` ({})", info.name, method, list),
                ));
                for x in args {
                    self.check_expr_noref(x);
                }
                return Ty::Err;
            }
        };
        self.methods.insert(e.id, k);
        t
    }
}

fn contains_ivar(t: &Ty) -> bool {
    match t {
        Ty::IntVar(_) | Ty::Var(_) | Ty::FloatVar(_) => true,
        Ty::Option(i) | Ty::Own(i) | Ty::Array(i, _) | Ty::Ref(_, i) => contains_ivar(i),
        Ty::Fn(ps, r) => ps.iter().any(contains_ivar) || contains_ivar(r),
        _ => false,
    }
}

/// 式を前順にすべて訪ねる / visit every expression in pre-order
pub fn walk_block(b: &Block, f: &mut dyn FnMut(&Expr)) {
    fn ex(e: &Expr, f: &mut dyn FnMut(&Expr)) {
        f(e);
        for c in crate::flow::children(e) {
            match c {
                crate::flow::Child::E(x) => ex(x, f),
                crate::flow::Child::B(b) => walk_block(b, f),
            }
        }
    }
    for st in &b.stmts {
        match st {
            Stmt::Let(l) => {
                if let Some(i) = &l.init {
                    ex(i, f);
                }
            }
            Stmt::Expr(e) => ex(e, f),
        }
    }
    if let Some(t) = &b.tail {
        ex(t, f);
    }
}

/// 関数の本体の let の束縛番号をすべて訪ねる / visit the bind ids of every let in a function body
fn collect_let_binds(b: &Block, f: &mut dyn FnMut(BindId)) {
    fn ex(e: &Expr, f: &mut dyn FnMut(BindId)) {
        for c in crate::flow::children(e) {
            match c {
                crate::flow::Child::E(x) => ex(x, f),
                crate::flow::Child::B(b) => collect_let_binds(b, f),
            }
        }
    }
    for st in &b.stmts {
        match st {
            Stmt::Let(l) => {
                f(l.bind);
                if let Some(i) = &l.init {
                    ex(i, f);
                }
            }
            Stmt::Expr(e) => ex(e, f),
        }
    }
    if let Some(t) = &b.tail {
        ex(t, f);
    }
}

/// 関数の中の式の番号の範囲 [lo, hi)（構文解析は式に順に番号を振るので、1 つの関数の式は連続している）
/// the range [lo, hi) of expression ids inside a function (the parser numbers expressions in order, so they are contiguous)
fn fn_expr_range(f: &FnDef) -> (ExprId, ExprId) {
    let mut lo = u32::MAX;
    let mut hi = 0u32;
    fn ex(e: &Expr, lo: &mut u32, hi: &mut u32) {
        *lo = (*lo).min(e.id);
        *hi = (*hi).max(e.id + 1);
        for c in crate::flow::children(e) {
            match c {
                crate::flow::Child::E(x) => ex(x, lo, hi),
                crate::flow::Child::B(b) => bl(b, lo, hi),
            }
        }
        if let ExprKind::For { iter: ForIter::Range { lo: a, hi: b, .. }, .. } = &e.kind {
            ex(a, lo, hi);
            ex(b, lo, hi);
        }
    }
    fn bl(b: &Block, lo: &mut u32, hi: &mut u32) {
        for st in &b.stmts {
            match st {
                Stmt::Let(l) => {
                    if let Some(i) = &l.init {
                        ex(i, lo, hi);
                    }
                    if let Some(TypeExpr::Array { len, .. }) = &l.ty {
                        ex(len, lo, hi);
                    }
                }
                Stmt::Expr(e) => ex(e, lo, hi),
            }
        }
        if let Some(t) = &b.tail {
            ex(t, lo, hi);
        }
    }
    bl(&f.body, &mut lo, &mut hi);
    if lo == u32::MAX {
        (0, 0)
    } else {
        (lo, hi)
    }
}

fn collect_value_struct_deps(t: &Ty, out: &mut Vec<usize>) {
    match t {
        Ty::Struct(s) => out.push(*s),
        Ty::Option(i) | Ty::Array(i, _) => collect_value_struct_deps(i, out),
        _ => {}
    }
}

pub fn builtin_fn_name(n: &str) -> bool {
    matches!(
        n,
        "print_int"
            | "print_str"
            | "put_int"
            | "put_hex"
            | "put_str"
            | "put"
            | "panic"
            | "drop"
            | "cpu_relax"
            | "fence"
            | "compiler_fence"
            | "cpu_id"
            | "mmio_read8"
            | "mmio_read16"
            | "mmio_read32"
            | "mmio_read64"
            | "mmio_write8"
            | "mmio_write16"
            | "mmio_write32"
            | "mmio_write64"
            | "heap_live_blocks"
            | "heap_live_bytes"
            | "halt"
            | "exit"
            | "Some"
            | "chan"
            | "block_on"
    )
}

fn builtin_type_name(n: &str) -> bool {
    IntTy::from_name(n).is_some() || FloatTy::from_name(n).is_some() || matches!(n, "bool" | "Str" | "Option" | "Id" | "Chan")
}

/// 場所式（変数・フィールド・添字） / place expressions (variables, fields, indexing)
pub fn is_place_expr(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Path(_) => true,
        ExprKind::Field(b, _, _) => is_place_expr(b),
        ExprKind::Index(b, _) => matches!(b.kind, ExprKind::ArenaRef(_)) || is_place_expr(b),
        _ => false,
    }
}

/// static の初期値として C の定数式に落とせるか / can the static initializer be lowered to a C constant?
fn is_const_expr(e: &Expr, t: &Tables) -> bool {
    match &e.kind {
        ExprKind::Int(..) | ExprKind::Float(..) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Unit => true,
        // 関数の名前（関数ポインタ）は C の番地定数 / a function name (a function pointer) is a C address constant
        ExprKind::Path(n) => {
            n == "None"
                || t.static_by_name.get(n).map_or(false, |&s| t.statics[s].int_value.is_some())
                || (t.fn_by_name.contains_key(n) && n != "main")
                || n.split_once("::").map_or(false, |(a, b)| crate::lexer::FloatTy::from_name(a).is_some() && FLOAT_CONSTS.contains(&b))
        }
        // 浮動小数点から整数への as は飽和の関数を通るので C の定数式にならない / a float-to-integer `as` goes through the
        // saturating function, so it is not a C constant expression
        ExprKind::Cast(x, TypeExpr::Path { name, .. }) if IntTy::from_name(name).is_some() && has_float_leaf(x) => false,
        ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => is_const_expr(x, t),
        ExprKind::Binary(op, a, b) => !matches!(op, BinOp::And | BinOp::Or) && is_const_expr(a, t) && is_const_expr(b, t),
        ExprKind::StructLit { fields, .. } => fields.iter().all(|(_, _, v)| is_const_expr(v, t)),
        ExprKind::ArrayLit(v) => v.iter().all(|x| is_const_expr(x, t)),
        ExprKind::ArrayRepeat(v, _) => is_const_expr(v, t),
        ExprKind::Call { callee, args, .. } => callee == "Some" && args.iter().all(|x| is_const_expr(x, t)),
        _ => false,
    }
}


/// 浮動小数点のリテラルか定数を含むか / whether a float literal or constant appears
fn has_float_leaf(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Float(..) => true,
        ExprKind::Path(n) => n.starts_with("f32::") || n.starts_with("f64::"),
        ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => has_float_leaf(x),
        ExprKind::Binary(_, a, b) => has_float_leaf(a) || has_float_leaf(b),
        _ => false,
    }
}

// ───────────── ユニバーサル OS 拡張の型検査 / type checks of the universal-OS extension ─────────────

impl<'p> Sema<'p> {
    /// `atomic { 場所 op= 値; }` の形（F3201）: 文は 1 つ、アリーナのスロットの整数（か bool）フィールドへの代入か複合代入、
    /// 右辺は純粋（呼び出し・ループ・ほかのスロットの読み出しなし）
    /// the shape of `atomic { place op= value; }` (F3201): one statement, an assignment or compound assignment to an integer
    /// (or bool) field of an arena slot, with a pure right-hand side (no calls, loops or reads of other slots)
    fn check_atomic_shape(&mut self, e: &Expr, b: &Block) {
        // gc_arena の確保・gc_collect・promote_to_gc を含む区間は F2622 だけで報告する（形の誤り F3201 を重ねない）
        // a section containing a gc_arena allocation, gc_collect or promote_to_gc is reported by F2622 alone (no F3201 on top)
        fn has_gc(s: &Sema, e: &Expr) -> bool {
            let hit = match &e.kind {
                ExprKind::Call { callee, .. } => callee == "gc_collect" || callee == "promote_to_gc",
                ExprKind::MethodCall { recv, method, .. } if method == "alloc" => match &recv.kind {
                    ExprKind::ArenaRef(n) | ExprKind::Path(n) => s.t.arena_of_name(n).map_or(false, |a| s.t.arenas[a].gc),
                    _ => false,
                },
                _ => false,
            };
            hit || crate::flow::children(e).into_iter().any(|c| match c {
                crate::flow::Child::E(x) => has_gc(s, x),
                crate::flow::Child::B(bb) => bb.stmts.iter().any(|st| match st {
                    Stmt::Expr(x) => has_gc(s, x),
                    Stmt::Let(l) => l.init.as_ref().map_or(false, |i| has_gc(s, i)),
                }) || bb.tail.as_ref().map_or(false, |t| has_gc(s, t)),
            })
        }
        let any_gc = b.stmts.iter().any(|st| match st {
            Stmt::Expr(x) => has_gc(self, x),
            Stmt::Let(l) => l.init.as_ref().map_or(false, |i| has_gc(self, i)),
        }) || b.tail.as_ref().map_or(false, |t| has_gc(self, t));
        if any_gc {
            return;
        }
        let single = match (b.stmts.as_slice(), &b.tail) {
            ([Stmt::Expr(x)], None) => Some(x),
            ([], Some(t)) => Some(&**t),
            _ => None,
        };
        let bad = |s: &mut Sema, sp: Span, ja: &str, en: &str| {
            s.err(
                Diag::error("F3201", sp, format!("`atomic {{ .. }}` の中身が使えない形です: {}", ja), format!("unsupported `atomic {{ .. }}` body: {}", en))
                    .hint(
                        "`atomic { 表[id].フィールド op= 値; }` の 1 文だけ（op は = += -= |= &= ^=）。複数のフィールドや呼び出しは `critical { .. }` で書いてください",
                        "exactly one `atomic { table[id].field op= value; }` statement (op is = += -= |= &= ^=); several fields or calls go in `critical { .. }`",
                    ),
            );
        };
        let Some(x) = single else {
            bad(self, e.span, "文が 1 つではありません", "it is not exactly one statement");
            return;
        };
        // `atomic { 表[id].f }`: 不可分な読み出し（acquire）/ an indivisible load (acquire)
        if b.stmts.is_empty() && matches!(x.kind, ExprKind::Field(..)) {
            let mut path = Vec::new();
            let mut cur: &Expr = x;
            while let ExprKind::Field(base, f, _) = &cur.kind {
                path.push(f.clone());
                cur = base;
            }
            path.reverse();
            let arena = match &cur.kind {
                ExprKind::Index(base, _) => match &base.kind {
                    ExprKind::ArenaRef(n) => self.t.arena_of_name(n),
                    _ => None,
                },
                _ => None,
            };
            let lt = self.resolve(&self.expr_ty[x.id as usize].clone());
            match arena {
                Some(a) if !self.t.arenas[a].bump && matches!(lt, Ty::Int(_) | Ty::Bool | Ty::Err) => {
                    if !self.atomic_fields.iter().any(|(q, p)| *q == a && *p == path) {
                        self.atomic_fields.push((a, path.clone()));
                    }
                    self.atomic_sites.insert(e.id, (a, path));
                }
                _ => bad(self, x.span, "不可分に読めるのはアリーナのスロットの整数か bool のフィールドだけです", "only an integer or bool field of an arena slot can be loaded indivisibly"),
            }
            return;
        }
        let ExprKind::Assign { op, lhs, rhs } = &x.kind else {
            bad(self, x.span, "代入（= += -= |= &= ^=）ではありません", "it is not an assignment (= += -= |= &= ^=)");
            return;
        };
        if !matches!(op, None | Some(BinOp::Add) | Some(BinOp::Sub) | Some(BinOp::BitOr) | Some(BinOp::BitAnd) | Some(BinOp::BitXor)) {
            bad(self, x.span, "演算は = += -= |= &= ^= だけです", "the operation must be = += -= |= &= ^=");
            return;
        }
        // 左辺: 表[id].f(.g)* / the left side: table[id].f(.g)*
        let mut path = Vec::new();
        let mut cur: &Expr = lhs;
        while let ExprKind::Field(base, f, _) = &cur.kind {
            path.push(f.clone());
            cur = base;
        }
        path.reverse();
        let arena = match &cur.kind {
            ExprKind::Index(base, _) => match &base.kind {
                ExprKind::ArenaRef(n) => self.t.arena_of_name(n),
                _ => None,
            },
            _ => None,
        };
        let Some(a) = arena.filter(|_| !path.is_empty()) else {
            bad(self, lhs.span, "左辺がアリーナのスロットのフィールド（`表[id].フィールド`）ではありません", "the left side is not a field of an arena slot (`table[id].field`)");
            return;
        };
        if self.t.arenas[a].bump {
            bad(self, lhs.span, "bump_arena は対象外です", "bump_arena slots are not supported");
            return;
        }
        let lt = self.resolve(&self.expr_ty[lhs.id as usize].clone());
        if !matches!(lt, Ty::Int(_) | Ty::Bool | Ty::Err) {
            let d = self.t.display(&lt);
            bad(self, lhs.span, &format!("フィールドの型 `{}` は整数でも bool でもありません", d), &format!("the field type `{}` is neither an integer nor bool", d));
            return;
        }
        if lt == Ty::Bool && op.is_some() {
            bad(self, x.span, "bool のフィールドには `=` だけが使えます", "only `=` applies to a bool field");
            return;
        }
        // 右辺は純粋: 呼び出し・アリーナ・ループを含まない / the right side is pure: no calls, arenas or loops
        let mut impure = None;
        fn scan(e: &Expr, out: &mut Option<Span>) {
            match &e.kind {
                ExprKind::Call { .. } | ExprKind::CallIndirect { .. } | ExprKind::MethodCall { .. } | ExprKind::ArenaRef(_) | ExprKind::Loop { .. } | ExprKind::While { .. } | ExprKind::For { .. } | ExprKind::Assign { .. } => {
                    if out.is_none() {
                        *out = Some(e.span);
                    }
                }
                _ => {}
            }
            for c in crate::flow::children(e) {
                if let crate::flow::Child::E(x) = c {
                    scan(x, out);
                }
            }
        }
        scan(rhs, &mut impure);
        // 添字（Id）にも呼び出し・ほかのスロットの読み出しを書かない / no calls or other slot reads in the index (the Id) either
        if let ExprKind::Index(_, idx) = &cur.kind {
            scan(idx, &mut impure);
        }
        if let Some(sp) = impure {
            bad(self, sp, "右辺と添字には呼び出し・ほかのスロット・ループを書けません（1 回の RMW に収まらない）", "the right side and the index cannot contain calls, other slots or loops (it would not fit in one RMW)");
            return;
        }
        if !self.atomic_fields.iter().any(|(x, p)| *x == a && *p == path) {
            self.atomic_fields.push((a, path.clone()));
        }
        self.atomic_sites.insert(e.id, (a, path));
    }

    /// fascon-gc: `gc_collect(a)`（回収したスロット数 u32）・`promote_to_gc(a, bump_id)`（バンプアリーナの要素を gc_arena へ
    /// 写す。Option<Id<a>>）。第 1 引数はアリーナの名前（値ではない）
    /// fascon-gc: `gc_collect(a)` (u32: slots reclaimed) and `promote_to_gc(a, bump_id)` (copies a bump-arena element into
    /// the gc_arena; Option<Id<a>>). The first argument is an arena name (not a value)
    fn check_gc_call(&mut self, e: &Expr, callee: &str, csp: Span, args: &[Expr]) -> Ty {
        let want = if callee == "gc_collect" { 1 } else { 2 };
        if args.len() != want {
            self.err(Diag::error("F0061", csp, format!("`{}` の引数は {} 個です", callee, want), format!("`{}` takes {} argument(s)", callee, want)));
            for a in args {
                self.check_expr_noref(a);
            }
            return Ty::Err;
        }
        let name = match &args[0].kind {
            ExprKind::Path(n) | ExprKind::ArenaRef(n) => Some(n.clone()),
            _ => None,
        };
        let ga = name.as_ref().and_then(|n| self.t.arena_of_name(n)).filter(|&a| self.t.arenas[a].gc);
        let Some(ga) = ga else {
            self.err(
                Diag::error(
                    "F0061",
                    args[0].span,
                    format!("`{}` の第 1 引数は gc_arena の名前です", callee),
                    format!("the first argument of `{}` is the name of a gc_arena", callee),
                )
                .hint("例: `gc_collect(nodes);`（`gc_arena nodes: Node[1024];` を宣言しておく）", "e.g. `gc_collect(nodes);` (with `gc_arena nodes: Node[1024];` declared)"),
            );
            for a in &args[1..] {
                self.check_expr_noref(a);
            }
            return Ty::Err;
        };
        if callee == "gc_collect" {
            self.gc_calls.insert(e.id, (ga, None));
            return Ty::Int(IntTy::U32);
        }
        let bt = self.check_expr(&args[1], None);
        let bt = self.resolve(&bt);
        match bt {
            Ty::Id(b) if self.t.arenas[b].bump && self.t.arenas[b].elem == self.t.arenas[ga].elem => {
                self.gc_calls.insert(e.id, (ga, Some(b)));
                Ty::Option(Box::new(Ty::Id(ga)))
            }
            Ty::Err => Ty::Err,
            other => {
                let d = self.t.display(&other);
                let ge = self.t.display(&self.t.arenas[ga].elem.clone());
                self.err(Diag::error(
                    "F0308",
                    args[1].span,
                    format!("`promote_to_gc` の第 2 引数は、要素が `{}` の bump_arena の Id です（`{}` は不可）", ge, d),
                    format!("the second argument of `promote_to_gc` is an Id of a bump_arena whose element is `{}` (not `{}`)", ge, d),
                ));
                Ty::Err
            }
        }
    }

    /// `bind_mmio(Regs, 番地)`・`dma_alloc(T)`・`complete_dma(T, iova)` / type calls
    fn check_type_call(&mut self, e: &Expr, name: &str, nsp: Span, te: &TypeExpr, args: &[Expr]) -> Ty {
        let t = self.resolve_type(te, false, &HashMap::new());
        let nargs = |s: &mut Sema, n: usize| {
            if args.len() != n {
                s.err(Diag::error("F0061", nsp, format!("`{}` の引数は型名を含めて {} 個です", name, n + 1), format!("`{}` takes {} argument(s) including the type name", name, n + 1)));
            }
        };
        match name {
            "bind_mmio" => {
                nargs(self, 1);
                if let Some(a) = args.first() {
                    let at = self.check_expr(a, Some(&Ty::Int(IntTy::U64)));
                    self.expect_ty(a.span, &Ty::Int(IntTy::U64), &at);
                }
                match t {
                    Ty::Struct(sid) if self.is_reg_struct(sid, 0) => {
                        self.type_calls.insert(e.id, TypeCallKind::BindMmio(sid));
                        Ty::MmioBind(sid)
                    }
                    Ty::Err => Ty::Err,
                    other => {
                        let d = self.t.display(&other);
                        self.err(
                            Diag::error("F3404", te.span(), format!("`bind_mmio({}, ..)` は書けません（レジスタ構造体: u8〜u64 の整数かその配列だけのフィールド）", d), format!("`bind_mmio({}, ..)` is not allowed (a register struct has only u8-u64 integer fields or arrays of them)", d))
                                .hint("予約域は `_rsvd: [u32; 3]` のような配列のフィールドで埋めてください", "fill reserved gaps with array fields such as `_rsvd: [u32; 3]`"),
                        );
                        Ty::Err
                    }
                }
            }
            "dma_alloc" | "complete_dma" => {
                nargs(self, if name == "dma_alloc" { 0 } else { 1 });
                if name == "complete_dma" {
                    if let Some(a) = args.first() {
                        let at = self.check_expr(a, Some(&Ty::Int(IntTy::U64)));
                        self.expect_ty(a.span, &Ty::Int(IntTy::U64), &at);
                    }
                }
                if t.is_err() {
                    return Ty::Err;
                }
                if !self.is_pod(&t) {
                    let d = self.t.display(&t);
                    self.err(Diag::error("F3403", te.span(), format!("`{}({}, ..)` は書けません（デバイスと共有する値は POD: 整数・その配列・ワイヤ構造体）", name, d), format!("`{}({}, ..)` is not allowed (values shared with a device are POD: integers, arrays of them, wire structs)", name, d)));
                    return Ty::Err;
                }
                if self.user_mode {
                    self.user_ban(e.span, name);
                }
                if name == "dma_alloc" {
                    self.type_calls.insert(e.id, TypeCallKind::DmaAlloc(t.clone()));
                    Ty::DmaOwn(Box::new(t))
                } else {
                    self.type_calls.insert(e.id, TypeCallKind::CompleteDma(t.clone()));
                    Ty::Option(Box::new(Ty::DmaOwn(Box::new(t))))
                }
            }
            _ => Ty::Err,
        }
    }

    /// `copy_from_user(ubuf)` / `copy_to_user(ubuf, &kbuf)`: ubuf は #[syscall] の `user T` 引数の名前そのもの
    /// ubuf must be the name of a `user T` parameter of the #[syscall] function itself
    fn check_user_copy(&mut self, e: &Expr, b: Builtin, args: &[Expr]) -> Ty {
        let from = b == Builtin::CopyFromUser;
        let need = if from { 1 } else { 2 };
        if args.len() != need {
            self.err(Diag::error("F0061", e.span, format!("`{}` の引数は {} 個です", if from { "copy_from_user" } else { "copy_to_user" }, need), format!("`{}` takes {} argument(s)", if from { "copy_from_user" } else { "copy_to_user" }, need)));
        }
        let fault = Ty::Fault;
        let mut ut = Ty::Err;
        if let Some(a) = args.first() {
            let t = match &a.kind {
                ExprKind::Path(_) => self.check_expr(a, None),
                _ => {
                    self.check_expr_noref(a);
                    Ty::Err
                }
            };
            ut = self.resolve(&t);
            if !matches!(ut, Ty::User(_) | Ty::Err) || !matches!(a.kind, ExprKind::Path(_)) {
                let d = self.t.display(&ut);
                self.err(
                    Diag::error("F3101", a.span, format!("ここには `user T` の引数の名前を書きます（`{}` は不可）", d), format!("the name of a `user T` parameter is expected here (not `{}`)", d))
                        .hint("ユーザー空間のバッファは `#[syscall(N)] fn f(ubuf: user [u8; 256])` の引数として受け取ります", "a user-space buffer arrives as a parameter of `#[syscall(N)] fn f(ubuf: user [u8; 256])`"),
                );
                ut = Ty::Err;
            }
        }
        if from {
            let inner = match &ut {
                Ty::User(i) => match &**i {
                    Ty::Array(..) => (**i).clone(),
                    // `user Str`: 長さは実行時に上限（既定 4096 バイト）と比べ、超えれば写さずに EFAULT
                    // `user Str`: the length is checked against the limit (4096 bytes by default) at run time; beyond it, EFAULT without copying
                    _ => Ty::Own(Box::new(Ty::Array(Box::new(Ty::Int(IntTy::U8)), self.user_str_max))),
                },
                _ => Ty::Err,
            };
            if inner.is_err() {
                return Ty::Err;
            }
            Ty::Result(Box::new(inner), Box::new(fault))
        } else {
            if let Some(k) = args.get(1) {
                // 書き出す元: `&kbuf`（`[u8; M]`）か窓 / the source: `&kbuf` (a `[u8; M]`) or a window
                self.check_slice_arg(k, false);
            }
            Ty::Result(Box::new(Ty::Unit), Box::new(fault))
        }
    }
}

// ───────────── プロセス表（generic-os のアドレス空間・TLB）/ process tables (generic-os address spaces and TLBs) ─────────────

impl<'p> Sema<'p> {
    /// 表のアリーナに足すプロセスのメソッド。名前が違えば None（ふつうのメソッドとして調べる）
    /// process methods added to table arenas; None when the name is not one of them (checked as an ordinary method)
    fn check_process_method(&mut self, e: &Expr, a: usize, method: &str, msp: Span, args: &[Expr]) -> Option<Ty> {
        const NAMES: [&str; 11] = ["on_cpu", "off_cpu", "cpu_mask", "map_page", "share_page", "cow_copy", "unmap_page", "tlb_flush", "peek", "tlb_local_count", "tlb_ipi_count"];
        if !NAMES.contains(&method) {
            return None;
        }
        let an = self.t.arenas[a].name.clone();
        if self.t.arenas[a].bump || self.t.arenas[a].dma || self.t.arenas[a].gpu {
            self.err(Diag::error("F0599", msp, format!("`{}.{}()` はふつうの表のアリーナ（プロセス表）にだけ使えます", an, method), format!("`{}.{}()` applies only to an ordinary table arena (a process table)", an, method)));
            return Some(Ty::Err);
        }
        if self.profile != crate::target::Profile::GenericOs {
            self.err(
                Diag::error("F3011", msp, format!("`{}.{}()`（アドレス空間・TLB の操作）は `--target generic-os` だけで使えます", an, method), format!("`{}.{}()` (address-space / TLB operations) is available with `--target generic-os` only", an, method))
                    .hint("ファイルの先頭に `// fascon-target: generic-os` を書くか、`--target generic-os` を付けてください", "put `// fascon-target: generic-os` at the top of the file or pass `--target generic-os`"),
            );
            return Some(Ty::Err);
        }
        self.process_arenas.insert(a);
        let want = |s: &mut Sema, i: usize, t: Ty| {
            if let Some(x) = args.get(i) {
                let at = s.check_expr(x, Some(&t));
                s.expect_ty(x.span, &t, &at);
            }
        };
        let n = |s: &mut Sema, k: usize| {
            if args.len() != k {
                s.err(Diag::error("F0061", e.span, format!("`{}.{}()` の引数は {} 個です", an, method, k), format!("`{}.{}()` takes {} argument(s)", an, method, k)));
            }
        };
        let page = Ty::Own(Box::new(Ty::Array(Box::new(Ty::Int(IntTy::U8)), 4096)));
        let (k, t) = match method {
            "on_cpu" | "off_cpu" => {
                n(self, 2);
                want(self, 0, Ty::Id(a));
                want(self, 1, Ty::Int(IntTy::U32));
                (if method == "on_cpu" { MethodKind::ProcOnCpu(a) } else { MethodKind::ProcOffCpu(a) }, Ty::Unit)
            }
            "cpu_mask" => {
                n(self, 1);
                want(self, 0, Ty::Id(a));
                (MethodKind::ProcCpuMask(a), Ty::Int(IntTy::U64))
            }
            // ページの所有権はアドレス空間へ移る（以後の page の使用は F0382） / the page's ownership moves into the address space
            "map_page" => {
                n(self, 4);
                want(self, 0, Ty::Id(a));
                want(self, 1, Ty::Int(IntTy::U64));
                want(self, 2, page.clone());
                want(self, 3, Ty::Bool);
                (MethodKind::ProcMapPage(a), Ty::Result(Box::new(Ty::Unit), Box::new(Ty::Fault)))
            }
            "share_page" => {
                n(self, 3);
                want(self, 0, Ty::Id(a));
                want(self, 1, Ty::Int(IntTy::U64));
                want(self, 2, Ty::Id(a));
                (MethodKind::ProcSharePage(a), Ty::Result(Box::new(Ty::Unit), Box::new(Ty::Fault)))
            }
            // 共有ページ（CoW）の中身を新しい own ページへ写して返す（共有でなければ None） / copy a shared (CoW) page into a fresh
            // own page (None when the page is not shared)
            "cow_copy" => {
                n(self, 2);
                want(self, 0, Ty::Id(a));
                want(self, 1, Ty::Int(IntTy::U64));
                (MethodKind::ProcCowCopy(a), Ty::Option(Box::new(page.clone())))
            }
            "unmap_page" => {
                n(self, 2);
                want(self, 0, Ty::Id(a));
                want(self, 1, Ty::Int(IntTy::U64));
                (MethodKind::ProcUnmapPage(a), Ty::Option(Box::new(page)))
            }
            "tlb_flush" => {
                n(self, 2);
                want(self, 0, Ty::Id(a));
                want(self, 1, Ty::Int(IntTy::U64));
                (MethodKind::ProcTlbFlush(a), Ty::Unit)
            }
            "peek" => {
                n(self, 3);
                want(self, 0, Ty::Id(a));
                want(self, 1, Ty::Int(IntTy::U64));
                want(self, 2, Ty::Int(IntTy::U64));
                (MethodKind::ProcPeek(a), Ty::Int(IntTy::U8))
            }
            _ => {
                n(self, 0);
                (MethodKind::ProcTlbCount(a, method == "tlb_ipi_count"), Ty::Int(IntTy::U64))
            }
        };
        self.methods.insert(e.id, k);
        Some(t)
    }
}
