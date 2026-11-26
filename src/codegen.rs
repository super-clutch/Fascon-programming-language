//! C コード生成（読みやすい C11） / C code generation (readable C11)
//!
//! 方針 / policy
//! - 構造体は値（インライン）。own T は所有リージョンへのポインタ、Id<@a> は uint32_t。
//!   structs are inline values; own T is a pointer to its own region; Id<@a> is a uint32_t.
//! - 構造体のフィールドにポインタが現れるのは own T（唯一の所有者）だけ。参照はローカル変数と引数にしか現れない。
//!   the only pointers stored in struct fields are own T (the unique owner); references live only in locals and parameters.
//! - 評価順は Fascon の左から右。副作用のある部分式は一時変数へ順に取り出す。
//!   evaluation order is Fascon's left-to-right; side-effecting subexpressions are hoisted into temporaries in order.

use crate::ast::*;
use crate::diag::Lang;
use crate::flow::FlowResults;
use crate::lexer::{FloatTy, IntTy};
use crate::sema::{Builtin, CallKind, LocalKind, MethodKind, Res, Sema};
use crate::span::{SourceFile, Span};
use crate::types::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

#[path = "codegen_gc.rs"]
mod gc;
#[path = "codegen_tsoa.rs"]
mod tsoa;
use tsoa::TsField;

/// 非時間的ストアの使い方（`--nontemporal=auto|on|off`） / how non-temporal stores are used (`--nontemporal=auto|on|off`)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NtMode {
    /// 大きな完全列化の列への一括上書きだけ（割り込み・タスクと共有するアリーナと、直後に読み直す列は除く）
    /// only bulk overwrites of large True-SoA columns (not arenas shared with interrupt handlers or tasks, nor a column
    /// the next statement reads again)
    Auto,
    /// 一括上書きのループすべて（ループの出口に F20_SFENCE） / every bulk-overwrite loop (F20_SFENCE at the loop exit)
    On,
    /// 使わない / never
    Off,
}

pub struct CgOpts {
    pub lang: Lang,
    pub ptr_bits: u32,
    pub version: &'static str,
    /// `for id in @a` の集計ごとに列（集計・ビット・鍵）を表に足すか（--columns off で 1.0.0 と同じ表）
    /// whether reductions get columns (aggregate / bit / key) in the table (--columns off gives the 1.0.0 table)
    pub columns: bool,
    /// ターゲット（I/O ポートの写し方を決める） / the target (decides how I/O ports are lowered)
    pub target: crate::target::Target,
    /// OS のプロファイル（Classic の C は今までと 1 文字も変わらない） / the OS profile (Classic C is unchanged to the byte)
    pub profile: crate::target::Profile,
    /// `--board board.toml` で作った fascon_board.h を読み、main の最初に装置の窓を登録する
    /// read the fascon_board.h made from `--board board.toml` and register device windows at the start of main
    pub board: bool,
    /// 完全列化（True SoA）: 条件を満たすフィールドをスロットから外し、列を唯一の正本にする（`--columns mirrored` で切る）
    /// True SoA: eligible fields leave the slot and their column becomes the only copy (`--columns mirrored` turns it off)
    pub true_soa: bool,
    /// 非時間的ストア / non-temporal stores
    pub nontemporal: NtMode,
}

/// `fascon regions` に出す列 1 本 / one column, as reported by `fascon regions`
#[derive(Default, Clone)]
pub struct ColReport {
    pub arena: String,
    pub name: String,
    /// 1 スロットあたりの大きさ（"8 B" / "1 bit"） / size per slot ("8 B" / "1 bit")
    pub per_slot: String,
    /// 列全体のバイト数（番兵込み） / bytes of the whole column (sentinel included)
    pub bytes: u64,
    pub kind_ja: String,
    pub kind_en: String,
    /// その列を読む集計の行 / source lines of the reductions that read it
    pub lines: Vec<usize>,
    /// True-SoA (single-write) / Mirrored-SoA (dual-write)
    pub mode_ja: String,
    pub mode_en: String,
}

#[derive(Default, Clone)]
pub struct CgReport {
    /// 実行ルートと文脈（割り込みハンドラ・タスク・モード・共有アリーナ・閉じた区間） / execution roots and contexts
    pub ctx_lines: Vec<String>,
    pub own_sites: Vec<(usize, String, usize)>,
    /// その場で止める検査（own を持つ要素の @a[id]） / checks that stop on the spot (@a[id] on elements holding own)
    pub gen_checked: usize,
    /// Option を返すルックアップ（None の腕はソースが書く） / lookups returning an Option (the source handles None)
    pub gen_lookups: usize,
    pub gen_elided: usize,
    /// 世代が合わなければ番兵スロットへ逸らしてラッチを立てる検査（分岐なし） / checks that divert to the sentinel and latch (branch-free)
    pub gen_latched: usize,
    pub bounds_checked: usize,
    pub bounds_elided: usize,
    /// 総和の特殊化を適用した `for id in @a` ループ / `for id in @a` loops lowered to the sum specialization
    pub reductions: usize,
    /// そのうちマスク付き（条件・個数・最小 / 最大） / of those, masked ones (filtered sums, counts, min / max)
    pub masked_reductions: usize,
    /// 生存ビットマップ + ctz で回す一般の `for id in @a` / general `for id in @a` loops driven by the live bitmap + ctz
    pub bitmap_loops: usize,
    /// ループの中からループの出口へ移したコミット点 / commit points moved from inside a loop to its exit
    pub hoisted_commits: usize,
    /// ループの終了条件に混ぜたラッチ（while / loop） / latches folded into a loop's exit condition (while / loop)
    pub folded_commits: usize,
    /// フレームリージョンに置いた own（エスケープしない let） / own values placed in the frame region (non-escaping lets)
    pub frame_sites: Vec<(usize, String)>,
    /// 表に足した列 / columns added to the tables
    pub columns: Vec<ColReport>,
    /// 集計のうち列だけを読むもの / reductions that read columns only
    pub column_reductions: usize,
    /// 列の写しを作り直す場所（Id での書き込み・&mut の借用の終わり・&mut を渡した呼び出し） / places that rebuild a column
    /// entry (writes by Id, ends of &mut borrows, calls that received &mut)
    pub mirror_sites: usize,
    /// バンプアリーナの呼び出しリージョン（関数名, アリーナ名） / call regions of bump arenas (function, arena)
    pub call_regions: Vec<(String, String)>,
    /// I/O ポートの読み書きの数 / I/O port reads and writes
    pub port_accesses: usize,
    /// own_alloc の場所（行, 型） / own_alloc sites (line, type)
    pub own_alloc_sites: Vec<(usize, String)>,
    /// バンプアリーナを回す `for id in B`（0..top の直線ループ） / `for id in B` over bump arenas (straight 0..top loops)
    pub bump_loops: usize,
    /// 列を作らなかった集計（アリーナ, 行, 理由 ja, en） / reductions that got no columns (arena, line, reason ja, en)
    pub column_skips: Vec<(String, usize, String, String)>,
    /// Spectre v1 のマスク（CSDB）を入れた添字 / indices that got the Spectre v1 mask (CSDB)
    pub spectre_sites: usize,
    /// PAC で署名した関数ポインタの値・認証した間接呼び出し / function-pointer values signed and indirect calls authenticated with PAC
    pub pac_signs: usize,
    pub pac_auths: usize,
    /// BTI c を付けた関数（コールバックの候補） / functions given BTI c (callback candidates)
    pub bti_fns: usize,
    /// 値として使われた関数と割り込みハンドラの C 名（fascon stack が間接呼び出しと根に使う）
    /// C names of functions used as values and of interrupt handlers (used by fascon stack for indirect calls and roots)
    pub callback_fns: Vec<String>,
    pub irq_fns: Vec<String>,
    /// 完全列化を見送ったフィールド（アリーナ, フィールド, 理由 ja, en） / fields kept off True SoA (arena, field, reason ja, en)
    pub tsoa_skips: Vec<(String, String, String, String)>,
    /// 一括上書きのループ（アリーナ, フィールド, 行, 非時間的ストアか, 理由 ja, en） / bulk-overwrite loops (arena, fields, line,
    /// non-temporal or not, reason ja, en)
    pub nt_loops: Vec<(String, String, usize, bool, String, String)>,
    /// 条件付き一括更新（Masked Column RMW）のループ（アリーナ, フィールド, 行, 走査関数, 条件のビット列, 全部 1 の語は非時間的
    /// ストアか） / masked column RMW loops (arena, field, line, kernel, filter bit column, full words non-temporal or not)
    pub rmw_loops: Vec<(String, String, usize, String, String, bool)>,
    /// 完全列化したフィールド（アリーナ, ソースのフィールド名）: fascon regions のスロットの大きさに使う / True-SoA fields
    /// (arena, source field names), used for the slot size in fascon regions
    pub tsoa_layout: Vec<(String, Vec<String>)>,
    /// 写しの列を作り直す書き込みを含む `for id in @a`（関数, ループの場所, アリーナ）: 表ごとの作り直し（--target hpc の
    /// F3902） / `for id in @a` loops containing writes that rebuild mirror columns (function, loop site, arena): table-wide
    /// rebuilds (F3902 under --target hpc)
    pub mirror_loops: Vec<(usize, crate::span::Span, String)>,
    /// 写しの列を作り直す書き込みを持つ関数（関数, アリーナ）。`for id in @a` の中から呼ぶと、呼び出しの回数だけ作り直す
    /// = 表ごとの作り直し（--target hpc の F3902 が呼び出しグラフでたどる） / functions containing writes that rebuild mirror
    /// columns (function, arena); called from inside `for id in @a` they rebuild once per call = table-wide (F3902 under
    /// --target hpc follows them through the call graph)
    pub mirror_fns: Vec<(usize, usize)>,
}

/// 列の種類 / the kind of a column
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColKind {
    /// 集計の列 agg[i] = 条件 ? 値 : 0（wide: u64、そうでなければ u32） / aggregate column (wide: u64, else u32)
    Agg { wide: bool },
    /// 絞り込みのビット列（生存 && 条件） / filter bit column (live && filter)
    Bits,
    /// 選択の鍵の列（生存 && 条件 ? enc(値) : NONE） / pick-key column (live && filter ? enc(value) : NONE)
    Key { it: IntTy, max: bool },
    /// 浮動小数点の選択の鍵の列（IEEE-754 の順序を保つ鍵 F20_KEY_F32 / F20_KEY_F64。NaN と死んだスロットは NONE）
    /// float pick-key column (the IEEE-754 order-preserving keys F20_KEY_F32 / F20_KEY_F64; NaN and dead slots are NONE)
    FKey { ft: FloatTy, max: bool },
}

/// 表に足す列 1 本（コンパイラが集計から決める） / one column added to a table (chosen by the compiler from the reductions)
struct DerivedCol {
    kind: ColKind,
    /// C の名前（`arena_<名前>.col.<name>`） / the C member name
    name: String,
    /// 値の C 式（要素は `(*v)`。Bits は空） / the value in C (the element is `(*v)`; empty for Bits)
    val: String,
    /// 条件の C 式（空 = 条件なし） / the filter in C (empty = none)
    cond: String,
    /// 列が読む要素のフィールドの道筋（これに届く書き込みだけが写しを作り直す） / element field paths the column reads
    /// (only writes reaching one of them rebuild the mirror)
    deps: Vec<Vec<String>>,
    lines: Vec<usize>,
}

/// `for id in @a` の集計がどの列を読むか / which columns a `for id in @a` reduction reads
#[derive(Clone)]
enum ColUse {
    /// 項ごとの集計の列 / one aggregate column per term
    Sum(Vec<usize>),
    Count(usize),
    /// last: `<=` / `>=`（同じ値なら後の添字） / last: `<=` / `>=` (the later index wins a tie)
    Pick { col: usize, last: bool },
    /// 項のどれかが完全列化の列を読む総和 / a sum where some term reads a True-SoA column
    SumT(Vec<SumSrc>),
    /// 完全列化の列の選択（mask: 条件のビット列。None なら生存ビットマップ） / a pick over a True-SoA column (mask: the
    /// filter bit column; None = the live bitmap)
    PickT { f: usize, mask: Option<usize>, last: bool },
}

/// 総和の項の読み先 / where a sum term is read from
#[derive(Clone, Copy)]
enum SumSrc {
    /// 写しの集計の列 / a mirrored aggregate column
    Col(usize),
    /// 完全列化の列（mask: 条件のビット列。None なら条件なし） / a True-SoA column (mask: the filter bit column; None = no filter)
    Ts { f: usize, mask: Option<usize> },
}

/// 総和の特殊化の項: 要素からのフィールドの道筋と、その整数型
/// a term of the sum specialization: the field path from the element and its integer type
struct RedTerm {
    path: Vec<String>,
    it: IntTy,
    /// 浮動小数点のフィールド（#[fast_math] の集計だけ。it は同じ幅の符号なし整数） / a float field (only in #[fast_math]
    /// reductions; it is the unsigned integer of the same width)
    fty: Option<FloatTy>,
}

struct RedPlan<'b> {
    acc: BindId,
    acc_it: IntTy,
    /// 浮動小数点の集計（#[fast_math]）: 列で走査できるときだけ使い、できなければふつうのループに戻す
    /// a float reduction (#[fast_math]): used only when columns serve it, otherwise the plain loop stays
    fty: Option<FloatTy>,
    kind: RedKind,
    /// Sum: 足すフィールド。MinMax: 比べて写すフィールド 1 個 / Sum: the fields added; MinMax: the one field compared and copied
    terms: Vec<RedTerm>,
    /// Count: 1 スロットあたりに足す定数 / Count: the constant added per qualifying slot
    count_c: u128,
    /// 絞り込みの条件（純粋。死んだスロットの 0 に対しても評価してよい） / the filter (pure; may be evaluated on dead, all-zero slots)
    conds: Vec<&'b Expr>,
    /// 本体が `if let Some(t) = @a.get(id)` で包まれていたか / whether the body was wrapped in `if let Some(t) = @a.get(id)`
    lookup: bool,
    elem_bind: Option<BindId>,
    var: BindId,
    /// MinMax: 最小なら true / true for a minimum
    less: bool,
    /// MinMax: `<` / `>`（最初に見つけた方が勝つ）なら true、`<=` / `>=`（最後が勝つ）なら false
    /// MinMax: true for `<` / `>` (the first one found wins), false for `<=` / `>=` (the last one wins)
    strict: bool,
    /// MinMax: `pick = Some(id)` / `pick = id` の pick / the pick of `pick = Some(id)` / `pick = id`
    pick: Option<BindId>,
}

/// マスク付き集計の文脈（どのローカルが何か） / context of a masked reduction (which local is which)
#[derive(Clone, Copy)]
struct RedCx {
    a: usize,
    var: BindId,
    elem: Option<BindId>,
    acc: Option<BindId>,
    pick: Option<BindId>,
}

struct LoopInfo {
    depth: usize,
    cont_label: Option<String>,
    /// C の 2 重ループで書いたとき、`break` の飛び先のラベル / the `break` target label when lowered to two nested C loops
    brk_label: Option<String>,
    brk_used: bool,
}

impl LoopInfo {
    fn new(depth: usize, cont_label: Option<String>) -> LoopInfo {
        LoopInfo { depth, cont_label, brk_label: None, brk_used: false }
    }
}

/// マスク付き集計の種類 / the kind of a masked reduction
/// 条件付き一括更新（Masked Column RMW）の割り当て: 完全列化のフィールド・条件のビット列・`+=` か `=` か
/// a masked column RMW assignment: the True-SoA field, the filter bit column, `+=` or `=`
#[derive(Clone, Copy)]
struct RmwUse {
    f: usize,
    mask: usize,
    add: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RedKind {
    /// フィールドの和（死んだスロットは 0 なので生存マスク不要） / a sum of fields (dead slots are zero: no live mask)
    Sum,
    /// 定数 c の和 = 条件を満たす生存スロットの数 × c / a sum of a constant c = c × the number of live slots that qualify
    Count,
    /// 最小・最大（と、その Id）。死んだスロットを生存マスクで外し、CSEL / CMOV で選ぶ
    /// minimum / maximum (and its Id): dead slots are masked out with the live bitmap and the pick is a CSEL / CMOV
    MinMax,
}

pub struct Cg<'a, 'p> {
    s: &'a Sema<'p>,
    fr: &'a FlowResults,
    src: &'a SourceFile,
    o: CgOpts,
    file_tag: String,
    types_out: String,
    glue_protos: String,
    glue_out: String,
    // 型の出力管理 / type emission bookkeeping
    emitted_structs: HashSet<usize>,
    emitting: HashSet<usize>,
    opt_done: HashSet<String>,
    need_drop: BTreeSet<usize>,
    need_own_drop: BTreeSet<usize>,
    /// fascon-gc: 解放の前に持ち主の変数・フィールドを NULL にする解放関数（fsc_own_drop_at_*） / fascon-gc: drop functions
    /// that clear the owning variable or field to NULL before freeing (fsc_own_drop_at_*)
    need_own_drop_at: BTreeSet<usize>,
    need_own_drop_at_ty: BTreeMap<String, Ty>,
    need_clone: BTreeSet<usize>,
    need_own_clone: BTreeSet<usize>,
    /// `own_alloc(T)` が使う 0 埋めの子作り（fsc_zfill_T） / zero-filled child builders used by `own_alloc(T)` (fsc_zfill_T)
    need_zfill: BTreeSet<usize>,
    /// 構造体でない own の解放・複製の糊（名前の部品 → 中身の型） / drop / clone glue of non-struct owns (name part → contents)
    need_own_drop_ty: BTreeMap<String, Ty>,
    need_own_clone_ty: BTreeMap<String, Ty>,
    /// 関数ポインタ型ごとの C の typedef 名 / the C typedef name per function pointer type
    fn_types: HashMap<Ty, String>,
    // 関数ごとの状態 / per-function state
    lines: Vec<String>,
    ind: usize,
    tmp: u32,
    names: HashMap<BindId, String>,
    used: HashSet<String>,
    scopes: Vec<Vec<BindId>>,
    loops: Vec<LoopInfo>,
    ret_ty: Ty,
    fn_c_names: HashMap<usize, String>,
    /// この関数が遅延フォールト `_flt` を使うか / whether this function uses the deferred-fault latch `_flt`
    flt_used: bool,
    /// 2 回目の生成で確定した「ラッチを使うか」（1 回目は None） / whether the latch is used, known in the second pass (None in the first)
    flt_known: Option<bool>,
    /// fascon-gc: 型記述子（前方宣言・本体・作ったもの） / type descriptors (forward declarations, bodies, done set)
    gc_desc_fwd: String,
    gc_desc_out: String,
    gc_desc_done: HashSet<String>,
    /// fascon-gc: この関数が根の枠を使うか（1 回目は None） / whether this function uses a root frame (None in the first pass)
    gc_known: Option<bool>,
    gc_used: bool,
    /// fascon-gc: この関数の根の番号・宣言の行を待っている根 / root numbers of this function, roots awaiting their declaration line
    gc_k: u32,
    gc_pending: Vec<(String, String)>,
    /// fascon-gc: gc_arena の Id 専用の射影列（列の番号, 指すヒープ） / Id-only projection columns (column, target heap)
    gc_cols: HashMap<usize, Vec<(usize, usize)>>,
    /// fascon-gc: この関数は共有の gc_arena に触れる（ループの頭に安全点）・rcu_read の深さ / this function touches a shared gc
    /// arena (safepoints at loop heads), rcu_read depth
    gc_poll_fn: bool,
    gc_rcu_depth: u32,
    /// fascon-gc: 回収を起こしうる関数（gc_collect・安全点・切り替えを推移的に含む。最初に使うときに求める）
    /// functions that may collect (gc_collect, safepoints or switches, transitively; computed on first use)
    gc_collectors: Option<Vec<bool>>,
    /// fascon-gc: いまの文で根に登録した一時値の番号（文の終わりで外す） / root numbers of temporaries registered in the
    /// current statement (removed at the end of the statement)
    gc_stmt_temps: Vec<u32>,
    /// fascon-gc: 根にした own を持つ一時値（受け手へ渡すときに根から外す） / fascon-gc: rooted temporaries holding own
    /// (removed from the roots when handed to their receiver)
    gc_own_temps: Vec<u32>,
    /// 値として読むだけの場所を下ろしている最中か（読み出しは番兵へ逸らさなくてよい）
    /// whether a place is being lowered for a pure read (reads need not be diverted to the sentinel)
    read_ctx: bool,
    /// これまでに出したラッチ付きアクセスの数（式の評価をコミット点より前へ出すための目印）
    /// latched accesses emitted so far (used to hoist their evaluation above a commit point)
    at_count: u32,
    reduce_out: String,
    reduce_n: usize,
    /// 次の own 式の根をフレームリージョンから取るか（エスケープしない let の初期化のときだけ立つ）
    /// whether the root of the next own expression comes from the frame region (set only for a non-escaping let)
    own_frame: bool,
    /// フレームに置いたローカルとその印 / locals placed in the frame region and their marks
    frame_marks: HashMap<BindId, String>,
    par_wrapped: Option<ExprId>,
    /// アリーナごとの列（collect_columns が全関数から集める） / columns per arena (collected from all functions)
    arena_cols: Vec<Vec<DerivedCol>>,
    /// 完全列化したフィールド（アリーナごと） / True-SoA fields (per arena)
    tsoa: Vec<Vec<TsField>>,
    /// 書き込みの多いフィールド（写しの列を作らない。書き込みの重み, 読み出しの重み） / write-heavy fields (no mirrored
    /// columns; write weight, read weight)
    ts_heavy: Vec<BTreeMap<String, (f64, f64)>>,
    /// ルックアップの束縛 → 完全列化のアリーナ / lookup bindings → their True-SoA arena
    bind_arena: HashMap<BindId, usize>,
    /// 非時間的ストアで書くループの中（アリーナ, フィールド） / inside a non-temporal loop (arena, fields)
    nt_cur: Option<(usize, Vec<String>)>,
    /// 直後の文が同じ列を読み直す一括上書きのループ / bulk-overwrite loops whose next statement reads the same column
    nt_reread: HashSet<ExprId>,
    /// 完全列化の表を回す `for id in @a` のループ変数 → スロットの添字の C 変数（証明済みの @a[id] は gen[] を読まずにこの添字を使う）
    /// the loop variable of a `for id in @a` over a True-SoA table → the C variable of its slot index (proven @a[id]
    /// accesses use it instead of rebuilding the index from gen[])
    loop_ix: HashMap<BindId, String>,
    /// `#[fast_math]`（ループ・関数）か --fast-math の範囲にある `for id in @a` のループ変数: 浮動小数点の集計を列で走査して
    /// よい（再結合を許す） / loop variables of `for id in @a` inside `#[fast_math]` (loop or function) or --fast-math:
    /// their float reductions may be served by columns (reassociation allowed)
    fast_vars: HashSet<BindId>,
    /// 条件付き一括更新（Masked Column RMW）に割り当てたループ / loops assigned to a masked column RMW
    rmw_use: HashMap<ExprId, RmwUse>,
    /// 整数の加算があふれる語から続ける逐次のループ: 始める語（とその間は特殊化しない） / the sequential loop that continues from
    /// the word where an integer add would overflow: the starting word (and no specialization meanwhile)
    rmw_start: Option<String>,
    rmw_off: bool,
    /// 列を読む集計（`for` の式 → 列） / reductions that read columns (`for` expression → columns)
    col_use: HashMap<ExprId, ColUse>,
    /// 次にこの式（`@a[i]`）の要素ポインタを作るとき、一時変数にも取っておく（書き込みのあとで写しを作り直すため）
    /// when the element pointer of this expression (`@a[i]`) is formed next, also keep it in a temporary (to rebuild
    /// the mirror after the write)
    arena_cap: Option<ExprId>,
    arena_cap_out: Option<String>,
    /// big-endian のフィールドの読み出しで、入れ替えを掛けている最中の式 / the big-endian field read being wrapped right now
    be_skip: Option<ExprId>,
    /// スコープの出口で出す写しの作り直し（get_mut の束縛 → C の文） / mirror rebuilds emitted at scope exits
    /// (get_mut binding → C statement)
    exit_mirror: HashMap<BindId, String>,
    /// 一部の列だけを作り直す関数が要る組（アリーナごとの列のビットの集合） / column subsets that need their own rebuild
    /// function (bit sets per arena)
    mirror_masks: Vec<BTreeSet<u64>>,
    /// この関数が呼び出しリージョンになっているバンプアリーナ（出口で先頭を巻き戻す） / bump arenas this function is a call
    /// region of (their top is rewound at its exits)
    rewinds: Vec<usize>,
    /// 生成中の catch_fault（飛び先のラベル, 失敗の符号の変数, 区間の始まりのスコープの深さ）/ the catch_fault sections being
    /// generated (target label, failure-code variable, scope depth at the start of the section)
    catch_stack: Vec<(String, String, usize)>,
    /// #[syscall(N)] で Result を返す関数を生成中なら、その Result の C の型 / the C Result type while generating a #[syscall(N)]
    /// function that returns a Result
    syscall_v2_ret: Option<String>,
    /// catch_fault の区間から（推移的に）呼ばれる関数: コミット点で止まらず、ラッチをコアごとの保留に移して戻る
    /// functions called (transitively) from catch_fault sections: at commit points they move the latch into the per-core
    /// pending word and return instead of stopping
    propagate_fns: HashSet<usize>,
    cur_fn: usize,
    /// 今いる `for id in @a`（ビットマップで回すもの）のアリーナとループの場所 / the arenas and sites of the enclosing
    /// `for id in @a` loops (the bitmap-driven ones)
    arena_loops: Vec<(usize, crate::span::Span)>,
    /// generic-os: 外から来た値（ユーザー・デバイス・回線・C）に由来する添字の式（Spectre v1 のマスク + CSDB を入れる）
    /// generic-os: index expressions derived from outside data (user, device, network, C); they get the Spectre v1 mask + CSDB
    spectre_idx: HashSet<ExprId>,
    /// static の初期化子を生成中（PAC の署名をしない: 表は読み取り専用で、読み出すときに署名する）
    /// generating static initializers (no PAC signing: the tables are read-only and are signed when read)
    in_static_init: bool,
    pub report: CgReport,
}

const C_RESERVED: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else", "enum", "extern", "float", "for", "goto", "if",
    "inline", "int", "long", "register", "restrict", "return", "short", "signed", "sizeof", "static", "struct", "switch", "typedef", "union",
    "unsigned", "void", "volatile", "while", "bool", "true", "false", "NULL", "errno", "stdin", "stdout", "stderr", "offsetof", "assert", "EOF",
    "main", "memcpy", "memset", "memmove", "memcmp", "exit", "abort", "printf", "malloc", "free", "int8_t", "int16_t", "int32_t", "int64_t",
    "uint8_t", "uint16_t", "uint32_t", "uint64_t", "intptr_t", "uintptr_t", "size_t",
];

fn c_escape(s: &str) -> String {
    let mut out = String::from("\"");
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            0x20..=0x7e => out.push(b as char),
            // 8 進 3 桁は長さが固定なので後続の文字と混ざらない / 3-digit octal has a fixed length
            _ => out.push_str(&format!("\\{:03o}", b)),
        }
    }
    out.push('"');
    out
}

fn arith_suffix(it: IntTy) -> &'static str {
    it.name()
}

impl<'a, 'p> Cg<'a, 'p> {
    pub fn new(s: &'a Sema<'p>, fr: &'a FlowResults, src: &'a SourceFile, o: CgOpts) -> Cg<'a, 'p> {
        let file_tag = std::path::Path::new(&src.path).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| src.path.clone());
        Cg {
            s,
            fr,
            src,
            o,
            file_tag,
            types_out: String::new(),
            glue_protos: String::new(),
            glue_out: String::new(),
            emitted_structs: HashSet::new(),
            emitting: HashSet::new(),
            opt_done: HashSet::new(),
            need_drop: BTreeSet::new(),
            need_own_drop: BTreeSet::new(),
            need_own_drop_at: BTreeSet::new(),
            need_own_drop_at_ty: BTreeMap::new(),
            need_clone: BTreeSet::new(),
            need_own_clone: BTreeSet::new(),
            need_zfill: BTreeSet::new(),
            need_own_drop_ty: BTreeMap::new(),
            need_own_clone_ty: BTreeMap::new(),
            fn_types: HashMap::new(),
            lines: Vec::new(),
            ind: 1,
            tmp: 0,
            names: HashMap::new(),
            used: HashSet::new(),
            scopes: Vec::new(),
            loops: Vec::new(),
            ret_ty: Ty::Unit,
            fn_c_names: HashMap::new(),
            flt_used: false,
            flt_known: None,
            gc_desc_fwd: String::new(),
            gc_desc_out: String::new(),
            gc_desc_done: HashSet::new(),
            gc_known: None,
            gc_used: false,
            gc_k: 0,
            gc_pending: Vec::new(),
            gc_cols: HashMap::new(),
            gc_poll_fn: false,
            gc_rcu_depth: 0,
            gc_collectors: None,
            gc_stmt_temps: Vec::new(),
            gc_own_temps: Vec::new(),
            read_ctx: false,
            at_count: 0,
            reduce_out: String::new(),
            reduce_n: 0,
            own_frame: false,
            frame_marks: HashMap::new(),
            par_wrapped: None,
            arena_cols: Vec::new(),
            tsoa: Vec::new(),
            ts_heavy: Vec::new(),
            bind_arena: HashMap::new(),
            nt_cur: None,
            nt_reread: HashSet::new(),
            loop_ix: HashMap::new(),
            fast_vars: fast_math_vars(s),
            rmw_use: HashMap::new(),
            rmw_start: None,
            rmw_off: false,
            col_use: HashMap::new(),
            arena_cap: None,
            arena_cap_out: None,
            be_skip: None,
            exit_mirror: HashMap::new(),
            mirror_masks: Vec::new(),
            rewinds: Vec::new(),
            catch_stack: Vec::new(),
            syscall_v2_ret: None,
            propagate_fns: HashSet::new(),
            cur_fn: usize::MAX,
            arena_loops: Vec::new(),
            spectre_idx: HashSet::new(),
            in_static_init: false,
            report: CgReport::default(),
        }
    }

    fn c(&self, ja: &str, en: &str) -> String {
        self.o.lang.pick(ja, en).to_string()
    }

    fn line_of(&self, sp: Span) -> usize {
        self.src.line_of(sp.lo) + 1
    }

    fn site(&self, sp: Span) -> String {
        let (l, c) = self.src.line_col(sp.lo);
        c_escape(&format!("{}:{}:{}", self.file_tag, l + 1, c + 1))
    }

    fn emit(&mut self, s: impl AsRef<str>) {
        let pad = "    ".repeat(self.ind);
        for l in s.as_ref().lines() {
            self.lines.push(format!("{}{}", pad, l));
            if !self.gc_pending.is_empty() {
                let l2 = l.to_string();
                self.gc_after_line(&l2);
            }
        }
    }

    fn new_tmp(&mut self, base: &str) -> String {
        self.tmp += 1;
        format!("_{}{}", base, self.tmp)
    }

    fn ty(&self, e: &Expr) -> Ty {
        self.s.expr_ty[e.id as usize].clone()
    }

    fn local(&self, b: BindId) -> &crate::sema::LocalInfo {
        self.s.locals[b as usize].as_ref().expect("local info")
    }

    // ───────────── 型 / types ─────────────

    fn struct_name(&self, sid: usize) -> String {
        format!("fsc_{}", self.s.t.structs[sid].name)
    }

    fn id_name(&self, a: usize) -> String {
        format!("fsc_Id_{}", self.s.t.arenas[a].name)
    }

    /// プログラムが f32 / f64 を使うか（式・ローカル・構造体のフィールド・static・関数の型のどこかに） / whether the program
    /// uses f32 / f64 (in any expression, local, struct field, static or function signature)
    /// gpu_arena を使うか / whether the program has a gpu_arena
    fn uses_gpu(&self) -> bool {
        self.s.t.arenas.iter().any(|a| a.gpu)
    }

    fn uses_float(&self) -> bool {
        let is_f = |t: &Ty| matches!(t, Ty::Float(_) | Ty::FloatVar(_));
        let s = self.s;
        s.expr_ty.iter().any(|t| crate::universal::ty_contains(s, t, &is_f))
            || s.t.structs.iter().any(|st| st.fields.iter().any(|f| crate::universal::ty_contains(s, &f.ty, &is_f)))
            || s.t.statics.iter().any(|st| crate::universal::ty_contains(s, &st.ty, &is_f))
            || s.t.fns.iter().any(|f| f.params.iter().any(|p| crate::universal::ty_contains(s, &p.ty, &is_f)) || crate::universal::ty_contains(s, &f.ret, &is_f))
    }

    fn mangle(&self, t: &Ty) -> String {
        match t {
            Ty::Int(i) => i.name().to_string(),
            Ty::Bool => "bool".to_string(),
            Ty::Str => "Str".to_string(),
            Ty::Slice(_) => "slice".to_string(),
            Ty::Unit => "unit".to_string(),
            Ty::Struct(s) => self.s.t.structs[*s].name.clone(),
            Ty::Option(i) => format!("Opt_{}", self.mangle(i)),
            Ty::Own(i) => format!("Own_{}", self.mangle(i)),
            Ty::Id(a) => format!("Id_{}", self.s.t.arenas[*a].name),
            Ty::Dma(a) => format!("Dma_{}", self.s.t.arenas[*a].name),
            Ty::Float(f) => f.name().to_string(),
            Ty::GpuBuf(a) => format!("GpuBuf_{}", self.s.t.arenas[*a].name),
            Ty::GpuFence(a) => format!("GpuFence_{}", self.s.t.arenas[*a].name),
            Ty::Array(e, n) => format!("arr{}_{}", n, self.mangle(e)),
            Ty::Ref(_, i) => format!("Ref_{}", self.mangle(i)),
            Ty::Fn(ps, r) => format!("Fn{}_{}_{}", ps.len(), ps.iter().map(|p| self.mangle(p)).collect::<Vec<_>>().join("_"), self.mangle(r)),
            Ty::Result(a, b) => format!("Res_{}_{}", self.mangle(a), self.mangle(b)),
            Ty::Fault => "Fault".to_string(),
            Ty::User(i) => format!("User_{}", self.mangle(i)),
            Ty::DmaOwn(i) => format!("DmaOwn_{}", self.mangle(i)),
            Ty::MmioBind(s) => format!("Mmio_{}", self.s.t.structs[*s].name),
            _ => "err".to_string(),
        }
    }

    /// Result<T, E> の C の型: Option と同じ並び（is_some = 成功・value = 成功値）に失敗値 err を足した構造体
    /// the C type of Result<T, E>: Option's layout (is_some = success, value = the success value) plus the failure err
    fn res_name(&mut self, t: &Ty, e: &Ty) -> String {
        let name = format!("fsc_{}", self.mangle(&Ty::Result(Box::new(t.clone()), Box::new(e.clone()))));
        if !self.opt_done.contains(&name) {
            self.opt_done.insert(name.clone());
            self.ensure_value_type(t);
            self.ensure_value_type(e);
            let vd = if matches!(t, Ty::Unit | Ty::Never | Ty::Err) { "uint8_t value".to_string() } else { self.cdecl(t, "value") };
            let ed = self.cdecl(e, "err");
            let disp = self.s.t.display(&Ty::Result(Box::new(t.clone()), Box::new(e.clone())));
            let _ = writeln!(self.types_out, "/* {} — {} */\ntypedef struct {{\n    bool is_some;\n    {};\n    {};\n}} {};\n", disp, self.c("is_some = Ok", "is_some = Ok"), vd, ed, name);
        }
        name
    }

    /// Option の表現: ニッチ（ポインタ / Id / Str）か、{is_some, value} 構造体か
    /// Option representation: a niche (pointer / Id / Str) or an {is_some, value} struct
    fn opt_niche(t: &Ty) -> bool {
        // 関数ポインタは NULL にならないので NULL = None に使える / a function pointer is never NULL, so NULL can be None
        matches!(t, Ty::Own(_) | Ty::Id(_) | Ty::Dma(_) | Ty::GpuBuf(_) | Ty::Str | Ty::Ref(..) | Ty::Fn(..) | Ty::DmaOwn(_))
    }

    /// own T の C の型（中身へのポインタ。配列なら先頭要素へのポインタ） / the C type of own T (a pointer to its contents; for
    /// an array, a pointer to the first element)
    fn own_cty(&mut self, x: &Ty) -> String {
        match x {
            Ty::Array(e, _) => format!("{} *", self.cty(e)),
            other => format!("{} *", self.cty(other)),
        }
    }

    /// own の中身の C の型名（sizeof / FASCON_OWN_SPAN 用。配列は `uint8_t [16]`） / the C type name of an own's contents
    /// (for sizeof / FASCON_OWN_SPAN; an array is `uint8_t [16]`)
    fn payload_cty(&mut self, x: &Ty) -> String {
        self.ensure_value_type(x);
        let d = self.cdecl(x, "");
        d.trim_end().to_string()
    }

    /// 関数ポインタ型の typedef（初めて使われたときに型の節へ出す） / the typedef of a function pointer type (emitted into
    /// the types section on first use)
    fn fn_typedef(&mut self, t: &Ty) -> String {
        if let Some(n) = self.fn_types.get(t) {
            return n.clone();
        }
        let (ps, r) = match t {
            Ty::Fn(ps, r) => (ps.clone(), (**r).clone()),
            _ => return "void *".to_string(),
        };
        let name = format!("fsc_fn{}", self.fn_types.len() + 1);
        self.fn_types.insert(t.clone(), name.clone());
        for p in &ps {
            self.ensure_value_type(p);
        }
        self.ensure_value_type(&r);
        let mut pcs = Vec::new();
        for p in &ps {
            pcs.push(self.cty(p));
        }
        let rc = self.cty(&r);
        let params = if pcs.is_empty() { "void".to_string() } else { pcs.join(", ") };
        let disp = self.s.t.display(t);
        let _ = writeln!(
            self.types_out,
            "/* {} — {} */\ntypedef {} (*{})({});\n",
            disp,
            self.c("関数ポインタ（コールバック）。参照の引数は呼び出しの間だけ生きる（F0520 で検査済み）", "function pointer (callback); reference parameters live for the call only (checked by F0520)"),
            rc,
            name,
            params
        );
        name
    }

    fn opt_name(&mut self, inner: &Ty) -> String {
        let name = format!("fsc_Opt_{}", self.mangle(inner));
        if !self.opt_done.contains(&name) {
            self.opt_done.insert(name.clone());
            self.ensure_value_type(inner);
            let decl = self.cdecl(inner, "value");
            let disp = self.s.t.display(&Ty::Option(Box::new(inner.clone())));
            let _ = writeln!(self.types_out, "/* {} */\ntypedef struct {{\n    bool is_some;\n    {};\n}} {};\n", disp, decl, name);
        }
        name
    }

    /// 値として埋め込む型の定義を先に出す / emit definitions of types embedded by value first
    fn ensure_value_type(&mut self, t: &Ty) {
        match t {
            Ty::Struct(s) => self.emit_struct(*s),
            Ty::Option(i) => {
                if !Self::opt_niche(i) {
                    self.opt_name(i);
                }
            }
            Ty::Result(a, b) => {
                let (a, b) = ((**a).clone(), (**b).clone());
                self.res_name(&a, &b);
            }
            Ty::Array(e, _) => self.ensure_value_type(e),
            _ => {}
        }
    }

    pub fn cty(&mut self, t: &Ty) -> String {
        match t {
            Ty::Int(i) => i.c_name().to_string(),
            Ty::IntVar(_) | Ty::Var(_) => "int32_t".to_string(),
            Ty::Float(f) => f.c_name().to_string(),
            Ty::FloatVar(_) => "double".to_string(),
            // GpuBuf は Id と同じ表現（0 = なし）、GpuFence は [列番号 32 | 添字 32] の 64 ビット / GpuBuf is an Id (0 = none),
            // GpuFence a 64-bit [sequence 32 | index 32]
            Ty::GpuBuf(a) => self.id_name(*a),
            Ty::GpuFence(_) => "uint64_t".to_string(),
            Ty::Bool => "bool".to_string(),
            Ty::Unit | Ty::Never | Ty::Err => "void".to_string(),
            Ty::Str => "FasconStr".to_string(),
            Ty::Slice(_) => "f20_slice_t".to_string(),
            Ty::Struct(s) => self.struct_name(*s),
            Ty::Own(i) => self.own_cty(i),
            Ty::Id(a) | Ty::Dma(a) => self.id_name(*a),
            Ty::Fn(..) => self.fn_typedef(t),
            // ユニバーサル OS 拡張の型 / types of the universal-OS extension
            Ty::Result(a, b) => {
                let (a, b) = ((**a).clone(), (**b).clone());
                self.res_name(&a, &b)
            }
            Ty::Fault => "int64_t".to_string(),
            Ty::User(i) => if matches!(**i, Ty::Str) { "f20_ustr_t".to_string() } else { "uint64_t".to_string() },
            Ty::DmaOwn(i) => self.own_cty(i),
            Ty::MmioBind(_) => "uintptr_t".to_string(),
            Ty::Option(i) => match &**i {
                Ty::Own(x) | Ty::DmaOwn(x) => self.own_cty(x),
                Ty::Id(a) | Ty::Dma(a) | Ty::GpuBuf(a) => self.id_name(*a),
                Ty::Str => "FasconStr".to_string(),
                Ty::Ref(m, x) => self.cty(&Ty::Ref(*m, x.clone())),
                Ty::Fn(..) => self.fn_typedef(i),
                other => self.opt_name(&other.clone()),
            },
            Ty::Array(e, _) => format!("{} *", self.cty(e)),
            Ty::Ref(m, i) => match &**i {
                Ty::Own(x) | Ty::DmaOwn(x) => {
                    let base = match &**x {
                        Ty::Array(e, _) => self.cty(e),
                        other => self.cty(other),
                    };
                    if base.ends_with('*') {
                        if *m {
                            format!("{}*", base)
                        } else {
                            format!("{} const *", base.trim_end())
                        }
                    } else {
                        format!("{}{} *", if *m { "" } else { "const " }, base)
                    }
                }
                Ty::Array(e, _) => {
                    let ec = self.cty(e);
                    if !*m && ec.ends_with('*') {
                        // ポインタ（own など）の配列への共有参照は「要素のポインタ自体が const」（`T **` から暗黙に変換できる形）
                        // a shared ref to an array of pointers (own, ...) makes the element pointers const (the form `T **`
                        // converts to implicitly)
                        format!("{} const *", ec.trim_end())
                    } else {
                        format!("{}{} *", if *m { "" } else { "const " }, ec)
                    }
                }
                other => {
                    let inner = self.cty(other);
                    if inner.ends_with('*') {
                        // ポインタ型への共有参照は「ポインタ自体が const」 / a shared ref to a pointer makes the pointer const
                        if *m {
                            format!("{}*", inner)
                        } else {
                            format!("{} const *", inner.trim_end())
                        }
                    } else {
                        format!("{}{} *", if *m { "" } else { "const " }, inner)
                    }
                }
            },
        }
    }

    /// 宣言子（配列は `T name[N]`） / declarator (arrays become `T name[N]`)
    pub fn cdecl(&mut self, t: &Ty, name: &str) -> String {
        match t {
            Ty::Array(e, n) => self.cdecl(e, &format!("{}[{}]", name, n)),
            _ => {
                let c = self.cty(t);
                if c.ends_with('*') {
                    format!("{}{}", c, name)
                } else {
                    format!("{} {}", c, name)
                }
            }
        }
    }

    fn emit_struct(&mut self, sid: usize) {
        if self.emitted_structs.contains(&sid) || self.emitting.contains(&sid) {
            return;
        }
        self.emitting.insert(sid);
        let info = self.s.t.structs[sid].clone();
        for f in &info.fields {
            self.ensure_value_type(&f.ty);
        }
        let mut body = String::new();
        for f in &info.fields {
            let d = self.cdecl(&f.ty, &f.name);
            let note = match &f.ty {
                Ty::Own(i) => format!("  /* own {} — {} */", self.s.t.display(i), self.c("唯一の所有者（所有リージョンへのポインタ）", "unique owner (pointer to its own region)")),
                Ty::Option(i) if matches!(**i, Ty::Own(_)) => format!("  /* Option<own {}> — {} */", self.s.t.display(i.deref_all()), self.c("NULL = None", "NULL = None")),
                Ty::Id(a) => format!("  /* Id<{}> */", self.s.t.arenas[*a].name),
                Ty::Option(i) if matches!(**i, Ty::Id(_)) => format!("  /* Option<{}> — 0 = None */", self.s.t.display(i)),
                Ty::Array(e, _) if matches!(&**e, Ty::Option(x) if matches!(**x, Ty::Id(_))) => format!("  /* {} — 0 = None */", self.s.t.display(&f.ty)),
                _ => String::new(),
            };
            let _ = writeln!(body, "    {};{}", d, note);
        }
        let name = self.struct_name(sid);
        if info.align > 0 {
            // #[repr(align(N))]: 型の整列を N バイト以上に上げる（下げることはない） / raise the type's alignment to at least N
            let _ = writeln!(
                self.types_out,
                "/* {} {}: #[repr(align({}))] — {} */\nstruct __attribute__((aligned({}))) {} {{\n{}}};\n_Static_assert(_Alignof(struct {}) >= {}u, \"#[repr(align({}))]\");\n",
                self.c("line", "line"),
                self.line_of(info.span),
                info.align,
                self.c(&format!("{} バイト境界（アリーナのスロットも同じ境界になる）", info.align), &format!("{}-byte boundary (arena slots get the same boundary)", info.align)),
                info.align,
                name,
                body,
                name,
                info.align,
                info.align
            );
        } else if let Some(be) = info.wire {
            // ワイヤ構造体: 整列 1・詰め物なし。バイト列の上に重ねるので may_alias。big-endian の 2 バイト以上の整数は
            // 読み書きのたびに fascon_beN で入れ替える
            // a wire struct: alignment 1, no padding; laid over bytes, hence may_alias. Integers of 2 bytes or more in a
            // big-endian struct are swapped with fascon_beN at every read and write
            let _ = writeln!(
                self.types_out,
                "/* {} {}: #[wire({})] — {} */\nstruct __attribute__((packed, may_alias)) {} {{\n{}}};\n_Static_assert(_Alignof(struct {}) == 1, \"#[wire]\");\n",
                self.c("line", "line"),
                self.line_of(info.span),
                if be { "be" } else { "le" },
                self.c("整列 1・詰め物なし・バイト順固定", "alignment 1, no padding, fixed byte order"),
                name,
                body,
                name
            );
        } else {
            let _ = writeln!(self.types_out, "/* {} {} */\nstruct {} {{\n{}}};\n", self.c("line", "line"), self.line_of(info.span), name, body);
        }
        self.emitting.remove(&sid);
        self.emitted_structs.insert(sid);
    }

    // ───────────── 解放・複製の糊 / drop and clone glue ─────────────

    /// 場所 `place`（型 t の左辺値）を解放するコード / code that drops the lvalue `place` of type t
    fn drop_code(&mut self, t: &Ty, place: &str) -> String {
        match t {
            Ty::Own(i) => {
                // fascon-gc: 持ち主（変数・フィールド）を NULL にしてから記憶域を返す（#[on_drop] の間は持ち主が根のまま、
                // 解放した先を根が指すことはない）。gc_arena の無いプログラムは今までの形
                // fascon-gc: the owner (variable or field) becomes NULL before the storage is returned (it stays a root while
                // #[on_drop] runs, and no root ever points at freed storage). Programs without gc_arena keep the old form
                let at = self.gc_any() && is_c_lvalue(place);
                if let Ty::Struct(s) = &**i {
                    if at {
                        self.need_own_drop_at.insert(*s);
                        return format!("fsc_own_drop_at_{}(&{});", self.s.t.structs[*s].name, place);
                    }
                    self.need_own_drop.insert(*s);
                    format!("fsc_own_drop_{}({});", self.s.t.structs[*s].name, place)
                } else {
                    // 構造体でない own（`own i32` / `own [u8; 4096]` / `own Option<own Node>`）: 中身を解放してから所有リージョンを返す
                    // a non-struct own: drop its contents, then return its own region
                    let m = self.mangle(i);
                    if at {
                        self.need_own_drop_at_ty.insert(m.clone(), (**i).clone());
                        return format!("fsc_own_drop_at_T_{}(&{});", m, place);
                    }
                    self.need_own_drop_ty.insert(m.clone(), (**i).clone());
                    format!("fsc_own_drop_T_{}({});", m, place)
                }
            }
            // DMA のハンドル: CPU が持っているスロットを表へ返す（貸し出し中のスロットにはハンドルが無い）
            // a DMA handle: return the slot the CPU owns to its table (a lent slot has no handle)
            Ty::Dma(a) | Ty::GpuBuf(a) => format!("if ({} != 0u) {{ (void)arena_{}_free({}); }}", place, self.s.t.arenas[*a].name, place),
            // dma own: 独立したページを DMA プールへ返す（中身は POD なので子の解放は無い） / return the dedicated pages to the
            // DMA pool (the contents are POD: no children to drop)
            Ty::DmaOwn(_) => format!("fascon_dma_free({});", place),
            Ty::Result(i, _) if self.s.t.needs_drop(i) => {
                let inner = self.drop_code(&i.clone(), &format!("{}.value", place));
                format!("if ({}.is_some) {{ {} }}", place, inner)
            }
            Ty::Struct(s) if self.s.t.structs[*s].needs_drop => {
                self.need_drop.insert(*s);
                format!("fsc_drop_{}(&{});", self.s.t.structs[*s].name, place)
            }
            Ty::Option(i) if self.s.t.needs_drop(i) => {
                if Self::opt_niche(i) {
                    self.drop_code(i, place)
                } else {
                    let inner = self.drop_code(&i.clone(), &format!("{}.value", place));
                    format!("if ({}.is_some) {{ {} }}", place, inner)
                }
            }
            Ty::Array(e, n) if self.s.t.needs_drop(e) => {
                let inner = self.drop_code(&e.clone(), &format!("{}[_i]", place));
                format!("for (size_t _i = 0; _i < {}u; _i++) {{ {} }}", n, inner)
            }
            _ => String::new(),
        }
    }

    fn clone_expr(&mut self, t: &Ty, v: &str) -> String {
        match t {
            Ty::Own(i) => {
                if let Ty::Struct(s) = &**i {
                    self.need_own_clone.insert(*s);
                    format!("fsc_own_clone_{}({})", self.s.t.structs[*s].name, v)
                } else {
                    // 構造体でない own の深い複製（新しい所有リージョン） / a deep copy of a non-struct own (a new own region)
                    let m = self.mangle(i);
                    self.need_own_clone_ty.insert(m.clone(), (**i).clone());
                    format!("fsc_own_clone_T_{}({})", m, v)
                }
            }
            Ty::Struct(s) if self.s.t.structs[*s].needs_drop => {
                self.need_clone.insert(*s);
                format!("fsc_clone_{}(&{})", self.s.t.structs[*s].name, v)
            }
            _ => v.to_string(),
        }
    }

    fn gen_glue(&mut self) {
        // 依存を閉じるまで繰り返す / iterate until all dependencies are generated
        let mut done_drop = HashSet::new();
        let mut done_own_drop = HashSet::new();
        let mut done_clone = HashSet::new();
        let mut done_own_clone = HashSet::new();
        let mut done_zfill = HashSet::new();
        let mut done_own_drop_ty: HashSet<String> = HashSet::new();
        let mut done_own_drop_at: HashSet<usize> = HashSet::new();
        let mut done_own_drop_at_ty: HashSet<String> = HashSet::new();
        let mut done_own_clone_ty: HashSet<String> = HashSet::new();
        loop {
            let mut progressed = false;
            for s in self.need_drop.clone() {
                if done_drop.insert(s) {
                    self.gen_drop(s);
                    progressed = true;
                }
            }
            for s in self.need_own_drop.clone() {
                if done_own_drop.insert(s) {
                    let n = self.s.t.structs[s].name.clone();
                    let sn = self.struct_name(s);
                    let nd = self.s.t.structs[s].needs_drop;
                    if nd {
                        self.need_drop.insert(s);
                    }
                    let _ = writeln!(self.glue_protos, "static inline void fsc_own_drop_{}({} *x);", n, sn);
                    let _ = writeln!(
                        self.glue_out,
                        "/* own {n}: {c} */\nstatic inline void fsc_own_drop_{n}({sn} *x) {{\n    if (x != NULL) {{\n{inner}        fascon_own_free(x);\n    }}\n}}\n",
                        n = n,
                        sn = sn,
                        c = self.c("子を解放してから自分の所有リージョンを返す（詰めた子は親と一緒に消える）", "drop the children, then return its own region (packed children go with the parent)"),
                        inner = if nd { format!("        fsc_drop_{}(x);\n", n) } else { String::new() }
                    );
                    progressed = true;
                }
            }
            for s in self.need_clone.clone() {
                if done_clone.insert(s) {
                    self.gen_clone(s);
                    progressed = true;
                }
            }
            for s in self.need_zfill.clone() {
                if done_zfill.insert(s) {
                    self.gen_zfill(s);
                    progressed = true;
                }
            }
            for (m, it) in self.need_own_drop_ty.clone() {
                if done_own_drop_ty.insert(m.clone()) {
                    self.gen_own_drop_ty(&m, &it);
                    progressed = true;
                }
            }
            for s in self.need_own_drop_at.clone() {
                if done_own_drop_at.insert(s) {
                    self.gen_own_drop_at(s);
                    progressed = true;
                }
            }
            for (m, it) in self.need_own_drop_at_ty.clone() {
                if done_own_drop_at_ty.insert(m.clone()) {
                    self.gen_own_drop_at_ty(&m, &it);
                    progressed = true;
                }
            }
            for (m, it) in self.need_own_clone_ty.clone() {
                if done_own_clone_ty.insert(m.clone()) {
                    self.gen_own_clone_ty(&m, &it);
                    progressed = true;
                }
            }
            for s in self.need_own_clone.clone() {
                if done_own_clone.insert(s) {
                    let n = self.s.t.structs[s].name.clone();
                    let sn = self.struct_name(s);
                    let nd = self.s.t.structs[s].needs_drop;
                    if nd {
                        self.need_clone.insert(s);
                    }
                    let _ = writeln!(self.glue_protos, "static {sn} *fsc_own_clone_{n}(const {sn} *x);", sn = sn, n = n);
                    let _ = writeln!(
                        self.glue_out,
                        "static {sn} *fsc_own_clone_{n}(const {sn} *x) {{\n    if (x == NULL) return NULL;\n    {sn} *y = ({sn} *)fascon_own_alloc(FASCON_OWN_SPAN({sn}), \"clone\");\n    *y = {v};\n    return y;\n}}\n",
                        sn = sn,
                        n = n,
                        v = if nd { format!("fsc_clone_{}(x)", n) } else { "*x".to_string() }
                    );
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
    }

    fn gen_drop(&mut self, sid: usize) {
        let info = self.s.t.structs[sid].clone();
        let n = info.name.clone();
        let sn = self.struct_name(sid);
        let _ = writeln!(self.glue_protos, "static void fsc_drop_{}({} *p);", n, sn);
        // 自己再帰の Option<own Self> は再帰せずループで解放する（長いリストでもスタックを使わない）
        // a self-recursive Option<own Self> field is freed with a loop (no stack growth even for long lists)
        let rec_field = info.fields.iter().position(|f| matches!(&f.ty, Ty::Option(i) if matches!(&**i, Ty::Own(x) if **x == Ty::Struct(sid))));
        let mut body = String::new();
        let mut shallow = String::new();
        for (i, f) in info.fields.iter().enumerate() {
            if Some(i) == rec_field {
                continue;
            }
            let c = self.drop_code(&f.ty.clone(), &format!("p->{}", f.name));
            if !c.is_empty() {
                let _ = writeln!(shallow, "    {}", c);
            }
        }
        match rec_field {
            Some(ri) if self.gc_any() => {
                // fascon-gc: 残りの鎖は p から届いたまま 1 つずつ外す（解放中の #[on_drop] が回収しても、まだ解放していない
                // 要素は根から届き、解放した要素へ届く道は無い） / fascon-gc: the rest of the chain stays reachable from p
                // and nodes are unlinked one at a time (if an #[on_drop] collects meanwhile, nodes not yet freed are still
                // reached from the roots and nothing reaches a freed node)
                let rf = info.fields[ri].name.clone();
                body.push_str(&shallow);
                let inner: String = shallow.lines().filter(|l| !l.trim().is_empty()).map(|l| format!("        {}\n", l.trim().replace("p->", "n->"))).collect();
                let _ = writeln!(
                    body,
                    "    /* {c} */\n    while (p->{rf} != NULL) {{\n        {sn} *n = p->{rf};\n{inner}        p->{rf} = n->{rf};\n        n->{rf} = NULL;\n        fascon_own_free(n);\n    }}",
                    c = self.c("再帰的なリストは反復で解放（fascon-gc: 残りの鎖は p から届いたまま外す）", "a recursive list is freed iteratively (fascon-gc: the rest of the chain stays reachable from p while nodes are unlinked)"),
                    sn = sn,
                    rf = rf,
                    inner = inner
                );
            }
            Some(ri) => {
                let rf = info.fields[ri].name.clone();
                body.push_str(&shallow);
                let inner: String = shallow.lines().filter(|l| !l.trim().is_empty()).map(|l| format!("        {}\n", l.trim().replace("p->", "n->"))).collect();
                let _ = writeln!(
                    body,
                    "    /* {c} */\n    {sn} *next = p->{rf};\n    p->{rf} = NULL;\n    while (next != NULL) {{\n        {sn} *n = next;\n        next = n->{rf};\n{inner}        fascon_own_free(n);\n    }}",
                    c = self.c("再帰的なリストは再帰せず反復で解放（長いリストでもスタックを消費しない）", "a recursive list is freed iteratively (no stack growth for long lists)"),
                    sn = sn,
                    rf = rf,
                    inner = inner
                );
            }
            None => body.push_str(&shallow),
        }
        // #[on_drop(f)]: 最初に f を呼ぶ → 子の解放 → #[secret] の揮発性 0 埋め + バリア → （呼び出し元が）記憶域を返す
        // #[on_drop(f)]: call f first → drop the children → #[secret] volatile zeroing + barrier → (the caller) frees the storage
        if let Some(f) = info.on_drop.clone() {
            let cn = self.s.t.fn_by_name.get(&f).and_then(|fi| self.fn_c_names.get(fi).cloned()).unwrap_or_else(|| format!("fsc_{}", f));
            body = format!("    {}(p); /* {} */\n{}", cn, self.c("#[on_drop]: 1. 後始末（0 埋め・解放より前）", "#[on_drop]: 1. clean-up (before zeroing and freeing)"), body);
        }
        if info.secret {
            let _ = writeln!(body, "    fascon_secure_zero(p, sizeof *p); /* {} */", self.c("#[secret]: 2. 揮発性の 0 埋め + メモリバリア（最適化で消えない）。3. 記憶域の解放は呼び出し元", "#[secret]: 2. volatile zeroing + a memory barrier (never optimized away); 3. the storage is freed by the caller"));
        }
        let _ = writeln!(self.glue_out, "static void fsc_drop_{}({} *p) {{\n{}}}\n", n, sn, body);
    }

    fn gen_clone(&mut self, sid: usize) {
        let info = self.s.t.structs[sid].clone();
        let n = info.name.clone();
        let sn = self.struct_name(sid);
        let _ = writeln!(self.glue_protos, "static {} fsc_clone_{}(const {} *p);", sn, n, sn);
        let mut body = format!("    {} r = *p;\n", sn);
        for f in &info.fields {
            if !self.s.t.needs_drop(&f.ty) {
                continue;
            }
            match &f.ty {
                Ty::Own(_) | Ty::Struct(_) => {
                    let c = self.clone_expr(&f.ty.clone(), &format!("p->{}", f.name));
                    let _ = writeln!(body, "    r.{} = {};", f.name, c);
                }
                Ty::Option(i) => {
                    if Self::opt_niche(i) {
                        let c = self.clone_expr(&(**i).clone(), &format!("p->{}", f.name));
                        let _ = writeln!(body, "    r.{} = {};", f.name, c);
                    } else {
                        let c = self.clone_expr(&(**i).clone(), &format!("p->{}.value", f.name));
                        let _ = writeln!(body, "    if (p->{f}.is_some) r.{f}.value = {c};", f = f.name, c = c);
                    }
                }
                Ty::Array(e, len) => {
                    let et = (**e).clone();
                    let c = match &et {
                        Ty::Option(i) if Self::opt_niche(i) => self.clone_expr(&(**i).clone(), &format!("p->{}[_i]", f.name)),
                        other => self.clone_expr(other, &format!("p->{}[_i]", f.name)),
                    };
                    let _ = writeln!(body, "    for (size_t _i = 0; _i < {}u; _i++) r.{}[_i] = {};", len, f.name, c);
                }
                _ => {}
            }
        }
        body.push_str("    return r;\n");
        let _ = writeln!(self.glue_out, "static {} fsc_clone_{}(const {} *p) {{\n{}}}\n", sn, n, sn, body);
    }

    /// 構造体でない own の解放関数 fsc_own_drop_T_<型>: 中身（own を含むなら）を解放してから所有リージョンを返す
    /// the drop function of a non-struct own, fsc_own_drop_T_<type>: drop the contents (when they hold own), then return
    /// the own region
    fn gen_own_drop_ty(&mut self, m: &str, it: &Ty) {
        let pc = self.own_cty(it);
        let place = if matches!(it, Ty::Array(..)) { "x".to_string() } else { "(*x)".to_string() };
        let inner = if self.s.t.needs_drop(it) { self.drop_code(it, &place) } else { String::new() };
        let disp = self.s.t.display(it);
        let _ = writeln!(self.glue_protos, "static inline void fsc_own_drop_T_{}({}x);", m, pc);
        let _ = writeln!(
            self.glue_out,
            "/* own {d}: {c} */\nstatic inline void fsc_own_drop_T_{m}({pc}x) {{\n    if (x != NULL) {{\n{inner}        fascon_own_free(x);\n    }}\n}}\n",
            d = disp,
            c = self.c("中身を解放してから所有リージョンを返す（親に詰めた子・フレームの中身なら返す処理は何もしない）", "drop the contents, then return the own region (nothing to return for a child packed into its parent or a frame block)"),
            m = m,
            pc = pc,
            inner = if inner.is_empty() { String::new() } else { format!("        {}\n", inner) }
        );
    }

    /// fascon-gc: fsc_own_drop_at_<構造体>(&持ち主): 子を解放（#[on_drop] はこの間も持ち主から届く）→ 持ち主を NULL →
    /// 記憶域を返す。解放した記憶域を指す根は一瞬も無い
    /// fascon-gc: fsc_own_drop_at_<struct>(&owner): drop the children (#[on_drop] still reaches it through the owner) →
    /// owner = NULL → return the storage. No root ever points at freed storage, not even for an instant
    fn gen_own_drop_at(&mut self, s: usize) {
        let n = self.s.t.structs[s].name.clone();
        let sn = self.struct_name(s);
        let nd = self.s.t.structs[s].needs_drop;
        if nd {
            self.need_drop.insert(s);
        }
        let _ = writeln!(self.glue_protos, "static inline void fsc_own_drop_at_{}({} **pp);", n, sn);
        let _ = writeln!(
            self.glue_out,
            "/* own {n}: {c} */\nstatic inline void fsc_own_drop_at_{n}({sn} **pp) {{\n    {sn} *x = *pp;\n    if (x != NULL) {{\n{inner}        *pp = NULL;\n        fascon_own_free(x);\n    }}\n}}\n",
            n = n,
            sn = sn,
            c = self.c("fascon-gc: 子を解放し、持ち主を NULL にしてから所有リージョンを返す", "fascon-gc: drop the children, clear the owner to NULL, then return the own region"),
            inner = if nd { format!("        fsc_drop_{}(x);\n", n) } else { String::new() }
        );
    }

    /// fascon-gc: 構造体でない own の fsc_own_drop_at_T_<型>(&持ち主) / fascon-gc: fsc_own_drop_at_T_<type>(&owner) of a
    /// non-struct own
    fn gen_own_drop_at_ty(&mut self, m: &str, it: &Ty) {
        let pc = self.own_cty(it);
        let place = if matches!(it, Ty::Array(..)) { "x".to_string() } else { "(*x)".to_string() };
        let inner = if self.s.t.needs_drop(it) { self.drop_code(it, &place) } else { String::new() };
        let disp = self.s.t.display(it);
        let _ = writeln!(self.glue_protos, "static inline void fsc_own_drop_at_T_{}({}*pp);", m, pc);
        let _ = writeln!(
            self.glue_out,
            "/* own {d}: {c} */\nstatic inline void fsc_own_drop_at_T_{m}({pc}*pp) {{\n    {pc}x = *pp;\n    if (x != NULL) {{\n{inner}        *pp = NULL;\n        fascon_own_free(x);\n    }}\n}}\n",
            d = disp,
            c = self.c("fascon-gc: 中身を解放し、持ち主を NULL にしてから所有リージョンを返す", "fascon-gc: drop the contents, clear the owner to NULL, then return the own region"),
            m = m,
            pc = pc,
            inner = if inner.is_empty() { String::new() } else { format!("        {}\n", inner) }
        );
    }

    /// 構造体でない own の深い複製 fsc_own_clone_T_<型> / the deep copy of a non-struct own, fsc_own_clone_T_<type>
    fn gen_own_clone_ty(&mut self, m: &str, it: &Ty) {
        let pc = self.own_cty(it);
        let pay = self.payload_cty(it);
        let mut body = String::new();
        match it {
            Ty::Array(e, n) => {
                let _ = writeln!(body, "    __builtin_memcpy(y, x, sizeof({}));", pay);
                if self.s.t.needs_drop(e) {
                    let mut inner = String::new();
                    self.clone_into(&(**e).clone(), "y[_i]", "x[_i]", &mut inner, 2);
                    let _ = writeln!(body, "    for (size_t _i = 0; _i < {}u; _i++) {{\n{}    }}", n, inner);
                }
            }
            other => {
                let other = other.clone();
                self.clone_into(&other, "(*y)", "(*x)", &mut body, 1);
            }
        }
        let cpc = if pc.trim_end().ends_with("* *") || pc.contains("* *") { pc.clone() } else { format!("const {}", pc) };
        let _ = writeln!(self.glue_protos, "static {}fsc_own_clone_T_{}({}x);", pc, m, cpc);
        let _ = writeln!(
            self.glue_out,
            "static {pc}fsc_own_clone_T_{m}({cpc}x) {{\n    if (x == NULL) return NULL;\n    {pc}y = ({pc})fascon_own_alloc(FASCON_OWN_SPAN({pay}), \"clone\");\n{body}    return y;\n}}\n",
            pc = pc,
            cpc = cpc,
            m = m,
            pay = pay,
            body = body
        );
    }

    /// 値の深い複製を文として書く（dst = src の複製） / write a deep copy of a value as statements (dst = copy of src)
    fn clone_into(&mut self, t: &Ty, dst: &str, src: &str, body: &mut String, ind: usize) {
        let pad = "    ".repeat(ind);
        if !self.s.t.needs_drop(t) {
            if let Ty::Array(..) = t {
                let _ = writeln!(body, "{}__builtin_memcpy({}, {}, sizeof({}));", pad, if dst.starts_with("(*") { dst.to_string() } else { format!("&{}", dst) }, src, dst);
            } else {
                let _ = writeln!(body, "{}{} = {};", pad, dst, src);
            }
            return;
        }
        match t {
            Ty::Own(_) | Ty::Struct(_) => {
                let c = self.clone_expr(t, src);
                let _ = writeln!(body, "{}{} = {};", pad, dst, c);
            }
            Ty::Option(i) => {
                if Self::opt_niche(i) {
                    let c = self.clone_expr(i, src);
                    let _ = writeln!(body, "{}{} = {};", pad, dst, c);
                } else {
                    let _ = writeln!(body, "{}{} = {};", pad, dst, src);
                    let mut inner = String::new();
                    self.clone_into(&(**i).clone(), &format!("{}.value", dst), &format!("{}.value", src), &mut inner, ind + 1);
                    let _ = writeln!(body, "{}if ({}.is_some) {{\n{}{}}}", pad, src, inner, pad);
                }
            }
            Ty::Array(e, n) => {
                let iv = format!("_i{}", ind);
                let mut inner = String::new();
                self.clone_into(&(**e).clone(), &format!("{}[{}]", dst, iv), &format!("{}[{}]", src, iv), &mut inner, ind + 1);
                let _ = writeln!(body, "{}for (size_t {iv} = 0; {iv} < {n}u; {iv}++) {{\n{inner}{pad}}}", pad, iv = iv, n = n, inner = inner, pad = pad);
            }
            _ => {
                let _ = writeln!(body, "{}{} = {};", pad, dst, src);
            }
        }
    }

    /// `own_alloc(T)` の子の数（中身の型ごと・初めて出た順）。own の子（Option でないフィールド。配列・値として埋め込んだ
    /// 構造体・構造体でない own の中も）を 0 で作る / the children `own_alloc(T)` creates (per contents type, in first-seen
    /// order): own fields that are not Option, including those inside arrays, inline structs and non-struct owns, are
    /// created zero-filled
    fn zero_children(&self, sid: usize, mult: u64, out: &mut Vec<(Ty, u64)>, depth: u32) {
        if depth > 64 {
            return;
        }
        for f in &self.s.t.structs[sid].fields {
            self.zero_children_ty(&f.ty, mult, out, depth + 1);
        }
    }
    fn zero_children_ty(&self, t: &Ty, mult: u64, out: &mut Vec<(Ty, u64)>, depth: u32) {
        if depth > 64 {
            return;
        }
        match t {
            Ty::Own(i) => {
                match out.iter_mut().find(|(s, _)| s == &**i) {
                    Some(x) => x.1 = x.1.saturating_add(mult),
                    None => out.push(((**i).clone(), mult)),
                }
                match &**i {
                    Ty::Struct(c) => self.zero_children(*c, mult, out, depth + 1),
                    other => self.zero_children_ty(other, mult, out, depth + 1),
                }
            }
            Ty::Array(e, n) => self.zero_children_ty(e, mult.saturating_mul(*n), out, depth + 1),
            Ty::Struct(c) => self.zero_children(*c, mult, out, depth + 1),
            _ => {}
        }
    }

    /// 0 埋めの子作り関数 fsc_zfill_T(p, cur): p の own の子を cur から順に詰め、次の空きを返す（ブロックは呼ぶ前に 0 埋め）
    /// the zero-fill builder fsc_zfill_T(p, cur): packs p's own children from cur onward and returns the next free byte (the
    /// block is zero-filled before the call)
    fn gen_zfill(&mut self, sid: usize) {
        let n = self.s.t.structs[sid].name.clone();
        let sn = self.struct_name(sid);
        let _ = writeln!(self.glue_protos, "static char *fsc_zfill_{}({} *p, char *cur);", n, sn);
        let mut body = String::new();
        let fields = self.s.t.structs[sid].fields.clone();
        let mut k = 0usize;
        for f in &fields {
            let place = format!("p->{}", sanitize_field(&f.name));
            self.zfill_ty(&f.ty, &place, &mut body, 1, &mut k);
        }
        let _ = writeln!(
            self.glue_out,
            "/* own_alloc({n}): {c} */\nstatic char *fsc_zfill_{n}({sn} *p, char *cur) {{\n{body}    return cur;\n}}\n",
            n = n,
            sn = sn,
            c = self.c("0 で埋めたブロックに own の子を順に詰める（子の見出しは EMBEDDED = 親と一緒に解放）", "pack the own children into the zero-filled block in order (child headers are EMBEDDED = freed with the parent)"),
            body = body
        );
    }
    fn zfill_ty(&mut self, t: &Ty, place: &str, body: &mut String, ind: usize, k: &mut usize) {
        let pad = "    ".repeat(ind);
        match t {
            Ty::Own(i) | Ty::DmaOwn(i) => match &**i {
                Ty::Struct(c) => {
                    let csn = self.struct_name(*c);
                    let cn = self.s.t.structs[*c].name.clone();
                    let v = format!("c{}", *k);
                    *k += 1;
                    let _ = writeln!(body, "{pad}{{\n{pad}    {csn} *{v} = ({csn} *)fascon_own_embed(cur, 0u);\n{pad}    cur += FASCON_OWN_CHILD_SPAN({csn});", pad = pad, csn = csn, v = v);
                    let mut grand = Vec::new();
                    self.zero_children(*c, 1, &mut grand, 0);
                    if !grand.is_empty() {
                        self.need_zfill.insert(*c);
                        let _ = writeln!(body, "{pad}    cur = fsc_zfill_{cn}({v}, cur);", pad = pad, cn = cn, v = v);
                    }
                    let _ = writeln!(body, "{pad}    {place} = {v};\n{pad}}}", pad = pad, place = place, v = v);
                }
                // 構造体でない own の子（`own i32` / `own [own Leaf; 3]`）: 0 の中身を詰め、中の own の子も作る
                // a non-struct own child (`own i32` / `own [own Leaf; 3]`): pack zeroed contents and create the own children
                // inside them
                other => {
                    let other = other.clone();
                    let pc = self.own_cty(&other);
                    let pay = self.payload_cty(&other);
                    let v = format!("c{}", *k);
                    *k += 1;
                    let _ = writeln!(body, "{pad}{{\n{pad}    {pc}{v} = ({pc})fascon_own_embed(cur, 0u);\n{pad}    cur += FASCON_OWN_CHILD_SPAN({pay});", pad = pad, pc = pc, v = v, pay = pay);
                    let inner_place = if matches!(other, Ty::Array(..)) { v.clone() } else { format!("(*{})", v) };
                    self.zfill_ty(&other, &inner_place, body, ind + 1, k);
                    let _ = writeln!(body, "{pad}    {place} = {v};\n{pad}}}", pad = pad, place = place, v = v);
                }
            },
            Ty::Array(e, len) => {
                let mut probe = Vec::new();
                self.zero_children_ty(e, 1, &mut probe, 0);
                if probe.is_empty() {
                    return;
                }
                let iv = format!("i{}", *k);
                *k += 1;
                let _ = writeln!(body, "{pad}for (size_t {iv} = 0; {iv} < {len}u; {iv}++) {{", pad = pad, iv = iv, len = len);
                self.zfill_ty(e, &format!("{}[{}]", place, iv), body, ind + 1, k);
                let _ = writeln!(body, "{pad}}}", pad = pad);
            }
            Ty::Struct(c) => {
                let mut probe = Vec::new();
                self.zero_children(*c, 1, &mut probe, 0);
                if probe.is_empty() {
                    return;
                }
                self.need_zfill.insert(*c);
                let _ = writeln!(body, "{pad}cur = fsc_zfill_{cn}(&{place}, cur);", pad = pad, cn = self.s.t.structs[*c].name, place = place);
            }
            _ => {}
        }
    }

    /// own の子の中身の型の名前（コメント用） / the name of an own child's contents type (for comments)
    fn kid_name(&self, t: &Ty) -> String {
        match t {
            Ty::Struct(c) => self.s.t.structs[*c].name.clone(),
            other => self.s.t.display(other),
        }
    }

    /// `own_alloc(T)`: 0 で埋めた T を新しい所有リージョンに作る（ジェネリクスを使わない動的確保）。own の子（Option で
    /// ないフィールド）も同じブロックに 0 で詰めるので、確保 1 回・解放 1 回。スコープの終わりで自動的に解放される
    /// `own_alloc(T)`: a zero-filled T in a new own region (dynamic allocation without generics). Its own children (non-Option
    /// fields) are zero-filled into the same block, so it is one allocation and one free; it is released automatically at
    /// the end of its owner's scope
    fn own_alloc_expr(&mut self, e: &Expr) -> String {
        if let Some(t) = self.s.own_alloc_tys.get(&e.id).cloned() {
            return self.own_alloc_value(e, &t);
        }
        let frame = std::mem::replace(&mut self.own_frame, false);
        let alloc_fn = if frame { "fascon_frame_alloc" } else { "fascon_own_alloc" };
        let sid = match self.s.own_allocs.get(&e.id) {
            Some(&s) => s,
            None => return "NULL".to_string(),
        };
        let sn = self.struct_name(sid);
        let name = self.s.t.structs[sid].name.clone();
        let line = self.line_of(e.span);
        let site = self.site(e.span);
        let mut kids = Vec::new();
        self.zero_children(sid, 1, &mut kids, 0);
        self.report.own_alloc_sites.push((line, name.clone()));
        if kids.is_empty() {
            let o = self.new_tmp("o");
            self.emit(format!(
                "{sn} *{o} = ({sn} *){af}(FASCON_OWN_SPAN({sn}), {site}); /* {l} {line}: own_alloc({name}) — {c} */",
                sn = sn,
                o = o,
                af = alloc_fn,
                site = site,
                l = self.c("line", "line"),
                line = line,
                name = name,
                c = self.c("0 で埋めた新しい所有リージョン", "a new zero-filled own region")
            ));
            self.emit(format!("__builtin_memset({o}, 0, sizeof({sn}));", o = o, sn = sn));
            self.report.own_sites.push((line, name, 0));
            return o;
        }
        let mut terms = vec![format!("FASCON_OWN_SPAN({})", sn)];
        let mut nkids = 0u64;
        let mut names = Vec::new();
        for (c, m) in &kids {
            let csn = self.payload_cty(c);
            terms.push(if *m == 1 { format!("FASCON_OWN_CHILD_SPAN({})", csn) } else { format!("{}u * FASCON_OWN_CHILD_SPAN({})", m, csn) });
            nkids += m;
            for _ in 0..(*m).min(8) {
                names.push(self.kid_name(c));
            }
        }
        let total = self.new_tmp("sz");
        let blk = self.new_tmp("blk");
        self.emit(format!("const size_t {} = {};", total, terms.join(" + ")));
        self.emit(format!(
            "char *{blk} = (char *){af}({total}, {site}); /* {l} {line}: own_alloc({name}) — {c} */",
            blk = blk,
            af = alloc_fn,
            total = total,
            site = site,
            l = self.c("line", "line"),
            line = line,
            name = name,
            c = self.c(&format!("{} と own の子 {} 個を 0 で埋めて 1 つの所有リージョンに詰める", name, nkids), &format!("{} and its {} own children, zero-filled and packed into one own region", name, nkids))
        ));
        self.emit(format!("__builtin_memset({blk}, 0, {total});", blk = blk, total = total));
        self.need_zfill.insert(sid);
        self.emit(format!("(void)fsc_zfill_{n}(({sn} *){blk}, {blk} + FASCON_OWN_SPAN({sn}));", n = name, sn = sn, blk = blk));
        self.report.own_sites.push((line, name, nkids as usize));
        format!("(({} *){})", sn, blk)
    }

    /// 構造体でない `own_alloc(T)`（`own_alloc(i32)` / `own_alloc([u8; 4096])`）: 0 の中身を新しい所有リージョンに作り、
    /// 中の own の子も同じブロックに詰める / a non-struct `own_alloc(T)`: zeroed contents in a new own region, with the own
    /// children inside packed into the same block
    fn own_alloc_value(&mut self, e: &Expr, t: &Ty) -> String {
        let frame = std::mem::replace(&mut self.own_frame, false);
        let alloc_fn = if frame { "fascon_frame_alloc" } else { "fascon_own_alloc" };
        let pc = self.own_cty(t);
        let pay = self.payload_cty(t);
        let disp = self.s.t.display(t);
        let line = self.line_of(e.span);
        let site = self.site(e.span);
        let mut kids = Vec::new();
        self.zero_children_ty(t, 1, &mut kids, 0);
        self.report.own_alloc_sites.push((line, disp.clone()));
        let mut terms = vec![format!("FASCON_OWN_SPAN({})", pay)];
        for (c, m) in &kids {
            let csn = self.payload_cty(c);
            terms.push(if *m == 1 { format!("FASCON_OWN_CHILD_SPAN({})", csn) } else { format!("{}u * FASCON_OWN_CHILD_SPAN({})", m, csn) });
        }
        let total = self.new_tmp("sz");
        let o = self.new_tmp("o");
        self.emit(format!("const size_t {} = {};", total, terms.join(" + ")));
        self.emit(format!(
            "{pc}{o} = ({pc}){af}({total}, {site}); /* {l} {line}: own_alloc({d}) — {c} */",
            pc = pc,
            o = o,
            af = alloc_fn,
            total = total,
            site = site,
            l = self.c("line", "line"),
            line = line,
            d = disp,
            c = self.c("0 で埋めた新しい所有リージョン", "a new zero-filled own region")
        ));
        self.emit(format!("__builtin_memset({}, 0, {});", o, total));
        if !kids.is_empty() {
            let mut body = String::new();
            let mut k = 0usize;
            let place = if matches!(t, Ty::Array(..)) { o.clone() } else { format!("(*{})", o) };
            body.push_str(&format!("char *cur = (char *){} + FASCON_OWN_SPAN({});\n", o, pay));
            self.zfill_ty(t, &place, &mut body, 0, &mut k);
            self.emit("{");
            self.ind += 1;
            for l in body.lines() {
                self.emit(l);
            }
            self.emit("(void)cur;");
            self.ind -= 1;
            self.emit("}");
        }
        self.report.own_sites.push((line, disp, kids.iter().map(|x| x.1 as usize).sum()));
        o
    }

    // ───────────── 全体 / whole program ─────────────

    pub fn generate(mut self) -> (String, CgReport) {
        let prog = self.s.prog;
        // 関数の C 名 / C names of functions
        let struct_names: HashSet<String> = self.s.t.structs.iter().map(|s| format!("fsc_{}", s.name)).collect();
        for (fi, f) in self.s.t.fns.iter().enumerate() {
            let mut n = if f.is_main { "fascon2_main".to_string() } else { format!("fsc_{}", f.name) };
            if struct_names.contains(&n) {
                n.push_str("_fn");
            }
            // `extern "C" fn` の公開（本体あり）は C の名前そのまま / an export to C keeps its bare C name
            if f.extern_c == Some(true) {
                n = f.name.clone();
            }
            self.fn_c_names.insert(fi, n);
        }
        self.propagate_fns = self.compute_propagate();
        // Id の型 / Id types
        let mut ids = String::new();
        for (ai, a) in self.s.t.arenas.iter().enumerate() {
            let k = arena_k(a.cap);
            if a.bump {
                let _ = writeln!(
                    ids,
                    "typedef f20_bid_t {}; /* Id<{}> ({}): {} */",
                    self.id_name(ai),
                    a.name,
                    "bump_arena",
                    self.c(
                        &format!("64 ビット [世代 {} ビット | 添字 {} ビット]（K = {}、0 = None）", 64 - k, k, k),
                        &format!("64 bits [generation {} bits | index {} bits] (K = {}, 0 = None)", 64 - k, k, k)
                    )
                );
                continue;
            }
            if self.o.profile.id64() {
                // generic-os: 64 ビット [上位 32 = 世代 | 下位 32 = 添字]。表の大きさ（2^K）とソースの書き方は変わらない
                // generic-os: 64 bits [upper 32 = generation | lower 32 = index]; the table size (2^K) and the source are unchanged
                let _ = writeln!(
                    ids,
                    "typedef f20_id_t {}; /* Id<{}>: {} */",
                    self.id_name(ai),
                    a.name,
                    self.c(
                        &format!("64 ビット [世代 32 ビット | 添字 32 ビット（表は 2^{} スロット）]（0ULL = None）", k),
                        &format!("64 bits [generation 32 bits | index 32 bits (the table has 2^{} slots)] (0ULL = None)", k)
                    )
                );
                continue;
            }
            let _ = writeln!(
                ids,
                "typedef f20_id_t {}; /* Id<{}>: {} */",
                self.id_name(ai),
                a.name,
                self.c(
                    &format!("[世代 {} ビット | 添字 {} ビット]（K = {}、0 = None）", 32 - k, k, k),
                    &format!("[generation {} bits | index {} bits] (K = {}, 0 = None)", 32 - k, k, k)
                )
            );
        }
        // 構造体（依存順） / structs (dependency order)
        for sid in 0..self.s.t.structs.len() {
            self.emit_struct(sid);
        }
        // アリーナの要素型・static の型も先に / element types of arenas and static types too
        for a in self.s.t.arenas.clone() {
            self.ensure_value_type(&a.elem);
        }
        for st in self.s.t.statics.clone() {
            self.ensure_value_type(&st.ty);
        }
        // static の初期値が使う関数ポインタの関数は、static より前に宣言する / functions whose pointers appear in static
        // initializers are declared before the statics
        let mut early_protos = String::new();
        {
            let mut seen = Vec::new();
            let ids = self.static_init_ids();
            for (fi, eid, _) in &self.s.fn_value_sites {
                if ids.contains(eid) && !seen.contains(fi) {
                    seen.push(*fi);
                }
            }
            seen.sort_unstable();
            for fi in seen {
                let sig = self.fn_sig(fi);
                let _ = writeln!(early_protos, "static FASCON_MAYBE_UNUSED {};", sig);
            }
        }
        // static
        let mut statics = String::new();
        for item in &prog.items {
            if let Item::Static(sd) = item {
                if let Some(&si) = self.s.t.static_by_name.get(&sd.name) {
                    let ty = self.s.t.statics[si].ty.clone();
                    let init = match self.s.embeds.get(&si) {
                        // `embed("..")`: ファイルの中身（残りは 0） / the bytes of the file (the rest is zero)
                        Some(bytes) if bytes.is_empty() => "{ 0 }".to_string(),
                        Some(bytes) => {
                            let rows: Vec<String> = bytes.chunks(24).map(|c| format!("    {}", c.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(","))).collect();
                            format!("{{\n{}\n}}", rows.join(",\n"))
                        }
                        None => {
                            self.in_static_init = true;
                            let v = self.const_init(&sd.value, &ty);
                            self.in_static_init = false;
                            v
                        }
                    };
                    let d = self.cdecl(&ty, &format!("fsc_{}", sd.name));
                    let _ = writeln!(statics, "static const {} = {}; /* {} {} */", d, init, self.c("line", "line"), self.line_of(sd.span));
                }
            }
        }
        // 列: 全関数の `for id in @a` の集計から、どの表にどの列を足すかを先に決める（アリーナの型を出す前に要る）
        // columns: decide from every function's `for id in @a` reductions which columns each table gets (needed before
        // the arena types are emitted)
        self.collect_columns();
        // アリーナ / arenas
        let mut arenas = String::new();
        for ai in 0..self.s.t.arenas.len() {
            let mut a = if self.s.t.arenas[ai].bump { self.gen_bump(ai) } else { self.gen_arena(ai) };
            if self.mag_arena(ai) {
                a = self.gen_mag_arena(ai, a);
            }
            if self.s.process_arenas.contains(&ai) {
                a = self.gen_process_cols(ai, a);
            }
            if self.smp_on() && !self.s.t.arenas[ai].shared && !self.s.t.arenas[ai].dma && !self.s.t.arenas[ai].gpu {
                // マルチコア: ふつうのアリーナはコアごとに 1 つの実体（TLS）。アクセスのコードは 1 コアのときと同じで、ロックも
                // アトミック命令も入らない
                // multi-core: a plain arena has one instance per core (TLS); the access code is the single-core code, with no
                // locks and no atomic instructions
                let n = self.s.t.arenas[ai].name.clone();
                a = a.replace(&format!("static f20_arena_t({n}) arena_{n} F20_ARENA_STORAGE(f20_arena_t({n}))", n = n), &format!("static F20_PERCPU f20_arena_t({n}) arena_{n} F20_PERCPU_STORAGE", n = n));
            }
            arenas.push_str(&a);
            if self.s.t.arenas[ai].task_stack.is_some() {
                let t = self.gen_task_arena(ai);
                arenas.push_str(&t);
                if let Some(u) = self.s.t.umems.iter().find(|u| u.task == ai).cloned() {
                    let n = self.s.t.arenas[ai].name.clone();
                    let id = self.id_name(ai);
                    let et = self.cty(&self.s.t.arenas[ai].elem.clone());
                    let c1 = self.c(
                        "user_mem: タスクごとのユーザーメモリ（2 MiB 境界。起動時には 0 にせず、start_user がそのタスクの分だけ 0 にする）。EL0 からは全タスク同じ仮想番地に見える",
                        "user_mem: per-task user memory (2 MiB aligned; not zeroed at boot, start_user zeroes the block of its task). Every task sees it at the same virtual address from EL0",
                    );
                    let c2 = self.c("EL0 のタスクの生成: スロットの確保・ユーザーメモリの 0 埋め・EL0 の文脈の用意を 1 つの閉じた区間で行う", "creating an EL0 task: allocate the slot, zero its user memory and set up the EL0 context in one closed section");
                    let c3 = self.c("カーネルから見たそのタスクのユーザーメモリの窓（Id が無効なら NULL）", "the window over that task's user memory as seen by the kernel (NULL for an invalid Id)");
                    let _ = write!(
                        arenas,
                        "/* {c1} */\n#define UMEM_{un}_SIZE UINT64_C({size:#x})\n#define UMEM_{un}_VA UINT64_C({va:#x})\nstatic uint8_t fsc_umem_{un}[ARENA_{n}_CAP][UMEM_{un}_SIZE] __attribute__((aligned(0x200000), section(\".bss.fascon_user\")));\n\
/* {c2} */\nstatic FASCON_MAYBE_UNUSED {id} arena_{n}_start_user(const {et} *v, uint64_t entry, uint64_t sp) {{\n    const uint64_t im = fascon_plat_irq_save();\n    {id} id = arena_{n}_alloc(v);\n    if (id != 0u) {{\n        const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K);\n        if (!fascon_plat_task_create_user(i, id, entry, sp, fsc_task_stacks_{n}[i], ARENA_{n}_STACK, fsc_task_exit_{n})) {{\n            (void)arena_{n}_free(id);\n            id = 0u;\n        }}\n    }}\n    fascon_plat_irq_restore(im);\n    return id;\n}}\n\
/* {c3} */\nstatic inline FASCON_MAYBE_UNUSED f20_slice_t *umem_{un}_of(f20_slice_t *w, {id} id) {{\n    if (arena_{n}_get(id) == NULL) return NULL;\n    w->p = fsc_umem_{un}[F20_ID_INDEX(id, ARENA_{n}_K)];\n    w->n = UMEM_{un}_SIZE;\n    return w;\n}}\n\n",
                        c1 = c1, c2 = c2, c3 = c3, un = u.name, n = n, id = id, et = et, size = u.size, va = u.at
                    );
                    // generic-os: スタックは守りページ込みの大きさ（配列の要素の大きさ）で渡す / generic-os passes the size including the guard page
                    if self.o.profile == crate::target::Profile::GenericOs {
                        let from = format!("fsc_task_stacks_{n}[i], ARENA_{n}_STACK, fsc_task_exit_{n}", n = n);
                        let to = format!("fsc_task_stacks_{n}[i], sizeof fsc_task_stacks_{n}[i], fsc_task_exit_{n}", n = n);
                        arenas = arenas.replace(&from, &to);
                    }
                    if self.smp_on() {
                        // マルチコア: タスクの表はコアごとの実体なので、ユーザーメモリもコアごと（[コア][スロット][大きさ]）
                        // multi-core: the task table is per core, so user memory is per core as well ([core][slot][size])
                        let un = u.name.clone();
                        arenas = arenas
                            .replace(&format!("static uint8_t fsc_umem_{un}[ARENA_{n}_CAP][UMEM_{un}_SIZE]", un = un, n = n), &format!("static uint8_t fsc_umem_{un}[FASCON2_MAX_CPUS][ARENA_{n}_CAP][UMEM_{un}_SIZE]", un = un, n = n))
                            .replace(&format!("w->p = fsc_umem_{un}[F20_ID_INDEX(id, ARENA_{n}_K)];", un = un, n = n), &format!("w->p = fsc_umem_{un}[F20_CORE_INDEX()][F20_ID_INDEX(id, ARENA_{n}_K)];", un = un, n = n));
                    }
                }
                if self.smp_on() {
                    // マルチコア: カーネルスタックもコアごと（タスクの表のスロット i はコアごとに別のタスク）
                    // multi-core: kernel stacks per core as well (slot i of the task table is a different task on each core)
                    let n = self.s.t.arenas[ai].name.clone();
                    let decl_from = format!("static uint8_t fsc_task_stacks_{n}[ARENA_{n}_CAP]", n = n);
                    let decl_to = format!("static uint8_t fsc_task_stacks_{n}[FASCON2_MAX_CPUS][ARENA_{n}_CAP]", n = n);
                    arenas = arenas.replace(&decl_from, &decl_to).replace(&format!("fsc_task_stacks_{n}[i]", n = n), &format!("fsc_task_stacks_{n}[F20_CORE_INDEX()][i]", n = n));
                }
            }
            if self.s.t.arenas[ai].dma {
                let n = self.s.t.arenas[ai].name.clone();
                let id = self.id_name(ai);
                let c = self.c(
                    "dma_arena: スロットごとの「貸し出し中」の印。lend はハンドルを消して印を立て、reclaim は範囲内かつ印のある添字にだけハンドルを返す",
                    "dma_arena: one lent mark per slot; lend consumes the handle and sets the mark, reclaim returns a handle only for an index that is in range and marked",
                );
                let mk = if self.o.profile.id64() { format!("F20_ID_MAKE(arena_{n}.gen[raw], raw, ARENA_{n}_K)", n = n) } else { format!("((uint32_t)arena_{n}.gen[raw] << ARENA_{n}_K) | raw", n = n) };
                let _ = write!(
                    arenas,
                    "/* {c} */\nstatic uint8_t arena_{n}_lent[F20_SLOTS(ARENA_{n}_K) + 1u];\nstatic inline FASCON_MAYBE_UNUSED void arena_{n}_lend({id} h) {{\n    if (arena_{n}_get(h) != NULL) {{\n        fascon_plat_dma_sync(arena_{n}_slot(h), sizeof(arena_{n}.slot[0].v), 1);\n        arena_{n}_lent[F20_ID_INDEX(h, ARENA_{n}_K)] = 1u;\n    }}\n}}\nstatic inline FASCON_MAYBE_UNUSED {id} arena_{n}_reclaim(uint32_t raw) {{\n    if (raw >= F20_SLOTS(ARENA_{n}_K) || !arena_{n}_lent[raw]) return 0u;\n    arena_{n}_lent[raw] = 0u;\n    const {id} h = ({id})({mk});\n    if (arena_{n}_get(h) == NULL) return 0u;\n    fascon_plat_dma_sync(arena_{n}_slot(h), sizeof(arena_{n}.slot[0].v), 0);\n    return h;\n}}\n\n",
                    c = c, n = n, id = id, mk = mk
                );
            }
        }
        // gpu_arena: スロットごとの「装置にある」印（dispatch の番号）と装置の引換券。dispatch はハンドルを消して印を付け、
        // reclaim は同じ番号の引換券にだけ、仕事が終わっていればハンドルを返す
        // gpu_arena: a per-slot "on the device" mark (the dispatch's sequence number) and the platform's fence. dispatch
        // consumes the handle and sets the mark; reclaim returns the handle only for a ticket with the same number, once
        // the work is done
        let gpus: Vec<usize> = (0..self.s.t.arenas.len()).filter(|&a| self.s.t.arenas[a].gpu).collect();
        for (gi, &ai) in gpus.iter().enumerate() {
            let n = self.s.t.arenas[ai].name.clone();
            let id = self.id_name(ai);
            let mk = if self.o.profile.id64() { format!("F20_ID_MAKE(arena_{n}.gen[i], i, ARENA_{n}_K)", n = n) } else { format!("((uint32_t)arena_{n}.gen[i] << ARENA_{n}_K) | i", n = n) };
            let c1 = self.c(
                "gpu_arena: fly[i] = 0 なら CPU がスロットを持つ、それ以外は装置へ出した dispatch の番号。tok[i] はプラットフォーム層の引換券",
                "gpu_arena: fly[i] = 0 means the CPU owns the slot, otherwise it is the number of the dispatch that sent it to the device; tok[i] is the platform layer's fence",
            );
            let c2 = self.c(
                "装置へ出す: 最初の 1 回でスロットの配列を登録（ピン留め）し、ハンドルを番号付きの引換券 [番号 32 | 添字 32] に替える",
                "offload: the first call registers (pins) the slot array; the handle becomes a numbered ticket [number 32 | index 32]",
            );
            let c3 = self.c(
                "回収: 番号が合い、仕事が終わっていればハンドルを作り直して返す（wait = 1 なら終わるまで待つ）。古い・2 度目の引換券は 0（None）",
                "reclaim: when the number matches and the work is done, rebuild and return the handle (wait = 1 waits until it is done); a stale or second ticket gives 0 (None)",
            );
            let _ = write!(
                arenas,
                "/* {c1} */\n#define ARENA_{n}_GPU_ID {gi}u\nstatic uint32_t arena_{n}_gpu_seq;\nstatic uint8_t arena_{n}_gpu_on;\nstatic uint32_t arena_{n}_gpu_fly[F20_SLOTS(ARENA_{n}_K) + 1u];\nstatic uint64_t arena_{n}_gpu_tok[F20_SLOTS(ARENA_{n}_K) + 1u];\n\
/* {c2} */\nstatic FASCON_MAYBE_UNUSED uint64_t arena_{n}_dispatch(uint32_t kernel, uint64_t grid, uint64_t block, {id} h) {{\n    if (FASCON_UNLIKELY(arena_{n}_get(h) == NULL)) fascon_plat_kernel_panic(FASCON_PANIC_GPU, \"gpu_arena {n}: dispatch with a stale handle\");\n    if (FASCON_UNLIKELY(!arena_{n}_gpu_on)) {{\n        f20_gpu_register(&arena_{n}.slot[0], sizeof arena_{n}.slot, ARENA_{n}_GPU_ID);\n        arena_{n}_gpu_on = 1u;\n    }}\n    const uint32_t i = F20_ID_INDEX(h, ARENA_{n}_K);\n    uint32_t seq = arena_{n}_gpu_seq + 1u;\n    if (seq == 0u) seq = 1u;\n    arena_{n}_gpu_seq = seq;\n    arena_{n}_gpu_fly[i] = seq;\n    arena_{n}_gpu_tok[i] = f20_gpu_dispatch(ARENA_{n}_GPU_ID, i, kernel, grid, block, &arena_{n}.slot[i].v);\n    return ((uint64_t)seq << 32) | (uint64_t)i;\n}}\n\
/* {c3} */\nstatic FASCON_MAYBE_UNUSED {id} arena_{n}_reclaim(uint64_t f, int wait) {{\n    const uint32_t i = (uint32_t)f, seq = (uint32_t)(f >> 32);\n    if (i >= F20_SLOTS(ARENA_{n}_K) || seq == 0u || arena_{n}_gpu_fly[i] != seq) return 0u;\n    if (wait) {{\n        f20_gpu_wait(arena_{n}_gpu_tok[i]);\n    }} else if (!f20_gpu_poll(arena_{n}_gpu_tok[i])) {{\n        return 0u;\n    }}\n    arena_{n}_gpu_fly[i] = 0u;\n    return ({id})({mk});\n}}\n\n",
                c1 = c1, c2 = c2, c3 = c3, n = n, id = id, mk = mk, gi = gi
            );
        }
        if !gpus.is_empty() {
            let c1 = self.c(
                "gpu_arena の記述子（プラットフォーム層が arena_id と添字からスロットの要素の番地を求める: base + 添字 × stride）",
                "gpu_arena descriptors (the platform layer finds a slot's element from arena_id and the index: base + index × stride)",
            );
            let c2 = self.c(
                "カーネルの登録簿: FASCON_GPU_KERNEL(番号) が起動前に登録し、PC のワーカースレッドと --gpu=off の実行がここで引く",
                "the kernel registry: FASCON_GPU_KERNEL(id) registers before start-up; the PC worker thread and --gpu=off runs look kernels up here",
            );
            let rows: Vec<String> = gpus.iter().map(|&ai| {
                let n = self.s.t.arenas[ai].name.clone();
                format!("    {{ \"{n}\", (void *)&arena_{n}.slot[0], sizeof arena_{n}.slot, sizeof arena_{n}.slot[0], sizeof arena_{n}.slot[0].v }},", n = n)
            }).collect();
            let _ = write!(
                arenas,
                "/* {c1} */\nconst FasconGpuArena fascon2_gpu_arenas[] = {{\n{rows}\n}};\nconst uint32_t fascon2_gpu_arena_count = {cnt}u;\n\
/* {c2} */\nstatic struct {{\n    uint32_t id;\n    fascon_gpu_kernel_fn fn;\n}} fascon__gpu_kernels[FASCON2_GPU_KERNELS_MAX];\nstatic uint32_t fascon__gpu_kernel_count;\nvoid fascon_gpu_kernel_register(uint32_t kernel_id, fascon_gpu_kernel_fn fn) {{\n    for (uint32_t k = 0u; k < fascon__gpu_kernel_count; k++) {{\n        if (fascon__gpu_kernels[k].id == kernel_id) {{\n            fascon__gpu_kernels[k].fn = fn;\n            return;\n        }}\n    }}\n    if (fascon__gpu_kernel_count >= FASCON2_GPU_KERNELS_MAX) fascon_plat_kernel_panic(FASCON_PANIC_GPU, \"too many GPU kernels (FASCON2_GPU_KERNELS_MAX)\");\n    fascon__gpu_kernels[fascon__gpu_kernel_count].id = kernel_id;\n    fascon__gpu_kernels[fascon__gpu_kernel_count].fn = fn;\n    fascon__gpu_kernel_count++;\n}}\nfascon_gpu_kernel_fn fascon_gpu_kernel_lookup(uint32_t kernel_id) {{\n    for (uint32_t k = 0u; k < fascon__gpu_kernel_count; k++) {{\n        if (fascon__gpu_kernels[k].id == kernel_id) return fascon__gpu_kernels[k].fn;\n    }}\n    return NULL;\n}}\n\n",
                c1 = c1, c2 = c2, rows = rows.join("\n"), cnt = gpus.len()
            );
        }
        // --target hpc: コアの入口で 1 回、このコアを物理コアへピン留めし、コアごとのメモリ（コアごとのアリーナ・コアの
        // 文脈・既定の own ヒープ）をこのコアの NUMA ノードへ固定する（fascon_plat_* だけを呼ぶ）
        // --target hpc: once at each core's entry, pin the core to a physical core and bind its per-core memory (per-core
        // arenas, the core context, the default own heap) to the core's NUMA node (only fascon_plat_* calls)
        if self.o.profile == crate::target::Profile::Hpc {
            let smp = self.smp_on();
            let mut binds = String::new();
            for a in &self.s.t.arenas {
                let percore = !a.shared && !a.dma && !a.gpu;
                // 1 コアのプログラムではどの表もコア 0 のもの / in a one-core program every table belongs to core 0
                if percore || !smp {
                    // #[numa_node(n)] の表は main が固定する / tables with #[numa_node(n)] are bound by main
                    if self.s.hpc.as_ref().map_or(false, |h| h.numa_arenas.iter().any(|&(x, _)| self.s.t.arenas[x].name == a.name)) {
                        continue;
                    }
                    binds.push_str(&format!("    (void)fascon_plat_numa_bind((void *)&arena_{n}, sizeof arena_{n}, node);\n", n = a.name));
                }
            }
            let c1 = self.c(
                "--target hpc: このコアの最初の 1 回だけ。ピン留め（SMT の兄弟を避ける）と、コアごとのメモリの NUMA ローカルな固定（MPOL_BIND）",
                "--target hpc: only the first time on this core; pinning (avoiding SMT siblings) and binding per-core memory NUMA-locally (MPOL_BIND)",
            );
            let percpu = if smp {
                "#if defined(FASCON2_SMP)\n    (void)fascon_plat_numa_bind((void *)&fascon__cpu[core % FASCON2_MAX_CPUS], sizeof fascon__cpu[0], node);\n    (void)fascon_plat_numa_bind((void *)fascon__default_heaps[core % FASCON2_MAX_CPUS], sizeof fascon__default_heaps[0], node);\n#endif\n"
            } else {
                ""
            };
            let _ = write!(
                arenas,
                "/* {c1} */\nstatic F20_PERCPU uint8_t fascon__hpc_entered;\nstatic FASCON_MAYBE_UNUSED void f20_hpc_core_enter(void) {{\n    if (fascon__hpc_entered) return;\n    fascon__hpc_entered = 1u;\n    const uint32_t core = fascon_plat_cpu_id();\n    fascon_plat_pin_core(core);\n    const uint32_t node = fascon_plat_numa_node(core);\n{binds}{percpu}    (void)node;\n}}\n\n",
                c1 = c1, binds = binds, percpu = percpu
            );
        }
        // `dma` リージョン: デバイスと共有する RAM（volatile。4 KiB 境界） / `dma` regions: RAM shared with devices
        for d in self.s.t.dmas.clone() {
            let decl = self.cdecl(&Ty::Array(Box::new(d.elem.clone()), d.len), &format!("dma_{}", d.name));
            let c = self.c("dma: デバイスと共有する RAM（読み書きは常に volatile）", "dma: RAM shared with a device (every access is volatile)");
            let _ = writeln!(arenas, "/* {} */\nstatic volatile {} __attribute__((aligned(4096), section(\".bss.fascon_dma\")));\n", c, decl);
        }
        // チャネル / channels
        for c in self.s.t.chans.clone() {
            self.ensure_value_type(&c.elem);
            let ct = self.cty(&c.elem);
            let note = self.c(
                "chan: コア間のロックフリーなリングバッファ（送り手・受け手が何本でもよい。セルごとの番号を acquire / release で読み書き、先頭と末尾は別のキャッシュライン）",
                "chan: a lock-free ring buffer between cores (any number of senders and receivers; per-cell sequence numbers with acquire / release, head and tail on separate cache lines)",
            );
            let _ = writeln!(arenas, "/* ───── chan {}: {}[{}] — {} ───── */\nF20_DEFINE_CHAN({}, {}, {}u);\n", c.name, self.s.t.display(&c.elem), c.cap, note, c.name, ct.trim_end(), c.cap);
        }
        // MMIO と I/O ポート / MMIO and I/O ports
        let mut mmios = String::new();
        for mi in 0..self.s.t.mmios.len() {
            mmios.push_str(&self.gen_mmio(mi));
        }
        for pi in 0..self.s.t.ports.len() {
            mmios.push_str(&self.gen_port(pi));
        }
        // 関数 / functions
        let mut protos = String::new();
        let mut bodies = String::new();
        for (fi, f) in self.s.t.fns.clone().iter().enumerate() {
            // C から取り込む関数・安全インラインアセンブリは、本体の代わりに境界の関数を出す / imports from C and asm functions
            // get a boundary function instead of a body
            if f.extern_c == Some(false) || f.asm.is_some() {
                let b = if f.asm.is_some() { self.gen_asm_fn(fi) } else { self.gen_c_import(fi) };
                protos.push_str(&b);
                continue;
            }
            let sig = self.fn_sig(fi);
            if f.extern_c == Some(true) {
                let _ = writeln!(protos, "{}; /* extern \"C\" */", sig);
            } else if !f.is_main {
                let _ = writeln!(protos, "static FASCON_MAYBE_UNUSED {};", sig);
            }
            if let Item::Fn(fd) = &prog.items[f.item] {
                let mut body = self.gen_fn(fi, fd);
                if f.extern_c == Some(true) {
                    body = body.replacen(&format!("static {}", sig), &sig, 1);
                }
                bodies.push_str(&body);
            }
        }
        self.gen_glue();
        let mirror_fns = self.gen_mirror_masks();
        // fascon-gc: 記述子・ヒープ・根の関数（型の定義が組み立てに入る前に作る） / descriptors, heaps and the root
        // function (made before the type definitions are assembled)
        let gc_block = self.gc_prepare();
        // 組み立て / assemble
        let mut out = String::new();
        let _ = writeln!(
            out,
            "/*\n * {}.c — generated by Fascon {} (1.0 engine) from {} — do not edit\n * {}\n * {}\n */",
            self.file_tag.trim_end_matches(".fsc").trim_end_matches(".rs"),
            self.o.version,
            self.file_tag,
            self.c(
                "構造体のフィールドに置かれるポインタは own T（唯一の所有者）だけ。参照は引数とローカル変数にしか現れない。",
                "the only pointers stored in struct fields are own T (unique owners); references appear only in parameters and locals."
            ),
            self.c(
                "アリーナ間のリンクはすべて Id（f20_id_t = uint32_t: [世代 32-K | 添字 K]、K はアリーナの容量から決まる）。ソースのライフタイム注釈: 0 個。",
                "every cross-arena link is an Id (f20_id_t = uint32_t: [generation 32-K | index K], K follows from the arena's capacity). lifetime annotations in the source: 0."
            )
        );
        // 割り込み・タスク・閉じた区間を使うプログラムだけ、ランタイムの割り込み対応を有効にする（使わないプログラムの C は
        // 以前と同じ）
        // only programs with interrupts, tasks or closed sections turn on the runtime's interrupt support (the C of other
        // programs is unchanged)
        if self.s.ctx.any_roots || self.s.ctx.criticals > 0 {
            let _ = writeln!(out, "#define FASCON2_PREEMPT 1 /* {} */", self.c("irq / task_arena / critical を使う", "uses irq / task_arena / critical"));
        }
        if self.smp_on() {
            let _ = writeln!(out, "#define FASCON2_SMP 1 /* {} */", self.c("spawn_on を使う: コアごとの実体（アリーナ・フレーム・own ヒープ）と、ロックとしての critical", "uses spawn_on: per-core instances (arenas, frame, own heap) and critical as a lock"));
        }
        if self.s.user_mode {
            let _ = writeln!(out, "#define FASCON2_USER 1 /* {} */", self.c("ユーザープログラム（EL0）: システムコールは svc", "a user program (EL0): system calls are svc instructions"));
        }
        out.push_str(&self.profile_defines());
        if self.o.profile == crate::target::Profile::Classic && self.uses_universal() {
            let _ = writeln!(out, "#define FASCON2_UNIVERSAL 1 /* {} */", self.c("ユニバーサル OS 拡張の構文を使う（user T・catch_fault・atomic・rcu_read・#[secret]・dma own・bind_mmio・asm・extern \"C\" …）", "uses the universal-OS syntax (user T, catch_fault, atomic, rcu_read, #[secret], dma own, bind_mmio, asm, extern \"C\" ...)"));
        }
        if self.o.board {
            let _ = writeln!(out, "#define FASCON2_BOARD 1 /* {} */\n#include \"fascon_board.h\"", self.c("--board の board.toml から作ったボード定義", "the board definition made from the board.toml given with --board"));
        }
        if self.gc_any() {
            let _ = writeln!(out, "#define FASCON2_GC 1 /* {} */", self.c("fascon-gc（gc_arena）を使う", "uses fascon-gc (gc_arena)"));
        }
        if self.uses_gpu() {
            let _ = writeln!(out, "#define FASCON2_GPU 1 /* {} */", self.c("gpu_arena を使う: 装置への貸し出しは fascon_plat_gpu_* だけを通す（PC はワーカースレッドで真似る）", "uses gpu_arena: offloading goes through fascon_plat_gpu_* only (the PC emulates it with a worker thread)"));
            if crate::universal::GPU_OFF.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = writeln!(out, "#define FASCON2_GPU_OFF 1 /* {} */", self.c("--gpu=off: 装置へは出さず、dispatch は呼んだコアでその場で走る（fascon_plat_gpu_* を呼ばない）", "--gpu=off: nothing goes to a device; dispatch runs in place on the calling core (no fascon_plat_gpu_* calls)"));
            }
        }
        if self.uses_float() {
            let _ = writeln!(out, "#define FASCON2_FLOAT 1 /* {} */", self.c("f32 / f64 を使う: IEEE-754 の演算をそのまま（FMA への縮約なし）、整数への変換は飽和、出力は往復できる最短の 10 進", "uses f32 / f64: IEEE-754 arithmetic as written (no contraction into FMA), saturating conversion to integers, output as the shortest decimal that reads back"));
        }
        // --nontemporal=on: 大きな完全列化の列の走査にソフトウェアのプリフェッチ（auto では入れない: このホストの x86 では
        // ハードウェアのプリフェッチで帯域の上限に届き、効果がなかった。bench/RESULTS.md）
        // --nontemporal=on: software prefetch in scans of large True-SoA columns (not under auto: on the x86 host measured,
        // hardware prefetch already reaches the bandwidth limit and it gained nothing; bench/RESULTS.md)
        if self.o.nontemporal == NtMode::On && self.tsoa.iter().enumerate().any(|(a, fs)| fs.iter().any(|f| self.s.t.arenas[a].cap as u64 * (f.it.bits(self.o.ptr_bits) as u64 / 8) >= tsoa::NT_AUTO_MIN_BYTES)) {
            let _ = writeln!(out, "#ifndef F20_PREFETCH_DIST\n#define F20_PREFETCH_DIST 1024 /* {} */\n#endif", self.c("--nontemporal=on: 列の走査で 1 KiB 先を読む合図（F20_PREFETCH_R）", "--nontemporal=on: column scans hint 1 KiB ahead (F20_PREFETCH_R)"));
        }
        let _ = writeln!(out, "#define FASCON2_RT_IMPLEMENTATION\n#include \"fascon2_rt.h\"\n");
        let _ = writeln!(
            out,
            "_Static_assert(sizeof(void *) * 8 == {}, \"Fascon: --ptr-bits does not match the target pointer width\");\n",
            self.o.ptr_bits
        );
        {
            let mut done: Vec<usize> = Vec::new();
            for (_, fi, _) in &self.s.spawns {
                if !done.contains(fi) {
                    done.push(*fi);
                    let _ = writeln!(out, "static void fsc_spawn_{}(uint64_t a);", self.s.t.fns[*fi].name);
                }
            }
        }
        if !self.s.user_mode && self.s.t.fns.iter().any(|g| g.syscall.is_some() && !g.stub) {
            let _ = writeln!(out, "extern const FasconSyscallFn fascon2_syscall_table[256];\n");
        }
        if self.s.t.fns.iter().any(|g| g.irq.is_some()) {
            let _ = writeln!(out, "extern const FasconIrqEntry fascon2_irq_table[];\nextern const uint32_t fascon2_irq_count;\n");
        }
        if !ids.is_empty() {
            out.push_str(&ids);
            out.push('\n');
        }
        let _ = writeln!(out, "typedef struct fsc__unit fsc__unit;");
        for s in &self.s.t.structs {
            let _ = writeln!(out, "typedef struct fsc_{} fsc_{};", s.name, s.name);
        }
        out.push('\n');
        out.push_str(&self.types_out);
        if !self.glue_protos.is_empty() {
            let _ = writeln!(out, "/* ───── {} ───── */", self.c("解放・複製の糊（own を含む型だけ）", "drop / clone glue (only for types that contain own)"));
            out.push_str(&self.glue_protos);
            out.push('\n');
        }
        if !early_protos.is_empty() {
            let _ = writeln!(out, "/* ───── {} ───── */", self.c("static の関数ポインタの表が指す関数", "functions referenced by function-pointer tables in statics"));
            out.push_str(&early_protos);
            out.push('\n');
        }
        if !statics.is_empty() {
            let _ = writeln!(out, "/* ───── static ({}) ───── */", self.c("Kernel リージョン: 不変・全 CPU 共有", "Kernel region: immutable, shared by all CPUs"));
            out.push_str(&statics);
            out.push('\n');
        }
        out.push_str(&arenas);
        if !mirror_fns.is_empty() {
            let _ = writeln!(out, "/* ───── {} ───── */", self.c("列の写しの一部だけを作り直す関数（書き込みが届く列の組ごと）", "partial column-mirror rebuilds (one per set of columns a write can reach)"));
            out.push_str(&mirror_fns);
        }
        out.push_str(&mmios);
        if !self.reduce_out.is_empty() {
            let _ = writeln!(out, "/* ───── {} ───── */", self.c("`for id in @a` の総和の特殊化（表の直線走査・分岐なし・世代検査なし）", "sum specializations of `for id in @a` (straight table scans: no branches, no generation checks)"));
            out.push_str(&self.reduce_out);
        }
        if !protos.is_empty() {
            let _ = writeln!(out, "/* ───── prototypes ───── */");
            out.push_str(&protos);
            out.push('\n');
        }
        out.push_str(&self.glue_out);
        out.push_str(&gc_block);
        out.push_str(&bodies);
        // MMIO リージョン表 / the MMIO region table
        let _ = writeln!(out, "/* ───── {} ───── */", self.c("MMIO リージョン表（BSP が起動時に読む: フレームバッファの設定・カーネル像との重なり検査）", "MMIO region table (read by the BSP at boot: framebuffer setup, overlap check against the kernel image)"));
        let mut rows = Vec::new();
        for m in &self.s.t.mmios {
            let (w, h) = if m.dims.len() == 2 { (m.dims[1], m.dims[0]) } else { (m.dims[0], 1) };
            rows.push(format!(
                "    {{ \"{}\", {:#x}u, {}u, {}, {}u, {}u, {}u }},",
                m.name,
                m.addr,
                m.size_bytes(),
                if m.framebuffer { "FASCON_MMIO_FRAMEBUFFER" } else { "0u" },
                w,
                h,
                m.elem.bits(64)
            ));
        }
        if rows.is_empty() {
            let _ = writeln!(out, "const FasconMmioRegion fascon2_mmio_regions[1] = {{ {{ NULL, 0u, 0u, 0u, 0u, 0u, 0u }} }};\nconst uint32_t fascon2_mmio_region_count = 0u;");
        } else {
            let _ = writeln!(out, "const FasconMmioRegion fascon2_mmio_regions[] = {{\n{}\n}};\nconst uint32_t fascon2_mmio_region_count = {}u;", rows.join("\n"), rows.len());
        }
        let _ = writeln!(out, "#if defined(FASCON2_HOSTED_MMIO)\nuintptr_t fascon2_host_mmio_base[{}];\n#endif", rows.len().max(1));
        // アリーナ表 / the arena table
        let _ = writeln!(
            out,
            "/* ───── {} ───── */",
            self.c(
                "アリーナ表（BSP は起動時に TLB の覆い方を表示し、PC の実行層は 1 MiB 以上の表に大きなページを頼む）",
                "arena table (the BSP prints the TLB coverage at boot; the PC layer asks for huge pages for tables of 1 MiB or more)"
            )
        );
        let mut arows = Vec::new();
        for a in &self.s.t.arenas {
            if self.smp_on() && !a.shared && !a.gpu {
                continue; // コアごとの実体は番地が 1 つではない / a per-core instance has no single address
            }
            arows.push(format!(
                "    {{ \"{n}\", &arena_{n}, sizeof arena_{n}, F20_SLOTS(ARENA_{n}_K) + 1u, (uint32_t)sizeof(f20_slot_t({n})) }},",
                n = a.name
            ));
        }
        if arows.is_empty() {
            let _ = writeln!(out, "const FasconArenaRegion fascon2_arena_regions[1] = {{ {{ NULL, NULL, 0u, 0u, 0u }} }};\nconst uint32_t fascon2_arena_region_count = 0u;");
        } else {
            let _ = writeln!(out, "const FasconArenaRegion fascon2_arena_regions[] = {{\n{}\n}};\nconst uint32_t fascon2_arena_region_count = {}u;", arows.join("\n"), arows.len());
        }
        // 割り込み表（irq 項目があるときだけ。BSP は起動時に読んで GIC で許可し、割り込みごとに引く）
        // the interrupt table (only with irq items; the BSP reads it at boot to enable the lines and looks handlers up in it)
        self.gen_boundary(&mut out);
        // spawn_on の入口: レジスタ 1 本で届いた引数を元の型に戻して呼ぶ / spawn_on entries: turn the one-register argument back into its type
        let mut done: Vec<usize> = Vec::new();
        for (_, fi, _) in self.s.spawns.clone() {
            if done.contains(&fi) {
                continue;
            }
            done.push(fi);
            let f = self.s.t.fns[fi].clone();
            let cname = self.fn_c_names[&fi].clone();
            let call = match f.params.first() {
                Some(p) => {
                    let ct = self.cty(&p.ty);
                    if matches!(p.ty, Ty::Own(_)) { format!("{}(({})(uintptr_t)a)", cname, ct) } else { format!("{}(({})a)", cname, ct) }
                }
                None => format!("{}()", cname),
            };
            let enter = if self.o.profile == crate::target::Profile::Hpc { "    f20_hpc_core_enter();\n" } else { "" };
            let _ = writeln!(out, "/* {} */\nstatic void fsc_spawn_{}(uint64_t a) {{\n    (void)a;\n{}    {};\n}}", self.c("spawn_on の入口", "spawn_on entry"), f.name, enter, call);
        }
        let mut irows = Vec::new();
        for (fi, f) in self.s.t.fns.iter().enumerate() {
            if let Some(intid) = f.irq {
                irows.push(format!("    {{ {}u, {}, \"{}\" }},", intid, self.fn_c_names[&fi], f.name));
            }
        }
        if !irows.is_empty() {
            let _ = writeln!(
                out,
                "/* ───── {} ───── */\nconst FasconIrqEntry fascon2_irq_table[] = {{\n{}\n}};\nconst uint32_t fascon2_irq_count = {}u;",
                self.c("割り込み表（INTID・ハンドラ・名前）", "interrupt table (INTID, handler, name)"),
                irows.join("\n"),
                irows.len()
            );
        }
        // 実行ルートと文脈の報告（`fascon regions`） / the report of execution roots and contexts
        let cx = &self.s.ctx;
        if cx.any_roots || cx.criticals > 0 {
            let names = |v: &Vec<usize>| v.iter().map(|f| self.s.t.fns[*f].name.clone()).collect::<Vec<_>>().join(", ");
            let mut lines = Vec::new();
            if !cx.irq_fns.is_empty() {
                lines.push(format!("{}: {}", self.c("割り込みハンドラ", "interrupt handlers"), names(&cx.irq_fns)));
            }
            if !cx.task_entries.is_empty() {
                lines.push(format!(
                    "{}: {}  ({})",
                    self.c("タスクの入口", "task entries"),
                    names(&cx.task_entries),
                    if cx.preemptive { self.c("プリエンプティブモード: irq が resume を呼ぶ", "preemptive mode: an irq calls resume") } else { self.c("協調モード: 切り替えは yield_to だけ", "cooperative mode: switches only at yield_to") }
                ));
            }
            lines.push(format!("{}: {}", self.c("閉じた区間（critical）", "closed sections (critical)"), cx.criticals));
            for (a, r) in &cx.shared {
                lines.push(format!("{}: {} ← {}", self.c("保護の無い共有アリーナ", "shared arena without protection"), self.s.t.arenas[*a].name, r));
            }
            self.report.ctx_lines = lines;
        }
        for (fi, f) in self.s.t.fns.iter().enumerate() {
            if let Some(n) = self.fn_c_names.get(&fi) {
                if self.s.fn_value_sites.iter().any(|(g, _, _)| *g == fi) {
                    self.report.callback_fns.push(n.clone());
                }
                if f.irq.is_some() {
                    self.report.irq_fns.push(n.clone());
                }
            }
        }
        (out, self.report)
    }

    /// static の初期値の中の式の番号 / the expression ids inside static initializers
    fn static_init_ids(&self) -> HashSet<ExprId> {
        let mut out = HashSet::new();
        fn ex(e: &Expr, out: &mut HashSet<ExprId>) {
            out.insert(e.id);
            for c in crate::flow::children(e) {
                if let crate::flow::Child::E(x) = c {
                    ex(x, out);
                }
            }
        }
        for item in &self.s.prog.items {
            if let Item::Static(sd) = item {
                ex(&sd.value, &mut out);
            }
        }
        out
    }

    /// task_arena: スロットごとのカーネルスタックと、生成（alloc + 文脈の用意）・終了（解放 + 切り替え）の関数
    /// task_arena: the per-slot kernel stacks and the functions for creation (alloc + context setup) and exit (free + switch)
    fn gen_task_arena(&mut self, ai: usize) -> String {
        let a = self.s.t.arenas[ai].clone();
        let n = a.name.clone();
        let et = self.cty(&a.elem);
        let id = self.id_name(ai);
        let stack = a.task_stack.unwrap_or(16384);
        let c1 = self.c(
            "task_arena: スロットごとのカーネルスタック（下 1/4 はそのタスクのフレームリージョン）",
            "task_arena: one kernel stack per slot (its lowest quarter is the task's frame region)",
        );
        let c2 = self.c("終了: 自分のスロットを返して、ほかのタスクへ切り替える（戻らない）", "exit: free the own slot and switch to another task (never returns)");
        let c3 = self.c("生成: スロットの確保と文脈の用意を 1 つの閉じた区間で行う", "creation: allocate the slot and set up the context in one closed section");
        // generic-os: スタックは 4 KiB 境界・4 KiB の守りページ込み（BSP がフレームリージョンとスタックの間を未マップにする）
        // generic-os: stacks are 4 KiB aligned and include a 4 KiB guard page (the BSP unmaps it between the frame region and
        // the stack)
        let (sa, sz) = if self.o.profile == crate::target::Profile::GenericOs { ("4096", format!("ARENA_{n}_STACK_ALLOC", n = n)) } else { ("16", format!("ARENA_{n}_STACK", n = n)) };
        let guard_def = if self.o.profile == crate::target::Profile::GenericOs { format!("#define ARENA_{n}_STACK_ALLOC ((((ARENA_{n}_STACK) + 4095u) & ~4095u) + 4096u) /* {} */\n", self.c("守りページ 4 KiB を含む", "including a 4 KiB guard page"), n = n) } else { String::new() };
        // generic-os のスタックは別の区分（BSP が起動時に 4 KiB のページへ分ける範囲） / generic-os stacks get their own section
        // (the range the BSP splits into 4 KiB pages at boot)
        let sect = if self.o.profile == crate::target::Profile::GenericOs { ".bss.fascon_kstacks" } else { ".bss.fascon_stacks" };
        let stack_body = format!(
            "/* {c1} */\n#define ARENA_{n}_STACK {stack}u\n{gd}static uint8_t fsc_task_stacks_{n}[ARENA_{n}_CAP][{sz}] __attribute__((aligned({sa}), section(\"{sect}\")));\n",
            c1 = c1, n = n, stack = stack, gd = guard_def, sz = sz, sa = sa, sect = sect
        );
        format!(
            "{stack_body}\
/* {c2} */\nstatic FASCON_NORETURN FASCON_MAYBE_UNUSED void fsc_task_exit_{n}(void) {{\n    (void)fascon_plat_irq_save();\n    (void)arena_{n}_free(({id})fascon_plat_task_current());\n    fascon_plat_task_exit();\n}}\n\
/* {c3} */\nstatic FASCON_MAYBE_UNUSED {id} arena_{n}_start(const {et} *v, void (*entry)(uint64_t), uint64_t arg) {{\n    const uint64_t im = fascon_plat_irq_save();\n    {id} id = arena_{n}_alloc(v);\n    if (id != 0u) {{\n        const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K);\n        if (!fascon_plat_task_create(i, id, entry, arg, fsc_task_stacks_{n}[i], {sz}, fsc_task_exit_{n})) {{\n            (void)arena_{n}_free(id);\n            id = 0u;\n        }}\n    }}\n    fascon_plat_irq_restore(im);\n    return id;\n}}\n\n",
            stack_body = stack_body, c2 = c2, c3 = c3, n = n, id = id, et = et, sz = sz
        )
    }

    fn fn_sig(&mut self, fi: usize) -> String {
        let f = self.s.t.fns[fi].clone();
        let name = self.fn_c_names[&fi].clone();
        if f.is_main {
            return format!("void {}(void)", name);
        }
        let ret = self.cty(&f.ret);
        let mut ps = Vec::new();
        for p in &f.params {
            let n = sanitize(&p.name);
            ps.push(self.cdecl(&p.ty, &n));
        }
        let params = if ps.is_empty() { "void".to_string() } else { ps.join(", ") };
        // generic-os: コールバックの候補（値として使われた関数）の入口に BTI c / BTI c at the entry of callback candidates
        let bti = if self.o.profile == crate::target::Profile::GenericOs && self.s.fn_value_sites.iter().any(|(g, _, _)| *g == fi) { "F20_BTI_C " } else { "" };
        if ret.ends_with('*') {
            format!("{}{}{}({})", bti, ret, name, params)
        } else {
            format!("{}{} {}({})", bti, ret, name, params)
        }
    }

    /// generic-os: 間接呼び出しの前に AUTIA で検証した関数ポインタ / the function pointer authenticated with AUTIA before an
    /// indirect call (generic-os)
    fn pac_auth(&mut self, ct: &Ty, cv: String) -> String {
        if self.o.profile != crate::target::Profile::GenericOs || !matches!(ct, Ty::Fn(..)) {
            return cv;
        }
        self.report.pac_auths += 1;
        let c = self.cty(ct);
        let m = pac_modifier(&self.mangle(ct));
        format!("(({})f20_pac_auth((void *){}, {}))", c, cv, m)
    }

    // ───────────── アリーナ / arenas ─────────────
    /// 世代の配列の型（generic-os はいつも 32 ビット） / the generation array type (always 32 bits on generic-os)
    fn gen_ty(&self, k: u32) -> &'static str {
        if self.o.profile.id64() {
            "f20_gen32_t"
        } else {
            gen_type(k)
        }
    }

    /// プロファイルごとの #define（Classic は空 = 今までと同じ C） / per-profile #defines (empty for Classic = the same C as before)
    fn profile_defines(&self) -> String {
        use crate::target::Profile;
        match self.o.profile {
            Profile::Classic => String::new(),
            Profile::Rtos => format!(
                "#define FASCON2_RTOS 1 /* {} */\n",
                self.c("--target rtos: 32 ビット Id・固定長 .fascon_arena・再帰なし（スタック消費はコンパイラが静的に証明）", "--target rtos: 32-bit Ids, fixed .fascon_arena, no recursion (stack use proven statically by the compiler)")
            ),
            Profile::GenericOs => format!(
                "#define FASCON2_GENERIC_OS 1 /* {} */\n#define FASCON2_ID64 1 /* {} */\n",
                self.c("--target generic-os: 仮想番地を予約したアリーナ・Per-CPU マガジン・RCU・PAN / PAC / BTI", "--target generic-os: VA-reserved arenas, per-CPU magazines, RCU, PAN / PAC / BTI"),
                self.c("f20_id_t = uint64_t [上位 32 = 世代 | 下位 32 = 添字]、0ULL = None", "f20_id_t = uint64_t [upper 32 = generation | lower 32 = index], 0ULL = None")
            ),
            Profile::Hpc => {
                let t = self.s.topology.unwrap_or(crate::target::Topology::HPC_DEFAULT);
                format!(
                    "#define FASCON2_HPC 1 /* {} */\n#define FASCON2_ID64 1 /* {} */\n#define FASCON2_MAX_CPUS {}u /* {} */\n",
                    self.c("--target hpc: 2 MiB 境界のアリーナ（MADV_HUGEPAGE）・計算コアの物理コアへのピン留め・コアごとのメモリの NUMA ローカルな固定", "--target hpc: 2 MiB-aligned arenas (MADV_HUGEPAGE), compute cores pinned to physical cores, per-core memory bound NUMA-locally"),
                    self.c("f20_id_t = uint64_t [上位 32 = 世代 | 下位 32 = 添字]、0ULL = None", "f20_id_t = uint64_t [upper 32 = generation | lower 32 = index], 0ULL = None"),
                    t.cores,
                    self.c(&format!("計算ノードの形: {}", t.spec()), &format!("the compute node's shape: {}", t.spec()))
                )
            }
        }
    }


    fn gen_arena(&mut self, ai: usize) -> String {
        let a = self.s.t.arenas[ai].clone();
        let n = a.name.clone();
        let cap = a.cap;
        let k = arena_k(cap);
        let et = self.cty(&a.elem);
        let id = self.id_name(ai);
        let nd = self.s.t.needs_drop(&a.elem);
        // 完全列化: スロットの要素は残った部分の型 st（et は要素まるごとの型のまま） / True SoA: the slot element is the
        // part type st (et stays the whole element type)
        let ts = self.tsoa.get(ai).map_or(false, |v| !v.is_empty());
        let st = if ts { self.ts_part(ai) } else { et.clone() };
        // drop: (*p) を解放する（確保に失敗した値 v にも使う）。drop_slot: free でスロットの要素を解放する（完全列化なら列から
        // 組み立て直した一時値を解放する。#[on_drop] の関数は列の値を見る）
        // drop: drops (*p) (also used for the value v when alloc fails); drop_slot: drops the slot element in free (for
        // True SoA a temporary rebuilt from the columns, so the #[on_drop] function sees the column values)
        let drop = if nd { self.drop_code(&a.elem, "(*p)") } else { String::new() };
        let drop_slot = if nd && ts {
            format!("{{ {et} _tv = arena_{n}_load(F20_ID_INDEX(id, ARENA_{n}_K)); {d} }}", et = et, n = n, d = self.drop_code(&a.elem, "_tv"))
        } else {
            drop.clone()
        };
        let slots = 1u64 << k;
        // generic-os（64 ビット Id）では世代はいつも 32 ビット / with 64-bit Ids (generic-os) generations are always 32 bits
        let gbits = if self.o.profile.id64() { 32 } else { 32 - k };
        let gens = (1u64 << gbits) - 1;
        let total = cap as u64 * gens;
        let mut o = String::new();
        let _ = writeln!(
            o,
            "/* ───── arena {n}: {t}[{cap}] — {d} ───── */",
            n = n,
            t = self.s.t.display(&a.elem),
            cap = cap,
            d = self.c("Kernel リージョン（静的・.bss）", "Kernel region (static, .bss)")
        );
        let _ = writeln!(
            o,
            "/* {} */",
            self.c(
                &format!(
                    "K = {k}: Id = [世代 {g} ビット | 添字 {k} ビット]。表は 2^{k} = {slots} スロット + 番兵 1（容量を超える {pad} スロットは配らない詰め物）。\n * 1 スロットあたり {gens} 世代まで再利用し、使い切ったスロットは引退（世代は一周しない）。確保はアリーナ全体で {total} 回まで。",
                    k = k,
                    g = gbits,
                    slots = group(slots),
                    pad = group(slots - cap as u64),
                    gens = group(gens),
                    total = group(total)
                ),
                &format!(
                    "K = {k}: Id = [generation {g} bits | index {k} bits]. The table has 2^{k} = {slots} slots + 1 sentinel ({pad} slots past the capacity are padding that is never handed out).\n * Each slot is reused for up to {gens} generations, then retires (generations never wrap). {total} allocations over the arena's life.",
                    k = k,
                    g = gbits,
                    slots = group(slots),
                    pad = group(slots - cap as u64),
                    gens = group(gens),
                    total = group(total)
                )
            )
        );
        let _ = writeln!(o, "#define ARENA_{n}_K {k}u\n#define ARENA_{n}_CAP {cap}u", n = n, k = k, cap = cap);
        let al = self.s.t.type_align_attr(&a.elem).max(16);
        if ts {
            o.push_str(&self.gen_tsoa_part(ai));
        }
        let _ = writeln!(
            o,
            "typedef struct {al16}{{\n    {decl};\n}} f20_slot_{n}_t; /* {c} */",
            al16 = if ts { "" } else { "F20_ALIGN16 " },
            decl = if ts { format!("{} v", st) } else { self.cdecl(&a.elem, "v") },
            n = n,
            c = if ts {
                self.c("スロットに残る部分の自然な整列（完全列化のフィールドは列へ出た。表は FFI の境界を通らない）", "the natural alignment of the part left in the slot (the True-SoA fields moved to columns; the table never crosses an FFI boundary)")
            } else if al > 16 {
                self.c(&format!("{} バイト境界のスロット（要素の #[repr(align({}))]）", al, al), &format!("a slot on a {}-byte boundary (the element's #[repr(align({}))])", al, al))
            } else {
                self.c("16 バイト境界のスロット", "a slot on a 16-byte boundary")
            }
        );
        let has_mirror = !self.arena_cols[ai].is_empty();
        let has_cols = has_mirror || ts;
        if has_cols {
            let _ = writeln!(
                o,
                "/* {} */\ntypedef struct F20_ALIGN128 {{",
                self.c(
                    "列: この表を走査する集計が読む値だけを密に並べた写し（スロットが正本。書くのは arena_{名前}_mirror / _unmirror だけ）。どの列も 128 B 境界から",
                    "columns: dense mirrors of exactly the values the reductions over this table read (the slot stays authoritative; only arena_<name>_mirror / _unmirror write them); every column starts on 128 B"
                )
                .replace("{名前}", &n)
            );
            let cols: Vec<(String, ColKind, String, String, Vec<usize>)> = self.arena_cols[ai].iter().map(|c| (c.name.clone(), c.kind, c.val.clone(), c.cond.clone(), c.lines.clone())).collect();
            for (cn, kind, val, cond, lines) in &cols {
                let ls = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(", ");
                let what = match kind {
                    ColKind::Agg { .. } => {
                        if cond.is_empty() {
                            self.c(&format!("集計の列: {}（{} 行目の総和）", val, ls), &format!("aggregate column: {} (the sum on line {})", val, ls))
                        } else {
                            self.c(&format!("集計の列: {} ? {} : 0（{} 行目）", cond, val, ls), &format!("aggregate column: {} ? {} : 0 (line {})", cond, val, ls))
                        }
                    }
                    ColKind::Bits => self.c(&format!("ビット列: 生存 && {}（{} 行目の個数）", cond, ls), &format!("bit column: live && {} (the count on line {})", cond, ls)),
                    ColKind::FKey { ft, max } => {
                        let f = format!("{}F20_KEY_{}", if *max { "~" } else { "" }, ft.name().to_uppercase());
                        if cond.is_empty() {
                            self.c(&format!("浮動小数点の鍵の列: 生存 ? {}({}) : NONE（NaN も NONE、-0.0 は +0.0 の鍵。{} 行目の選択）", f, val, ls), &format!("float key column: live ? {}({}) : NONE (NaN is NONE too, -0.0 gets the key of +0.0; the pick on line {})", f, val, ls))
                        } else {
                            self.c(&format!("浮動小数点の鍵の列: 生存 && {} ? {}({}) : NONE（NaN も NONE。{} 行目の選択）", cond, f, val, ls), &format!("float key column: live && {} ? {}({}) : NONE (NaN is NONE too; the pick on line {})", cond, f, val, ls))
                        }
                    }
                    ColKind::Key { max, .. } => {
                        let f = if *max { "~enc" } else { "enc" };
                        if cond.is_empty() {
                            self.c(&format!("鍵の列: 生存 ? {}({}) : NONE（{} 行目の選択）", f, val, ls), &format!("key column: live ? {}({}) : NONE (the pick on line {})", f, val, ls))
                        } else {
                            self.c(&format!("鍵の列: 生存 && {} ? {}({}) : NONE（{} 行目の選択）", cond, f, val, ls), &format!("key column: live && {} ? {}({}) : NONE (the pick on line {})", cond, f, val, ls))
                        }
                    }
                };
                let decl = match kind {
                    ColKind::Agg { wide: true } => format!("uint64_t {}[F20_COL_LEN(ARENA_{}_K)]", cn, n),
                    ColKind::Agg { wide: false } => format!("uint32_t {}[F20_COL_LEN(ARENA_{}_K)]", cn, n),
                    ColKind::Bits => format!("uint64_t {}[F20_COL_WORDS(ARENA_{}_K)]", cn, n),
                    ColKind::Key { .. } | ColKind::FKey { .. } => format!("int64_t {}[F20_COL_LEN(ARENA_{}_K)]", cn, n),
                };
                let _ = writeln!(o, "    F20_ALIGN128 {}; /* {} */", decl, what);
            }
            o.push_str(&self.gen_tsoa_cols(ai));
            let _ = writeln!(o, "}} f20_cols_{}_t;", n);
        }
        let gpu = self.s.t.arenas[ai].gpu;
        let _ = writeln!(
            o,
            "{def}; /* {c} */\nstatic f20_arena_t({n}) arena_{n} {st}(f20_arena_t({n})); /* {pl} */\n_Static_assert(ARENA_{n}_CAP <= F20_SLOTS(ARENA_{n}_K), \"arena {n}: the capacity must fit in 2^K slots\");\n",
            st = if gpu { "F20_GPU_STORAGE" } else { "F20_ARENA_STORAGE" },
            n = n,
            def = if has_cols { format!("F20_DEFINE_TABLE_COLS({n}, ARENA_{n}_K, {gt}, f20_cols_{n}_t)", n = n, gt = self.gen_ty(k)) } else { format!("F20_DEFINE_TABLE({n}, ARENA_{n}_K, {gt})", n = n, gt = self.gen_ty(k)) },
            c = if has_cols {
                self.c(
                    &format!("管理情報（128 B を占有）| 生存ビットマップ live[{}] | 列 col | gen[2^K]（世代 {} ビットを {} に）| link[2^K] | slot[2^K + 1]（SoA・各配列は 128 B 境界）", (slots + 63) / 64, gbits, self.gen_ty(k)),
                    &format!("bookkeeping (a 128 B block of its own) | live bitmap live[{}] | columns col | gen[2^K] ({}-bit generations in {}) | link[2^K] | slot[2^K + 1] (SoA, every array on a 128 B boundary)", (slots + 63) / 64, gbits, self.gen_ty(k))
                )
            } else {
                self.c(
                    &format!("管理情報（128 B を占有）| 生存ビットマップ live[{}] | gen[2^K]（世代 {} ビットを {} に）| link[2^K] | slot[2^K + 1]（SoA・各配列は 128 B 境界）", (slots + 63) / 64, gbits, self.gen_ty(k)),
                    &format!("bookkeeping (a 128 B block of its own) | live bitmap live[{}] | gen[2^K] ({}-bit generations in {}) | link[2^K] | slot[2^K + 1] (SoA, every array on a 128 B boundary)", (slots + 63) / 64, gbits, self.gen_ty(k))
                )
            },
            pl = if gpu {
                self.c(
                    ".bss.fascon_gpu（ELF）。2 MiB 境界（PE / Mach-O は 4 KiB）: 装置と共有するピン留めできるページ（Unified Memory / Pinned Host Memory）",
                    ".bss.fascon_gpu (ELF); a 2 MiB boundary (4 KiB on PE / Mach-O): pinnable pages shared with the device (Unified Memory / Pinned Host Memory)",
                )
            } else {
                self.c(
                    ".bss.fascon_arena（ELF）。1 MiB 以上の表は 2 MiB 境界: ラージページ 1 枚で TLB 1 エントリ",
                    ".bss.fascon_arena (ELF); tables of 1 MiB or more on a 2 MiB boundary: one large page, one TLB entry",
                )
            }
        );
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED {et} *arena_{n}_get({id} id) {{\n    const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K), g = F20_ID_GEN(id, ARENA_{n}_K);\n    if (FASCON_LIKELY(arena_{n}.gen[i] == g && g != 0u)) return &arena_{n}.slot[i].v;\n    return NULL;\n}}\n",
            c = self.c(
                "ルックアップ（Option を返す。古い Id は NULL = None の腕）。呼び出し側がこの結果で分岐する（ソースに書いた if let / match）ので、\n * 比較は予測の効く分岐のまま: CPU は世代とスロットの読み出しを並行して進める。分岐を持たない検査は @a[id] 側（番兵 + ラッチ）",
                "lookup (returns an Option; a stale Id gives NULL = the None arm). The caller branches on the result (the if let / match in the source),\n * so the compare stays a predicted branch and the CPU fetches the generation and the slot in parallel; the branch-free check is the @a[id] one (sentinel + latch)"
            ),
            et = st,
            n = n,
            id = id
        );
        if nd {
            let _ = writeln!(
                o,
                "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED {et} *arena_{n}_at_strict({id} id, const char *site) {{\n    {et} *p = arena_{n}_get(id);\n    if (FASCON_UNLIKELY(p == NULL)) fascon_stale_id_panic(id, ARENA_{n}_K, \"{n}\", site);\n    return p;\n}}\n",
                c = self.c(
                    "生存を証明できない @a[id]（要素が own を持つ型）: 番兵へ逸らすと own の唯一性が崩れるので、ここだけはその場で止める",
                    "an unproven @a[id] whose element holds own: diverting to the sentinel would break own's uniqueness, so this one stops on the spot"
                ),
                et = st,
                n = n,
                id = id
            );
        } else {
            let _ = writeln!(
                o,
                "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED const {et} *arena_{n}_at_r({id} id, uint32_t *F20_RESTRICT flt) {{\n    const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K), g = F20_ID_GEN(id, ARENA_{n}_K);\n    *flt |= (uint32_t)(arena_{n}.gen[i] != g) | (uint32_t)(g == 0u); /* F20_FAULT_STALE */\n    return &arena_{n}.slot[i].v;\n}}\n",
                c = self.c(
                    "生存を証明できない @a[id] の読み出し: 世代の比較はラッチへ流すだけで、スロットの番地は比較を待たない（検査なしの読み出しと同じ速さ）。\n * 古い Id でも読むのは表の中の同じ型の値か 0（死んだスロット）なので、メモリは壊れない。止めるのは次のコミット点",
                    "a read of an unproven @a[id]: the generation compare only feeds the latch and the slot address does not wait for it (as fast as an unchecked read).\n * a stale Id still reads a value of the same type inside the table, or zero (a dead slot), so memory stays intact; the stop happens at the next commit point"
                ),
                et = st,
                n = n,
                id = id
            );
            let _ = writeln!(
                o,
                "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED {et} *arena_{n}_at({id} id, uint32_t *F20_RESTRICT flt) {{\n    const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K), g = F20_ID_GEN(id, ARENA_{n}_K);\n    const uint32_t bad = (uint32_t)(arena_{n}.gen[i] != g) | (uint32_t)(g == 0u);\n    *flt |= bad; /* F20_FAULT_STALE */\n    return &arena_{n}.slot[(i & (bad - 1u)) | (F20_SENTINEL(ARENA_{n}_K) & (0u - bad))].v;\n}}\n",
                c = self.c(
                    "生存を証明できない @a[id] への書き込み・&mut: 世代が違えば番兵スロットへ逸らしてラッチを立てる。分岐なし。本物のスロットは汚れず、止めるのは次のコミット点（外へ影響が出る前）",
                    "a write or &mut borrow of an unproven @a[id]: a wrong generation is diverted to the sentinel slot and latched, with no branch; no real slot is touched, and the stop happens at the next commit point (before any external effect)"
                ),
                et = st,
                n = n,
                id = id
            );
        }
        let _ = writeln!(
            o,
            "/* {c} */\n#define arena_{n}_slot(id) (&arena_{n}.slot[F20_ID_INDEX((id), ARENA_{n}_K)].v)\n",
            c = self.c(
                "生存がコンパイル時に証明された Id 用: マスク 1 回だけ（世代検査なし。どんな値でも表の外は指せない）",
                "for Ids proven live at compile time: one mask only (no generation check; no value can point outside the table)"
            ),
            n = n
        );
        let fail_drop = if nd { format!("    if (id == 0u) {{ {et} *p = ({et} *)v; {d} }}\n", et = et, d = drop) } else { String::new() };
        if has_mirror {
            o.push_str(&self.gen_mirror(ai, &st));
        }
        if ts {
            o.push_str(&self.gen_tsoa_fns(ai));
        }
        if has_cols {
            let keys: Vec<String> = self.arena_cols[ai].iter().filter(|c| matches!(c.kind, ColKind::Key { .. } | ColKind::FKey { .. })).map(|c| c.name.clone()).collect();
            let fresh = if keys.is_empty() {
                String::new()
            } else {
                format!(
                    "        if ((i & 63u) == 0u && arena_{n}.st.next_unused == i + 1u) {{ /* {c} */\n{fills}        }}\n",
                    n = n,
                    c = self.c("新しい 64 スロットの語を初めて使う: 鍵の列のその語を NONE で埋める（64 回の確保に 1 回）", "first use of a fresh 64-slot word: fill that word of every key column with NONE (once per 64 allocations)"),
                    fills = keys.iter().map(|kn| format!("            f20_col_key_fresh(arena_{}.col.{}, i);\n", n, kn)).collect::<String>()
                )
            };
            let _ = writeln!(
                o,
                "static inline FASCON_MAYBE_UNUSED {id} arena_{n}_alloc(const {et} *v) {{\n    const f20_id_t id = f20_arena_acquire(&arena_{n}.st, arena_{n}.gen, arena_{n}.link, arena_{n}.live, ARENA_{n}_CAP, ARENA_{n}_K);\n    if (id != 0u) {{\n        const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K);\n{put}    }}\n{fd}    return id; /* 0 = None ({full}) */\n}}\n",
                id = id,
                n = n,
                et = et,
                put = if ts {
                    format!("{fresh}        arena_{n}_put(i, v); /* {mc} */\n", fresh = fresh, n = n, mc = self.c("スロットに残る部分と完全列化の列（初期値は列へ）・写しの列", "the part in the slot and the True-SoA columns (initial values go to the columns), and the mirrored columns"))
                } else {
                    format!("        arena_{n}.slot[i].v = *v;\n{fresh}        arena_{n}_mirror(&arena_{n}.slot[i].v, i); /* {mc} */\n", fresh = fresh, n = n, mc = self.c("列の写し", "column mirrors"))
                },
                fd = fail_drop,
                full = self.c("満杯", "full")
            );
        } else {
            let _ = writeln!(
                o,
                "static inline FASCON_MAYBE_UNUSED {id} arena_{n}_alloc(const {et} *v) {{\n    const f20_id_t id = f20_arena_acquire(&arena_{n}.st, arena_{n}.gen, arena_{n}.link, arena_{n}.live, ARENA_{n}_CAP, ARENA_{n}_K);\n    if (id != 0u) arena_{n}.slot[F20_ID_INDEX(id, ARENA_{n}_K)].v = *v;\n{fd}    return id; /* 0 = None ({full}) */\n}}\n",
                id = id,
                n = n,
                et = et,
                fd = fail_drop,
                full = self.c("満杯", "full")
            );
        }
        let mut unmirror = if has_mirror { format!("    arena_{}_unmirror(F20_ID_INDEX(id, ARENA_{}_K)); /* {} */\n", n, n, self.c("列: 集計 0・ビット 0・鍵 NONE", "columns: aggregates 0, bits 0, keys NONE")) } else { String::new() };
        if ts {
            unmirror = format!("    arena_{n}_ts_zero(F20_ID_INDEX(id, ARENA_{n}_K)); /* {c} */\n{u}", n = n, c = self.c("完全列化の列も 0（死んだスロット）", "the True-SoA columns are 0 too (a dead slot)"), u = unmirror);
        }
        let _ = writeln!(
            o,
            "static inline FASCON_MAYBE_UNUSED bool arena_{n}_free({id} id) {{\n    {st} *p = arena_{n}_get(id);\n    if (p == NULL) return false;\n{d}    __builtin_memset(p, 0, sizeof *p); /* {z} */\n    f20_arena_release(&arena_{n}.st, arena_{n}.gen, arena_{n}.link, arena_{n}.live, F20_ID_INDEX(id, ARENA_{n}_K), ARENA_{n}_K); /* {c} */\n{um}    return true;\n}}\n",
            n = n,
            id = id,
            st = st,
            d = if nd { format!("    {} /* {} */\n", drop_slot, self.c("スロットが所有する own を解放", "drop the own regions the slot owns")) } else { String::new() },
            z = self.c("死んだスロットは 0（総和の特殊化が生存印を読まずに済む）", "dead slots are zero (lets the sum specialization skip the live marks)"),
            c = self.c("世代 +1 → 古い Id はすべて無効", "generation + 1 → every older Id becomes invalid"),
            um = unmirror
        );
        let _ = writeln!(
            o,
            "static inline FASCON_MAYBE_UNUSED bool arena_{n}_remove({id} id, {et} *out) {{\n    {st} *p = arena_{n}_get(id);\n    if (p == NULL) return false;\n    *out = {mv}; /* {c} */\n    __builtin_memset(p, 0, sizeof *p);\n    f20_arena_release(&arena_{n}.st, arena_{n}.gen, arena_{n}.link, arena_{n}.live, F20_ID_INDEX(id, ARENA_{n}_K), ARENA_{n}_K);\n{um}    return true;\n}}\n",
            n = n,
            id = id,
            et = et,
            st = st,
            mv = if ts { format!("arena_{}_load(F20_ID_INDEX(id, ARENA_{}_K))", n, n) } else { "*p".to_string() },
            c = self.c("所有権ごと取り出す（解放処理は呼ばない）", "move out with ownership (no drop)"),
            um = unmirror
        );
        o
    }

    /// 列の写しを作る関数 3 つ（_mirror: 値から 1 スロット分、_mirror_p: 要素ポインタから、_unmirror: 死んだスロット）
    /// the three mirror functions (_mirror: one slot from a value, _mirror_p: from an element pointer, _unmirror: a dead slot)
    /// 列ごとの「1 スロット分を作り直す」C の行と「死んだスロットにする」C の行 / per column, the C line that rebuilds
    /// one slot's entry and the one that marks it dead
    fn col_lines(&self, ai: usize) -> Vec<(String, String)> {
        let n = self.s.t.arenas[ai].name.clone();
        let mut out = Vec::new();
        for c in &self.arena_cols[ai] {
            let (cn, val, cond) = (&c.name, &c.val, &c.cond);
            let (set, clear) = match c.kind {
                ColKind::Agg { wide } => {
                    let ct = if wide { "uint64_t" } else { "uint32_t" };
                    let set = if cond.is_empty() {
                        format!("    arena_{n}.col.{cn}[i] = ({ct}){val};", n = n, cn = cn, ct = ct, val = val)
                    } else {
                        format!("    arena_{n}.col.{cn}[i] = ({cond}) ? ({ct}){val} : 0u;", n = n, cn = cn, ct = ct, val = val, cond = cond)
                    };
                    (set, format!("    arena_{}.col.{}[i] = 0u;", n, cn))
                }
                ColKind::Bits => (format!("    F20_BIT_PUT(arena_{n}.col.{cn}, i, {cond});", n = n, cn = cn, cond = cond), format!("    F20_BIT_PUT(arena_{}.col.{}, i, 0);", n, cn)),
                ColKind::FKey { ft, max } => {
                    let key = format!("f20_fkey_{}({}, {})", ft.name(), val, if max { 1 } else { 0 });
                    let set = if cond.is_empty() {
                        format!("    arena_{n}.col.{cn}[i] = {key};", n = n, cn = cn, key = key)
                    } else {
                        format!("    arena_{n}.col.{cn}[i] = ({cond}) ? {key} : F20_KEY_NONE;", n = n, cn = cn, key = key, cond = cond)
                    };
                    (set, format!("    arena_{}.col.{}[i] = F20_KEY_NONE;", n, cn))
                }
                ColKind::Key { it, max } => {
                    let key = self.key_enc(it, max, val);
                    let set = if cond.is_empty() {
                        format!("    arena_{n}.col.{cn}[i] = {key};", n = n, cn = cn, key = key)
                    } else {
                        format!("    arena_{n}.col.{cn}[i] = ({cond}) ? {key} : F20_KEY_NONE;", n = n, cn = cn, key = key, cond = cond)
                    };
                    (set, format!("    arena_{}.col.{}[i] = F20_KEY_NONE;", n, cn))
                }
            };
            out.push((set, clear));
        }
        out
    }

    /// 一部の列だけを作り直す関数（書き込みが届く列の組ごとに 1 つ。関数本体を出したあとで、使われた組だけ）
    /// rebuild functions for column subsets (one per set of columns some write can reach; emitted after the function
    /// bodies, only for the sets actually used)
    fn gen_mirror_masks(&mut self) -> String {
        let mut o = String::new();
        for ai in 0..self.s.t.arenas.len() {
            let masks: Vec<u64> = self.mirror_masks.get(ai).map(|m| m.iter().copied().collect()).unwrap_or_default();
            if masks.is_empty() {
                continue;
            }
            let n = self.s.t.arenas[ai].name.clone();
            let et = if self.tsoa.get(ai).map_or(false, |v| !v.is_empty()) { self.ts_part(ai) } else { self.cty(&self.s.t.arenas[ai].elem.clone()) };
            let lines = self.col_lines(ai);
            for m in masks {
                let names: Vec<String> = self.arena_cols[ai].iter().enumerate().filter(|(k, _)| *k < 64 && m & (1u64 << k) != 0).map(|(_, c)| c.name.clone()).collect();
                let body: String = lines.iter().enumerate().filter(|(k, _)| *k < 64 && m & (1u64 << *k) != 0).map(|(_, (set, _))| format!("{}\n", set)).collect();
                let _ = writeln!(
                    o,
                    "/* {} */\nstatic inline FASCON_MAYBE_UNUSED void arena_{n}_mirror_m{m:x}(const {et} *F20_RESTRICT v) {{\n    const uint32_t i = F20_COL_OF_SLOT((uint32_t)((const f20_slot_{n}_t *)(const void *)v - arena_{n}.slot), ARENA_{n}_K);\n{body}}}\n",
                    self.c(&format!("書き込みが届く列だけを作り直す: {}", names.join(", ")), &format!("rebuild only the columns the write can reach: {}", names.join(", "))),
                    n = n,
                    m = m,
                    et = et,
                    body = body
                );
            }
        }
        o
    }

    fn gen_mirror(&mut self, ai: usize, et: &str) -> String {
        let n = self.s.t.arenas[ai].name.clone();
        let mut set = String::new();
        let mut clear = String::new();
        for (st, cl) in self.col_lines(ai) {
            set.push_str(&st);
            set.push('\n');
            clear.push_str(&cl);
            clear.push('\n');
        }
        let mut o = String::new();
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED void arena_{n}_mirror(const {et} *F20_RESTRICT v, uint32_t i) {{\n{set}}}",
            c = self.c(
                "列の写しを 1 スロット分作り直す（alloc・列が読むフィールドへの Id での書き込み・&mut の借用の終わり）。条件は & でつないだ比較なので分岐なし（CMOV / CSEL）",
                "rebuild one slot's column entries (alloc, writes by Id to a field a column reads, the end of a &mut borrow); filters are comparisons joined with &, so no branch (CMOV / CSEL)"
            ),
            n = n,
            et = et,
            set = set
        );
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED void arena_{n}_mirror_p(const {et} *v) {{\n    const uint32_t i = (uint32_t)((const f20_slot_{n}_t *)(const void *)v - arena_{n}.slot);\n    arena_{n}_mirror(v, F20_COL_OF_SLOT(i, ARENA_{n}_K));\n}}",
            c = self.c(
                "要素ポインタから: 添字はポインタの差（v はスロットの先頭メンバ）。番兵へ逸れた書き込みの写しは列の番兵へ（走査は読まない）",
                "from an element pointer: the index is the pointer difference (v is the slot's first member); the mirror of a write diverted to the sentinel goes to the column sentinel (never scanned)"
            ),
            n = n,
            et = et
        );
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED void arena_{n}_unmirror(uint32_t i) {{\n{clear}}}\n",
            c = self.c("死んだスロットの列（free / remove）: 集計 0・ビット 0・鍵 NONE", "the columns of a dead slot (free / remove): aggregates 0, bits 0, keys NONE"),
            n = n,
            clear = clear
        );
        o
    }

    // ───────────── MMIO ─────────────

    fn gen_mmio(&mut self, mi: usize) -> String {
        let m = self.s.t.mmios[mi].clone();
        let n = m.name.clone();
        let et = m.elem.c_name();
        let mut o = String::new();
        let shape = m.dims.iter().rev().fold(m.elem.name().to_string(), |acc, d| format!("[{}; {}]", acc, d));
        let kind = if m.framebuffer {
            self.c("通常メモリ（フレームバッファ: 書き込みの結合・ベクトル化を許す）", "normal memory (framebuffer: write combining and vectorization allowed)")
        } else {
            self.c("デバイスメモリ（volatile + I/O バリア）", "device memory (volatile + I/O barriers)")
        };
        let _ = writeln!(
            o,
            "/* ───── mmio {n}: {shape} at {addr:#x} — {kind}, {size} bytes ───── */\n#if defined(FASCON2_HOSTED_MMIO)\n#define MMIO_{N}_BASE (fascon2_host_mmio_base[{mi}]) /* {hosted} */\n#else\n#define MMIO_{N}_BASE {va}\n#endif",
            n = n,
            shape = shape,
            addr = m.addr,
            va = if self.o.profile == crate::target::Profile::GenericOs { format!("F20_MMIO_VA({:#x}u) /* {} */", m.addr, self.c("generic-os: 高位の仮想番地の写し（仮想 0〜2 MiB は守りページ）", "generic-os: the high virtual mirror (virtual 0-2 MiB is a guard region)")) } else { format!("((uintptr_t){:#x}u)", m.addr) },
            kind = kind,
            size = m.size_bytes(),
            N = n.to_uppercase(),
            mi = mi,
            hosted = self.c("PC 実行用: プラットフォーム層が確保した番地", "PC run: address chosen by the platform layer")
        );
        let total = m.total_elems();
        if m.framebuffer {
            if m.dims.len() == 2 {
                let (h, w) = (m.dims[0], m.dims[1]);
                let _ = writeln!(o, "#define MMIO_{N}_W {w}u\n#define MMIO_{N}_H {h}u", N = n.to_uppercase(), w = w, h = h);
                let _ = writeln!(o, "static inline FASCON_MAYBE_UNUSED {et} *mmio_{n}_ptr(uintptr_t y, uintptr_t x) {{ return ({et} *)MMIO_{N}_BASE + y * {w}u + x; }}", et = et, n = n, N = n.to_uppercase(), w = w);
            } else {
                let _ = writeln!(o, "static inline FASCON_MAYBE_UNUSED {et} *mmio_{n}_ptr(uintptr_t i) {{ return ({et} *)MMIO_{N}_BASE + i; }}", et = et, n = n, N = n.to_uppercase());
            }
        } else {
            let eb = m.elem_bytes();
            let bits = m.elem.bits(64);
            if m.dims.len() == 2 {
                let w = m.dims[1];
                let _ = writeln!(o, "static inline FASCON_MAYBE_UNUSED uintptr_t mmio_{n}_addr(uintptr_t y, uintptr_t x) {{ return MMIO_{N}_BASE + (y * {w}u + x) * {eb}u; }}", n = n, N = n.to_uppercase(), w = w, eb = eb);
            } else {
                let _ = writeln!(o, "static inline FASCON_MAYBE_UNUSED uintptr_t mmio_{n}_addr(uintptr_t i) {{ return MMIO_{N}_BASE + i * {eb}u; }}", n = n, N = n.to_uppercase(), eb = eb);
            }
            let _ = writeln!(o, "/* {} */", self.c(&format!("読み書きは fascon_mmio_read{b} / write{b}（Linux の readl / writel と同じ順序保証）", b = bits), &format!("accesses go through fascon_mmio_read{b} / write{b} (same ordering as Linux readl / writel)", b = bits)));
        }
        let _ = writeln!(o, "#define MMIO_{N}_LEN {t}u\n", N = n.to_uppercase(), t = total);
        o
    }

    // ───────────── バンプアリーナ / bump arenas ─────────────

    /// バンプアリーナ: 静的な表（.bss）+ 先頭 top。確保は top を 1 進めるだけ、解放は呼び出しリージョンの出口で top を印へ
    /// 戻す 1 回の格納。Id は 64 ビット [世代 64-K | 添字 K]: スロットを確保するたびにそのスロットの世代を +1 し、
    /// 検査は「添字 < top かつ 世代が一致」。巻き戻した後の古い Id は top の外か世代違いなので、他人のデータには届かない
    /// a bump arena: a static table (.bss) + a top. Allocation advances top by one; release is one store that resets top to
    /// the call region's mark at its exit. Ids are 64 bits [generation 64-K | index K]: allocating a slot bumps that slot's
    /// generation, and the check is "index < top and the generation matches"; after a rewind an old Id is beyond top or of
    /// another generation, so it never reaches someone else's data
    fn gen_bump(&mut self, ai: usize) -> String {
        let a = self.s.t.arenas[ai].clone();
        let n = a.name.clone();
        let cap = a.cap;
        let k = arena_k(cap);
        let et = self.cty(&a.elem);
        let id = self.id_name(ai);
        let mut o = String::new();
        let _ = writeln!(
            o,
            "/* ───── bump_arena {n}: {t}[{cap}] — {d} ───── */",
            n = n,
            t = self.s.t.display(&a.elem),
            cap = cap,
            d = self.c("呼び出しリージョンごとの O(1) 一括解放（静的・.bss）", "O(1) bulk release per call region (static, .bss)")
        );
        let _ = writeln!(
            o,
            "/* {} */",
            self.c(
                &format!("K = {k}: Id = 64 ビット [世代 {g} ビット | 添字 {k} ビット]。確保 = top を 1 進める（満杯なら None）。解放 = 呼び出しリージョンの出口で `top = 印` の 1 回の格納。\n * スロットを確保するたびに世代 +1（1 スロット 2^{g} - 1 回まで。使い切ったら確保は None = 一周しない）", k = k, g = 64 - k),
                &format!("K = {k}: Id = 64 bits [generation {g} bits | index {k} bits]. alloc = advance top by one (None when full). release = one store `top = mark` at the exit of the call region.\n * every allocation of a slot bumps its generation (up to 2^{g} - 1 per slot; after that alloc returns None, generations never wrap)", k = k, g = 64 - k)
            )
        );
        let _ = writeln!(o, "#define ARENA_{n}_K {k}u\n#define ARENA_{n}_CAP {cap}u", n = n, k = k, cap = cap);
        let _ = writeln!(o, "typedef struct F20_ALIGN16 {{\n    {decl};\n}} f20_slot_{n}_t;", decl = self.cdecl(&a.elem, "v"), n = n);
        let _ = writeln!(
            o,
            "F20_DEFINE_BUMP({n}, ARENA_{n}_K); /* {c} */\nstatic f20_arena_t({n}) arena_{n} F20_ARENA_STORAGE(f20_arena_t({n}));\n_Static_assert(ARENA_{n}_CAP <= F20_SLOTS(ARENA_{n}_K), \"bump_arena {n}: the capacity must fit in 2^K slots\");\n",
            n = n,
            c = self.c("管理情報（top・最大の深さ、128 B を占有）| gen[2^K]（64 ビット）| slot[2^K + 1]（番兵込み）", "bookkeeping (top, peak; a 128 B block of its own) | gen[2^K] (64-bit) | slot[2^K + 1] (with the sentinel)")
        );
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED {et} *arena_{n}_get({id} id) {{\n    const uint32_t i = F20_BID_INDEX(id, ARENA_{n}_K);\n    const uint64_t g = F20_BID_GEN(id, ARENA_{n}_K);\n    if (FASCON_LIKELY(i < arena_{n}.st.top && arena_{n}.gen[i] == g && g != 0u)) return &arena_{n}.slot[i].v;\n    return NULL;\n}}\n",
            c = self.c("ルックアップ（古い Id = 巻き戻された・別の世代 → NULL = None の腕）", "lookup (a stale Id = rewound or of another generation → NULL = the None arm)"),
            et = et,
            n = n,
            id = id
        );
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED const {et} *arena_{n}_at_r({id} id, uint32_t *F20_RESTRICT flt) {{\n    const uint32_t i = F20_BID_INDEX(id, ARENA_{n}_K);\n    const uint64_t g = F20_BID_GEN(id, ARENA_{n}_K);\n    *flt |= (uint32_t)(i >= arena_{n}.st.top) | (uint32_t)(arena_{n}.gen[i] != g) | (uint32_t)(g == 0u); /* F20_FAULT_STALE */\n    return &arena_{n}.slot[i].v;\n}}\n",
            c = self.c("生存を証明できない読み出し: 比較はラッチへ流すだけ（分岐なし）", "an unproven read: the compares only feed the latch (no branch)"),
            et = et,
            n = n,
            id = id
        );
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED {et} *arena_{n}_at({id} id, uint32_t *F20_RESTRICT flt) {{\n    const uint32_t i = F20_BID_INDEX(id, ARENA_{n}_K);\n    const uint64_t g = F20_BID_GEN(id, ARENA_{n}_K);\n    const uint32_t bad = (uint32_t)(i >= arena_{n}.st.top) | (uint32_t)(arena_{n}.gen[i] != g) | (uint32_t)(g == 0u);\n    *flt |= bad; /* F20_FAULT_STALE */\n    return &arena_{n}.slot[(i & (bad - 1u)) | (F20_SENTINEL(ARENA_{n}_K) & (0u - bad))].v;\n}}\n",
            c = self.c("生存を証明できない書き込み: 古い Id は番兵スロットへ逸らしてラッチ（分岐なし）", "an unproven write: a stale Id is diverted to the sentinel slot and latched (no branch)"),
            et = et,
            n = n,
            id = id
        );
        let _ = writeln!(
            o,
            "/* {c} */\n#define arena_{n}_slot(id) (&arena_{n}.slot[F20_BID_INDEX((id), ARENA_{n}_K)].v)\n",
            c = self.c("生存が証明された Id（この呼び出しリージョンで確保したもの）: マスクだけ", "an Id proven live (allocated in this call region): a mask only"),
            n = n
        );
        let _ = writeln!(
            o,
            "/* {c} */\nstatic inline FASCON_MAYBE_UNUSED {id} arena_{n}_alloc(const {et} *v) {{\n    const uint32_t i = arena_{n}.st.top;\n    if (FASCON_UNLIKELY(i >= ARENA_{n}_CAP)) return 0u; /* {full} */\n    const uint64_t g = arena_{n}.gen[i] + 1u;\n    if (FASCON_UNLIKELY(g > F20_BGEN_MAX(ARENA_{n}_K))) return 0u; /* {ret} */\n    arena_{n}.gen[i] = g;\n    arena_{n}.st.top = i + 1u;\n    arena_{n}.st.peak = arena_{n}.st.peak > i + 1u ? arena_{n}.st.peak : i + 1u;\n    arena_{n}.st.allocs++;\n    arena_{n}.slot[i].v = *v;\n    return F20_BID_MAKE(g, i, ARENA_{n}_K);\n}}\n",
            c = self.c("確保: top を 1 進めるだけ（比較 1 回 + 格納）。解放は呼び出しリージョンの出口でまとめて", "alloc: just advance top (one compare + stores); release happens all at once at the call region's exit"),
            id = id,
            n = n,
            et = et,
            full = self.c("満杯 = None", "full = None"),
            ret = self.c("このスロットの世代を使い切った = None（世代は一周しない）", "this slot's generations are used up = None (generations never wrap)")
        );
        o
    }

    // ───────────── I/O ポート / I/O ports ─────────────

    /// ポートの読み書きの関数。x86_64 は in / out 命令そのもの、AArch64 はコンパイラが決めた MMIO（PL011 など）、PC 実行は
    /// プラットフォーム層（COM1 の出力は標準出力へ）。`--target` を付けると、そのターゲットの形だけを出す
    /// read / write functions of a port: x86_64 uses the in / out instructions themselves, AArch64 the MMIO the compiler picked
    /// (PL011 ...), PC runs the platform layer (COM1 output goes to stdout). With `--target` only that target's form is emitted
    fn gen_port(&mut self, pi: usize) -> String {
        use crate::target::{aarch64_map, describe, A64Map, Target};
        let p = self.s.t.ports[pi].clone();
        let n = p.name.clone();
        let bits = p.elem.bits(64);
        let wb = p.width_bytes();
        let ct = p.elem.c_name();
        let shape = match p.len {
            Some(l) => format!("[{}; {}]", p.elem.name(), l),
            None => p.elem.name().to_string(),
        };
        let (sfx, reg) = match bits {
            8 => ("b", "al"),
            16 => ("w", "ax"),
            _ => ("l", "eax"),
        };
        let mut o = String::new();
        let _ = writeln!(
            o,
            "/* ───── port_io {n}: {shape} at_port {port:#x} — {d} ───── */",
            n = n,
            shape = shape,
            port = p.port,
            d = self.c("x86 の I/O ポート空間（in / out 命令）。AArch64 ではコンパイラが MMIO に写す", "the x86 I/O port space (in / out instructions); remapped to MMIO by the compiler on AArch64")
        );
        let count = p.len.unwrap_or(1);
        // AArch64 での写し方（レジスタごと） / the AArch64 mapping (per register)
        let maps: Vec<A64Map> = (0..count).map(|i| aarch64_map(p.port + i * wb, bits, p.port, p.mmio_map)).collect();
        let base_mmio = match maps[0] {
            A64Map::Mmio { addr, .. } => Some(addr),
            _ => None,
        };
        let uniform_mmio = base_mmio.map_or(false, |a0| maps.iter().enumerate().all(|(i, m)| *m == A64Map::Mmio { addr: a0 + i as u64 * wb, bits }));
        let (want_x86, want_a64) = match self.o.target {
            Target::Portable => (true, true),
            Target::X86_64 => (true, false),
            Target::Aarch64 => (false, true),
        };
        let addr = format!("{:#x}u + (uint32_t)i * {}u", p.port, wb);
        let err = format!("#else\n#error \"port_io {}: this target has no I/O port mapping (build with --target x86_64 or aarch64)\"\n#endif", n);
        // 読み出し / reads
        let _ = writeln!(o, "static inline FASCON_MAYBE_UNUSED {ct} pio_{n}_in(uintptr_t i) {{", ct = ct, n = n);
        let _ = writeln!(
            o,
            "#if defined(FASCON2_HOSTED_MMIO)\n    return ({ct})fascon2_host_pio_in({addr}, {bits}u); /* {c} */",
            ct = ct,
            addr = addr,
            bits = bits,
            c = self.c("PC 実行: プラットフォーム層が真似る", "PC run: emulated by the platform layer")
        );
        if want_x86 {
            let _ = writeln!(o, "#elif defined(__x86_64__) || defined(__i386__)\n    return fascon_in{sfx}((uint16_t)({addr})); /* in {reg}, dx */", sfx = sfx, addr = addr, reg = reg);
        }
        if want_a64 {
            let _ = writeln!(o, "#elif defined(__aarch64__)");
            if uniform_mmio {
                let a0 = base_mmio.unwrap_or(0);
                let _ = writeln!(o, "    return fascon_mmio_read{bits}({a0:#x}u + (uintptr_t)i * {wb}u); /* {d} */", bits = bits, a0 = a0, wb = wb, d = describe(&maps[0]));
            } else {
                let _ = writeln!(o, "    switch (i) {{");
                for (i, m) in maps.iter().enumerate() {
                    let v = match m {
                        A64Map::Mmio { addr, bits: b } => format!("({})fascon_mmio_read{}({:#x}u)", ct, b, addr),
                        A64Map::Com1(off) => format!("({})f20_com1_in({}u)", ct, off),
                        A64Map::Const(v) => format!("({}){:#x}u", ct, v),
                        A64Map::DebugExit => format!("({})0u", ct),
                    };
                    let _ = writeln!(o, "    case {}u: return {}; /* {} */", i, v, describe(m));
                }
                let _ = writeln!(o, "    default: return ({})0u;\n    }}", ct);
            }
        }
        let _ = writeln!(o, "{}\n}}", err);
        // 書き込み / writes
        let _ = writeln!(o, "static inline FASCON_MAYBE_UNUSED void pio_{n}_out(uintptr_t i, {ct} v) {{", n = n, ct = ct);
        let _ = writeln!(o, "#if defined(FASCON2_HOSTED_MMIO)\n    fascon2_host_pio_out({addr}, {bits}u, (uint32_t)v);", addr = addr, bits = bits);
        if want_x86 {
            let _ = writeln!(o, "#elif defined(__x86_64__) || defined(__i386__)\n    fascon_out{sfx}((uint16_t)({addr}), v); /* out dx, {reg} */", sfx = sfx, addr = addr, reg = reg);
        }
        if want_a64 {
            let _ = writeln!(o, "#elif defined(__aarch64__)");
            if uniform_mmio {
                let a0 = base_mmio.unwrap_or(0);
                let _ = writeln!(o, "    fascon_mmio_write{bits}({a0:#x}u + (uintptr_t)i * {wb}u, v); /* {d} */", bits = bits, a0 = a0, wb = wb, d = describe(&maps[0]));
            } else {
                let _ = writeln!(o, "    switch (i) {{");
                for (i, m) in maps.iter().enumerate() {
                    let w = match m {
                        A64Map::Mmio { addr, bits: b } => format!("fascon_mmio_write{}({:#x}u, (uint{}_t)v)", b, addr, b),
                        A64Map::Com1(off) => format!("f20_com1_out({}u, (uint32_t)v)", off),
                        A64Map::Const(_) => "(void)v".to_string(),
                        A64Map::DebugExit => "fascon_plat_exit((int)(((uint32_t)v << 1) | 1u))".to_string(),
                    };
                    let _ = writeln!(o, "    case {}u: {}; break; /* {} */", i, w, describe(m));
                }
                let _ = writeln!(o, "    default: (void)v; break;\n    }}");
            }
        }
        let _ = writeln!(o, "{}\n}}\n", err);
        o
    }

    // ───────────── static の定数初期化子 / constant initializers of statics ─────────────

    fn const_init(&mut self, e: &Expr, t: &Ty) -> String {
        match (&e.kind, t) {
            (ExprKind::Str(s), _) => format!("{{ {}, {}u }}", c_escape(s), s.len()),
            (ExprKind::StructLit { fields, .. }, Ty::Struct(sid)) => {
                let info = self.s.t.structs[*sid].clone();
                let mut parts = Vec::new();
                for f in &info.fields {
                    if let Some((_, _, v)) = fields.iter().find(|(n, _, _)| *n == f.name) {
                        parts.push(format!(".{} = {}", f.name, self.const_init(v, &f.ty)));
                    }
                }
                format!("{{ {} }}", parts.join(", "))
            }
            (ExprKind::ArrayLit(v), Ty::Array(et, _)) => {
                let et = (**et).clone();
                let parts: Vec<String> = v.iter().map(|x| self.const_init(x, &et)).collect();
                // 長い表は 1 行 16 個ずつ折り返す / wrap long tables at 16 entries per line
                if parts.len() > 16 {
                    let rows: Vec<String> = parts.chunks(16).map(|c| format!("    {}", c.join(", "))).collect();
                    format!("{{\n{}\n}}", rows.join(",\n"))
                } else {
                    format!("{{ {} }}", parts.join(", "))
                }
            }
            (ExprKind::ArrayRepeat(v, _), Ty::Array(et, n)) => {
                if self.is_zero_like(v) {
                    "{ 0 }".to_string()
                } else {
                    let one = self.const_init(v, &(**et).clone());
                    format!("{{ {} }}", vec![one; *n as usize].join(", "))
                }
            }
            (ExprKind::Path(p), Ty::Option(i)) if p == "None" => {
                if Self::opt_niche(i) {
                    match &**i {
                        Ty::Str => "{ NULL, 0u }".to_string(),
                        Ty::Id(_) | Ty::GpuBuf(_) => "0u".to_string(),
                        _ => "NULL".to_string(),
                    }
                } else {
                    "{ .is_some = false }".to_string()
                }
            }
            (ExprKind::Call { callee, args, .. }, Ty::Option(i)) if callee == "Some" && args.len() == 1 => {
                let inner = self.const_init(&args[0], &(**i).clone());
                if Self::opt_niche(i) {
                    inner
                } else {
                    format!("{{ .is_some = true, .value = {} }}", inner)
                }
            }
            (ExprKind::Bool(b), _) => if *b { "true".to_string() } else { "false".to_string() },
            // 関数ポインタの表（`static OPS: [fn(i32) -> i32; 2] = [inc, dbl];`）: C の関数の番地 / tables of function
            // pointers: the C functions' addresses
            (ExprKind::Path(_), Ty::Fn(..)) => match self.s.res.get(&e.id) {
                Some(Res::Fn(fi)) => {
                    let n = self.fn_c_names.get(fi).cloned().unwrap_or_else(|| "NULL".to_string());
                    // generic-os: 関数ポインタは値になった時点で PACIA 署名（修飾子は関数の型から）。呼ぶ前に AUTIA で検証
                    // generic-os: a function pointer is signed with PACIA when it becomes a value (modifier from its type)
                    if self.o.profile == crate::target::Profile::GenericOs && !self.in_static_init {
                        self.report.pac_signs += 1;
                        let ct = self.cty(&t);
                        format!("(({})f20_pac_sign((void *){}, {}))", ct, n, pac_modifier(&self.mangle(&t)))
                    } else {
                        n
                    }
                }
                _ => "NULL".to_string(),
            },
            _ => match t {
                Ty::Int(it) => {
                    let val = const_eval_simple(self.s, e).unwrap_or(0);
                    self.int_lit_c(val, *it, Some(e.span))
                }
                // 浮動小数点の定数式は C の定数式のまま（C コンパイラが同じ IEEE-754 の規則で畳む）
                // a float constant expression stays a C constant expression (the C compiler folds it by the same IEEE-754 rules)
                Ty::Float(_) => self.float_const_expr(e),
                _ => "{ 0 }".to_string(),
            },
        }
    }

    /// static の初期値の浮動小数点の定数式（リテラル・f32::NAN など・単項 -・四則・as） / a float constant expression in a
    /// static initializer (literals, f32::NAN and the like, unary -, arithmetic, as)
    fn float_const_expr(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Float(text, _) => float_lit_c(text, &self.ty(e)),
            ExprKind::Int(v, _) => {
                let val = if self.s.neg_lits.contains(&e.id) { -(*v as i128) } else { *v as i128 };
                format!("{}", val)
            }
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::FloatConst(ft, c)) => float_const_c(*ft, c),
                _ => format!("{}", const_eval_simple(self.s, e).unwrap_or(0)),
            },
            ExprKind::Unary(UnOp::Neg, x) => {
                if let ExprKind::Int(..) = x.kind {
                    self.float_const_expr(x)
                } else {
                    format!("(-{})", self.float_const_expr(x))
                }
            }
            ExprKind::Binary(op, a, b) => format!("({} {} {})", self.float_const_expr(a), op.text(), self.float_const_expr(b)),
            ExprKind::Cast(x, _) => match self.ty(e) {
                Ty::Float(ft) => format!("(({}){})", ft.c_name(), self.float_const_expr(x)),
                _ => self.float_const_expr(x),
            },
            _ => "0".to_string(),
        }
    }

    fn is_zero_like(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Int(0, _) => true,
            ExprKind::Bool(false) => true,
            ExprKind::Path(p) => p == "None" && matches!(self.s.res.get(&e.id), Some(Res::NoneLit)),
            _ => false,
        }
    }

    fn int_lit_c(&self, v: i128, it: IntTy, sp: Option<Span>) -> String {
        let hex = sp.map(|s| self.src.snippet(s).starts_with("0x") || self.src.snippet(s).starts_with("0X")).unwrap_or(false);
        let body = if hex && v >= 0 { format!("{:#x}", v) } else { format!("{}", v) };
        match it {
            IntTy::I32 => {
                if v == i32::MIN as i128 {
                    "INT32_MIN".to_string()
                } else {
                    body
                }
            }
            IntTy::U32 => format!("{}u", body),
            IntTy::I64 => {
                if v == i64::MIN as i128 {
                    "INT64_MIN".to_string()
                } else {
                    format!("INT64_C({})", body)
                }
            }
            IntTy::U64 => format!("UINT64_C({})", body),
            // 32 ビットに収まる小さな型は素のリテラル（C の暗黙変換で正しく広がる） / small types use plain literals
            IntTy::I8 | IntTy::I16 => body,
            IntTy::U8 | IntTy::U16 => format!("{}u", body),
            IntTy::Isize => {
                if v >= i32::MIN as i128 && v <= i32::MAX as i128 {
                    body
                } else {
                    format!("(intptr_t)INT64_C({})", body)
                }
            }
            IntTy::Usize => {
                if v >= 0 && v <= u32::MAX as i128 {
                    format!("{}u", body)
                } else {
                    format!("(uintptr_t)UINT64_C({})", body)
                }
            }
        }
    }

    // ───────────── 関数本体 / function bodies ─────────────

    fn gen_fn(&mut self, fi: usize, fd: &FnDef) -> String {
        // 1 回目: ラッチ（遅延フォールト）を使うかだけを調べる。出力・集計は捨てる
        // first pass: only find out whether the function uses the latch (deferred faults); output and counts are discarded
        let saved_report = self.report.clone();
        let saved_reduce = (self.reduce_out.clone(), self.reduce_n);
        self.flt_known = None;
        self.gc_known = None;
        let _ = self.gen_fn_pass(fi, fd);
        let used = self.flt_used;
        let gc_used = self.gc_used;
        self.report = saved_report;
        self.reduce_out = saved_reduce.0;
        self.reduce_n = saved_reduce.1;
        // 2 回目: 本番。ラッチを使わない関数にはコミット点も `_flt` も出ない
        // second pass: the real one; a function that does not use the latch gets no commit points and no `_flt`
        self.flt_known = Some(used);
        self.gc_known = Some(gc_used);
        self.gen_fn_pass(fi, fd)
    }

    /// ラッチを使う関数を生成中か（1 回目は「使う」とみなす） / whether the latch is in use (assumed in the first pass)
    fn flt_on(&self) -> bool {
        self.flt_known.unwrap_or(true)
    }

    /// コミット点: ラッチが立っていればここで止める / a commit point: stop here if the latch is set
    fn commit(&mut self, site: &str) {
        if self.flt_on() {
            // catch_fault の中: 止めずに区間の後始末へ局所的に飛ぶ（区間の中で宣言した値を解放してから goto）
            // inside catch_fault: no stop; a local jump to the section's handler (dropping the values declared inside first)
            if let Some((label, _code, depth)) = self.catch_stack.last().cloned() {
                self.emit("if (FASCON_UNLIKELY(_flt != 0u)) {");
                self.ind += 1;
                self.emit_drops_from(depth);
                self.emit(format!("goto {};", label));
                self.ind -= 1;
                self.emit("}");
                return;
            }
            // 直前の行がすでにコミット点なら重ねない（間でラッチが立つことはない）
            // skip when the previous line is already a commit point (nothing can latch in between)
            if self.lines.last().map_or(false, |l| l.trim_start().starts_with("F20_COMMIT(_flt,")) {
                return;
            }
            // catch_fault から呼ばれうる関数: 止めずにラッチを保留へ移して戻る（呼び出し元が自分のラッチに取り込む。大域脱出なし）
            // a function catch_fault may call: move the latch to the pending word and return (the caller takes it into its
            // own latch; no non-local exit)
            if self.propagate_fns.contains(&self.cur_fn) {
                let ret = self.ret_ty.clone();
                self.emit("if (FASCON_UNLIKELY(_flt != 0u)) {");
                self.ind += 1;
                self.emit_drops_from(0);
                self.emit_rewinds();
                let c = self.c("ラッチを呼び出し元へ渡して戻る", "hand the latch to the caller and return");
                if matches!(ret, Ty::Unit | Ty::Never) {
                    self.emit(format!("fascon__flt_pending |= _flt; return; /* {} */", c));
                } else {
                    let z = self.zero_value(&ret);
                    let rc = self.cdecl(&ret, "_z");
                    self.emit(format!("fascon__flt_pending |= _flt; {{ {} = {}; return _z; }} /* {} */", rc, z, c));
                }
                self.ind -= 1;
                self.emit("}");
                return;
            }
            // #[syscall(N)] の本体: ラッチの異常はカーネルを止めず、errno で EL0 へ返す（信頼できない値で止まらない）
            // the body of a #[syscall(N)]: a latched fault does not stop the kernel but returns an errno to EL0
            if let Some(rc) = self.syscall_v2_ret.clone() {
                self.emit("if (FASCON_UNLIKELY(_flt != 0u)) {");
                self.ind += 1;
                self.emit_drops_from(0);
                self.emit_rewinds();
                self.emit(format!("return ({}){{ .is_some = false, .err = f20_fault_errno(_flt) }}; /* {} */", rc, self.c("ラッチの異常 → Err(errno)", "a latched fault → Err(errno)")));
                self.ind -= 1;
                self.emit("}");
                return;
            }
            self.emit(format!("F20_COMMIT(_flt, {});", site));
        }
    }

    /// コミット点より前に評価を済ませる: ラッチ付きアクセスを含む値を一時変数へ出す
    /// finish evaluation before a commit point: hoist a value that contains a latched access into a temporary
    fn effect_arg(&mut self, before: u32, t: &Ty, v: String) -> String {
        if self.flt_on() && self.at_count > before {
            self.hoist_any(t, &v)
        } else {
            v
        }
    }

    /// 型をそのまま C の型にして一時変数へ（配列は先頭要素へのポインタ） / hoist with the plain C type (arrays as element pointers)
    fn hoist_any(&mut self, t: &Ty, v: &str) -> String {
        if is_simple_c(v) || matches!(t, Ty::Unit | Ty::Never) || v == "(void)0" {
            return v.to_string();
        }
        let n = self.new_tmp("t");
        let ct = self.cty(t);
        let d = if ct.ends_with('*') { format!("{}{}", ct, n) } else { format!("{} {}", ct, n) };
        self.emit(format!("{} = {};", d, v));
        n
    }

    /// ユーザー側のシステムコール: 引数をレジスタに並べて svc / a system call on the user side: arguments into registers, then svc
    /// コアごとの実体とロックを使う C を出すか: spawn_on を使い、既定の起動（AArch64 の -kernel・PC）のとき。仮想 BIOS の
    /// ターゲットは 1 コアなので、1 コアの C のまま（spawn_on は実行時に止まる）
    /// whether the C uses per-core instances and locks: the program uses spawn_on and the boot path is the default one
    /// (AArch64 -kernel, or the PC). The virtual-BIOS targets run one core, so their C stays single-core (spawn_on stops
    /// at run time)
    /// マルチコアの C を出すか: spawn_on を使い、コアを起こせるターゲット（アーキテクチャを決めない C と、INIT-SIPI-SIPI で
    /// コアを起こす x86_64 の仮想 BIOS）。AArch64 の仮想 BIOS（-bios）には PSCI が無いので 1 コアのまま
    /// whether multi-core C is emitted: spawn_on is used and the target can start cores (architecture-neutral C, and the
    /// x86_64 virtual BIOS, which starts them with INIT-SIPI-SIPI); the AArch64 virtual BIOS (-bios) has no PSCI and stays
    /// single-core
    fn smp_on(&self) -> bool {
        self.s.smp && self.o.target != crate::target::Target::Aarch64
    }

    fn gen_svc_stub(&mut self, fi: usize, fd: &FnDef) -> String {
        let f = self.s.t.fns[fi].clone();
        let sig = self.fn_sig(fi);
        let mut regs: Vec<String> = Vec::new();
        for p in &f.params {
            let n = sanitize(&p.name);
            match &p.ty {
                Ty::Ref(_, inner) if matches!(**inner, Ty::Slice(_)) => {
                    regs.push(format!("(uint64_t)(uintptr_t){}->p", n));
                    regs.push(format!("(uint64_t){}->n", n));
                }
                Ty::Ref(..) => regs.push(format!("(uint64_t)(uintptr_t){}", n)),
                _ => regs.push(format!("(uint64_t){}", n)),
            }
        }
        regs.truncate(8);
        while regs.len() < 8 {
            regs.push("0u".to_string());
        }
        format!(
            "/* {} {}: syscall {} at {} — {} */\nstatic {} {{\n    return fascon_user_svc({}u, {});\n}}\n\n",
            self.c("line", "line"),
            self.line_of(fd.span),
            fd.name,
            f.syscall.unwrap_or(0),
            self.c("番号を x8、引数を x0〜x7 に入れて svc。戻り値は x0", "the number in x8, arguments in x0 to x7, then svc; the result comes back in x0"),
            sig,
            f.syscall.unwrap_or(0),
            regs.join(", ")
        )
    }

    /// EL0 との境界: システムコールの入口（引数の検証）と表、ユーザー側の出力・終了のフック
    /// the EL0 boundary: system call entries (argument validation) and their table; console and exit hooks on the user side
    fn gen_boundary(&mut self, out: &mut String) {
        let fns = self.s.t.fns.clone();
        if self.s.user_mode {
            // print 系は ABI の `write(fd: u32, buf: &[u8])` の fd 1 へ、main の終わりは `exit(code)` へ流す
            // printing goes to fd 1 of the `write(fd: u32, buf: &[u8])` of the ABI, the end of main to `exit(code)`
            for (fi, f) in fns.iter().enumerate() {
                if !f.stub {
                    continue;
                }
                let nr = f.syscall.unwrap_or(0);
                let _ = fi;
                if f.name == "write" && f.params.len() == 2 && matches!(f.params[0].ty, Ty::Int(_)) && matches!(&f.params[1].ty, Ty::Ref(_, i) if matches!(**i, Ty::Slice(_))) {
                    let _ = writeln!(out, "/* {} */\nvoid fascon_user_console_write(const char *s, size_t n) {{\n    (void)fascon_user_svc({}u, 1u, (uint64_t)(uintptr_t)s, (uint64_t)n, 0u, 0u, 0u, 0u, 0u);\n}}", self.c("print 系の出力先: ABI の write の fd 1", "where printing goes: fd 1 of the write of the ABI"), nr);
                }
                if f.name == "exit" && f.params.len() == 1 && matches!(f.params[0].ty, Ty::Int(_)) {
                    let _ = writeln!(out, "/* {} */\nvoid fascon_user_exit(int code) {{\n    (void)fascon_user_svc({}u, (uint64_t)(int64_t)code, 0u, 0u, 0u, 0u, 0u, 0u, 0u);\n}}", self.c("main が戻ったとき・パニックのとき: ABI の exit", "when main returns or panics: the exit of the ABI"), nr);
                }
            }
            return;
        }
        // `#[trap] fn 名前(pid: Id<表>, addr: u64, is_write: bool)`: 例外の (ESR, FAR, PC) から引数を作る入口。
        // pid はいま走っているタスクの Id を表で validate したもの、is_write は ESR の WnR（データアボートのとき）
        // the entry that builds the arguments from the exception's (ESR, FAR, PC): pid is the running task's Id validated
        // against the table, is_write the WnR bit of the ESR (for data aborts)
        for (fi, f) in fns.iter().enumerate() {
            if !f.trap || !f.trap_v2 {
                continue;
            }
            let Some(Ty::Id(a)) = f.params.first().map(|p| p.ty.clone()) else { continue };
            let idt = self.id_name(a);
            let v = self.validate_c(a, "(f20_id_t)fascon_plat_task_current()");
            let cname = self.fn_c_names[&fi].clone();
            let _ = writeln!(
                out,
                "/* {} */\nstatic void fsc_trapent_{}(uint64_t esr, uint64_t far, uint64_t pc) {{\n    (void)pc;\n    const {} pid = ({}){};\n    const bool is_write = ((esr >> 26) == 0x24u) && ((esr >> 6) & 1u);\n    {}(pid, far, is_write);\n}}",
                self.c("#[trap] の入口: (ESR, FAR, PC) → (pid, addr, is_write)。トラップ専用のラッチは本体の _flt", "#[trap] entry: (ESR, FAR, PC) → (pid, addr, is_write); the trap's own latch is the body's _flt"),
                f.name,
                idt,
                idt,
                v,
                cname
            );
        }
        let mut rows = Vec::new();
        for (fi, f) in fns.iter().enumerate() {
            let Some(nr) = f.syscall else { continue };
            if f.stub {
                continue;
            }
            let cname = self.fn_c_names[&fi].clone();
            let mut body = String::new();
            let mut args: Vec<String> = Vec::new();
            let mut ranges: Vec<(String, String, bool)> = Vec::new(); // (番地, 長さ, &mut か)
            // マルチコア: ユーザーの窓は入口で 1 回だけカーネルの影の写しへ写す（ほかのコアの同じプロセスのスレッドが
            // 並行に書き換えても、本体は 2 回読みをしない = TOCTOU なし）。&mut の窓は本体の後で書き戻す
            // multi-core: user windows are copied once at the entry into kernel shadow buffers (even if a thread of the same
            // process rewrites them concurrently on another core, the body never reads twice = no TOCTOU); &mut windows are
            // written back after the body
            let smp = self.smp_on();
            let mut shadows: Vec<(usize, String, String, bool)> = Vec::new(); // (pi, 番地, 長さ, &mut か)
            let mut r = 0usize;
            for (pi, p) in f.params.iter().enumerate() {
                let ct = self.cty(&p.ty);
                match &p.ty {
                    Ty::Ref(m, inner) if smp && matches!(**inner, Ty::Slice(_)) => {
                        let _ = writeln!(
                            body,
                            "    if (!fascon_plat_user_window(x[{a}], x[{l}], &_chk)) return -14; /* EFAULT */\n    if (x[{l}] > FASCON2_SYSCALL_SHADOW_MAX) return -7; /* E2BIG */\n    w{pi}.n = x[{l}];\n    w{pi}.p = (uint8_t *)fascon_frame_alloc((size_t)x[{l}], \"syscall {fname}\");\n    if (fascon_plat_copy_from_user(w{pi}.p, x[{a}], x[{l}]) != 0u) return -14; /* {c} */",
                            pi = pi,
                            a = r,
                            l = r + 1,
                            fname = f.name,
                            c = self.c("EFAULT: 範囲外・写せない（1 回だけ写す。ここから先は影の写しだけを読む）", "EFAULT: out of range or not copyable (copied exactly once; only the shadow is read from here on)")
                        );
                        ranges.push((format!("x[{}]", r), format!("x[{}]", r + 1), *m));
                        shadows.push((pi, format!("x[{}]", r), format!("x[{}]", r + 1), *m));
                        args.push(format!("&w{}", pi));
                        r += 2;
                    }
                    Ty::Ref(m, inner) if smp => {
                        let st = self.cty(inner);
                        let _ = writeln!(
                            body,
                            "    if (!fascon_plat_user_window(x[{a}], sizeof({st}), &_chk)) return -14; /* EFAULT */\n    w{pi}.n = sizeof({st});\n    w{pi}.p = (uint8_t *)fascon_frame_alloc(sizeof({st}), \"syscall {fname}\");\n    if (fascon_plat_copy_from_user(w{pi}.p, x[{a}], sizeof({st})) != 0u) return -14; /* EFAULT */",
                            pi = pi,
                            a = r,
                            st = st,
                            fname = f.name
                        );
                        ranges.push((format!("x[{}]", r), format!("sizeof({})", st), *m));
                        shadows.push((pi, format!("x[{}]", r), format!("sizeof({})", st), *m));
                        args.push(format!("({})(void *)w{}.p", ct, pi));
                        r += 1;
                    }
                    Ty::Ref(m, inner) if matches!(**inner, Ty::Slice(_)) => {
                        let _ = writeln!(body, "    f20_slice_t w{pi};\n    if (!fascon_plat_user_window(x[{a}], x[{l}], &w{pi})) return -14; /* EFAULT */", pi = pi, a = r, l = r + 1);
                        ranges.push((format!("x[{}]", r), format!("x[{}]", r + 1), *m));
                        args.push(format!("&w{}", pi));
                        r += 2;
                    }
                    Ty::Ref(m, inner) => {
                        let st = self.cty(inner);
                        let _ = writeln!(body, "    f20_slice_t w{pi};\n    if (!fascon_plat_user_window(x[{a}], sizeof({st}), &w{pi})) return -14; /* EFAULT */", pi = pi, a = r, st = st);
                        ranges.push((format!("x[{}]", r), format!("sizeof({})", st), *m));
                        args.push(format!("({})(void *)w{}.p", ct, pi));
                        r += 1;
                    }
                    // 属性の形 `#[syscall(N)]`: レジスタの値を型ごとに検証してから渡す（外れたら本体を呼ばずに -EINVAL / -EBADF / -EFAULT）
                    // the attribute form: register values are validated per type before the call (else -EINVAL / -EBADF / -EFAULT
                    // without entering the body)
                    Ty::Bool if f.syscall_v2 => {
                        let _ = writeln!(body, "    if (x[{r}] > 1u) return -22; /* {c} */", r = r, c = self.c("bool は 0 か 1", "a bool is 0 or 1"));
                        args.push(format!("(x[{}] != 0u)", r));
                        r += 1;
                    }
                    Ty::Id(a) if f.syscall_v2 => {
                        let v = self.validate_c(*a, &format!("x[{}]", r));
                        let _ = writeln!(body, "    const {idt} id{pi} = ({idt}){v};
    if (id{pi} == 0u) return -9; /* {c} */", idt = ct, pi = pi, v = v, c = self.c("偽造・古い Id → EBADF", "a forged or stale Id → EBADF"));
                        args.push(format!("id{}", pi));
                        r += 1;
                    }
                    Ty::User(inner) if f.syscall_v2 => {
                        if matches!(**inner, Ty::Str) {
                            let _ = writeln!(body, "    if (!fascon_user_range_ok(x[{a}], x[{l}])) return -14; /* {c} */", a = r, l = r + 1, c = self.c("ユーザー空間の範囲の外 → EFAULT（中身はまだ読まない）", "outside user space → EFAULT (the contents are not read yet)"));
                            args.push(format!("(f20_ustr_t){{ x[{}], x[{}] }}", r, r + 1));
                            r += 2;
                        } else {
                            let n = match &**inner {
                                Ty::Array(_, n) => *n,
                                _ => 0,
                            };
                            let _ = writeln!(body, "    if (!fascon_user_range_ok(x[{a}], {n}u)) return -14; /* {c} */", a = r, n = n, c = self.c("ユーザー空間の範囲の外 → EFAULT（中身はまだ読まない）", "outside user space → EFAULT (the contents are not read yet)"));
                            args.push(format!("x[{}]", r));
                            r += 1;
                        }
                    }
                    Ty::Int(it) if f.syscall_v2 && it.bits(64) < 64 => {
                        let (lo, hi) = it.min_max(64);
                        if it.signed() {
                            let _ = writeln!(body, "    if ((int64_t)x[{r}] < INT64_C({lo}) || (int64_t)x[{r}] > INT64_C({hi})) return -22; /* {c} */", r = r, lo = lo, hi = hi, c = self.c(&format!("{} の範囲の外 → EINVAL", it.name()), &format!("outside the range of {} → EINVAL", it.name())));
                        } else {
                            let _ = writeln!(body, "    if (x[{r}] > UINT64_C({hi})) return -22; /* {c} */", r = r, hi = hi, c = self.c(&format!("{} の範囲の外 → EINVAL", it.name()), &format!("outside the range of {} → EINVAL", it.name())));
                        }
                        args.push(format!("({})x[{}]", ct, r));
                        r += 1;
                    }
                    _ => {
                        args.push(format!("({})x[{}]", ct, r));
                        r += 1;
                    }
                }
            }
            let mut overlaps = String::new();
            for i in 0..ranges.len() {
                for j in (i + 1)..ranges.len() {
                    if ranges[i].2 || ranges[j].2 {
                        let _ = writeln!(overlaps, "    if (f20_user_overlap({}, {}, {}, {})) return -14; /* {} */", ranges[i].0, ranges[i].1, ranges[j].0, ranges[j].1, self.c("&mut の窓がほかの窓と重なる", "a &mut window overlaps another window"));
                    }
                }
            }
            if smp {
                // 重なりの検査は番地だけ（中身を読む前）に行う / the overlap check looks at addresses only (before any read)
                body = format!("{}{}", overlaps, body);
            } else {
                body.push_str(&overlaps);
            }
            // 戻り値: Result<T, FaultError> は Ok(v) → v、Err(e) → -e（errno）。今までの形は i64 そのまま
            // the result: Result<T, FaultError> gives v for Ok(v) and -e (errno) for Err(e); the older form returns its i64
            let call = format!("{}({})", cname, args.join(", "));
            let ret = match &f.ret {
                Ty::Result(t, _) => {
                    let rt = self.cty(&f.ret.clone());
                    let ok = if matches!(**t, Ty::Unit) { "0".to_string() } else { "(int64_t)_r.value".to_string() };
                    format!("const {} _r = {};\n    return _r.is_some ? {} : -_r.err;", rt, call, ok)
                }
                _ => format!("return {};", call),
            };
            if smp {
                // 影の写しはこのタスクのフレームリージョンに置き、どの出口でも返す（早い return は goto で後始末へ）
                // shadow buffers live in the frame region of this task and are released on every exit (early returns jump to the cleanup)
                let mut decl = String::from("    uint8_t *const _sm = fascon_frame_mark();\n    int64_t _rc = 0;\n");
                if !shadows.is_empty() {
                    // 範囲の検査だけに使う（番地の形を見るだけで、中身は読まない） / used only for the range check (looks at
                    // the address, reads nothing)
                    decl.push_str("    f20_slice_t _chk;\n");
                }
                for (pi, _, _, _) in &shadows {
                    let _ = writeln!(decl, "    f20_slice_t w{} = {{NULL, 0u}};", pi);
                }
                let mut fixed = String::new();
                for line in body.lines() {
                    let l = match line.find("return ") {
                        Some(k) => {
                            let rest = &line[k + 7..];
                            let end = rest.find(';').unwrap_or(rest.len());
                            format!("{}{{ _rc = {}; goto fsc_out; }}{}", &line[..k], &rest[..end], &rest[end + 1..])
                        }
                        None => line.to_string(),
                    };
                    fixed.push_str(&l);
                    fixed.push('\n');
                }
                let call_rc = match &f.ret {
                    Ty::Result(t, _) => {
                        let rt = self.cty(&f.ret.clone());
                        let ok = if matches!(**t, Ty::Unit) { "0".to_string() } else { "(int64_t)_r.value".to_string() };
                        format!("    {{\n        const {} _r = {};\n        _rc = _r.is_some ? {} : -_r.err;\n    }}\n", rt, call, ok)
                    }
                    _ => format!("    _rc = {};\n", call),
                };
                let mut back = String::new();
                for (pi, a, l, m) in &shadows {
                    if *m {
                        let _ = writeln!(back, "    if (fascon_plat_copy_to_user({}, w{}.p, {}) != 0u) _rc = -14; /* {} */", a, pi, l, self.c("&mut の窓を書き戻す（1 回だけ）", "write the &mut window back (once)"));
                    }
                }
                let mut free = String::new();
                for (pi, _, _, _) in shadows.iter().rev() {
                    let _ = writeln!(free, "    fascon_own_free(w{}.p);", pi);
                }
                let label = if fixed.contains("goto fsc_out;") { "fsc_out:\n" } else { "" };
                let _ = writeln!(
                    out,
                    "/* {}: syscall {} at {} */\nstatic int64_t fsc_sysent_{}(const uint64_t *x) {{\n    (void)x;\n{}{}{}{}{}{}    fascon_frame_release(_sm);\n    return _rc;\n}}",
                    self.c("システムコールの入口（レジスタ x0〜x7 → 検証済みの引数。窓は影の写し）", "system call entry (registers x0 to x7 → validated arguments; windows are shadow copies)"),
                    f.name,
                    nr,
                    f.name,
                    decl,
                    fixed,
                    call_rc,
                    back,
                    label,
                    free
                );
                rows.push(format!("    [{}] = fsc_sysent_{},", nr, f.name));
                continue;
            }
            let _ = writeln!(
                out,
                "/* {}: syscall {} at {} */\nstatic int64_t fsc_sysent_{}(const uint64_t *x) {{\n    (void)x;\n{}    {}\n}}",
                self.c("システムコールの入口（レジスタ x0〜x7 → 検証済みの引数）", "system call entry (registers x0 to x7 → validated arguments)"),
                f.name,
                nr,
                f.name,
                body,
                ret
            );
            rows.push(format!("    [{}] = fsc_sysent_{},", nr, f.name));
        }
        if !rows.is_empty() {
            let _ = writeln!(
                out,
                "/* ───── {} ───── */\nconst FasconSyscallFn fascon2_syscall_table[256] = {{\n{}\n}};",
                self.c("システムコール表（番号 → 入口。空きは -38）", "system call table (number → entry; empty slots give -38)"),
                rows.join("\n")
            );
        }
    }

    fn gen_fn_pass(&mut self, fi: usize, fd: &FnDef) -> String {
        let f = self.s.t.fns[fi].clone();
        if f.stub {
            return self.gen_svc_stub(fi, fd);
        }
        self.flt_used = false;
        self.at_count = 0;
        self.lines.clear();
        self.ind = 1;
        self.tmp = 0;
        self.names.clear();
        self.used.clear();
        self.scopes.clear();
        self.loops.clear();
        self.frame_marks.clear();
        self.exit_mirror.clear();
        self.arena_cap = None;
        self.arena_cap_out = None;
        self.own_frame = false;
        self.ret_ty = f.ret.clone();
        self.catch_stack.clear();
        self.cur_fn = fi;
        self.spectre_idx = if self.o.profile == crate::target::Profile::GenericOs { crate::universal::spectre_taint(self.s, fi) } else { HashSet::new() };
        if self.propagate_fns.contains(&fi) {
            self.flt_used = true;
        }
        self.syscall_v2_ret = if f.syscall_v2 && matches!(f.ret, Ty::Result(..)) { Some(self.cty(&f.ret.clone())) } else { None };
        if !self.s.catch_sites.is_empty() && self.fn_has_catch(fi) {
            self.flt_used = true;
        }
        self.scopes.push(Vec::new());
        self.used.insert("_flt".to_string());
        for p in &f.params {
            let n = sanitize(&p.name);
            self.used.insert(n.clone());
            self.names.insert(p.bind, n);
            if self.s.t.needs_drop(&p.ty) && !matches!(p.ty, Ty::Ref(..)) {
                self.scopes.last_mut().unwrap().push(p.bind);
            }
        }
        let sig = self.fn_sig(fi);
        let unit = matches!(f.ret, Ty::Unit | Ty::Never);
        // `extern "C"` で公開した関数: C から入ってきた生の Id は入口で validate する（世代 0・範囲外の添字は None = 0）
        // a function exported to C: raw Ids coming in from C are validated at entry (generation 0 or an out-of-range index → None = 0)
        if f.extern_c == Some(true) {
            for p in &f.params {
                let a = match &p.ty {
                    Ty::Id(a) => Some(*a),
                    Ty::Option(i) => match **i {
                        Ty::Id(a) => Some(a),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(a) = a {
                    let n = self.names.get(&p.bind).cloned().unwrap_or_default();
                    let v = self.validate_c(a, &n);
                    let c = self.c("C から来た生の Id を検証（偽造・古い Id は 0 = None）", "validate a raw Id from C (forged or stale Ids become 0 = None)");
                    self.emit(format!("{} = ({}){}; /* {} */", n, self.id_name(a), v, c));
                }
            }
        }
        if self.flt_known == Some(true) {
            let c = self.c(
                "遅延フォールト（ラッチ）: 0 = 正常。コミット点（外へ影響が出る直前・戻る直前・ループの出口。while / loop は終了条件にも混ぜる）で 0 か確かめる",
                "deferred faults (the latch): 0 = healthy; checked at commit points (right before external effects and returns, and at loop exits; while / loop also fold it into their exit test)"
            );
            self.emit(format!("uint32_t _flt = 0u; /* {} */", c));
        }
        if f.is_main && !self.s.user_mode {
            if let Some(u) = self.s.t.umems.first().cloned() {
                if u.task != usize::MAX {
                    let c = self.c("ユーザーメモリをプラットフォーム層へ渡す（ページ表に差すのは切り替えのとき）", "hand the user memory to the platform layer (it is mapped at task switches)");
                    let first = if self.smp_on() { "[0][0][0]" } else { "[0][0]" };
                    self.emit(format!("fascon_plat_user_register(&fsc_umem_{}{}, UMEM_{}_SIZE, UMEM_{}_VA, ARENA_{}_CAP); /* {} */", u.name, first, u.name, u.name, self.s.t.arenas[u.task].name, c));
                }
            }
            if self.s.t.fns.iter().any(|g| g.syscall.is_some() && !g.stub) {
                let c = self.c("システムコール表をプラットフォーム層へ渡す", "hand the system call table to the platform layer");
                self.emit(format!("fascon_plat_syscall_register(fascon2_syscall_table, 256u); /* {} */", c));
            }
            if let Some(tf) = self.s.t.fns.iter().position(|g| g.trap) {
                let c = self.c("EL0 の例外のハンドラ", "the handler of EL0 exceptions");
                let h = if self.s.t.fns[tf].trap_v2 && matches!(self.s.t.fns[tf].params.first().map(|p| &p.ty), Some(Ty::Id(_))) { format!("fsc_trapent_{}", self.s.t.fns[tf].name) } else { self.fn_c_names[&tf].clone() };
                self.emit(format!("fascon_plat_trap_register({}); /* {} */", h, c));
            }
        }
        if f.is_main && self.o.board {
            let c = self.c("board.toml の装置の窓を登録する（bind_mmio が受け付ける範囲）", "register the board.toml device windows (the ranges bind_mmio accepts)");
            self.emit(format!("fascon_board_init(); /* {} */", c));
        }
        if f.is_main && self.o.profile == crate::target::Profile::Hpc {
            let c = self.c("管理コア（コア 0）のピン留めと、コアごとのメモリの NUMA ローカルな固定", "pin the management core (core 0) and bind its per-core memory NUMA-locally");
            self.emit(format!("f20_hpc_core_enter(); /* {} */", c));
            let nodes: Vec<(String, u32)> = self.s.hpc.as_ref().map(|h| h.numa_arenas.iter().map(|&(a, nd)| (self.s.t.arenas[a].name.clone(), nd)).collect()).unwrap_or_default();
            for (n, nd) in nodes {
                let c = self.c(&format!("#[numa_node({})]: 表を NUMA ノード {} の物理メモリへ固定する（MPOL_BIND）", nd, nd), &format!("#[numa_node({})]: bind the table to the physical memory of NUMA node {} (MPOL_BIND)", nd, nd));
                self.emit(format!("(void)fascon_plat_numa_bind((void *)&arena_{n}, sizeof arena_{n}, {nd}u); /* {c} */", n = n, nd = nd, c = c));
            }
        }
        if f.is_main {
            for ai in 0..self.s.t.arenas.len() {
                if self.mag_arena(ai) {
                    let n = self.s.t.arenas[ai].name.clone();
                    let c = self.c("Per-CPU マガジンの記述子と、仮想番地を予約した表の管理情報の写し", "the per-CPU magazine descriptor and the mapping of the VA-reserved table's bookkeeping");
                    self.emit(format!("arena_{}_mag_init(); /* {} */", n, c));
                }
            }
        }
        if f.is_main && self.s.t.fns.iter().any(|g| g.irq.is_some()) {
            let c = self.c("割り込み表をプラットフォーム層へ渡す", "hand the interrupt table to the platform layer");
            self.emit(format!("fascon_plat_irq_register(fascon2_irq_table, fascon2_irq_count); /* {} */", c));
        }
        // fascon-gc: 根の枠（gc_arena を使わないプログラム・関数には出ない） / fascon-gc: the root frame (absent in programs
        // and functions without gc_arena)
        self.gc_used = false;
        self.gc_k = 0;
        self.gc_pending.clear();
        self.gc_prologue(fi);
        // 呼び出しリージョン: 入口でバンプアリーナの先頭を印に取り、すべての出口で印へ戻す（O(1) の一括解放）
        // call regions: take the bump arenas' tops as marks at entry and reset them at every exit (an O(1) bulk free)
        self.rewinds = self.s.call_regions.get(fi).cloned().unwrap_or_default();
        for b in self.rewinds.clone() {
            let bn = self.s.t.arenas[b].name.clone();
            self.used.insert(format!("_bm_{}", bn));
            let c = self.c(
                &format!("呼び出しリージョン: この関数が {} から確保したものは、戻るときに先頭を印へ戻す 1 回の格納でまとめて解放", bn),
                &format!("call region: everything this function allocates from {} is released at once by one store that resets the top to this mark when it returns", bn),
            );
            self.emit(format!("const uint32_t _bm_{n} = arena_{n}.st.top; /* {c} */", n = bn, c = c));
            if self.flt_known.is_some() && !self.report.call_regions.iter().any(|(fname, an)| *fname == f.name && *an == bn) {
                self.report.call_regions.push((f.name.clone(), bn));
            }
        }
        let end_site = self.site(Span::new(fd.span.hi.saturating_sub(1), fd.span.hi));
        if unit {
            self.block_stmts(&fd.body, None);
            self.commit(&end_site);
            self.emit_rewinds();
        } else {
            let rv = self.new_tmp("rv");
            let decl = self.cdecl(&f.ret, &rv);
            let ret_ty = f.ret.clone();
            if self.gc_on() && self.gc_own_root(&ret_ty) {
                // fascon-gc: 戻り値の入れ物も根（末尾の式を入れてから、スコープの終わりの解放の #[on_drop] が回収しても漏れない）
                // fascon-gc: the return slot is a root too (after the tail is stored, an #[on_drop] at scope end may collect)
                let z = self.zero_value(&ret_ty);
                self.emit(format!("{} = {};", decl, z));
                self.gc_root_slot(&ret_ty, &rv);
            } else {
                self.emit(format!("{};", decl));
            }
            self.block_stmts(&fd.body, Some(&rv));
            self.commit(&end_site);
            self.emit_rewinds();
            let rv2 = rv.clone();
            self.emit(format!("return {};", rv2));
        }
        let body = std::mem::take(&mut self.lines).join("\n");
        let body = if body.contains("@GCN@") { body.replace("@GCN@", &format!("{}u", self.gc_k)) } else { body };
        self.gc_pending.clear();
        let kind = if f.is_main { self.c("エントリポイント（BSP から呼ばれる）", "entry point (called by the BSP)") } else { String::new() };
        format!(
            "/* {} {}: fn {}{} */\n{} {{\n{}\n}}\n\n",
            self.c("line", "line"),
            self.line_of(fd.span),
            fd.name,
            if kind.is_empty() { String::new() } else { format!(" — {}", kind) },
            if f.is_main { sig } else { format!("static {}", sig) },
            body
        )
    }

    /// 呼び出しリージョンの出口: バンプアリーナの先頭を入口の印へ戻す / the exit of a call region: reset the bump arenas'
    /// tops to the marks taken at entry
    fn emit_rewinds(&mut self) {
        for b in self.rewinds.clone() {
            let bn = self.s.t.arenas[b].name.clone();
            let c = self.c("一括解放（1 回の格納）", "bulk release (one store)");
            self.emit(format!("arena_{n}.st.top = _bm_{n}; /* {c} */", n = bn, c = c));
        }
    }

    /// ブロックの文を出す（関数本体用・スコープの解放付き） / emit a block's statements (with scope drops)
    fn block_stmts(&mut self, b: &Block, dest: Option<&str>) {
        for st in &b.stmts {
            self.stmt(st);
        }
        if let Some(t) = &b.tail {
            self.tail_into(t, dest);
        }
        self.emit_scope_drops(self.scopes.len() - 1, "scope end");
    }

    fn tail_into(&mut self, t: &Expr, dest: Option<&str>) {
        let tt = self.ty(t);
        match dest {
            Some(d) if tt != Ty::Never && tt != Ty::Unit => {
                let d = d.to_string();
                self.value_into(t, &d);
            }
            _ => self.expr_stmt(t),
        }
    }

    /// 値を変数 d へ代入する（if / match / ブロックは中へ押し込む） / assign a value into d (pushing into if / match / blocks)
    fn value_into(&mut self, e: &Expr, d: &str) {
        match &e.kind {
            ExprKind::If { .. } | ExprKind::IfLet { .. } | ExprKind::Match { .. } | ExprKind::Block(_) | ExprKind::Critical(_) | ExprKind::Atomic(_) | ExprKind::RcuRead(_) | ExprKind::CatchFault(_) => self.ctrl(e, Some(d)),
            _ => {
                let v = self.val(e);
                let v = self.coerce(&v, &self.ty(e), None);
                self.emit(format!("{} = {};", d, v));
            }
        }
    }

    fn coerce(&self, v: &str, _from: &Ty, _to: Option<&Ty>) -> String {
        v.to_string()
    }

    fn emit_scope_drops(&mut self, depth: usize, _why: &str) {
        let locals = self.scopes[depth].clone();
        for b in locals.iter().rev() {
            if let Some(m) = self.exit_mirror.get(b).cloned() {
                self.emit(m);
            }
            let t = self.local(*b).ty.clone();
            let n = self.names.get(b).cloned().unwrap_or_default();
            // フレームに置いた不変の own: 根は差し替えられないので、根の解放（ヘッダの確認）を省き、子だけ解放する
            // an immutable own in the frame: its root cannot be replaced, so skip freeing the root (the header check) and
            // drop only the children
            let frame_imm = self.frame_marks.contains_key(b) && !self.local(*b).mutable;
            let code = if frame_imm {
                match &t {
                    Ty::Own(i) | Ty::DmaOwn(i) => match &**i {
                        Ty::Struct(s) if self.s.t.structs[*s].needs_drop => {
                            self.need_drop.insert(*s);
                            format!("fsc_drop_{}({});", self.s.t.structs[*s].name, n)
                        }
                        // 構造体でない own の中身が own を持つ（`own Option<own Node>` など）: 中身だけ解放する
                        // a non-struct own whose contents hold own (`own Option<own Node>` ...): drop the contents only
                        other if !matches!(other, Ty::Struct(_)) && self.s.t.needs_drop(other) => {
                            let other = other.clone();
                            let place = if matches!(other, Ty::Array(..)) { n.clone() } else { format!("(*{})", n) };
                            self.drop_code(&other, &place)
                        }
                        _ => String::new(),
                    },
                    _ => String::new(),
                }
            } else {
                self.drop_code(&t, &n)
            };
            if !code.is_empty() {
                let moved = self.fr.moved_locals.contains(b);
                let note = if moved {
                    self.c(&format!("`{}` のスコープ終了: 所有リージョンを一括解放（移動済みなら NULL なので何もしない）", self.local(*b).name), &format!("end of `{}`'s scope: bulk-free its own region (a moved value is NULL, so nothing happens)", self.local(*b).name))
                } else {
                    self.c(&format!("`{}` のスコープ終了: 所有リージョンを一括解放", self.local(*b).name), &format!("end of `{}`'s scope: bulk-free its own region", self.local(*b).name))
                };
                self.emit(format!("{} /* {} */", code, note));
                // fascon-gc: 解放した構造体・Option・配列の変数は 0 にする（中の Id も、もう根ではない。own は解放関数が
                // NULL にしている） / fascon-gc: a dropped struct, Option or array variable is cleared to 0 (its Ids stop
                // being roots too; owns were already set to NULL by the drop functions)
                if !frame_imm && self.gc_on() && self.gc_own_root(&t) && !matches!(&t, Ty::Own(_) | Ty::DmaOwn(_)) && !matches!(&t, Ty::Option(i) if Self::opt_niche(i)) {
                    let z = if matches!(t, Ty::Array(..)) { format!("__builtin_memset({}, 0, sizeof {});", n, n) } else { format!("__builtin_memset(&{}, 0, sizeof {});", n, n) };
                    self.emit(format!("{} /* {} */", z, self.c("fascon-gc: 解放した値は根から外れる", "fascon-gc: a dropped value is no longer a root")));
                }
            }
            // fascon-gc: フレームに置いた own は、領域を戻す前に変数を NULL にする（戻した領域を根が指さない）
            // fascon-gc: an own placed in the frame has its variable cleared to NULL before the space is popped (no root
            // points into popped space)
            if frame_imm && self.gc_on() && self.gc_own_root(&t) {
                let z = self.zero_value(&t);
                self.emit(format!("{} = {}; /* {} */", n, z, self.c("fascon-gc: フレームの領域を戻す前に根から外す", "fascon-gc: off the roots before the frame space is popped")));
            }
            if let Some(mark) = self.frame_marks.get(b).cloned() {
                let note = self.c(
                    &format!("`{}` のフレーム領域を印まで戻す（スタックと同じ。ページテーブルには触らない）", self.local(*b).name),
                    &format!("pop `{}`'s frame space back to its mark (like a stack; no page-table work)", self.local(*b).name),
                );
                self.emit(format!("fascon_frame_release({}); /* {} */", mark, note));
            }
        }
    }

    fn emit_drops_from(&mut self, depth: usize) {
        let n = self.scopes.len();
        for d in (depth..n).rev() {
            self.emit_scope_drops(d, "exit");
        }
    }

    fn declare_local(&mut self, b: BindId) -> String {
        let l = self.local(b).clone();
        let mut base = sanitize(&l.name);
        if base.starts_with('_') && base[1..].chars().next().map_or(false, |c| c.is_ascii_alphabetic()) && base[1..].chars().skip(1).all(|c| c.is_ascii_digit()) {
            base.push_str("_v");
        }
        let mut n = base.clone();
        let mut k = 2;
        while self.used.contains(&n) {
            n = format!("{}_{}", base, k);
            k += 1;
        }
        self.used.insert(n.clone());
        self.names.insert(b, n.clone());
        if self.s.t.needs_drop(&l.ty) && !matches!(l.ty, Ty::Ref(..)) {
            self.scopes.last_mut().unwrap().push(b);
        }
        self.gc_note_local(&n, &l.ty);
        n
    }

    fn stmt(&mut self, st: &Stmt) {
        let mark = self.gc_stmt_temps.len();
        self.stmt_inner(st);
        // fascon-gc: 一時値の根は文の終わりまで（値は行き先の変数・スロットへ移っている） / temporaries stay roots until the
        // end of their statement (by then the value lives in its destination variable or slot)
        if self.gc_stmt_temps.len() > mark {
            let ks: Vec<u32> = self.gc_stmt_temps.drain(mark..).collect();
            let c = self.c("文の終わり: 一時値の根を外す", "end of statement: the temporaries stop being roots");
            let list: Vec<String> = ks.iter().map(|k| format!("_gcp[{}] = NULL;", k)).collect();
            self.emit(format!("{} /* {} */", list.join(" "), c));
        }
    }

    fn stmt_inner(&mut self, st: &Stmt) {
        match st {
            Stmt::Let(l) => {
                let t = self.local(l.bind).ty.clone();
                match &l.init {
                    Some(init) => {
                        let it = self.ty(init);
                        if it == Ty::Never {
                            self.expr_stmt(init);
                            let n = self.declare_local(l.bind);
                            let d = self.cdecl(&t, &n);
                            self.emit(format!("{}; (void){};", d, n));
                            return;
                        }
                        if let Ty::Array(..) = t {
                            self.let_array(l, init, &t);
                            return;
                        }
                        if let (ExprKind::Own(_) | ExprKind::OwnAlloc(_), Ty::Own(_)) = (&init.kind, &t) {
                            if !self.fr.moved_locals.contains(&l.bind) {
                                // エスケープしない own: フレームリージョンに置く（印を取り、スコープの終わりに戻す）
                                // a non-escaping own: placed in the frame region (take a mark, pop back to it at scope end)
                                let mark = self.new_tmp("fm");
                                let note = self.c(
                                    &format!("`{}` はどこへも移動しない（エスケープ解析）→ フレームリージョンから確保", self.local(l.bind).name),
                                    &format!("`{}` is never moved (escape analysis) → allocated from the frame region", self.local(l.bind).name),
                                );
                                self.emit(format!("uint8_t *{} = fascon_frame_mark(); /* {} */", mark, note));
                                self.own_frame = true;
                                let v = self.val(init);
                                self.own_frame = false;
                                let n = self.declare_local(l.bind);
                                let d = self.cdecl(&t, &n);
                                self.emit(format!("{} = {};", d, v));
                                self.frame_marks.insert(l.bind, mark);
                                let line = self.line_of(init.span);
                                let sname = self.s.t.display(&t);
                                self.report.frame_sites.push((line, sname));
                                return;
                            }
                        }
                        match &init.kind {
                            ExprKind::If { .. } | ExprKind::IfLet { .. } | ExprKind::Match { .. } | ExprKind::Block(_) | ExprKind::Critical(_) | ExprKind::Atomic(_) | ExprKind::RcuRead(_) | ExprKind::CatchFault(_) => {
                                let n = self.declare_local_deferred(l.bind);
                                let d = self.cdecl(&t, &n);
                                // fascon-gc: own の先まで根にする変数は、値が入る前（分岐の中で回収が起きても）NULL / 0
                                // fascon-gc: a variable followed behind own is NULL / 0 until it gets its value (even if
                                // a branch collects first)
                                if self.gc_on() && self.gc_own_root(&t) {
                                    let z = self.zero_value(&t);
                                    self.emit(format!("{} = {};", d, z));
                                } else {
                                    self.emit(format!("{};", d));
                                }
                                self.ctrl(init, Some(&n));
                                self.activate_local(l.bind);
                            }
                            _ => {
                                let v = self.val(init);
                                let n = self.declare_local(l.bind);
                                let d = self.cdecl(&t, &n);
                                self.emit(format!("{} = {};", d, v));
                            }
                        }
                    }
                    None => {
                        let n = self.declare_local(l.bind);
                        let d = self.cdecl(&t, &n);
                        if self.s.t.needs_drop(&t) {
                            let z = self.zero_value(&t);
                            self.emit(format!("{} = {};", d, z));
                        } else {
                            self.emit(format!("{};", d));
                        }
                    }
                }
            }
            Stmt::Expr(e) => self.expr_stmt(e),
        }
    }

    /// 初期化が終わるまで解放対象にしない宣言 / a declaration not registered for drop until initialized
    fn declare_local_deferred(&mut self, b: BindId) -> String {
        let n = self.declare_local(b);
        if let Some(s) = self.scopes.last_mut() {
            s.retain(|x| *x != b);
        }
        n
    }
    fn activate_local(&mut self, b: BindId) {
        let t = self.local(b).ty.clone();
        if self.s.t.needs_drop(&t) {
            self.scopes.last_mut().unwrap().push(b);
        }
    }

    fn zero_value(&mut self, t: &Ty) -> String {
        match t {
            Ty::Own(_) | Ty::Ref(..) | Ty::DmaOwn(_) => "NULL".to_string(),
            Ty::Result(..) => {
                let c = self.cty(t);
                format!("({}){{ .is_some = false }}", c)
            }
            Ty::User(i) if matches!(**i, Ty::Str) => "(f20_ustr_t){ 0u, 0u }".to_string(),
            Ty::Option(i) if Self::opt_niche(i) => match &**i {
                Ty::Id(_) | Ty::Dma(_) | Ty::GpuBuf(_) => "0u".to_string(),
                Ty::Str => "(FasconStr){ NULL, 0u }".to_string(),
                _ => "NULL".to_string(),
            },
            Ty::Option(_) => {
                let c = self.cty(t);
                format!("({}){{ .is_some = false }}", c)
            }
            Ty::Struct(_) => {
                let c = self.cty(t);
                format!("({}){{ 0 }}", c)
            }
            Ty::Int(_) => "0".to_string(),
            Ty::Bool => "false".to_string(),
            _ => "0".to_string(),
        }
    }

    fn let_array(&mut self, l: &LetStmt, init: &Expr, t: &Ty) {
        match &init.kind {
            ExprKind::ArrayLit(elems) => {
                let et = match t {
                    Ty::Array(e, _) => (**e).clone(),
                    _ => Ty::Err,
                };
                let refs: Vec<&Expr> = elems.iter().collect();
                let vals = self.operands(&refs);
                let vals: Vec<String> = vals.iter().zip(elems).map(|(v, x)| self.array_elem_init(v, x, &et)).collect();
                let n = self.declare_local(l.bind);
                let d = self.cdecl(t, &n);
                self.emit(format!("{} = {{ {} }};", d, vals.join(", ")));
            }
            ExprKind::ArrayRepeat(v, _) => {
                let n = self.declare_local(l.bind);
                let d = self.cdecl(t, &n);
                if self.is_zero_like(v) {
                    self.emit(format!("{} = {{ 0 }};", d));
                } else {
                    let val = self.val(v);
                    let tmpv = self.hoist(&self.ty(v), &val);
                    self.emit(format!("{};", d));
                    let len = if let Ty::Array(_, k) = t { *k } else { 0 };
                    self.emit(format!("for (size_t _i = 0; _i < {}u; _i++) {}[_i] = {};", len, n, tmpv));
                }
            }
            _ => {
                let src = self.lv(init);
                let n = self.declare_local(l.bind);
                let d = self.cdecl(t, &n);
                self.emit(format!("{};", d));
                self.emit(format!("__builtin_memcpy({}, {}, sizeof {});", n, src, n));
            }
        }
    }

    fn array_elem_init(&mut self, v: &str, _x: &Expr, _et: &Ty) -> String {
        v.to_string()
    }

    /// 文として評価する（値は捨てる） / evaluate as a statement (value discarded)
    fn expr_stmt(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::If { .. } | ExprKind::IfLet { .. } | ExprKind::Match { .. } | ExprKind::Block(_) | ExprKind::Critical(_) | ExprKind::Atomic(_) | ExprKind::RcuRead(_) | ExprKind::CatchFault(_) => self.ctrl(e, None),
            ExprKind::While { .. } | ExprKind::Loop { .. } | ExprKind::For { .. } => self.loop_stmt(e),
            ExprKind::VolatileBypass(b) => {
                self.emit("{");
                self.ind += 1;
                self.emit("FASCON_BYPASS_ENTER();");
                self.scopes.push(Vec::new());
                self.block_stmts(b, None);
                self.scopes.pop();
                self.emit("FASCON_BYPASS_EXIT();");
                self.ind -= 1;
                self.emit("}");
            }
            ExprKind::Assign { op, lhs, rhs } => self.assign(*op, lhs, rhs),
            ExprKind::Break => {
                let (depth, label) = match self.loops.last() {
                    Some(l) => (l.depth, l.brk_label.clone()),
                    None => (0, None),
                };
                self.emit_drops_from(depth);
                match label {
                    Some(lb) => {
                        if let Some(l) = self.loops.last_mut() {
                            l.brk_used = true;
                        }
                        self.emit(format!("goto {};", lb));
                    }
                    None => self.emit("break;"),
                }
            }
            ExprKind::Continue => {
                let (depth, label) = match self.loops.last() {
                    Some(l) => (l.depth, l.cont_label.clone()),
                    None => (0, None),
                };
                self.emit_drops_from(depth);
                match label {
                    Some(lb) => self.emit(format!("goto {};", lb)),
                    None => self.emit("continue;"),
                }
            }
            ExprKind::Return(v) => {
                let site = self.site(e.span);
                match v {
                    Some(v) => {
                        let rt = self.ret_ty.clone();
                        if self.ty(v) == Ty::Never {
                            self.expr_stmt(v);
                            return;
                        }
                        let before = self.at_count;
                        let val = self.val(v);
                        let latched = self.flt_on() && self.at_count > before;
                        // 巻き戻す前に戻り値を求め終える（戻り値の式がバンプアリーナを読んでもよいように）
                        // finish computing the return value before rewinding (the expression may read a bump arena)
                        let drops = self.scopes.iter().any(|s| !s.is_empty());
                        let tmp = if drops && self.gc_on() && self.gc_own_root(&rt) {
                            // fascon-gc: 戻り値を根にしてから残りのローカルを解放する（#[on_drop] が回収しうる）
                            // fascon-gc: root the return value, then drop the remaining locals (an #[on_drop] may collect)
                            let n = self.hoist_force(&rt, &val);
                            self.gc_root_slot(&rt, &n);
                            n
                        } else if drops || latched || !self.rewinds.is_empty() {
                            self.hoist(&rt, &val)
                        } else {
                            val
                        };
                        self.commit(&site);
                        self.emit_drops_from(0);
                        self.emit_rewinds();
                        self.emit(format!("return {};", tmp));
                    }
                    None => {
                        self.commit(&site);
                        self.emit_drops_from(0);
                        self.emit_rewinds();
                        self.emit("return;");
                    }
                }
            }
            _ => {
                let t = self.ty(e);
                let v = self.val(e);
                if v.is_empty() {
                    return;
                }
                if self.s.t.needs_drop(&t) && !crate::sema::is_place_expr(e) {
                    // 使わない own の値はその場で解放 / an unused own value is dropped immediately
                    let tmp = self.hoist(&t, &v);
                    let d = self.gc_drop_temp(&t, &tmp);
                    self.emit(format!("{} /* {} */", d, self.c("使われない値をその場で解放", "drop an unused value immediately")));
                } else if t == Ty::Unit || t == Ty::Never || t.is_err() {
                    if v != "(void)0" {
                        self.emit(format!("{};", v));
                    }
                } else if crate::sema::is_place_expr(e) || matches!(e.kind, ExprKind::Int(..) | ExprKind::Float(..) | ExprKind::Bool(_) | ExprKind::Str(_)) {
                    self.emit(format!("(void){};", v));
                } else if v.ends_with(')') && !v.starts_with('(') {
                    self.emit(format!("(void){};", v));
                } else {
                    self.emit(format!("(void)({});", v));
                }
            }
        }
    }

    fn hoist(&mut self, t: &Ty, v: &str) -> String {
        if is_simple_c(v) {
            return v.to_string();
        }
        let n = self.new_tmp("t");
        let d = self.cdecl(t, &n);
        self.emit(format!("{} = {};", d, v));
        n
    }

    // ───────────── 制御構造 / control flow ─────────────

    fn ctrl(&mut self, e: &Expr, dest: Option<&str>) {
        match &e.kind {
            ExprKind::Block(b) => {
                self.emit("{");
                self.ind += 1;
                self.scopes.push(Vec::new());
                self.block_stmts(b, dest);
                self.scopes.pop();
                self.ind -= 1;
                self.emit("}");
            }
            // 閉じた区間: 割り込みの状態を退避し、ブロックのどの出口（末尾・break・continue・return）でも戻す。出口の
            // 復帰は C の cleanup 属性に任せるので、出口ごとのコードは要らない
            // a closed section: save the interrupt state and restore it at every exit of the block (its end, break,
            // continue, return). The restore is left to the C cleanup attribute, so no per-exit code is needed
            ExprKind::Critical(b) => {
                let n = self.new_tmp("cs");
                let c = self.c("閉じた区間: 割り込まれない", "closed section: not interrupted");
                self.emit("{");
                self.ind += 1;
                self.emit(format!("F20_CRITICAL({}); /* critical — {} */", n, c));
                self.scopes.push(Vec::new());
                self.block_stmts(b, dest);
                self.scopes.pop();
                self.ind -= 1;
                self.emit("}");
            }
            ExprKind::Atomic(b) => self.lower_atomic(e, b, dest),
            ExprKind::RcuRead(b) => {
                let n = self.new_tmp("rcu");
                let c = self.c(
                    "RCU の読み手: プリエンプション禁止。&T の読み出しだけ（書き込み・解放は F3202 でコンパイル時に拒否済み）。区間のどの出口でも抜ける",
                    "RCU reader: preemption disabled; &T reads only (writes and frees were rejected at compile time by F3202); left at every exit of the section",
                );
                self.emit("{");
                self.ind += 1;
                self.emit(format!("F20_RCU_READ({}); /* rcu_read — {} */", n, c));
                self.scopes.push(Vec::new());
                self.gc_rcu_depth += 1;
                self.block_stmts(b, dest);
                self.gc_rcu_depth -= 1;
                self.scopes.pop();
                self.ind -= 1;
                self.emit("}");
            }
            ExprKind::CatchFault(b) => self.lower_catch(e, b, dest),
            ExprKind::If { cond, then, els } => {
                let c = self.val(cond);
                self.emit(format!("if ({}) {{", strip_all_parens(&c)));
                self.branch_block(then, dest);
                match els {
                    Some(el) => {
                        match &el.kind {
                            ExprKind::Block(b) => {
                                self.emit("} else {");
                                self.branch_block(b, dest);
                            }
                            _ => {
                                self.emit("} else {");
                                self.ind += 1;
                                self.scopes.push(Vec::new());
                                self.tail_into(el, dest);
                                self.scopes.pop();
                                self.ind -= 1;
                            }
                        }
                        self.emit("}");
                    }
                    None => self.emit("}"),
                }
            }
            ExprKind::IfLet { pat, scrut, then, els } => {
                let arms: Vec<(Pattern, ArmBody)> = vec![(pat.clone(), ArmBody::B(then)), (Pattern::Wild(e.span), match els {
                    Some(x) => ArmBody::E(x),
                    None => ArmBody::Empty,
                })];
                self.lower_match(scrut, &arms, dest, e.span);
            }
            ExprKind::Match { scrut, arms } => {
                let a: Vec<(Pattern, ArmBody)> = arms.iter().map(|a| (a.pat.clone(), ArmBody::E(&a.body))).collect();
                self.lower_match(scrut, &a, dest, e.span);
            }
            _ => {
                if let Some(d) = dest {
                    let d = d.to_string();
                    self.value_into(e, &d);
                } else {
                    self.expr_stmt(e);
                }
            }
        }
    }

    fn branch_block(&mut self, b: &Block, dest: Option<&str>) {
        self.ind += 1;
        self.scopes.push(Vec::new());
        self.block_stmts(b, dest);
        self.scopes.pop();
        self.ind -= 1;
    }

    fn lower_match(&mut self, scrut: &Expr, arms: &[(Pattern, ArmBody)], dest: Option<&str>, _sp: Span) {
        let st = self.ty(scrut);
        let is_lookup = self.s.lookup_scrut.contains(&scrut.id);
        // Result は常に {is_some, value, err} の構造体（ニッチを使わない）。成功値の型を Option の中身と同じに扱う
        // a Result is always an {is_some, value, err} struct (no niche); its success type is handled like an Option payload
        let is_res = matches!(st.strip_ref(), Ty::Result(..));
        let sline = self.line_of(scrut.span);
        // 対象の表現: (C 式, 種類) / representation of the scrutinee
        enum SK {
            /// NULL かどうかで判定するポインタ（ルックアップ・own のニッチ） / a pointer tested against NULL
            Ptr,
            /// Option 構造体へのポインタ（場所の借用） / pointer to an Option struct (borrowed place)
            OptPtr,
            /// 値として持つ（一時値・コピー） / held by value (temporary or copy)
            Val,
        }
        // get_mut の束縛（列を持つアリーナ）: スコープのどの出口でも写しを作り直す / a get_mut binding into an arena with
        // columns: rebuild the mirror at every exit of its scope
        let mut lookup_mut: Option<usize> = None;
        // 完全列化のアリーナのルックアップ / a lookup into a True-SoA arena
        let mut lookup_ts: Option<usize> = None;
        // 値全体を参照で受ける束縛に渡す、場所への参照（C 式） / the reference to the place (a C expression) given to a
        // binding that takes the whole value by reference
        let mut whole: Option<String> = None;
        // アリーナのスロットの中を指す &mut の束縛（`match @a[i].f { x => .. }`・`if let Some(x) = @a[i].o` など）: 束縛を
        // 通した書き込みが届く列の写しを、そのスコープのどの出口でも作り直す（get_mut の束縛と同じ。借用中は F2502 が
        // その場所と重なる読み出しを禁止しているので、古い写しは誰にも見えない）。ここでは束縛ごとの列の集合を先に求め、
        // 要素ポインタを取っておくよう印を付ける
        // a &mut binding pointing into an arena slot (`match @a[i].f { x => .. }`, `if let Some(x) = @a[i].o`, ...):
        // rebuild the mirrors of the columns its writes reach at every exit of its scope (as for get_mut bindings; F2502
        // forbids reads overlapping the place during the borrow, so the stale mirror is never seen). The column set of
        // each binding is computed first, and the element pointer is marked to be kept
        let mut place_mirror: Vec<(BindId, u64)> = Vec::new();
        let mut place_arena: Option<usize> = None;
        if !is_lookup && crate::sema::is_place_expr(scrut) {
            if let Some((a, root, path)) = self.arena_write_root(scrut) {
                if self.arena_cols.get(a).map_or(false, |c| !c.is_empty()) {
                    for (pat, body) in arms {
                        let b = match pat {
                            Pattern::Some(Some((_, b, _)), _) | Pattern::Bind(_, b, _) | Pattern::Ok(Some((_, b, _)), _) => *b,
                            _ => continue,
                        };
                        if self.local(b).kind != LocalKind::PatRef || !matches!(self.local(b).ty, Ty::Ref(true, _)) {
                            continue;
                        }
                        let mut paths: Vec<Vec<String>> = Vec::new();
                        let known = match body {
                            ArmBody::B(bl) => self.ref_writes_block(b, bl, &mut paths),
                            ArmBody::E(x) => self.ref_writes(b, x, &mut paths),
                            ArmBody::Empty => true,
                        };
                        // 束縛からの道筋の前に、要素から場所までの道筋を付ける（分からない使い方なら場所の下のすべて）
                        // prefix the paths with the path from the element to the place (everything below the place when
                        // the use cannot be followed)
                        let full: Vec<Vec<String>> = if known {
                            paths
                                .iter()
                                .map(|p| {
                                    let mut q = path.clone();
                                    q.extend(p.iter().cloned());
                                    q
                                })
                                .collect()
                        } else {
                            vec![path.clone()]
                        };
                        let mask = self.col_mask(a, &full);
                        if mask != 0 {
                            place_mirror.push((b, mask));
                        }
                    }
                    if !place_mirror.is_empty() {
                        self.arena_cap = Some(root.id);
                        self.arena_cap_out = None;
                        place_arena = Some(a);
                    }
                }
            }
        }
        let bytes_scrut = matches!(self.s.methods.get(&scrut.id), Some(MethodKind::View(..)) | Some(MethodKind::Window(_)));
        let umem_scrut = match self.s.methods.get(&scrut.id) {
            Some(MethodKind::UserMemOf(u)) => Some(*u),
            _ => None,
        };
        let (m, kind, owns) = if let (true, Some(u)) = (is_lookup, umem_scrut) {
            // `@u.of(id)`: そのタスクのユーザーメモリ（カーネルの番地で見た窓）。Id が古ければ NULL → None の腕
            // the user memory of that task (a window at its kernel address); NULL → the None arm for a stale Id
            let idv = match &scrut.kind {
                ExprKind::MethodCall { args, .. } => self.val(&args[0]),
                _ => unreachable!(),
            };
            let un = self.s.t.umems[u].name.clone();
            let n = self.new_tmp("m");
            let w = self.new_tmp("w");
            self.emit(format!("f20_slice_t {};", w));
            self.emit(format!(
                "f20_slice_t *{} = umem_{}_of(&{}, {}); /* {} {}: {} */",
                n,
                un,
                w,
                idv,
                self.c("line", "line"),
                sline,
                self.c("タスクのユーザーメモリの窓（Id が無効なら NULL → None の腕）", "the window over a task's user memory (NULL → the None arm for an invalid Id)")
            ));
            (n, SK::Ptr, false)
        } else if is_lookup && bytes_scrut {
            // ビュー・窓: 収まれば番地、収まらなければ NULL（None の腕） / a view or window: an address when it fits, NULL otherwise
            let (brecv, bargs, mk) = match &scrut.kind {
                ExprKind::MethodCall { recv, args, .. } => (&**recv, args, self.s.methods.get(&scrut.id).copied()),
                _ => unreachable!(),
            };
            let (p, len) = self.bytes_of(brecv);
            let n = self.new_tmp("m");
            match mk {
                Some(MethodKind::View(mutable, sid)) => {
                    let off = self.val(&bargs[1]);
                    let st_name = self.struct_name(sid);
                    self.emit(format!(
                        "{}{} *{} = ({} *)f20_view((uint8_t *)({}), {}, {}, sizeof({})); /* {} {}: {} */",
                        if mutable { "" } else { "const " },
                        st_name,
                        n,
                        st_name,
                        p,
                        len,
                        off,
                        st_name,
                        self.c("line", "line"),
                        sline,
                        self.c("バイト列の上のビュー（範囲外なら NULL → None の腕）", "a view over bytes (NULL → the None arm when out of range)")
                    ));
                }
                _ => {
                    let mutable = matches!(mk, Some(MethodKind::Window(true)));
                    let off = self.val(&bargs[0]);
                    let wl = self.val(&bargs[1]);
                    let w = self.new_tmp("w");
                    self.emit(format!("f20_slice_t {};", w));
                    self.emit(format!(
                        "{}f20_slice_t *{} = f20_window(&{}, (uint8_t *)({}), {}, {}, {}); /* {} {}: {} */",
                        if mutable { "" } else { "const " },
                        n,
                        w,
                        p,
                        len,
                        off,
                        wl,
                        self.c("line", "line"),
                        sline,
                        self.c("窓（範囲外なら NULL → None の腕）", "a window (NULL → the None arm when out of range)")
                    ));
                }
            }
            (n, SK::Ptr, false)
        } else if is_lookup {
            let (recv_args, mk) = match &scrut.kind {
                ExprKind::MethodCall { args, .. } => (args, self.s.methods.get(&scrut.id).copied()),
                _ => unreachable!(),
            };
            let (a, mutable) = match mk {
                Some(MethodKind::ArenaGet(a)) => (a, false),
                Some(MethodKind::ArenaGetMut(a)) => (a, true),
                _ => (0, false),
            };
            if mutable && self.arena_cols.get(a).map_or(false, |c| !c.is_empty()) {
                lookup_mut = Some(a);
            }
            let id = self.val(&recv_args[0]);
            // 完全列化のアリーナ: ルックアップが返すのはスロットに残る部分へのポインタ（束縛のフィールドは列へ行く）
            // a True-SoA arena: the lookup returns a pointer to the part left in the slot (the binding's fields go to the columns)
            let et = if self.tsoa.get(a).map_or(false, |v| !v.is_empty()) {
                lookup_ts = Some(a);
                self.ts_part(a)
            } else {
                self.cty(&self.s.t.arenas[a].elem.clone())
            };
            let n = self.new_tmp("m");
            let an = self.s.t.arenas[a].name.clone();
            if self.fr.proven.contains(&scrut.id) {
                self.emit(format!(
                    "{}{} *{} = arena_{}_slot({}); /* {} {}: {} */",
                    if mutable { "" } else { "const " },
                    et,
                    n,
                    an,
                    id,
                    self.c("line", "line"),
                    sline,
                    self.c("生存が静的に証明された Id なので世代検査なし", "the Id is proven live statically: no generation check")
                ));
                self.report.gen_elided += 1;
            } else {
                self.emit(format!(
                    "{}{} *{} = arena_{}_get({}); /* {} {}: {} */",
                    if mutable { "" } else { "const " },
                    et,
                    n,
                    an,
                    id,
                    self.c("line", "line"),
                    sline,
                    self.c("一時的なルックアップ（古い Id なら NULL → None の腕）", "temporary lookup (a stale Id gives NULL → the None arm)")
                ));
                self.report.gen_lookups += 1;
            }
            (n, SK::Ptr, false)
        } else if crate::sema::is_place_expr(scrut) {
            let inner = match st.strip_ref() {
                Ty::Option(i) => Some((**i).clone()),
                // Result はニッチを使わないので、中身はいつも「ニッチでない Option」と同じ扱い（Unit は uint8_t に置き換える）
                // a Result never uses a niche, so its payload is handled like a non-niche Option payload
                Ty::Result(..) => Some(Ty::Int(IntTy::U8)),
                _ => None,
            };
            // 値全体を束縛するアームの型（コピーできない場所は参照で束縛される: 構造体・own・コピーできない配列や Option）
            // the type of an arm binding the whole value (a place that cannot be copied is bound by reference: structs,
            // own, arrays and Options that cannot be copied)
            let whole_ref: Option<Ty> = arms.iter().find_map(|(p, _)| match p {
                Pattern::Bind(_, b, _) if self.local(*b).kind == LocalKind::PatRef => Some(self.local(*b).ty.clone()),
                _ => None,
            });
            // 書き換えうる参照の束縛があるか（無ければ場所へのポインタは const でよい） / is any binding a mutable reference?
            // (if none, the pointer to the place can be const)
            let any_mut = arms.iter().any(|(p, _)| match p {
                Pattern::Some(Some((_, b, _)), _) | Pattern::Bind(_, b, _) | Pattern::Ok(Some((_, b, _)), _) => matches!(self.local(*b).ty, Ty::Ref(true, _)),
                _ => false,
            });
            if let Some(rt) = whole_ref {
                // 場所への参照を一度だけ作る。参照の C の形は cty(&T)（構造体・Option は場所のアドレス、own・配列は中身への
                // ポインタ）で、addr_place が返す形と同じ。判定と Some(..) の束縛はこの参照を通して読む
                // take the reference to the place once. Its C form is cty(&T) (the address of the place for structs and
                // Options, the pointer to the contents for own and arrays), which is the form addr_place returns; the
                // tests and the Some(..) bindings read through it
                let p = self.addr_place(scrut);
                let r = self.new_tmp("m");
                let d = self.cdecl(&rt, &r);
                let c = self.c("場所を参照で束縛（値は写さない）", "the place, bound by reference (no copy)");
                self.emit(format!("{} = {}; /* {} */", d, p, c));
                whole = Some(r.clone());
                match inner {
                    // Option<Dma<..>>: ハンドル（0 = None）の置き場へのポインタ。None なら NULL
                    // Option<Dma<..>>: the pointer to where the handle (0 = None) is stored; NULL when None
                    Some(Ty::Dma(_) | Ty::GpuBuf(_)) => (format!("((*{r}) != 0u ? {r} : NULL)", r = r), SK::Ptr, false),
                    // Option<own T>: 所有ポインタは *r / the owning pointer is *r
                    Some(i) if Self::opt_niche(&i) => (format!("(*{})", r), SK::Ptr, false),
                    Some(_) => (r, SK::OptPtr, false),
                    None => (r, SK::Val, false),
                }
            } else {
                match inner {
                    // Option<Dma<..>> の場所: ハンドルは参照で束縛する（置き場へのポインタ。None なら NULL）
                    // an Option<Dma<..>> place: the handle is bound by reference (a pointer to its storage; NULL when None)
                    Some(Ty::Dma(a) | Ty::GpuBuf(a)) => {
                        let p = self.addr_place(scrut);
                        let ct = self.id_name(a);
                        let q = self.new_tmp("p");
                        self.emit(format!("{}{} *{} = {};", if any_mut { "" } else { "const " }, ct, q, p));
                        let n = self.new_tmp("m");
                        self.emit(format!("{}{} *{} = (*{} != 0u) ? {} : NULL;", if any_mut { "" } else { "const " }, ct, n, q, q));
                        (n, SK::Ptr, false)
                    }
                    Some(_) if is_res && !self.s.t.is_copy(&st.strip_ref().clone()) => {
                        let p = self.addr_place(scrut);
                        let ct = self.cty(&st.strip_ref().clone());
                        let n = self.new_tmp("m");
                        self.emit(format!("{}{} *{} = {};", if any_mut { "" } else { "const " }, ct, n, p));
                        (n, SK::OptPtr, false)
                    }
                    Some(i) if !self.s.t.is_copy(&i) => {
                        if Self::opt_niche(&i) {
                            let v = self.val(scrut);
                            let ct = self.cty(&st.strip_ref().clone());
                            let n = self.new_tmp("m");
                            self.emit(format!("{}{} = {};", ct, n, v));
                            (n, SK::Ptr, false)
                        } else {
                            let p = self.addr_place(scrut);
                            let ct = self.cty(&st.strip_ref().clone());
                            let n = self.new_tmp("m");
                            self.emit(format!("{}{} *{} = {};", if any_mut { "" } else { "const " }, ct, n, p));
                            (n, SK::OptPtr, false)
                        }
                    }
                    _ => {
                        let v = self.val(scrut);
                        if is_simple_c(&v) && v.chars().next().map_or(false, |c| c.is_ascii_alphabetic()) {
                            // 単純な変数はそのまま判定に使う / a plain variable is tested directly
                            (v, SK::Val, false)
                        } else if let Ty::Array(..) = st.strip_ref() {
                            // C の配列は値で写せないので、先頭要素へのポインタで持つ（値の束縛は memcpy で写す）
                            // a C array cannot be copied by value: hold a pointer to its first element (a by-value
                            // binding copies with memcpy)
                            let at = Ty::Ref(false, Box::new(st.strip_ref().clone()));
                            let n = self.new_tmp("m");
                            let d = self.cdecl(&at, &n);
                            self.emit(format!("{} = {};", d, v));
                            (n, SK::Val, false)
                        } else {
                            let ct = self.cdecl(&st.strip_ref().clone(), "");
                            let n = self.new_tmp("m");
                            self.emit(format!("{}{} = {};", ct, n, v));
                            (n, SK::Val, false)
                        }
                    }
                }
            }
        } else {
            let v = self.val(scrut);
            let n = self.new_tmp("m");
            let d = self.cdecl(&st, &n);
            self.emit(format!("{} = {};", d, v));
            let owns = self.s.t.needs_drop(&st);
            (n, SK::Val, owns)
        };
        // 場所の式の中で取った要素ポインタ（列の写しの作り直しに使う） / the element pointer kept inside the place expression
        // (used to rebuild the column mirrors)
        let place_w: Option<String> = if place_arena.is_some() {
            self.arena_cap = None;
            self.arena_cap_out.take()
        } else {
            None
        };
        let inner_ty = match st.strip_ref() {
            Ty::Option(i) => Some((**i).clone()),
            Ty::Result(i, _) => Some((**i).clone()),
            _ => None,
        };
        // 各アームの条件 / the condition of each arm
        let n_arms = arms.len();
        let mut first = true;
        let mut closed = false;
        let has_catch_all = arms.iter().any(|(p, _)| matches!(p, Pattern::Wild(_) | Pattern::Bind(..)));
        for (k, (pat, body)) in arms.iter().enumerate() {
            let is_last_needed = !has_catch_all && k + 1 == n_arms;
            let cond: Option<String> = match pat {
                Pattern::Wild(_) | Pattern::Bind(..) => None,
                Pattern::Int(v, _) => {
                    let it = match st.strip_ref() {
                        Ty::Int(i) => *i,
                        _ => IntTy::I32,
                    };
                    Some(format!("{} == {}", m, self.int_lit_c(*v, it, None)))
                }
                Pattern::Bool(b, _) => Some(if *b { m.clone() } else { format!("!{}", m) }),
                Pattern::None(_) | Pattern::Some(..) | Pattern::Ok(..) | Pattern::Err(..) => {
                    let some = match kind {
                        SK::Ptr => format!("{} != NULL", m),
                        SK::OptPtr => format!("{}->is_some", m),
                        SK::Val => match &inner_ty {
                            Some(i) if Self::opt_niche(i) && !is_res => match i {
                                Ty::Id(_) | Ty::Dma(_) | Ty::GpuBuf(_) => format!("{} != 0u", m),
                                Ty::Str => format!("{}.ptr != NULL", m),
                                _ => format!("{} != NULL", m),
                            },
                            _ => format!("{}.is_some", m),
                        },
                    };
                    if matches!(pat, Pattern::Some(..) | Pattern::Ok(..)) {
                        Some(some)
                    } else {
                        Some(negate(&some))
                    }
                }
            };
            // 網羅済みなら最後のアームは else / when exhaustive, the last arm is a plain else
            let cond = if is_last_needed && !first { None } else { cond };
            match (&cond, first) {
                (Some(c), true) => self.emit(format!("if ({}) {{", c)),
                (Some(c), false) => self.emit(format!("}} else if ({}) {{", c)),
                (None, true) => self.emit("{"),
                (None, false) => self.emit("} else {"),
            }
            first = false;
            self.ind += 1;
            self.scopes.push(Vec::new());
            // 束縛 / bindings
            let mut consumed = false;
            match pat {
                // `Err(e)`: 失敗値（コピーできる型）を値で受ける / the failure value (copyable), bound by value
                Pattern::Err(Some((_, b, _)), _) => {
                    let l = self.local(*b).clone();
                    let n = self.declare_local(*b);
                    let src = match kind {
                        SK::OptPtr => format!("{}->err", m),
                        _ => format!("{}.err", m),
                    };
                    let d = self.cdecl(&l.ty, &n);
                    self.emit(format!("{} = {};", d, src));
                }
                Pattern::Some(Some((_, b, _)), _) | Pattern::Bind(_, b, _) | Pattern::Ok(Some((_, b, _)), _) => {
                    let l = self.local(*b).clone();
                    let n = self.declare_local(*b);
                    let is_some_pat = matches!(pat, Pattern::Some(..) | Pattern::Ok(..));
                    let src = if !is_some_pat {
                        match (&whole, l.kind == LocalKind::PatRef) {
                            (Some(r), true) => r.clone(),
                            _ => m.clone(),
                        }
                    } else {
                        match kind {
                            SK::Ptr => m.clone(),
                            // 配列への参照は先頭要素へのポインタ（配列そのもののアドレスではない） / a reference to an
                            // array is the pointer to its first element (not the address of the array)
                            SK::OptPtr => match &l.ty {
                                Ty::Ref(_, x) if matches!(**x, Ty::Array(..)) => format!("{}->value", m),
                                _ => format!("&{}->value", m),
                            },
                            SK::Val => match &inner_ty {
                                Some(i) if Self::opt_niche(i) && !is_res => m.clone(),
                                _ => format!("{}.value", m),
                            },
                        }
                    };
                    let d = match (&l.ty, lookup_ts) {
                        (Ty::Array(..), _) => {
                            let d = self.cdecl(&l.ty, &n);
                            self.emit(format!("{};", d));
                            self.emit(format!("__builtin_memcpy({}, {}, sizeof {});", n, src, n));
                            String::new()
                        }
                        (Ty::Ref(m, _), Some(ta)) if is_some_pat && matches!(kind, SK::Ptr) => {
                            self.bind_arena.insert(*b, ta);
                            format!("{}{} *{}", if *m { "" } else { "const " }, self.ts_part(ta), n)
                        }
                        _ => self.cdecl(&l.ty, &n),
                    };
                    if !d.is_empty() {
                        self.emit(format!("{} = {};", d, src));
                    }
                    if l.kind == LocalKind::PatValue && owns {
                        consumed = true;
                    }
                    if let (Some(a), true) = (lookup_mut, is_some_pat) {
                        // 借用の終わり（このスコープのすべての出口）で、借用中の書き込みが届く列の写しを作り直す（届かなければ
                        // 何もしない）。借用中は F2502 がこのアリーナの読み出しを禁止しているので、古い写しは誰にも見えない
                        // when the borrow ends (every exit of this scope), rebuild the mirrors of the columns the writes made
                        // during the borrow can reach (nothing when none); F2502 forbids reading the arena during the borrow,
                        // so the stale mirror is never seen
                        let mut paths: Vec<Vec<String>> = Vec::new();
                        let known = match body {
                            ArmBody::B(bl) => self.ref_writes_block(*b, bl, &mut paths),
                            ArmBody::E(x) => self.ref_writes(*b, x, &mut paths),
                            ArmBody::Empty => true,
                        };
                        let mask = if known { self.col_mask(a, &paths) } else { self.col_all(a) };
                        if mask != 0 {
                            let call = self.mirror_call(a, &n, mask);
                            let c = self.c(
                                &format!("`{}` の &mut の借用の終わり → 書き込みが届く列の写しを作り直す", l.name),
                                &format!("end of the &mut borrow `{}` → rebuild the column mirrors its writes reach", l.name),
                            );
                            self.exit_mirror.insert(*b, format!("{} /* {} */", call, c));
                            self.scopes.last_mut().unwrap().push(*b);
                            self.report.mirror_sites += 1;
                        }
                    }
                    if let (Some(a), Some(w)) = (place_arena, place_w.clone()) {
                        if let Some(&(_, mask)) = place_mirror.iter().find(|(x, _)| *x == *b) {
                            // スロットの中の場所を指す &mut の束縛: 上と同じく、スコープのどの出口でも写しを作り直す
                            // a &mut binding pointing at a place inside a slot: as above, rebuild at every exit of its scope
                            let call = self.mirror_call(a, &w, mask);
                            let c = self.c(
                                &format!("`{}` の &mut の借用（スロットの中の場所）の終わり → 書き込みが届く列の写しを作り直す", l.name),
                                &format!("end of the &mut borrow `{}` (a place inside the slot) → rebuild the column mirrors its writes reach", l.name),
                            );
                            self.exit_mirror.insert(*b, format!("{} /* {} */", call, c));
                            self.scopes.last_mut().unwrap().push(*b);
                            self.report.mirror_sites += 1;
                        }
                    }
                }
                _ => {}
            }
            if owns && !consumed {
                let d = self.drop_code(&st, &m);
                if !d.is_empty() {
                    self.emit(format!("{} /* {} */", d, self.c("このアームでは使わない値を解放", "drop the value unused in this arm")));
                }
            }
            match body {
                ArmBody::B(b) => self.block_stmts(b, dest),
                ArmBody::E(x) => {
                    match &x.kind {
                        ExprKind::Block(b) => self.block_stmts(b, dest),
                        _ => {
                            self.tail_into(x, dest);
                            self.emit_scope_drops(self.scopes.len() - 1, "arm end");
                        }
                    }
                }
                ArmBody::Empty => {
                    self.emit_scope_drops(self.scopes.len() - 1, "arm end");
                    if self.lines.last().map_or(false, |l| l.trim() == "} else {") {
                        self.lines.pop();
                        self.ind -= 1;
                        self.scopes.pop();
                        self.emit("}");
                        closed = true;
                        break;
                    }
                }
            }
            self.scopes.pop();
            self.ind -= 1;
            if cond.is_none() {
                self.emit("}");
                closed = true;
                break;
            }
        }
        if !closed {
            self.emit("}");
        }
    }

    /// 回数の決まらないループ（loop）の各周回の先頭: ラッチが立っていればループを出る（出口のコミット点で止まる）。
    /// ループ自身の出口と同じ飛び先なので、C コンパイラはこの 2 本を 1 本の分岐にまとめられる
    /// the top of each iteration of a loop without a fixed trip count (`loop`): leave the loop if the latch is set (the
    /// commit point at the exit stops it); it jumps to the same place as the loop's own exits, so the C compiler can merge them
    fn loop_latch_exit(&mut self) {
        if self.flt_on() {
            self.report.folded_commits += 1;
            self.ind += 1;
            let c = self.c("ラッチ → ループの出口（出口のコミット点で止める）", "latch → loop exit (stopped by the commit point there)");
            self.emit(format!("if (FASCON_UNLIKELY(!F20_LATCH_OK(_flt))) break; /* {} */", c));
            self.ind -= 1;
        }
    }

    /// 回数の決まったループを出たところのコミット点（中には置かない） / the commit point at the exit of a loop with a fixed
    /// trip count (none inside)
    fn exit_commit(&mut self, site: &str) {
        if self.flt_on() {
            self.report.hoisted_commits += 1;
            self.commit(site);
        }
    }

    /// 本体（呼び出し先も含む）が @a の生存スロットの集合を変えるか（alloc / free / remove）
    /// whether the body (callees included) changes the set of live slots of @a (alloc / free / remove)
    fn body_changes_liveness(&self, a: usize, b: &Block) -> bool {
        fn ex(cg: &Cg, a: usize, e: &Expr) -> bool {
            match &e.kind {
                ExprKind::MethodCall { .. } => {
                    if let Some(MethodKind::ArenaAlloc(x)) | Some(MethodKind::ArenaFree(x)) | Some(MethodKind::ArenaRemove(x)) = cg.s.methods.get(&e.id) {
                        if *x == a {
                            return true;
                        }
                    }
                }
                ExprKind::Call { .. } => {
                    if let Some(CallKind::User(f)) = cg.s.calls.get(&e.id) {
                        let ef = &cg.s.t.fns[*f].effects;
                        if ef.frees.contains(&a) || ef.allocs.contains(&a) {
                            return true;
                        }
                    }
                }
                _ => {}
            }
            crate::flow::children(e).into_iter().any(|c| match c {
                crate::flow::Child::E(x) => ex(cg, a, x),
                crate::flow::Child::B(b) => bl(cg, a, b),
            })
        }
        fn bl(cg: &Cg, a: usize, b: &Block) -> bool {
            b.stmts.iter().any(|st| match st {
                Stmt::Let(l) => l.init.as_ref().map_or(false, |i| ex(cg, a, i)),
                Stmt::Expr(e) => ex(cg, a, e),
            }) || b.tail.as_ref().map_or(false, |t| ex(cg, a, t))
        }
        bl(self, a, b)
    }

    fn loop_stmt(&mut self, e: &Expr) {
        // 並列走査: 読み手として入る（ほかのコアの走査と同時に進む。書き手 = critical は待つ）。どの出口でも抜ける
        // a parallel scan enters as a reader (it runs alongside the scans of other cores; writers = critical wait); it
        // leaves at every exit
        if self.s.par_fors.contains_key(&e.id) && self.par_wrapped != Some(e.id) {
            let n = self.new_tmp("rs");
            let c = self.c("並列走査: 読み手の区間", "parallel scan: a reader section");
            self.emit("{");
            self.ind += 1;
            self.emit(format!("F20_READ_SECTION({}); /* par() — {} */", n, c));
            self.par_wrapped = Some(e.id);
            self.loop_stmt(e);
            self.par_wrapped = None;
            self.ind -= 1;
            self.emit("}");
            return;
        }
        let site = self.site(e.span);
        match &e.kind {
            ExprKind::While { cond, body } => {
                let saved = std::mem::take(&mut self.lines);
                let saved_ind = self.ind;
                self.ind = 0;
                let c = self.val(cond);
                let pre = std::mem::replace(&mut self.lines, saved);
                self.ind = saved_ind;
                let latch = self.flt_on();
                if latch {
                    self.report.folded_commits += 1;
                }
                if pre.is_empty() {
                    if latch {
                        // 条件を評価してからラッチを見る（&& は順序点）。C コンパイラは 2 つの比較を 1 本の分岐にまとめる
                        // evaluate the condition, then look at the latch (&& is a sequence point); the C compiler merges both
                        // compares into one branch
                        self.emit(format!(
                            "while (({}) && F20_LATCH_OK(_flt)) {{ /* {} */",
                            strip_all_parens(&c),
                            self.c("ラッチは終了条件に混ぜる（出口のコミット点で止める）", "the latch is folded into the exit condition (stopped by the commit point at the exit)")
                        ));
                    } else {
                        self.emit(format!("while ({}) {{", strip_all_parens(&c)));
                    }
                } else {
                    self.emit("for (;;) {");
                    self.ind += 1;
                    for l in pre {
                        self.emit(l);
                    }
                    if latch {
                        self.emit(format!("if (!(({}) && F20_LATCH_OK(_flt))) break;", c));
                    } else {
                        self.emit(format!("if (!({})) break;", c));
                    }
                    self.ind -= 1;
                }
                self.loops.push(LoopInfo::new(self.scopes.len(), None));
                self.gc_loop_poll();
                self.branch_block(body, None);
                self.loops.pop();
                self.emit("}");
                self.commit(&site);
            }
            ExprKind::Loop { body } => {
                self.emit("for (;;) {");
                self.loop_latch_exit();
                self.loops.push(LoopInfo::new(self.scopes.len(), None));
                self.gc_loop_poll();
                self.branch_block(body, None);
                self.loops.pop();
                self.emit("}");
                self.commit(&site);
            }
            ExprKind::For { bind, iter, body, .. } => match iter {
                ForIter::Range { lo, hi, inclusive } => {
                    let t = self.local(*bind).ty.clone();
                    let ct = self.cty(&t);
                    let l = self.val(lo);
                    let l = self.hoist(&t, &l);
                    let h = self.val(hi);
                    let h = self.hoist(&t, &h);
                    if !*inclusive {
                        // ループ変数は C の for の中だけで有効（外側の波括弧は不要） / the loop variable lives inside the C for
                        // 回数が決まっているので本体にコミット点は置かず、出口で 1 回 / fixed trip count: one commit at the exit
                        self.scopes.push(Vec::new());
                        let v = self.declare_local(*bind);
                        self.emit(format!("for ({} {} = {}; {} < {}; {}++) {{", ct, v, l, v, h, v));
                        self.loops.push(LoopInfo::new(self.scopes.len(), None));
                        self.gc_loop_poll();
                        self.branch_block(body, None);
                        self.loops.pop();
                        self.emit("}");
                        self.scopes.pop();
                        self.exit_commit(&site);
                        return;
                    }
                    self.emit("{");
                    self.ind += 1;
                    self.scopes.push(Vec::new());
                    let v = self.declare_local(*bind);
                    {
                        let lab = self.new_tmp("cont");
                        self.emit(format!("if ({} <= {}) {{", l, h));
                        self.ind += 1;
                        self.emit(format!("{} {} = {};", ct, v, l));
                        self.emit("for (;;) {");
                        self.loops.push(LoopInfo::new(self.scopes.len(), Some(lab.clone())));
                        self.gc_loop_poll();
                        self.branch_block(body, None);
                        self.loops.pop();
                        self.ind += 1;
                        self.emit(format!("{}:", lab));
                        self.emit(format!("if ({} == {}) break;", v, h));
                        self.emit(format!("{}++;", v));
                        self.ind -= 1;
                        self.emit("}");
                        self.ind -= 1;
                        self.emit("}");
                    }
                    self.scopes.pop();
                    self.ind -= 1;
                    self.emit("}");
                    self.exit_commit(&site);
                }
                ForIter::Arena(name, _) => {
                    let a = self.s.t.arena_of_name(name).unwrap_or(0);
                    if self.s.t.arenas[a].bump {
                        // バンプアリーナ: 生きているスロットは 0..top の連続した範囲（ビットマップも世代の検査も要らない）
                        // a bump arena: the live slots are the contiguous range 0..top (no bitmap, no generation check)
                        let idt = self.id_name(a);
                        let i = self.new_tmp("i");
                        self.emit(format!(
                            "/* for .. in {}: {} */",
                            name,
                            self.c(
                                "バンプアリーナは 0..top が生きているスロット。先頭から順に回す（本体が確保すれば top が伸び、その分も回る）",
                                "the live slots of a bump arena are 0..top; visit them in order (when the body allocates, top grows and those are visited too)"
                            )
                        ));
                        self.emit(format!("for (uint32_t {i} = 0u; {i} < arena_{n}.st.top; {i}++) {{", i = i, n = name));
                        self.ind += 1;
                        self.scopes.push(Vec::new());
                        let v = self.declare_local(*bind);
                        self.emit(format!("const {} {} = F20_BID_MAKE(arena_{n}.gen[{i}], {i}, ARENA_{n}_K);", idt, v, n = name, i = i));
                        self.loops.push(LoopInfo::new(self.scopes.len(), None));
                        self.gc_loop_poll();
                        self.emit("{");
                        self.branch_block(body, None);
                        self.emit("}");
                        self.loops.pop();
                        self.scopes.pop();
                        self.ind -= 1;
                        self.emit("}");
                        self.report.bump_loops += 1;
                        self.exit_commit(&site);
                        return;
                    }
                    if let Some(plan) = self.reduction_plan(a, *bind, body) {
                        // 浮動小数点の集計は列で走査できるときだけ（できなければ書いた順のふつうのループ = ビット一致）
                        // a float reduction only when columns serve it (otherwise the plain loop in written order = bit-identical)
                        if plan.fty.is_none() || self.col_use.contains_key(&e.id) {
                            self.emit_reduction(a, &plan, e);
                            return;
                        }
                    }
                    if !self.rmw_off {
                        if let Some(u) = self.rmw_use.get(&e.id).copied() {
                            self.emit_rmw(a, u, e, *bind, body);
                            self.exit_commit(&site);
                            return;
                        }
                    }
                    self.report.bitmap_loops += 1;
                    let idt = self.id_name(a);
                    let dynamic = self.body_changes_liveness(a, body);
                    let w = self.new_tmp("w");
                    let m = self.new_tmp("lm");
                    let i = self.new_tmp("i");
                    let brk = self.new_tmp("brk");
                    self.emit(format!(
                        "/* for .. in {}: {} */",
                        name,
                        self.c(
                            "生存ビットマップを 1 語（64 スロット）ずつ読み、次の生存スロットを ctz で求める（x86-64: TZCNT / AArch64: RBIT + CLZ）。スロットごとの分岐も link[] の読み出しもない",
                            "read the live bitmap one word (64 slots) at a time and find the next live slot with ctz (x86-64: TZCNT / AArch64: RBIT + CLZ); no per-slot branch, no link[] reads"
                        )
                    ));
                    let next = if dynamic {
                        // 本体が @a を確保・解放する: 語を読み直し、今の添字より上の生存スロットだけ残す（解放したものは飛ばし、
                        // 新しく確保したものは今より後ろなら回る = 以前の一般ループと同じ意味）
                        // the body allocates / frees in @a: reload the word and keep only live slots above the current index
                        // (freed ones are skipped, new ones further on are visited = the same meaning as before)
                        format!("{m} = arena_{n}.live[{w}] & f20_above_lowest64({m})", m = m, n = name, w = w)
                    } else {
                        format!("{m} = f20_blsr64({m})", m = m)
                    };
                    let note = if dynamic {
                        self.c("本体が生存スロットを変えるので語を読み直す", "the body changes live slots, so the word is reloaded")
                    } else {
                        self.c("最下位の 1 を消すだけ（BLSR）", "just clear the lowest set bit (BLSR)")
                    };
                    let start = self.rmw_start.take().unwrap_or_else(|| "0u".to_string());
                    self.emit(format!("for (uint32_t {w} = {start}; {w} < F20_LIVE_WORDS_USED(arena_{n}.st.next_unused); {w}++) {{", w = w, n = name, start = start));
                    self.ind += 1;
                    self.emit(format!("for (uint64_t {m} = arena_{n}.live[{w}]; {m} != 0u; {next}) {{ /* {note} */", m = m, n = name, w = w, next = next, note = note));
                    self.ind += 1;
                    self.emit(format!("const uint32_t {i} = ({w} << 6) | f20_ctz64({m});", i = i, w = w, m = m));
                    self.scopes.push(Vec::new());
                    let v = self.declare_local(*bind);
                    self.emit(format!("const {} {} = F20_ID_MAKE(arena_{n}.gen[{i}], {i}, ARENA_{n}_K);", idt, v, n = name, i = i));
                    if self.tsoa.get(a).map_or(false, |x| !x.is_empty()) {
                        self.loop_ix.insert(*bind, i.clone());
                        self.emit(format!("(void){}; /* {} */", v, self.c("証明済みの @a[id] はスロットの添字で引く（Id は使わないこともある）", "proven @a[id] accesses use the slot index (the Id may go unused)")));
                    }
                    self.loops.push(LoopInfo { depth: self.scopes.len(), cont_label: None, brk_label: Some(brk.clone()), brk_used: false });
                    self.gc_loop_poll();
                    // 完全列化の列への一括上書き: 大きな列なら非時間的ストア（出口に F20_SFENCE） / a bulk overwrite of True-SoA
                    // columns: non-temporal stores for large columns (F20_SFENCE at the exit)
                    let nt = match self.nt_fields(a, *bind, body) {
                        Some(fs) => {
                            let (on, ja, en) = self.nt_decide(a, &fs, self.nt_reread.contains(&e.id));
                            let line = self.line_of(e.span);
                            self.report.nt_loops.push((name.clone(), fs.join(", "), line, on, ja, en));
                            if on {
                                self.nt_cur = Some((a, fs));
                            }
                            on
                        }
                        None => false,
                    };
                    self.emit("{");
                    self.arena_loops.push((a, e.span));
                    self.branch_block(body, None);
                    self.arena_loops.pop();
                    self.emit("}");
                    self.nt_cur = None;
                    let li = self.loops.pop().unwrap();
                    self.scopes.pop();
                    self.ind -= 1;
                    self.emit("}");
                    self.ind -= 1;
                    self.emit("}");
                    if nt {
                        self.emit(format!("F20_SFENCE(); /* {} */", self.c("非時間的ストアを、このあとのストアより先に見えるようにする", "make the non-temporal stores visible before any later store")));
                    }
                    if li.brk_used {
                        self.emit(format!("{}:;", brk));
                    }
                    self.exit_commit(&site);
                }
            },
            _ => {}
        }
    }

    // ───────────── 条件付き一括更新（Masked Column RMW） / masked column RMW ─────────────

    /// `for id in @a { if 条件 { @a[id].f += x; } }` / `.. { @a[id].f = x; }` の形を読む: f は完全列化の 1 段のフィールド
    /// （u32 / u64 / i32 / i64、浮動小数点は #[fast_math] の下だけ）、x はループの中で変わらず失敗もしない値（リテラル・
    /// ローカル・static・定数とその as）、条件は条件のビット列にできるもの。表のアリーナで、ほかと共有しないものだけ
    /// read `for id in @a { if cond { @a[id].f += x; } }` / `.. { @a[id].f = x; }`: f is a single-step True-SoA field (u32 /
    /// u64 / i32 / i64; floats only under #[fast_math]), x a value that neither changes in the loop nor can fail (a literal,
    /// local, static or constant, possibly with `as`), and the condition one a filter bit column can hold; table arenas
    /// nobody else shares only
    fn rmw_plan<'b>(&self, a: usize, var: BindId, body: &'b Block) -> Option<(Vec<&'b Expr>, String, bool, &'b Expr)> {
        let x = &self.s.t.arenas[a];
        if x.bump || x.dma || x.gpu || x.gc || x.shared || x.task_stack.is_some() {
            return None;
        }
        if self.s.ctx.env_all.touches(a) || self.s.ctx.env_all.allocs.contains(&a) || self.s.ctx.shared.iter().any(|s| s.0 == a) {
            return None;
        }
        let one = single_expr(body)?;
        let ExprKind::If { cond, then, els: None } = &one.kind else { return None };
        let mut conds: Vec<&'b Expr> = Vec::new();
        flatten_and(cond, &mut conds);
        let act = single_expr(then)?;
        let ExprKind::Assign { op, lhs, rhs } = &act.kind else { return None };
        let add = match op {
            None => false,
            Some(BinOp::Add) => true,
            _ => return None,
        };
        let cx = RedCx { a, var, elem: None, acc: None, pick: None };
        let path = self.slot_path(lhs, &cx)?;
        if path.len() != 1 {
            return None;
        }
        self.ts_index(a, &path[0])?;
        match self.ty(lhs) {
            Ty::Int(IntTy::U32 | IntTy::U64 | IntTy::I32 | IntTy::I64) => {}
            Ty::Float(_) if self.fast_vars.contains(&var) => {}
            _ => return None,
        }
        if !self.rmw_value_ok(rhs, var) || !conds.iter().all(|c| self.cond_ok(c, &cx)) {
            return None;
        }
        Some((conds, path[0].clone(), add, &**rhs))
    }

    /// ループの中で変わらず、失敗しない値 / a value that does not change in the loop and cannot fail
    fn rmw_value_ok(&self, e: &Expr, var: BindId) -> bool {
        match &e.kind {
            ExprKind::Int(..) | ExprKind::Float(..) | ExprKind::Bool(_) => true,
            ExprKind::Unary(UnOp::Neg, x) => matches!(x.kind, ExprKind::Int(..) | ExprKind::Float(..)),
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Local(b)) => *b != var,
                Some(Res::Static(_)) | Some(Res::FloatConst(..)) => true,
                _ => false,
            },
            ExprKind::Cast(x, _) => self.rmw_value_ok(x, var),
            _ => false,
        }
    }

    /// 条件付き一括更新に条件のビット列を割り当てる（条件が完全列化のフィールドを読む・外のローカルを読む・列を作れない・
    /// 書き込みの多いフィールドを読むときはふつうのループのまま） / give a masked column RMW its filter bit column (it stays a
    /// plain loop when the condition reads a True-SoA field or an outer local, columns are vetoed, or it reads a write-heavy
    /// field)
    fn plan_rmw(&mut self, a: usize, var: BindId, conds: Vec<&Expr>, f: &str, add: bool, e: &Expr) {
        let cx = RedCx { a, var, elem: None, acc: None, pick: None };
        let mut caps: Vec<BindId> = Vec::new();
        let cond = conds.iter().map(|c| self.cond_c(c, &cx, "(*v)", &mut caps)).collect::<Vec<_>>().join(" & ");
        if !caps.is_empty() {
            return;
        }
        let mut deps: Vec<Vec<String>> = Vec::new();
        for c in &conds {
            self.cond_paths(c, &cx, &mut deps);
        }
        if deps.iter().any(|p| p.first().map_or(false, |x| self.ts_index(a, x).is_some())) {
            return;
        }
        let heavy = self.ts_heavy.get(a).map_or(false, |h| deps.iter().filter_map(|p| p.first()).any(|x| h.contains_key(x)));
        if heavy || self.column_veto(a, &deps).is_some() {
            return;
        }
        let Some(fi) = self.ts_index(a, f) else { return };
        let line = self.line_of(e.span);
        let k = self.add_col(a, ColKind::Bits, String::new(), cond, deps, line, &[]);
        self.rmw_use.insert(e.id, RmwUse { f: fi, mask: k, add });
    }

    /// 条件付き一括更新を出す: 条件のビット列を語ごとに読む走査関数 1 回（整数の加算があふれる語があれば、その語から逐次の
    /// ループで続けて同じ panic にする） / emit a masked column RMW: one call of the scan function that reads the filter bit
    /// column word by word (when an integer add would overflow in some word, the sequential loop continues from that word
    /// and panics the same way)
    fn emit_rmw(&mut self, a: usize, u: RmwUse, e: &Expr, var: BindId, body: &Block) {
        let an = self.s.t.arenas[a].name.clone();
        let Some((_, f, _, x)) = self.rmw_plan(a, var, body) else { return };
        let tf = self.tsoa[a][u.f].clone();
        let (suf, cty) = match (tf.fty, tf.it) {
            (Some(FloatTy::F32), _) => ("f32", "float"),
            (Some(FloatTy::F64), _) => ("f64", "double"),
            (None, IntTy::U32) => ("u32", "uint32_t"),
            (None, IntTy::I32) => ("i32", "int32_t"),
            (None, IntTy::I64) => ("i64", "int64_t"),
            _ => ("u64", "uint64_t"),
        };
        let mask = self.arena_cols[a][u.mask].name.clone();
        let line = self.line_of(e.span);
        let (nt, _, _) = self.nt_decide(a, &[f.clone()], self.nt_reread.contains(&e.id));
        let kernel = format!("f20_col_masked_{}_{}", if u.add { "add" } else { "set" }, suf);
        self.report.rmw_loops.push((an.clone(), f.clone(), line, kernel.clone(), mask.clone(), nt));
        let xs = self.val(x);
        let xn = self.new_tmp("x");
        let nn = self.new_tmp("n");
        self.emit(format!(
            "/* for .. in {}: {} */",
            an,
            self.c(
                &format!("条件付き一括更新（Masked Column RMW）: 条件のビット列 {} を 1 語（64 スロット）ずつ読み、0 の語は列 ts_{} の行に触れず、全部 1 の語はマスクなしのベクトル演算{}、それ以外は述語付きのベクトル演算でその場で更新（{}）", mask, tf.name, if nt { "と非時間的ストア" } else { "" }, kernel),
                &format!("masked column RMW: read the filter bit column {} one word (64 slots) at a time; all-zero words never touch the lines of column ts_{}, all-one words use an unmasked vector operation{}, the rest a predicated vector update in place ({})", mask, tf.name, if nt { " with non-temporal stores" } else { "" }, kernel)
            )
        ));
        self.emit("{");
        self.ind += 1;
        self.emit(format!("const {} {} = {};", cty, xn, strip_parens(&xs)));
        self.emit(format!("const uint32_t {} = F20_COL_SCAN_END(arena_{}.st.next_unused);", nn, an));
        let int_add = u.add && tf.fty.is_none();
        if int_add {
            let stop = self.new_tmp("stop");
            self.emit(format!("const uint32_t {} = {}(arena_{}.col.ts_{}, arena_{}.col.{}, {}, {}, {});", stop, kernel, an, tf.name, an, mask, nn, xn, if nt { 1 } else { 0 }));
            self.emit(format!("if (FASCON_UNLIKELY({} != F20_RMW_NONE)) {{ /* {} */", stop, self.c("あふれる語: そこから逐次のループで続ける（その前の語は更新済み、その語より後は手つかず。同じ場所で同じ panic になる）", "an overflowing word: continue from it with the sequential loop (earlier words are updated, it and later ones untouched; the panic happens at the same place, the same way)")));
            self.ind += 1;
            self.rmw_start = Some(format!("({} >> 6)", stop));
            self.rmw_off = true;
            self.loop_stmt(e);
            self.rmw_off = false;
            self.rmw_start = None;
            self.ind -= 1;
            self.emit("}");
        } else {
            self.emit(format!("{}(arena_{}.col.ts_{}, arena_{}.col.{}, {}, {}, {});", kernel, an, tf.name, an, mask, nn, xn, if nt { 1 } else { 0 }));
        }
        if nt {
            self.emit(format!("F20_SFENCE(); /* {} */", self.c("非時間的ストアを、このあとのストアより先に見えるようにする", "make the non-temporal stores visible before any later store")));
        }
        self.ind -= 1;
        self.emit("}");
    }

    // ───────────── 総和の特殊化 / the sum specialization ─────────────

    fn local_of(&self, e: &Expr) -> Option<BindId> {
        match (&e.kind, self.s.res.get(&e.id)) {
            (ExprKind::Path(_), Some(Res::Local(b))) => Some(*b),
            _ => None,
        }
    }

    /// `for id in @a { 本体 }` を表の直線走査（分岐なし）にできるなら、その形を返す。
    ///   Sum:    `acc += 項 + ..` / `acc = acc + 項 + ..`（項は生存スロットの符号なし整数フィールド。拡大キャスト可）
    ///   Count:  `acc += 定数`（= 条件を満たす生存スロットの数 × 定数）
    ///   MinMax: `if フィールド < acc { acc = フィールド; pick = Some(id); }`（< <= > >=。pick は省略可）
    /// どれも `if 条件 { .. }`（入れ子・&& 可・else なし）で絞り込める。条件にはスロットのフィールド・整数 / bool の定数・
    /// static・外のローカルの比較と、論理演算・ビット演算だけを使える（純粋なので死んだスロットの 0 に対して評価してもよい）。
    /// 全体を `if let Some(t) = @a.get(id) { .. }` で包んでもよい。
    /// returns the shape when `for id in @a { body }` can become a straight, branch-free table scan:
    ///   Sum:    `acc += term + ..` / `acc = acc + term + ..` (terms: unsigned integer fields of the live slot, widening casts allowed)
    ///   Count:  `acc += constant` (= the number of qualifying live slots × the constant)
    ///   MinMax: `if field < acc { acc = field; pick = Some(id); }` (< <= > >=; pick is optional)
    /// each may be filtered by `if cond { .. }` (nested, &&, no else); a condition may only compare slot fields, integer /
    /// bool constants, statics and outer locals, with logical and bitwise operators (pure, so evaluating it on a dead,
    /// all-zero slot is harmless); the whole body may be wrapped in `if let Some(t) = @a.get(id) { .. }`.
    fn reduction_plan<'b>(&self, a: usize, var: BindId, body: &'b Block) -> Option<RedPlan<'b>> {
        // バンプアリーナは [0, top) が全部生きているので、ふつうの直線のループで足りる（集計の特殊化・列は表のアリーナだけ）
        // in a bump arena all of [0, top) is live, so a plain straight loop suffices (reductions and columns are for table arenas)
        if self.s.t.arenas[a].bump {
            return None;
        }
        let one = single_expr(body)?;
        let (inner, elem_bind) = match &one.kind {
            ExprKind::IfLet { pat: Pattern::Some(Some((_, tb, _)), _), scrut, then, els: None } => {
                if !self.s.lookup_scrut.contains(&scrut.id) || !self.fr.proven.contains(&scrut.id) {
                    return None;
                }
                match (&scrut.kind, self.s.methods.get(&scrut.id)) {
                    (ExprKind::MethodCall { args, .. }, Some(MethodKind::ArenaGet(x))) | (ExprKind::MethodCall { args, .. }, Some(MethodKind::ArenaGetMut(x))) if *x == a => {
                        if args.len() != 1 || self.local_of(&args[0]) != Some(var) {
                            return None;
                        }
                    }
                    _ => return None,
                }
                (single_expr(then)?, Some(*tb))
            }
            _ => (one, None),
        };
        // 入れ子の if（else なし）を条件の列にほどく / unwrap nested ifs (without else) into a list of conditions
        let mut conds: Vec<&'b Expr> = Vec::new();
        let mut cur = inner;
        let action: Vec<&'b Expr> = loop {
            match &cur.kind {
                ExprKind::If { cond, then, els: None } => {
                    flatten_and(cond, &mut conds);
                    if let Some(y) = single_expr(then) {
                        if let ExprKind::If { els: None, .. } = &y.kind {
                            cur = y;
                            continue;
                        }
                    }
                    break block_exprs(then)?;
                }
                _ => break vec![cur],
            }
        };
        let cx0 = RedCx { a, var, elem: elem_bind, acc: None, pick: None };
        if action.len() == 1 {
            if let Some((acc, acc_it, kind, terms, c)) = self.sum_action(action[0], &cx0) {
                let cx = RedCx { acc: Some(acc), ..cx0 };
                if !conds.iter().all(|x| self.cond_ok(x, &cx)) {
                    return None;
                }
                let fty = terms.first().and_then(|t: &RedTerm| t.fty);
                return Some(RedPlan { acc, acc_it, fty, kind, terms, count_c: c, conds, lookup: elem_bind.is_some(), elem_bind, var, less: false, strict: false, pick: None });
            }
        }
        self.minmax_plan(&cx0, conds, &action)
    }

    /// 足し込みの文 `acc += ..` を読む（Sum か Count） / read the accumulation `acc += ..` (Sum or Count)
    fn sum_action(&self, x: &Expr, cx: &RedCx) -> Option<(BindId, IntTy, RedKind, Vec<RedTerm>, u128)> {
        let (acc, leaves) = match &x.kind {
            ExprKind::Assign { op: Some(BinOp::Add), lhs, rhs } => {
                let acc = self.local_of(lhs)?;
                let mut v = Vec::new();
                flatten_add(rhs, &mut v);
                (acc, v)
            }
            ExprKind::Assign { op: None, lhs, rhs } => {
                let acc = self.local_of(lhs)?;
                let mut v = Vec::new();
                flatten_add(rhs, &mut v);
                let pos: Vec<usize> = v.iter().enumerate().filter(|(_, x)| self.local_of(x) == Some(acc)).map(|(i, _)| i).collect();
                if pos.len() != 1 {
                    return None;
                }
                v.remove(pos[0]);
                (acc, v)
            }
            _ => return None,
        };
        if leaves.is_empty() || leaves.len() > 16 || acc == cx.var || Some(acc) == cx.elem {
            return None;
        }
        let li = self.local(acc);
        if li.kind != LocalKind::Let {
            return None;
        }
        // 浮動小数点の総和（#[fast_math] の範囲だけ。項は同じ型のフィールド、キャストなし）: 列の走査は並列のレーンで
        // 足すので結合の順序が変わる。属性がなければ書いた順のループのまま（ビット一致）
        // a float sum (only inside #[fast_math]; terms are fields of the same type, no casts): the column scan adds in
        // parallel lanes, which changes the association order; without the attribute the loop stays in written order
        // (bit-identical)
        if let Ty::Float(ft) = li.ty {
            if !self.fast_vars.contains(&cx.var) {
                return None;
            }
            let mut terms = Vec::new();
            for leaf in leaves {
                if self.ty(leaf) != Ty::Float(ft) {
                    return None;
                }
                let tcx = RedCx { a: cx.a, var: cx.var, elem: cx.elem, acc: None, pick: None };
                let path = self.slot_path(leaf, &tcx)?;
                terms.push(RedTerm { path, it: ft.bits_int(), fty: Some(ft) });
            }
            return Some((acc, ft.bits_int(), RedKind::Sum, terms, 0));
        }
        let acc_it = match li.ty {
            Ty::Int(it) if !it.signed() => it,
            _ => return None,
        };
        // 定数だけなら Count / constants only: Count
        if leaves.iter().all(|l| matches!(l.kind, ExprKind::Int(..)) && !self.s.neg_lits.contains(&l.id)) {
            let mut c: u128 = 0;
            for l in &leaves {
                if let ExprKind::Int(v, _) = l.kind {
                    c = c.checked_add(v)?;
                }
            }
            if c == 0 || c > u64::MAX as u128 {
                return None;
            }
            return Some((acc, acc_it, RedKind::Count, Vec::new(), c));
        }
        let mut terms = Vec::new();
        for leaf in leaves {
            terms.push(self.red_term(leaf, cx.a, cx.var, cx.elem, acc_it)?);
        }
        Some((acc, acc_it, RedKind::Sum, terms, 0))
    }

    /// `if フィールド < acc { acc = フィールド; pick = Some(id); }` を読む / read the min / max-with-pick shape
    fn minmax_plan<'b>(&self, cx0: &RedCx, mut conds: Vec<&'b Expr>, action: &[&'b Expr]) -> Option<RedPlan<'b>> {
        if action.is_empty() || action.len() > 2 {
            return None;
        }
        let mut acc: Option<BindId> = None;
        let mut field: Option<&Expr> = None;
        let mut pick: Option<BindId> = None;
        for x in action {
            match &x.kind {
                ExprKind::Assign { op: None, lhs, rhs } => {
                    let l = self.local_of(lhs)?;
                    if self.is_id_of_var(rhs, cx0.var) {
                        if pick.is_some() {
                            return None;
                        }
                        pick = Some(l);
                    } else {
                        if acc.is_some() {
                            return None;
                        }
                        acc = Some(l);
                        field = Some(&**rhs);
                    }
                }
                _ => return None,
            }
        }
        let (acc, field) = (acc?, field?);
        if Some(acc) == pick || acc == cx0.var || Some(acc) == cx0.elem {
            return None;
        }
        let li = self.local(acc);
        if li.kind != LocalKind::Let {
            return None;
        }
        // 浮動小数点の最小 / 最大（#[fast_math] の範囲だけ）: 鍵の列の走査（NaN は選ばれず、-0.0 と +0.0 は同じ鍵）
        // a float min / max (only inside #[fast_math]): a key-column scan (NaN is never picked, -0.0 and +0.0 share a key)
        let fty = match li.ty {
            Ty::Float(ft) if self.fast_vars.contains(&cx0.var) => Some(ft),
            Ty::Float(_) => return None,
            _ => None,
        };
        let acc_it = match (li.ty.clone(), fty) {
            (Ty::Int(it), _) => it,
            (_, Some(ft)) => ft.bits_int(),
            _ => return None,
        };
        let acc_ty = match fty {
            Some(ft) => Ty::Float(ft),
            None => Ty::Int(acc_it),
        };
        if let Some(p) = pick {
            let pl = self.local(p);
            if pl.kind != LocalKind::Let || p == cx0.var || Some(p) == cx0.elem {
                return None;
            }
            let ok = match &pl.ty {
                Ty::Id(x) => *x == cx0.a,
                Ty::Option(i) => matches!(&**i, Ty::Id(x) if *x == cx0.a),
                _ => false,
            };
            if !ok {
                return None;
            }
        }
        let cx = RedCx { acc: Some(acc), pick, ..*cx0 };
        if self.ty(field) != acc_ty {
            return None;
        }
        let path = self.slot_path(field, &cx)?;
        // 条件の中から acc との比較を 1 個だけ探す / find exactly one comparison against acc among the conditions
        let mut found: Option<(usize, BinOp)> = None;
        for (k, c) in conds.iter().enumerate() {
            if let ExprKind::Binary(op, l, r) = &c.kind {
                let (fe, op2) = if self.local_of(r) == Some(acc) {
                    (&**l, *op)
                } else if self.local_of(l) == Some(acc) {
                    (&**r, mirror_cmp(*op))
                } else {
                    continue;
                };
                if !matches!(op2, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                    return None;
                }
                if self.slot_path(fe, &cx).as_ref() != Some(&path) || self.ty(fe) != acc_ty {
                    return None;
                }
                if found.is_some() {
                    return None;
                }
                found = Some((k, op2));
            }
        }
        let (k, op) = found?;
        conds.remove(k);
        if !conds.iter().all(|x| self.cond_ok(x, &cx)) {
            return None;
        }
        Some(RedPlan {
            acc,
            acc_it,
            fty,
            kind: RedKind::MinMax,
            terms: vec![RedTerm { path, it: acc_it, fty }],
            count_c: 0,
            conds,
            lookup: cx0.elem.is_some(),
            elem_bind: cx0.elem,
            var: cx0.var,
            less: matches!(op, BinOp::Lt | BinOp::Le),
            strict: matches!(op, BinOp::Lt | BinOp::Gt),
            pick,
        })
    }

    /// `id` か `Some(id)`（id はループ変数） / `id` or `Some(id)` where id is the loop variable
    fn is_id_of_var(&self, e: &Expr, var: BindId) -> bool {
        if self.local_of(e) == Some(var) {
            return true;
        }
        if let ExprKind::Call { args, .. } = &e.kind {
            if matches!(self.s.calls.get(&e.id), Some(CallKind::Some)) && args.len() == 1 {
                return self.local_of(&args[0]) == Some(var);
            }
        }
        false
    }

    /// 要素のフィールドへの道筋（`@a[id].f.g` か `t.f.g`。途中は値として埋め込んだ構造体だけ）
    /// the path to a field of the element (`@a[id].f.g` or `t.f.g`; intermediate bases must be inline structs)
    fn slot_path(&self, e: &Expr, cx: &RedCx) -> Option<Vec<String>> {
        let mut path = Vec::new();
        let mut cur = e;
        while let ExprKind::Field(base, name, _) = &cur.kind {
            path.push(sanitize_field(name));
            cur = base;
            if let ExprKind::Field(..) = &cur.kind {
                if !matches!(self.ty(cur), Ty::Struct(_)) {
                    return None;
                }
            }
        }
        path.reverse();
        if path.is_empty() {
            return None;
        }
        match &cur.kind {
            ExprKind::Index(b, idx) => {
                if self.s.arena_index.get(&cur.id) != Some(&cx.a) || !matches!(b.kind, ExprKind::ArenaRef(_)) || self.local_of(idx) != Some(cx.var) || !self.fr.proven.contains(&cur.id) {
                    return None;
                }
            }
            ExprKind::Path(_) => {
                if cx.elem.is_none() || self.local_of(cur) != cx.elem {
                    return None;
                }
            }
            _ => return None,
        }
        Some(path)
    }

    /// 絞り込みの条件として使えるか / can this be a filter condition?
    fn cond_ok(&self, e: &Expr, cx: &RedCx) -> bool {
        match &e.kind {
            ExprKind::Binary(op, l, r) => match op {
                BinOp::And | BinOp::Or => self.cond_ok(l, cx) && self.cond_ok(r, cx),
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    matches!(self.ty(l), Ty::Int(_) | Ty::Bool | Ty::Float(_)) && self.cond_val_ok(l, cx) && self.cond_val_ok(r, cx)
                }
                _ => false,
            },
            ExprKind::Unary(UnOp::Not, x) if self.ty(x) == Ty::Bool => self.cond_ok(x, cx),
            _ => self.ty(e) == Ty::Bool && self.cond_val_ok(e, cx),
        }
    }

    fn cond_val_ok(&self, e: &Expr, cx: &RedCx) -> bool {
        let scalar = |t: Ty| matches!(t, Ty::Int(_) | Ty::Bool | Ty::Float(_));
        match &e.kind {
            ExprKind::Int(..) | ExprKind::Bool(_) | ExprKind::Float(..) => true,
            ExprKind::Unary(UnOp::Neg, x) if matches!(x.kind, ExprKind::Float(..)) => true,
            ExprKind::Unary(UnOp::Neg, x) => matches!(x.kind, ExprKind::Int(..)),
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Static(si)) => scalar(self.s.t.statics[*si].ty.clone()),
                Some(Res::Local(b)) => {
                    *b != cx.var && Some(*b) != cx.elem && Some(*b) != cx.acc && Some(*b) != cx.pick && scalar(self.local(*b).ty.clone()) && !self.s.auto_deref.contains(&e.id)
                }
                _ => false,
            },
            ExprKind::Field(..) => scalar(self.ty(e)) && self.slot_path(e, cx).is_some(),
            ExprKind::Cast(x, _) => matches!(self.ty(e), Ty::Int(_)) && matches!(self.ty(x), Ty::Int(_)) && self.cond_val_ok(x, cx),
            ExprKind::Binary(BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor, l, r) => matches!(self.ty(e), Ty::Int(_)) && self.cond_val_ok(l, cx) && self.cond_val_ok(r, cx),
            ExprKind::Unary(UnOp::Not, x) => scalar(self.ty(e)) && self.cond_val_ok(x, cx),
            _ => false,
        }
    }

    /// 条件を C に（`place` は 1 スロットの要素、例 `p[j + 1u].v`。&& / || は & / | にする: 副作用がないので同じ値）
    /// a condition in C (`place` is one slot's element, e.g. `p[j + 1u].v`; && / || become & / |: no side effects, same value)
    fn cond_c(&self, e: &Expr, cx: &RedCx, place: &str, caps: &mut Vec<BindId>) -> String {
        match &e.kind {
            ExprKind::Binary(op, l, r) => {
                let lc = self.cond_c(l, cx, place, caps);
                let rc = self.cond_c(r, cx, place, caps);
                match op {
                    BinOp::And => format!("({} & {})", lc, rc),
                    BinOp::Or => format!("({} | {})", lc, rc),
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => match self.ty(e) {
                        Ty::Int(it) => format!("(({})({} {} {}))", it.c_name(), lc, op.text(), rc),
                        _ => format!("({} {} {})", lc, op.text(), rc),
                    },
                    _ => format!("({} {} {})", lc, op.text(), rc),
                }
            }
            ExprKind::Unary(UnOp::Not, x) => {
                let xc = self.cond_c(x, cx, place, caps);
                match self.ty(e) {
                    Ty::Int(it) => format!("(({})~{})", it.c_name(), xc),
                    _ => format!("(!{})", xc),
                }
            }
            ExprKind::Unary(UnOp::Neg, x) => match (&x.kind, self.ty(e)) {
                (ExprKind::Int(v, _), Ty::Int(it)) => self.int_lit_c(-(*v as i128), it, None),
                (ExprKind::Float(t, _), ty @ Ty::Float(_)) => format!("(-{})", float_lit_c(t, &ty)),
                _ => "0".to_string(),
            },
            ExprKind::Float(t, _) => float_lit_c(t, &self.ty(e)),
            ExprKind::Cast(x, _) => {
                let xc = self.cond_c(x, cx, place, caps);
                match self.ty(e) {
                    Ty::Int(it) => format!("(({}){})", it.c_name(), xc),
                    _ => xc,
                }
            }
            ExprKind::Int(v, _) => {
                let val = if self.s.neg_lits.contains(&e.id) { -(*v as i128) } else { *v as i128 };
                match self.ty(e) {
                    Ty::Int(it) => self.int_lit_c(val, it, Some(e.span)),
                    _ => format!("{}", val),
                }
            }
            ExprKind::Bool(b) => if *b { "1".to_string() } else { "0".to_string() },
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Static(si)) => format!("fsc_{}", self.s.t.statics[*si].name),
                Some(Res::Local(b)) => {
                    let k = match caps.iter().position(|x| x == b) {
                        Some(k) => k,
                        None => {
                            caps.push(*b);
                            caps.len() - 1
                        }
                    };
                    format!("_c{}", k)
                }
                _ => "0".to_string(),
            },
            ExprKind::Field(..) => {
                let path = self.slot_path(e, cx).unwrap_or_default();
                format!("{}{}", place, path.iter().map(|p| format!(".{}", p)).collect::<String>())
            }
            _ => "0".to_string(),
        }
    }

    fn red_term(&self, e: &Expr, a: usize, var: BindId, elem_bind: Option<BindId>, acc_it: IntTy) -> Option<RedTerm> {
        let pb = self.o.ptr_bits;
        let uint = |t: Ty| match t {
            Ty::Int(it) if !it.signed() => Some(it),
            _ => None,
        };
        if uint(self.ty(e))?.bits(pb) > acc_it.bits(pb) {
            return None;
        }
        // 外側のキャストは符号なしの拡大だけ / outer casts: unsigned widening only
        let mut x = e;
        while let ExprKind::Cast(inner, _) = &x.kind {
            let to = uint(self.ty(x))?;
            let from = uint(self.ty(inner))?;
            if from.bits(pb) > to.bits(pb) {
                return None;
            }
            x = inner;
        }
        let it = uint(self.ty(x))?;
        let cx = RedCx { a, var, elem: elem_bind, acc: None, pick: None };
        let path = self.slot_path(x, &cx)?;
        Some(RedTerm { path, it, fty: None })
    }

    // ───────────── 列（集計の列・ビット列・鍵の列） / columns (aggregate, bit and key columns) ─────────────

    /// 全関数の `for id in @a` のうち集計になるものを探し、列で走査できるものに列を割り当てる。
    /// 条件が外のローカルを読むもの（前もって作れない）と、64 ビットの `<=` / `>=` の選択（鍵が NONE とぶつかる）は 1.0.0 の走査のまま
    /// find every `for id in @a` that is a reduction and give the ones a column can serve their columns; reductions whose
    /// filter reads a local (cannot be built ahead of time) and 64-bit `<=` / `>=` picks (the key would collide with NONE)
    /// keep the 1.0.0 scan
    fn collect_columns(&mut self) {
        self.arena_cols = (0..self.s.t.arenas.len()).map(|_| Vec::new()).collect();
        self.mirror_masks = (0..self.s.t.arenas.len()).map(|_| BTreeSet::new()).collect();
        self.col_use.clear();
        if !self.o.columns {
            return;
        }
        // 完全列化するフィールドを先に決める（`--columns mirrored` なら何もしない） / decide the True-SoA fields first
        // (nothing with `--columns mirrored`)
        if self.o.true_soa {
            self.plan_tsoa();
        } else {
            self.tsoa = (0..self.s.t.arenas.len()).map(|_| Vec::new()).collect();
            self.ts_heavy = (0..self.s.t.arenas.len()).map(|_| BTreeMap::new()).collect();
        }
        let s = self.s;
        let prog = s.prog;
        for f in &s.t.fns {
            if let Item::Fn(fd) = &prog.items[f.item] {
                self.cols_block(&fd.body);
            }
        }
        // fascon-gc: gc_arena の Id のフィールドに Id 専用の射影列 / Id-only projection columns for gc arena Id fields
        self.gc_columns();
        // 報告 / the report
        let mut rows = Vec::new();
        for (ai, cols) in self.arena_cols.iter().enumerate() {
            let a = &self.s.t.arenas[ai];
            let len = {
                let k = arena_k(a.cap);
                (1u64 << k).max(64) + 1
            };
            for f in &self.tsoa[ai] {
                let w = f.it.bits(self.o.ptr_bits) as u64 / 8;
                rows.push(ColReport {
                    arena: a.name.clone(),
                    name: format!("ts_{}", f.name),
                    per_slot: format!("{} B", w),
                    bytes: len * w,
                    kind_ja: format!("値の列（{}）", f.fty.map_or(f.it.name(), |ft| ft.name())),
                    kind_en: format!("value column ({})", f.fty.map_or(f.it.name(), |ft| ft.name())),
                    lines: f.lines.clone(),
                    mode_ja: "True-SoA (single-write)".to_string(),
                    mode_en: "True-SoA (single-write)".to_string(),
                });
            }
            for c in cols {
                let (per, bytes, ja, en) = match c.kind {
                    ColKind::Agg { wide: true } => ("8 B", len * 8, "集計の列（u64）", "aggregate column (u64)"),
                    ColKind::Agg { wide: false } => ("4 B", len * 4, "集計の列（u32）", "aggregate column (u32)"),
                    ColKind::Bits => ("1 bit", (len - 1) / 64 * 8 + 8, "絞り込みのビット列", "filter bit column"),
                    ColKind::Key { max: false, .. } => ("8 B", len * 8, "選択の鍵の列（最小）", "pick-key column (min)"),
                    ColKind::Key { max: true, .. } => ("8 B", len * 8, "選択の鍵の列（最大）", "pick-key column (max)"),
                    ColKind::FKey { max: false, .. } => ("8 B", len * 8, "浮動小数点の鍵の列（最小）", "float pick-key column (min)"),
                    ColKind::FKey { max: true, .. } => ("8 B", len * 8, "浮動小数点の鍵の列（最大）", "float pick-key column (max)"),
                };
                rows.push(ColReport {
                    arena: a.name.clone(),
                    name: c.name.clone(),
                    per_slot: per.to_string(),
                    bytes,
                    kind_ja: ja.to_string(),
                    kind_en: en.to_string(),
                    lines: c.lines.clone(),
                    mode_ja: "Mirrored-SoA (dual-write)".to_string(),
                    mode_en: "Mirrored-SoA (dual-write)".to_string(),
                });
            }
        }
        self.report.columns = rows;
    }

    fn cols_block(&mut self, b: &'p Block) {
        for st in &b.stmts {
            match st {
                Stmt::Let(l) => {
                    if let Some(i) = &l.init {
                        self.cols_expr(i);
                    }
                }
                Stmt::Expr(e) => self.cols_expr(e),
            }
        }
        if let Some(t) = &b.tail {
            self.cols_expr(t);
        }
    }

    fn cols_expr(&mut self, e: &'p Expr) {
        if let ExprKind::For { bind, iter: ForIter::Arena(name, _), body, .. } = &e.kind {
            if let Some(a) = self.s.t.arena_of_name(name) {
                if let Some(plan) = self.reduction_plan(a, *bind, body) {
                    self.plan_columns(a, &plan, e);
                } else if !self.s.par_fors.contains_key(&e.id) {
                    if let Some((conds, f, add, _)) = self.rmw_plan(a, *bind, body) {
                        self.plan_rmw(a, *bind, conds, &f, add, e);
                    }
                }
            }
        }
        for c in crate::flow::children(e) {
            match c {
                crate::flow::Child::E(x) => self.cols_expr(x),
                crate::flow::Child::B(b) => self.cols_block(b),
            }
        }
    }

    /// 1 つの集計に列を割り当てる / assign columns to one reduction
    fn plan_columns(&mut self, a: usize, plan: &RedPlan, e: &Expr) {
        let pb = self.o.ptr_bits;
        let cx = RedCx { a, var: plan.var, elem: plan.elem_bind, acc: Some(plan.acc), pick: plan.pick };
        let mut caps: Vec<BindId> = Vec::new();
        let cond = plan.conds.iter().map(|c| self.cond_c(c, &cx, "(*v)", &mut caps)).collect::<Vec<_>>().join(" & ");
        if !caps.is_empty() {
            // 条件が外のローカルを読む: 書き込みの時点では値が分からない / the filter reads a local: unknown at write time
            return;
        }
        let mut deps: Vec<Vec<String>> = Vec::new();
        for c in &plan.conds {
            self.cond_paths(c, &cx, &mut deps);
        }
        let line = self.line_of(e.span);
        // atomic で更新されるフィールド・#[no_column] のフィールド・#[stable_abi] の要素が関わる集計は、エラーにせず列を作らない
        // （生存ビットマップ + スロットを直接読むフェーズ 1 の走査へ自動で戻す。理由は fascon regions に出る）
        // reductions touching fields updated under atomic, #[no_column] fields or #[stable_abi] elements get no columns and
        // no error (they fall back to the phase-1 scan of the live bitmap + slots; the reason shows in fascon regions)
        let mut all: Vec<Vec<String>> = deps.clone();
        for t in &plan.terms {
            all.push(t.path.clone());
        }
        let veto = self.column_veto(a, &all).or_else(|| {
            // 書き込みの多いフィールド（写しの列に残るもの）: 二重書き込みのほうが走査の利益より重い
            // a write-heavy field (one that would stay on mirrored columns): the dual writes outweigh the scan's gain
            let heavy = self.ts_heavy.get(a)?;
            all.iter().filter_map(|p| p.first()).find_map(|f| heavy.get(f).map(|(w, r)| (f.clone(), *w, *r))).map(|(f, w, r)| {
                (
                    format!("`{}` は書き込みの多いフィールド（一括書き込みの重み {:.2} > 走査の重み {:.2}。写しの二重書き込みのほうが重い）", f, w, r),
                    format!("`{}` is write-heavy (bulk-write weight {:.2} > scan weight {:.2}; the mirror's dual write would cost more)", f, w, r),
                )
            })
        });
        if let Some((ja, en)) = veto {
            let an = self.s.t.arenas[a].name.clone();
            if !self.report.column_skips.iter().any(|(x, l, _, _)| *x == an && *l == line) {
                self.report.column_skips.push((an, line, ja, en));
            }
            return;
        }
        let path_c = |p: &[String]| p.iter().map(|x| format!(".{}", x)).collect::<String>();
        // 完全列化した 1 段のフィールド / a single-step True-SoA field
        let ts_of = |cg: &Self, p: &[String]| if p.len() == 1 { cg.ts_index(a, &p[0]) } else { None };
        // 浮動小数点の集計（#[fast_math]）: 総和は完全列化の float / double の列だけ（写しの集計の列は作らない。項のどれかが
        // 列にならなければふつうのループ）、最小 / 最大は IEEE-754 の順序を保つ鍵の列（f20_col_pick_key をそのまま使う）
        // float reductions (#[fast_math]): sums only over True-SoA float / double columns (no mirrored aggregate column;
        // if any term is not a column the plain loop stays), min / max over an IEEE-754 order-preserving key column (the
        // existing f20_col_pick_key)
        if let Some(ft) = plan.fty {
            match plan.kind {
                RedKind::Sum => {
                    if !plan.terms.iter().all(|t| ts_of(self, &t.path).is_some()) {
                        return;
                    }
                    let mask = if plan.conds.is_empty() { None } else { Some(self.add_col(a, ColKind::Bits, String::new(), cond.clone(), deps.clone(), line, &[])) };
                    let srcs: Vec<SumSrc> = plan.terms.iter().filter_map(|t| ts_of(self, &t.path)).map(|fi| SumSrc::Ts { f: fi, mask }).collect();
                    self.col_use.insert(e.id, ColUse::SumT(srcs));
                }
                RedKind::MinMax => {
                    let t = &plan.terms[0];
                    if ts_of(self, &t.path).is_some() {
                        return;
                    }
                    let mut d = deps.clone();
                    d.push(t.path.clone());
                    let val = format!("(*v){}", path_c(&t.path));
                    let k = self.add_col(a, ColKind::FKey { ft, max: !plan.less }, val, cond, d, line, &t.path);
                    self.col_use.insert(e.id, ColUse::Pick { col: k, last: !plan.strict });
                }
                RedKind::Count => {}
            }
            return;
        }
        match plan.kind {
            RedKind::Sum if plan.terms.iter().any(|t| ts_of(self, &t.path).is_some()) => {
                // 条件はビット列（生存 && 条件）にして、走査の時点でマスクにする / the filter becomes a bit column (live &&
                // filter) used as a mask at scan time
                let mask = if plan.conds.is_empty() { None } else { Some(self.add_col(a, ColKind::Bits, String::new(), cond.clone(), deps.clone(), line, &[])) };
                let mut srcs = Vec::new();
                for t in &plan.terms {
                    if let Some(fi) = ts_of(self, &t.path) {
                        srcs.push(SumSrc::Ts { f: fi, mask });
                    } else {
                        let mut d = deps.clone();
                        d.push(t.path.clone());
                        let wide = t.it.bits(pb) > 32;
                        let val = format!("(*v){}", path_c(&t.path));
                        srcs.push(SumSrc::Col(self.add_col(a, ColKind::Agg { wide }, val, cond.clone(), d, line, &t.path)));
                    }
                }
                self.col_use.insert(e.id, ColUse::SumT(srcs));
            }
            RedKind::MinMax if ts_of(self, &plan.terms[0].path).is_some() && (plan.strict || plan.acc_it.bits(pb) <= 32) => {
                let fi = ts_of(self, &plan.terms[0].path).unwrap_or(0);
                let mask = if plan.conds.is_empty() { None } else { Some(self.add_col(a, ColKind::Bits, String::new(), cond.clone(), deps.clone(), line, &[])) };
                self.col_use.insert(e.id, ColUse::PickT { f: fi, mask, last: !plan.strict });
            }
            RedKind::Sum => {
                let mut cols = Vec::new();
                for t in &plan.terms {
                    let mut d = deps.clone();
                    d.push(t.path.clone());
                    let wide = t.it.bits(pb) > 32;
                    let val = format!("(*v){}", path_c(&t.path));
                    cols.push(self.add_col(a, ColKind::Agg { wide }, val, cond.clone(), d, line, &t.path));
                }
                self.col_use.insert(e.id, ColUse::Sum(cols));
            }
            RedKind::Count => {
                if plan.conds.is_empty() {
                    // 条件なしの個数は st.live だけで決まる（O(1)、列は要らない） / an unfiltered count is st.live (O(1), no column)
                    return;
                }
                let k = self.add_col(a, ColKind::Bits, String::new(), cond, deps, line, &[]);
                self.col_use.insert(e.id, ColUse::Count(k));
            }
            RedKind::MinMax => {
                if !plan.strict && plan.acc_it.bits(pb) > 32 {
                    // `<=` / `>=` の 64 ビットの選択は NONE と同じ鍵の値（最大値 / 最小値）も選ばれうる / a 64-bit `<=` / `>=` pick
                    // may select the one value whose key equals NONE
                    return;
                }
                let t = &plan.terms[0];
                let mut d = deps.clone();
                d.push(t.path.clone());
                let val = format!("(*v){}", path_c(&t.path));
                let k = self.add_col(a, ColKind::Key { it: plan.acc_it, max: !plan.less }, val, cond, d, line, &t.path);
                self.col_use.insert(e.id, ColUse::Pick { col: k, last: !plan.strict });
            }
        }
    }

    /// 列を足す（同じ種類・値・条件の列はほかの集計と共有する） / add a column (shared with any reduction that needs the
    /// same kind, value and filter)
    fn add_col(&mut self, a: usize, kind: ColKind, val: String, cond: String, deps: Vec<Vec<String>>, line: usize, path: &[String]) -> usize {
        if let Some(k) = self.arena_cols[a].iter().position(|c| c.kind == kind && c.val == val && c.cond == cond) {
            if !self.arena_cols[a][k].lines.contains(&line) {
                self.arena_cols[a][k].lines.push(line);
            }
            return k;
        }
        let k = self.arena_cols[a].len();
        let p = path.join("_");
        let name = match kind {
            ColKind::Agg { .. } => format!("agg{}_{}", k, p),
            ColKind::Bits => format!("fb{}", k),
            ColKind::Key { max: false, .. } | ColKind::FKey { max: false, .. } => format!("min{}_{}", k, p),
            ColKind::Key { max: true, .. } | ColKind::FKey { max: true, .. } => format!("max{}_{}", k, p),
        };
        let mut uniq: Vec<Vec<String>> = Vec::new();
        for d in deps {
            if !uniq.contains(&d) {
                uniq.push(d);
            }
        }
        self.arena_cols[a].push(DerivedCol { kind, name, val, cond, deps: uniq, lines: vec![line] });
        k
    }

    /// 条件が読む要素のフィールドの道筋 / the element field paths a filter reads
    fn cond_paths(&self, e: &Expr, cx: &RedCx, out: &mut Vec<Vec<String>>) {
        if let ExprKind::Field(..) = &e.kind {
            if let Some(p) = self.slot_path(e, cx) {
                out.push(p);
                return;
            }
        }
        for c in crate::flow::children(e) {
            if let crate::flow::Child::E(x) = c {
                self.cond_paths(x, cx, out);
            }
        }
    }

    /// 書き込み先が `@a[i]` の要素の中なら（アリーナ, 根の `@a[i]`, 要素からのフィールドの道筋）。道筋は配列の要素の手前で
    /// 打ち切る（列は配列の中を読まない）。own・参照の先へ進む場所はスロットの外なので None
    /// when a write target lies inside an element `@a[i]`: (arena, the root `@a[i]`, the field path from the element). The
    /// path stops before an array element (columns never read inside arrays); a place behind own or a reference is
    /// outside the slot, so None
    fn arena_write_root<'e>(&self, e: &'e Expr) -> Option<(usize, &'e Expr, Vec<String>)> {
        let mut path: Vec<String> = Vec::new();
        let mut cur = e;
        loop {
            match &cur.kind {
                ExprKind::Field(base, name, _) => {
                    if !matches!(self.ty(base), Ty::Struct(_)) {
                        return None;
                    }
                    path.push(sanitize_field(name));
                    cur = base;
                }
                ExprKind::Index(base, _) => {
                    if let Some(&a) = self.s.arena_index.get(&cur.id) {
                        path.reverse();
                        return Some((a, cur, path));
                    }
                    path.clear();
                    cur = base;
                }
                _ => return None,
            }
        }
    }

    /// 要素の道筋 paths への書き込みが届く列（ビットの集合。path が列の読む道筋の先頭部分なら届く）。64 本を超える列は
    /// まとめて「全部」 / the columns a write to the element paths can reach (a bit set; a write reaches a column when its
    /// path is a prefix of a path the column reads); past 64 columns everything counts as reached
    fn col_mask(&self, a: usize, paths: &[Vec<String>]) -> u64 {
        let cols = match self.arena_cols.get(a) {
            Some(c) => c,
            None => return 0,
        };
        let mut m = 0u64;
        for (k, c) in cols.iter().enumerate() {
            if paths.iter().any(|p| c.deps.iter().any(|d| d.starts_with(p))) {
                if k >= 64 {
                    return u64::MAX;
                }
                m |= 1u64 << k;
            }
        }
        m
    }

    /// その arena のすべての列 / every column of the arena
    fn col_all(&self, a: usize) -> u64 {
        let n = self.arena_cols.get(a).map_or(0, |c| c.len());
        if n >= 64 {
            u64::MAX
        } else {
            (1u64 << n) - 1
        }
    }

    /// 場所の根の参照（ローカル）と、その先の要素の中の道筋（配列の要素の手前で打ち切る。own の先へ進むなら道筋は None:
    /// スロットの外）。根がローカルでなければ None
    /// the local reference at the root of a place, and the path inside its referent (stopped before an array element;
    /// None for the path when the place goes through own, i.e. outside the slot); None when the root is not a local
    fn place_root_path(&self, e: &Expr) -> Option<(BindId, Option<Vec<String>>)> {
        let mut path: Vec<String> = Vec::new();
        let mut inside = true;
        let mut cur = e;
        loop {
            match &cur.kind {
                ExprKind::Field(base, name, _) => {
                    if !matches!(self.ty(base), Ty::Struct(_)) && !matches!(base.kind, ExprKind::Path(_)) {
                        inside = false;
                    }
                    if let ExprKind::Path(_) = &base.kind {
                        // 参照のローカルの直下のフィールド（`p.f` は `(*p).f`） / a field right below a reference local
                        if let Some(b) = self.local_of(base) {
                            if !matches!(self.local(b).ty, Ty::Ref(..)) {
                                return None;
                            }
                        }
                    }
                    path.push(sanitize_field(name));
                    cur = base;
                }
                ExprKind::Index(base, _) => {
                    if self.s.arena_index.contains_key(&cur.id) {
                        return None;
                    }
                    path.clear();
                    cur = base;
                }
                ExprKind::Path(_) => {
                    let b = self.local_of(cur)?;
                    path.reverse();
                    return Some((b, if inside { Some(path) } else { None }));
                }
                _ => return None,
            }
        }
    }

    /// 参照 b（get_mut の束縛）を通した書き込みの道筋を集める。分からない使い方（参照そのものを別の形で使う）が
    /// あれば false（そのときは全部の列を作り直す） / collect the paths written through the reference b (a get_mut
    /// binding); false when b is used in a way this cannot follow (then every column is rebuilt)
    fn ref_writes_block(&self, b: BindId, blk: &Block, out: &mut Vec<Vec<String>>) -> bool {
        for st in &blk.stmts {
            let ok = match st {
                Stmt::Let(l) => l.init.as_ref().map_or(true, |i| self.ref_writes(b, i, out)),
                Stmt::Expr(e) => self.ref_writes(b, e, out),
            };
            if !ok {
                return false;
            }
        }
        blk.tail.as_ref().map_or(true, |t| self.ref_writes(b, t, out))
    }

    fn ref_writes(&self, b: BindId, e: &Expr, out: &mut Vec<Vec<String>>) -> bool {
        let rooted = |cg: &Cg, x: &Expr| -> Option<Option<Vec<String>>> {
            match cg.place_root_path(x) {
                Some((r, p)) if r == b => Some(p),
                _ => None,
            }
        };
        match &e.kind {
            ExprKind::Assign { lhs, rhs, .. } => {
                if let Some(p) = rooted(self, lhs) {
                    if let Some(p) = p {
                        out.push(p);
                    }
                    return self.ref_index_exprs(b, lhs, out) && self.ref_writes(b, rhs, out);
                }
            }
            ExprKind::Path(_) => {
                if self.local_of(e) == Some(b) {
                    return false;
                }
                return true;
            }
            ExprKind::Field(..) | ExprKind::Index(..) => {
                if rooted(self, e).is_some() {
                    // 読み出し: 添字の式だけを見る / a read: only the index expressions matter
                    return self.ref_index_exprs(b, e, out);
                }
            }
            ExprKind::Ref { inner, mutable } => {
                if let Some(p) = rooted(self, inner) {
                    if *mutable {
                        if let Some(p) = p {
                            out.push(p);
                        }
                    }
                    return self.ref_index_exprs(b, inner, out);
                }
            }
            ExprKind::Call { args, .. } => {
                let params = match self.s.calls.get(&e.id) {
                    Some(CallKind::User(fi)) => self.s.t.fns[*fi].params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                for (i, a) in args.iter().enumerate() {
                    if self.local_of(a) == Some(b) {
                        // 参照そのものを渡す: 可変参照の引数なら要素全体が書かれうる / the reference itself is passed: a mutable
                        // reference parameter may write the whole element
                        match params.get(i) {
                            Some(Ty::Ref(false, _)) => continue,
                            Some(Ty::Ref(true, _)) => {
                                out.push(Vec::new());
                                continue;
                            }
                            _ => return false,
                        }
                    }
                    if !self.ref_writes(b, a, out) {
                        return false;
                    }
                }
                return true;
            }
            ExprKind::MethodCall { recv, args, .. } => {
                if let Some(p) = rooted(self, recv) {
                    if matches!(self.s.methods.get(&e.id), Some(MethodKind::Take)) {
                        if let Some(p) = p {
                            out.push(p);
                        }
                    }
                    if !self.ref_index_exprs(b, recv, out) {
                        return false;
                    }
                    return args.iter().all(|a| self.ref_writes(b, a, out));
                }
            }
            ExprKind::IfLet { scrut, .. } | ExprKind::Match { scrut, .. } => {
                if let Some(p) = rooted(self, scrut) {
                    // 場所を借りる束縛は、その場所の下を書きうる / a binding that borrows the place may write below it
                    if let Some(p) = p {
                        out.push(p);
                    }
                }
            }
            _ => {}
        }
        crate::flow::children(e).into_iter().all(|c| match c {
            crate::flow::Child::E(x) => self.ref_writes(b, x, out),
            crate::flow::Child::B(bl) => self.ref_writes_block(b, bl, out),
        })
    }

    /// 場所の式の中の添字の式（根の参照の使い方には数えない） / the index expressions inside a place (not uses of the root)
    fn ref_index_exprs(&self, b: BindId, e: &Expr, out: &mut Vec<Vec<String>>) -> bool {
        match &e.kind {
            ExprKind::Field(base, _, _) => self.ref_index_exprs(b, base, out),
            ExprKind::Index(base, i) => self.ref_index_exprs(b, base, out) && self.ref_writes(b, i, out),
            _ => true,
        }
    }

    /// 選択の鍵（最小: enc(v)、最大: ~enc(v)。enc は u64 なら v ^ 2^63、ほかは int64 への拡大） / the pick key
    fn key_enc(&self, it: IntTy, max: bool, x: &str) -> String {
        let base = if !it.signed() && it.bits(self.o.ptr_bits) == 64 { format!("F20_KEY_U64({})", x) } else { format!("((int64_t)({}))", x) };
        if max {
            format!("(~{})", base)
        } else {
            base
        }
    }

    /// 鍵から値へ / from a key back to the value
    fn key_dec(&self, it: IntTy, max: bool, k: &str) -> String {
        let ct = it.c_name();
        let k2 = if max { format!("(~{})", k) } else { k.to_string() };
        if !it.signed() && it.bits(self.o.ptr_bits) == 64 {
            format!("(({})F20_UNKEY_U64({}))", ct, k2)
        } else {
            format!("(({}){})", ct, k2)
        }
    }

    /// 列を読む集計の走査関数の本体 / the body of a reduction that reads columns
    fn emit_col_reduction(&mut self, a: usize, plan: &RedPlan, e: &Expr, u: &ColUse) {
        self.report.reductions += 1;
        self.report.column_reductions += 1;
        if plan.kind != RedKind::Sum || !plan.conds.is_empty() {
            self.report.masked_reductions += 1;
        }
        self.report.gen_elided += if plan.lookup { 1 } else { plan.terms.len().max(1) };
        self.reduce_n += 1;
        let an = self.s.t.arenas[a].name.clone();
        let fname = format!("fsc_reduce_{}_{}", an, self.reduce_n);
        let act = match plan.fty {
            Some(ft) => ft.c_name().to_string(),
            None => self.cty(&Ty::Int(plan.acc_it)),
        };
        let snippet = self.snippet(e.span);
        let accn = self.names.get(&plan.acc).cloned().unwrap_or_else(|| "/*?*/acc".to_string());
        let mut f = String::new();
        let _ = writeln!(f, "/* {} {}: {}", self.c("line", "line"), self.line_of(e.span), snippet);
        let mut body = String::new();
        let cols: Vec<(String, ColKind, String)> = self.arena_cols[a].iter().map(|c| (c.name.clone(), c.kind, c.cond.clone())).collect();
        let what;
        match u {
            ColUse::Sum(ks) => {
                let names: Vec<String> = ks.iter().map(|k| cols[*k].0.clone()).collect();
                let filtered = !plan.conds.is_empty();
                let notes = [
                    if filtered {
                        self.c(
                            &format!("集計の列 {} だけを読む（agg[i] = 条件 ? 値 : 0 を書き込みの時点で作ってある）。条件もスロットも読まない。", names.join(", ")),
                            &format!("reads only the aggregate column {} (agg[i] = filter ? value : 0 is built at write time); neither the filter nor the slots are read.", names.join(", ")),
                        )
                    } else {
                        self.c(&format!("集計の列 {} だけを読む（値の写し）。スロットは読まない。", names.join(", ")), &format!("reads only the aggregate column {} (a mirror of the value); the slots are not read.", names.join(", ")))
                    },
                    self.c(
                        "死んだスロットと条件を満たさないスロットは列で 0。64 スロットの塊ごとにベクトルで足し、生存ビットマップの語が 0 の塊は飛ばす（f20_col_sum_u64 / f20_col_sum_u32）。",
                        "dead and filtered-out slots are 0 in the column; each 64-slot block is added with vectors, and blocks whose live-bitmap word is 0 are skipped (f20_col_sum_u64 / f20_col_sum_u32).",
                    ),
                    self.c(
                        "あふれ: u64 の列は値の OR のビット長 + 個数のビット長 ≤ 64 なら素の和が正確、そうでなければ正確な走査をやり直す。最後に acc へ足すときの検査はラッチへ（逐次の検査付き加算と同じ判定）。",
                        "overflow: for a u64 column the plain sum is exact when bit length of the OR of the values + bit length of the count ≤ 64, otherwise the exact pass reruns; the final add into acc is checked into the latch (the verdict of sequential checked additions).",
                    ),
                ];
                for nt in notes.iter() {
                    let _ = writeln!(f, " *   {}", nt);
                }
                let _ = writeln!(body, "    const uint32_t n = F20_COL_SCAN_END(arena->st.next_unused); /* {} */", self.c("64 の倍数へ切り上げ（next_unused から先は一度も使われていない 0）", "rounded up to a multiple of 64 (slots from next_unused on were never used and are 0)"));
                for (j, k) in ks.iter().enumerate() {
                    let (nm, kind, _) = &cols[*k];
                    match kind {
                        ColKind::Agg { wide: true } => {
                            let _ = writeln!(body, "    const uint64_t s{} = f20_col_sum_u64(arena->col.{}, arena->live, n, flt);", j, nm);
                        }
                        _ => {
                            let _ = writeln!(body, "    const uint64_t s{} = f20_col_sum_u32(arena->col.{}, arena->live, n);", j, nm);
                        }
                    }
                }
                let _ = writeln!(body, "    {} r = acc;\n    uint32_t ov = 0u;", act);
                for j in 0..ks.len() {
                    let _ = writeln!(body, "    ov |= (uint32_t)__builtin_add_overflow(r, s{}, &r);", j);
                }
                let _ = writeln!(body, "    *flt |= ov << 1; /* F20_FAULT_ARITH */\n    return r;");
                what = if filtered { self.c("条件付き総和（集計の列）", "filtered sum (aggregate column)") } else { self.c("総和（集計の列）", "sum (aggregate column)") };
            }
            ColUse::Count(k) => {
                let nm = cols[*k].0.clone();
                let _ = writeln!(
                    f,
                    " *   {}",
                    self.c(
                        &format!("絞り込みのビット列 {} だけを読む（i ビット目 = 生存 && 条件 を書き込みの時点で作ってある）。64 スロットに popcount 1 回（x86-64: POPCNT、AArch64: CNT）。", nm),
                        &format!("reads only the filter bit column {} (bit i = live && filter is built at write time): one popcount per 64 slots (x86-64: POPCNT, AArch64: CNT).", nm)
                    )
                );
                let _ = writeln!(body, "    const uint64_t cnt = f20_col_count_bits(arena->col.{}, F20_LIVE_WORDS_USED(arena->st.next_unused));", nm);
                let _ = writeln!(body, "    uint64_t total;\n    {} r;", act);
                let _ = writeln!(body, "    const uint32_t ov = (uint32_t)__builtin_mul_overflow(cnt, UINT64_C({}), &total) | (uint32_t)__builtin_add_overflow(acc, total, &r);", plan.count_c);
                let _ = writeln!(body, "    *flt |= ov << 1; /* F20_FAULT_ARITH */\n    return r;");
                what = self.c("個数（ビット列の popcount）", "count (popcount of a bit column)");
            }
            ColUse::Pick { col, last } if matches!(cols[*col].1, ColKind::FKey { .. }) => {
                let (nm, kind, _) = cols[*col].clone();
                let (ft, max) = match kind {
                    ColKind::FKey { ft, max } => (ft, max),
                    _ => (FloatTy::F64, false),
                };
                let fname_s = ft.name();
                let path: String = plan.terms[0].path.iter().map(|x| format!(".{}", x)).collect();
                let _ = writeln!(
                    f,
                    " *   {}",
                    self.c(
                        &format!("#[fast_math] の浮動小数点の{}: 鍵の列 {} だけを読む（key[i] = 生存 && 条件 ? {}F20_KEY_{}(値) : NONE を書き込みの時点で作ってある。IEEE-754 の順序を保つ鍵: 符号 0 なら符号ビットだけ、1 なら全ビットを反転。NaN は NONE で選ばれず、-0.0 は +0.0 と同じ鍵）。走査は f20_col_pick_key（整数の鍵の列と同じ）。", if max { "最大" } else { "最小" }, nm, if max { "~" } else { "" }, fname_s.to_uppercase()),
                        &format!("a #[fast_math] float {}: reads only the key column {} (key[i] = live && filter ? {}F20_KEY_{}(value) : NONE is built at write time; IEEE-754 order-preserving keys: sign 0 flips only the sign bit, sign 1 flips every bit; NaN is NONE and never picked, -0.0 shares the key of +0.0). The scan is f20_col_pick_key (as for integer key columns).", if max { "max" } else { "min" }, nm, if max { "~" } else { "" }, fname_s.to_uppercase())
                    )
                );
                let _ = writeln!(
                    f,
                    " *   {}",
                    self.c(
                        "選んだあとの値はスロットから読む（-0.0 の符号もそのまま）。同じ鍵なら `<` は先の添字、`<=` は後の添字（逐次のループと同じ結果）。今の値が NaN なら比較は常に偽なので何も選ばない。",
                        "the value is then read from the chosen slot (the sign of -0.0 included); among equal keys `<` takes the earlier index and `<=` the later one (the result of the sequential loop); when the current value is NaN every compare is false, so nothing is picked."
                    )
                );
                let _ = writeln!(body, "    if (f20_{}_is_nan(*acc)) return; /* {} */", fname_s, self.c("NaN との < / > は偽（逐次のループも何も選ばない）", "< / > against NaN is false (the sequential loop picks nothing either)"));
                let _ = writeln!(body, "    int64_t best = f20_fkey_{}(*acc, {}); /* {} */", fname_s, if max { 1 } else { 0 }, self.c("今の値の鍵（これより良いものだけが選ばれる）", "the key of the current value (only better ones are picked)"));
                let _ = writeln!(body, "    const uint32_t i = f20_col_pick_key(arena->col.{}, arena->live, F20_COL_SCAN_END(arena->st.next_unused), &best, {});", nm, if *last { 1 } else { 0 });
                if plan.pick.is_some() {
                    let _ = writeln!(body, "    if (i != 0xFFFFFFFFu) {{\n        *acc = arena->slot[i].v{};\n        *pick = F20_ID_MAKE(arena->gen[i], i, ARENA_{}_K);\n    }}", path, an);
                } else {
                    let _ = writeln!(body, "    if (i != 0xFFFFFFFFu) *acc = arena->slot[i].v{};", path);
                }
                what = self.c("浮動小数点の最小 / 最大の選択（鍵の列）", "float min / max pick (key column)");
            }
            ColUse::Pick { col, last } => {
                let (nm, kind, _) = cols[*col].clone();
                let (it, max) = match kind {
                    ColKind::Key { it, max } => (it, max),
                    _ => (plan.acc_it, !plan.less),
                };
                let _ = writeln!(
                    f,
                    " *   {}",
                    self.c(
                        &format!("選択の鍵の列 {} だけを読む（key[i] = 生存 && 条件 ? {} : NONE を書き込みの時点で作ってある）。走査は鍵の符号付き最小だけで、条件も生存ビットも比較の向きも走査に出てこない。", nm, if max { "~enc(値)" } else { "enc(値)" }),
                        &format!("reads only the pick-key column {} (key[i] = live && filter ? {} : NONE is built at write time); the scan is a signed minimum of the keys, with no filter, live bit or compare direction in it.", nm, if max { "~enc(value)" } else { "enc(value)" })
                    )
                );
                let _ = writeln!(
                    f,
                    " *   {}",
                    self.c(
                        &format!("段 1: 64 スロットの塊ごとにベクトルの最小、塊の選び直しは CMOV / CSEL。段 2: 勝った塊で「鍵 == 最小」のビットを作り、{}（逐次のループと同じ結果）。", if *last { "最後の 1 を CLZ で（`<=` / `>=`: 同じ値なら後の添字）" } else { "最初の 1 を TZCNT / RBIT+CLZ で（`<` / `>`: 同じ値なら先の添字）" }),
                        &format!("stage 1: a vector minimum per 64-slot block, the block re-selected with CMOV / CSEL; stage 2: one bit per \"key == minimum\" in the winning block, then {} (the same result as the sequential loop).", if *last { "the last set bit via CLZ (`<=` / `>=`: the later index wins a tie)" } else { "the first set bit via TZCNT / RBIT+CLZ (`<` / `>`: the earlier index wins a tie)" })
                    )
                );
                let enc = self.key_enc(it, max, "*acc");
                let dec = self.key_dec(it, max, "best");
                let _ = writeln!(body, "    int64_t best = {}; /* {} */", enc, self.c("今の値の鍵（これより良いものだけが選ばれる）", "the key of the current value (only better ones are picked)"));
                let _ = writeln!(body, "    const uint32_t i = f20_col_pick_key(arena->col.{}, arena->live, F20_COL_SCAN_END(arena->st.next_unused), &best, {});", nm, if *last { 1 } else { 0 });
                if plan.pick.is_some() {
                    let _ = writeln!(body, "    if (i != 0xFFFFFFFFu) {{\n        *acc = {};\n        *pick = F20_ID_MAKE(arena->gen[i], i, ARENA_{}_K);\n    }}", dec, an);
                } else {
                    let _ = writeln!(body, "    if (i != 0xFFFFFFFFu) *acc = {};", dec);
                }
                what = self.c("最小 / 最大の選択（鍵の列）", "min / max pick (pick-key column)");
            }
            ColUse::SumT(_) | ColUse::PickT { .. } => {
                what = self.ts_red_body(a, plan, u, &mut f, &mut body);
            }
        }
        let _ = writeln!(f, " */");
        if plan.kind == RedKind::MinMax {
            let pick_param = if plan.pick.is_some() { format!(", fsc_Id_{} *__restrict__ pick", an) } else { String::new() };
            let _ = writeln!(f, "static FASCON_MAYBE_UNUSED void {fname}(const f20_arena_t({an}) *__restrict__ arena, {act} *__restrict__ acc{pp}) {{", fname = fname, an = an, act = act, pp = pick_param);
        } else {
            self.flt_used = true;
            let _ = writeln!(f, "static FASCON_MAYBE_UNUSED {act} {fname}(const f20_arena_t({an}) *__restrict__ arena, uint32_t *__restrict__ flt, {act} acc) {{", act = act, fname = fname, an = an);
            if plan.fty.is_some() {
                let _ = writeln!(f, "    (void)flt; /* {} */", self.c("浮動小数点の総和はあふれの検査がない（IEEE-754 の無限大になる）", "a float sum has no overflow check (it becomes an IEEE-754 infinity)"));
            }
        }
        f.push_str(&body);
        f.push_str("}\n\n");
        self.reduce_out.push_str(&f);
        self.emit(format!("/* for .. in {}: {} ({}) */", an, what, fname));
        if plan.kind == RedKind::MinMax {
            let pick_arg = match plan.pick {
                Some(p) => format!(", &{}", self.names.get(&p).cloned().unwrap_or_default()),
                None => String::new(),
            };
            self.emit(format!("{f}(&arena_{an}, &{acc}{pa});", f = fname, an = an, acc = accn, pa = pick_arg));
        } else {
            self.emit(format!("{acc} = {f}(&arena_{an}, &_flt, {acc});", acc = accn, f = fname, an = an));
        }
    }

    /// 集計の特殊化を出す: 走査関数を 1 個作り、ループの場所には呼び出し 1 行だけを置く
    /// emit a reduction: one scan function, and a single call where the loop was
    fn emit_reduction(&mut self, a: usize, plan: &RedPlan, e: &Expr) {
        if let Some(u) = self.col_use.get(&e.id).cloned() {
            self.emit_col_reduction(a, plan, e, &u);
            return;
        }
        self.report.reductions += 1;
        if plan.kind != RedKind::Sum || !plan.conds.is_empty() {
            self.report.masked_reductions += 1;
        }
        self.report.gen_elided += if plan.lookup { 1 } else { plan.terms.len().max(1) };
        self.reduce_n += 1;
        let an = self.s.t.arenas[a].name.clone();
        let k = arena_k(self.s.t.arenas[a].cap);
        let fname = format!("fsc_reduce_{}_{}", an, self.reduce_n);
        let act = self.cty(&Ty::Int(plan.acc_it));
        let snippet = self.snippet(e.span);
        let cx = RedCx { a, var: plan.var, elem: plan.elem_bind, acc: Some(plan.acc), pick: plan.pick };
        let mut caps: Vec<BindId> = Vec::new();
        // 条件を 1 スロット分の C に / the filter for one slot, in C
        let cond_for = |cg: &Cg, place: &str, caps: &mut Vec<BindId>| -> String {
            if plan.conds.is_empty() {
                return String::new();
            }
            plan.conds.iter().map(|c| cg.cond_c(c, &cx, place, caps)).collect::<Vec<_>>().join(" & ")
        };
        let mut f = String::new();
        let _ = writeln!(f, "/* {} {}: {}", self.c("line", "line"), self.line_of(e.span), snippet);
        let mut body = String::new();
        let accn = self.names.get(&plan.acc).cloned().unwrap_or_else(|| "/*?*/acc".to_string());
        match plan.kind {
            RedKind::Sum => {
                let lanes = (1u32 << k).min(4);
                let pb = self.o.ptr_bits;
                let wide = plan.terms.iter().any(|t| t.it.bits(pb) > 32);
                let lane_names = |p: &str| (0..lanes).map(|l| format!("{}{} = 0u", p, l)).collect::<Vec<_>>().join(", ");
                let tree = |p: &str| match lanes {
                    1 => format!("{}0", p),
                    2 => format!("({p}0 + {p}1)", p = p),
                    _ => format!("({p}0 + {p}1) + ({p}2 + {p}3)", p = p),
                };
                let notes = [
                    self.c(
                        "死んだスロットは必ず 0（_free / _remove が 0 で埋める）。生存印を読まず先頭 n スロットを足せば、生存スロットの総和になる。",
                        "dead slots are always zero (_free / _remove zero-fill them), so adding the first n slots without reading the live marks gives the sum over live slots.",
                    ),
                    if plan.conds.is_empty() {
                        self.c("世代検査なし（ループ変数の生存は証明済み）。境界検査なし（n ≤ 2^K、表は 2^K + 1）。", "no generation checks (the loop variable is proven live), no bounds checks (n ≤ 2^K and the table has 2^K + 1 slots).")
                    } else {
                        self.c(
                            "条件はジャンプにせず全ビット 1 / 0 のマスクにして AND（m = 0 - 条件）。条件が偽のスロットと死んだスロットは 0 を足す。",
                            "the filter is not a jump: it becomes an all-ones / all-zeros mask that is ANDed in (m = 0 - cond); slots that fail it, and dead slots, add 0.",
                        )
                    },
                    self.c(&format!("{} 本の独立した足し込みに展開。本体に分岐はなく、あるのはループの戻り 1 本だけ。", lanes), &format!("unrolled into {} independent accumulations; the body has no branch, only the loop's back edge.", lanes)),
                    self.c(
                        "あふれは 64 ビット値を上位・下位 32 ビットに分けて正確に足し、最後に 1 回だけ判定してラッチへ（符号なしなので逐次の検査付き加算と同じ判定）。",
                        "overflow: 64-bit values are added exactly as high and low 32-bit halves and checked once at the end into the latch (for unsigned values this equals the sequential checked additions).",
                    ),
                ];
                for nt in notes.iter() {
                    let _ = writeln!(f, " *   {}", nt);
                }
                let _ = writeln!(body, "    const uint32_t n = (arena->st.next_unused + {m}u) & ~{m}u; /* {c} */", m = lanes - 1, c = self.c(&format!("{} の倍数へ切り上げ（≤ 2^K。next_unused から先は一度も使われていない 0 のスロット）", lanes), &format!("rounded up to a multiple of {} (≤ 2^K; slots from next_unused on were never used and are zero)", lanes)));
                let _ = writeln!(body, "    const f20_slot_t({an}) *p = arena->slot, *const end = arena->slot + n;", an = an);
                let _ = writeln!(body, "    uint64_t {}; /* {} */", lane_names("lo"), self.c(if wide { "下位 32 ビットの和" } else { "和（どの項も 32 ビット以下なので 64 ビットで正確）" }, if wide { "sums of the low 32 bits" } else { "sums (every term has 32 bits or fewer, so 64 bits hold them exactly)" }));
                if wide {
                    let _ = writeln!(body, "    uint64_t {}; /* {} */", lane_names("hi"), self.c("上位 32 ビットの和", "sums of the high 32 bits"));
                }
                let _ = writeln!(body, "    for (; p < end; p += {}) {{", lanes);
                for l in 0..lanes {
                    let place_v = format!("p[{}].v", l);
                    let mut st = Vec::new();
                    let masked = !plan.conds.is_empty();
                    if masked {
                        let c = cond_for(self, &place_v, &mut caps);
                        st.push(format!("const uint64_t m{l} = UINT64_C(0) - (uint64_t)({c});", l = l, c = c));
                    }
                    for t in &plan.terms {
                        let place = format!("{}{}", place_v, t.path.iter().map(|p| format!(".{}", p)).collect::<String>());
                        if t.it.bits(pb) > 32 {
                            if masked {
                                st.push(format!("lo{l} += (uint32_t){p} & m{l}; hi{l} += ((uint64_t){p} >> 32) & m{l};", l = l, p = place));
                            } else {
                                st.push(format!("lo{l} += (uint32_t){p}; hi{l} += (uint64_t){p} >> 32;", l = l, p = place));
                            }
                        } else if masked {
                            st.push(format!("lo{l} += (uint64_t){p} & m{l};", l = l, p = place));
                        } else {
                            st.push(format!("lo{} += {};", l, place));
                        }
                    }
                    let _ = writeln!(body, "        {}", st.join(" "));
                }
                let _ = writeln!(body, "    }}");
                if wide {
                    let _ = writeln!(body, "    const uint64_t lo = {};", tree("lo"));
                    let _ = writeln!(body, "    const uint64_t hi = {} + (lo >> 32);", tree("hi"));
                    let _ = writeln!(body, "    const uint64_t sum = (hi << 32) | (lo & 0xFFFFFFFFu);");
                    let _ = writeln!(body, "    {} r;", act);
                    let _ = writeln!(body, "    const uint32_t ov = (uint32_t)((hi >> 32) != 0u) | (uint32_t)__builtin_add_overflow(acc, sum, &r);");
                } else {
                    let _ = writeln!(body, "    const uint64_t sum = {};", tree("lo"));
                    let _ = writeln!(body, "    {} r;", act);
                    let _ = writeln!(body, "    const uint32_t ov = (uint32_t)__builtin_add_overflow(acc, sum, &r);");
                }
                let _ = writeln!(body, "    *flt |= ov << 1; /* F20_FAULT_ARITH */\n    return r;");
            }
            RedKind::Count if plan.conds.is_empty() => {
                let _ = writeln!(f, " *   {}", self.c("条件なしの個数: 生存スロットの数（st.live）× 定数。走査は要らない（O(1)）。", "an unfiltered count: the number of live slots (st.live) × the constant; no scan at all (O(1))."));
                let _ = writeln!(body, "    uint64_t total;\n    {} r;", act);
                let _ = writeln!(body, "    const uint32_t ov = (uint32_t)__builtin_mul_overflow((uint64_t)arena->st.live, UINT64_C({}), &total) | (uint32_t)__builtin_add_overflow(acc, total, &r);", plan.count_c);
                let _ = writeln!(body, "    *flt |= ov << 1; /* F20_FAULT_ARITH */\n    return r;");
            }
            RedKind::Count | RedKind::MinMax => {
                let per = (1u32 << k).min(64);
                // 最小 / 最大は 2 レーン（値と添字で 4 本のレジスタ）。4 レーンにすると x86-64 の GCC がレジスタを使い切り、
                // メモリへ逃がしたレーンの選択を分岐に戻すため。比較 → 選択の依存の鎖は 1 スロット 1 サイクル未満に収まる
                // min / max uses 2 lanes (value + index = 4 registers): with 4 lanes GCC runs out of x86-64 registers and turns
                // the select of a spilled lane back into a branch; the compare → select chain stays under one cycle per slot
                let lanes = if plan.kind == RedKind::MinMax { per.min(2) } else { per.min(4) };
                let notes = [
                    self.c(
                        "生存ビットマップを 1 語（64 スロット）ずつ読む。各スロットの生存ビットはシフトと AND で取り出し、条件と一緒にマスクにする（死んだスロットは 0 なので条件だけでは外せない）。",
                        "the live bitmap is read one word (64 slots) at a time; each slot's live bit is taken out with a shift and an AND and combined with the filter into a mask (a dead slot is all zero, which the filter alone cannot exclude).",
                    ),
                    if plan.kind == RedKind::Count {
                        self.c(&format!("個数は {} 本の独立したカウンタへマスクを足すだけ。スロットごとの分岐はない。", lanes), &format!("the count just adds masks into {} independent counters; no per-slot branch.", lanes))
                    } else {
                        self.c(
                            &format!("選択は {} 本の独立したレーン。k = 生存ビット AND 条件 AND 比較 は分岐なしで作り、更新（F20_PICK）は既定でめったに真にならない分岐 1 本、FASCON2_PICK_BRANCHLESS なら条件付き選択（AArch64: CSEL / x86-64: CMOV）。最後にレーンを 1 つにまとめる。", lanes),
                            &format!("the pick runs in {} independent lanes; k = live bit AND filter AND compare is built without a branch, and the update (F20_PICK) is one rarely-taken branch by default, or a conditional select (AArch64: CSEL / x86-64: CMOV) with FASCON2_PICK_BRANCHLESS; the lanes are merged once at the end.", lanes),
                        )
                    },
                    self.c("語がすべて 0（64 スロットとも死んでいる）ならその 64 スロットは読まない（語ごとの予測の効く分岐 1 本）。", "a word that is all zero (64 dead slots) skips those 64 slots entirely (one well-predicted branch per word)."),
                ];
                for nt in notes.iter() {
                    let _ = writeln!(f, " *   {}", nt);
                }
                let _ = writeln!(body, "    const uint32_t nw = F20_LIVE_WORDS_USED(arena->st.next_unused);");
                if plan.kind == RedKind::Count {
                    let _ = writeln!(body, "    uint64_t {};", (0..lanes).map(|l| format!("c{} = 0u", l)).collect::<Vec<_>>().join(", "));
                } else {
                    let _ = writeln!(body, "    {} {};", act, (0..lanes).map(|l| format!("b{} = *acc", l)).collect::<Vec<_>>().join(", "));
                    let _ = writeln!(
                        body,
                        "    uint32_t {}; /* {} */",
                        (0..lanes).map(|l| format!("i{} = 0xFFFFFFFFu", l)).collect::<Vec<_>>().join(", "),
                        self.c("0xFFFFFFFF = そのレーンはまだ何も選んでいない（添字は 24 ビット以下）", "0xFFFFFFFF = this lane has not picked anything yet (indices have at most 24 bits)")
                    );
                }
                let _ = writeln!(body, "    for (uint32_t w = 0u; w < nw; w++) {{");
                let _ = writeln!(body, "        const uint64_t lw = arena->live[w];");
                let _ = writeln!(body, "        if (lw == 0u) continue;");
                let _ = writeln!(body, "        const f20_slot_t({an}) *const p = arena->slot + ((size_t)w << 6);", an = an);
                if plan.kind == RedKind::MinMax {
                    let _ = writeln!(body, "        const uint32_t base = w << 6;");
                }
                let _ = writeln!(body, "        for (uint32_t j = 0u; j < {}u; j += {}u) {{", per, lanes);
                let cmp = match (plan.less, plan.strict) {
                    (true, true) => "<",
                    (true, false) => "<=",
                    (false, true) => ">",
                    (false, false) => ">=",
                };
                for l in 0..lanes {
                    let jl = if l == 0 { "j".to_string() } else { format!("j + {}u", l) };
                    let place_v = format!("p[{}].v", jl);
                    let c = cond_for(self, &place_v, &mut caps);
                    let livebit = format!("((uint32_t)(lw >> ({})) & 1u)", jl);
                    let m = if c.is_empty() { livebit } else { format!("{} & (uint32_t)({})", livebit, c) };
                    if plan.kind == RedKind::Count {
                        let _ = writeln!(body, "            c{l} += {m};", l = l, m = m);
                    } else {
                        let fp = format!("{}{}", place_v, plan.terms[0].path.iter().map(|p| format!(".{}", p)).collect::<String>());
                        let _ = writeln!(
                            body,
                            "            {{ const {act} v = {fp}; const uint32_t k = {m} & (uint32_t)(v {cmp} b{l}); F20_PICK(k, {act}, b{l}, v, i{l}, base + {jl}); }}",
                            act = act,
                            fp = fp,
                            m = m,
                            cmp = cmp,
                            l = l,
                            jl = jl
                        );
                    }
                }
                let _ = writeln!(body, "        }}\n    }}");
                if plan.kind == RedKind::Count {
                    let tree = match lanes {
                        1 => "c0".to_string(),
                        2 => "(c0 + c1)".to_string(),
                        _ => "(c0 + c1) + (c2 + c3)".to_string(),
                    };
                    let _ = writeln!(body, "    const uint64_t cnt = {};", tree);
                    let _ = writeln!(body, "    uint64_t total;\n    {} r;", act);
                    let _ = writeln!(body, "    const uint32_t ov = (uint32_t)__builtin_mul_overflow(cnt, UINT64_C({}), &total) | (uint32_t)__builtin_add_overflow(acc, total, &r);", plan.count_c);
                    let _ = writeln!(body, "    *flt |= ov << 1; /* F20_FAULT_ARITH */\n    return r;");
                } else {
                    let better = if plan.less { "<" } else { ">" };
                    let tie = if plan.strict { "<" } else { ">" };
                    let _ = writeln!(
                        body,
                        "    /* {} */",
                        self.c(
                            &format!("レーンを 1 つに: 値が{}方、同じ値なら添字が{}方（逐次のループと同じ結果）", if plan.less { "小さい" } else { "大きい" }, if plan.strict { "小さい" } else { "大きい" }),
                            &format!("merge the lanes: the {} value, on a tie the {} index (the same result as the sequential loop)", if plan.less { "smaller" } else { "larger" }, if plan.strict { "smaller" } else { "larger" })
                        )
                    );
                    let _ = writeln!(body, "    {act} best = *acc;\n    uint32_t bi = 0xFFFFFFFFu;", act = act);
                    for l in 0..lanes {
                        let _ = writeln!(
                            body,
                            "    if (i{l} != 0xFFFFFFFFu && (bi == 0xFFFFFFFFu || b{l} {b} best || (b{l} == best && i{l} {t} bi))) {{ best = b{l}; bi = i{l}; }}",
                            l = l,
                            b = better,
                            t = tie
                        );
                    }
                    if plan.pick.is_some() {
                        let _ = writeln!(body, "    if (bi != 0xFFFFFFFFu) {{\n        *acc = best;\n        *pick = F20_ID_MAKE(arena->gen[bi], bi, ARENA_{an}_K);\n    }}", an = an);
                    } else {
                        let _ = writeln!(body, "    if (bi != 0xFFFFFFFFu) *acc = best;");
                    }
                }
            }
        }
        let _ = writeln!(f, " */");
        // 引数: 外のローカル（条件が読むもの） / parameters: outer locals the filter reads
        let mut cap_params = String::new();
        let mut cap_args = String::new();
        for (i, b) in caps.iter().enumerate() {
            let t = self.local(*b).ty.clone();
            let ct = self.cty(&t);
            let _ = write!(cap_params, ", const {} _c{}", ct, i);
            let n = self.names.get(b).cloned().unwrap_or_else(|| "0".to_string());
            let _ = write!(cap_args, ", {}", n);
        }
        if plan.kind == RedKind::MinMax {
            let pick_param = if plan.pick.is_some() { format!(", fsc_Id_{} *__restrict__ pick", an) } else { String::new() };
            let _ = writeln!(f, "static FASCON_MAYBE_UNUSED void {fname}(const f20_arena_t({an}) *__restrict__ arena, {act} *__restrict__ acc{pp}{cp}) {{", fname = fname, an = an, act = act, pp = pick_param, cp = cap_params);
        } else {
            self.flt_used = true;
            let _ = writeln!(f, "static FASCON_MAYBE_UNUSED {act} {fname}(const f20_arena_t({an}) *__restrict__ arena, uint32_t *__restrict__ flt, {act} acc{cp}) {{", act = act, fname = fname, an = an, cp = cap_params);
        }
        f.push_str(&body);
        f.push_str("}\n\n");
        self.reduce_out.push_str(&f);
        let what = match plan.kind {
            RedKind::Sum if plan.conds.is_empty() => self.c("総和の特殊化", "sum specialization"),
            RedKind::Sum => self.c("条件付き総和（マスク）", "filtered sum (masked)"),
            RedKind::Count => self.c("個数（生存ビットマップ × 条件のマスク）", "count (live bitmap × filter mask)"),
            RedKind::MinMax => self.c("最小 / 最大の選択（条件はマスク、更新は F20_PICK）", "min / max pick (filter as a mask, update via F20_PICK)"),
        };
        self.emit(format!("/* for .. in {}: {} ({}) */", an, what, fname));
        if plan.kind == RedKind::MinMax {
            let pick_arg = match plan.pick {
                Some(p) => format!(", &{}", self.names.get(&p).cloned().unwrap_or_default()),
                None => String::new(),
            };
            self.emit(format!("{f}(&arena_{an}, &{acc}{pa}{ca});", f = fname, an = an, acc = accn, pa = pick_arg, ca = cap_args));
        } else {
            self.emit(format!("{acc} = {f}(&arena_{an}, &_flt, {acc}{ca});", acc = accn, f = fname, an = an, ca = cap_args));
        }
    }

    /// ソースの一部を 1 行に（C のコメント用） / a piece of the source on one line (for C comments)
    fn snippet(&self, sp: Span) -> String {
        let text = self.src.text.get(sp.lo as usize..sp.hi as usize).unwrap_or("");
        let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut out: String = one.chars().take(140).collect();
        if one.chars().count() > 140 {
            out.push_str(" ...");
        }
        out.replace("*/", "* /")
    }

    // ───────────── 代入 / assignment ─────────────

    fn assign(&mut self, op: Option<BinOp>, lhs: &Expr, rhs: &Expr) {
        // 書き込み先は読み出しの文脈ではない（古い Id の書き込みは番兵へ） / a write target is never a read context
        self.read_ctx = false;
        let lt = self.ty(lhs);
        // I/O ポートへの書き込み（`com1 = c;` → x86_64: `out dx, al` / AArch64: コンパイラが決めた MMIO）
        // a write to an I/O port (`com1 = c;` → x86_64: `out dx, al` / AArch64: the MMIO the compiler picked)
        let port = match &lhs.kind {
            ExprKind::ArenaRef(_) => self.s.port_reads.get(&lhs.id).map(|p| (*p, None)),
            ExprKind::Index(_, idx) => self.s.port_index.get(&lhs.id).map(|p| (*p, Some(&**idx))),
            _ => None,
        };
        if let Some((p, idx)) = port {
            let info = self.s.t.ports[p].clone();
            let before = self.at_count;
            let i = match idx {
                Some(x) => {
                    let len = info.len.unwrap_or(1);
                    let v = self.index_c(lhs.id, x, len);
                    self.hoist(&Ty::Int(IntTy::Usize), &v)
                }
                None => "0u".to_string(),
            };
            let rt = self.ty(rhs);
            let r = self.val(rhs);
            let r = self.effect_arg(before, &rt, r);
            let site = self.site(lhs.span);
            self.commit(&site);
            self.report.port_accesses += 1;
            let v = match op {
                None => r,
                Some(o) => {
                    let cur = format!("pio_{}_in({})", info.name, i);
                    self.binop_c(o, &cur, &r, &lt, &rt)
                }
            };
            self.emit(format!("pio_{}_out({}, {});", info.name, i, v));
            return;
        }
        // MMIO（デバイス）への書き込み / a write to a device MMIO region
        if let Some(&(m, depth)) = self.s.mmio_index.get(&lhs.id) {
            let info = self.s.t.mmios[m].clone();
            if !info.framebuffer && depth == info.dims.len() {
                let before = self.at_count;
                let addr = self.mmio_addr(lhs, m);
                let addr = self.hoist(&Ty::Int(IntTy::Usize), &addr);
                let bits = info.elem.bits(64);
                let rt = self.ty(rhs);
                let r = self.val(rhs);
                let r = self.effect_arg(before, &rt, r);
                let site = self.site(lhs.span);
                self.commit(&site);
                let v = match op {
                    None => r,
                    Some(o) => {
                        let cur = format!("fascon_mmio_read{}({})", bits, addr);
                        self.binop_c(o, &cur, &r, &lt, &rt)
                    }
                };
                self.emit(format!("fascon_mmio_write{}({}, {});", bits, addr, v));
                return;
            }
        }
        // big-endian のワイヤ構造体の 2 バイト以上の整数への書き込み: バイト順を入れ替えて格納する
        // a write to an integer of 2 bytes or more in a big-endian wire struct: stored byte-swapped
        if let Some((it, true)) = self.s.wire_int_field(lhs) {
            let place = self.lv(lhs);
            let rt = self.ty(rhs);
            let r = self.val(rhs);
            let f = format!("fascon_be{}", it.bits(64));
            let v = match op {
                None => r,
                Some(o) => {
                    let cur = format!("(({}){}({}))", it.c_name(), f, place);
                    self.binop_c(o, &cur, &r, &lt, &rt)
                }
            };
            self.emit(format!("{} = ({}){}({});", place, it.c_name(), f, v));
            return;
        }
        // 参照を通した書き込み（`p = v` は `*p = v`） / a write through a reference
        // 配列への参照は先頭要素へのポインタなので、配列全体の書き込みは下の配列の代入で行い、大きさは型から取る
        // a reference to an array is the pointer to its first element, so a whole-array write goes through the array
        // assignment below, with the size taken from the type
        let mut arr_ref: Option<(String, Ty)> = None;
        let own_deref = self.s.own_derefs.contains(&lhs.id);
        if let (ExprKind::Path(_), false) = (&lhs.kind, own_deref) {
            if let Some(Res::Local(b)) = self.s.res.get(&lhs.id) {
                if let Ty::Ref(_, inner) = self.local(*b).ty.clone() {
                    let n = self.names[b].clone();
                    if let Ty::Array(..) = *inner {
                        arr_ref = Some((n, (*inner).clone()));
                    } else {
                        let place = format!("(*{})", n);
                        self.assign_into(op, &place, &inner, rhs);
                        return;
                    }
                }
            }
        }
        let arr_ty = match &arr_ref {
            Some((_, t)) => t.clone(),
            None => lt.clone(),
        };
        if let Ty::Array(_, len) = &arr_ty {
            let len = *len;
            let (place, size) = match &arr_ref {
                Some((n, t)) => (n.clone(), format!("sizeof({})", self.cdecl(t, ""))),
                None => {
                    let p = self.lv(lhs);
                    // own の配列の中身はポインタで指すので、大きさは型から取る / the contents of an own array are reached through
                    // a pointer, so the size comes from the type
                    let s = if own_deref { format!("sizeof({})", self.cdecl(&arr_ty, "").trim_end()) } else { format!("sizeof({})", p) };
                    (p, s)
                }
            };
            match &rhs.kind {
                ExprKind::ArrayRepeat(v, _) => {
                    let val = self.val(v);
                    let val = self.hoist(&self.ty(v), &val);
                    let d = self.drop_code(&arr_ty, &place);
                    if !d.is_empty() {
                        self.emit(d);
                    }
                    self.emit(format!("for (size_t _i = 0; _i < {}u; _i++) {}[_i] = {};", len, place, val));
                }
                ExprKind::ArrayLit(elems) => {
                    let refs: Vec<&Expr> = elems.iter().collect();
                    let vals = self.operands(&refs);
                    let d = self.drop_code(&arr_ty, &place);
                    if !d.is_empty() {
                        self.emit(d);
                    }
                    for (i, v) in vals.iter().enumerate() {
                        self.emit(format!("{}[{}] = {};", place, i, v));
                    }
                }
                _ => {
                    let src = self.lv(rhs);
                    self.emit(format!("__builtin_memcpy({}, {}, {});", place, src, size));
                }
            }
            return;
        }
        // 完全列化のアリーナの要素まるごとの書き込み: 場所（添字）が先、次に新しい値、古い値の解放、スロットと列への分解
        // a whole-element write to a True-SoA arena: the place (index) first, then the new value, the old value's drop,
        // and the split into the slot and the columns
        if let (None, ExprKind::Index(_, idx)) = (op, &lhs.kind) {
            if let Some(&a) = self.s.arena_index.get(&lhs.id) {
                if self.tsoa.get(a).map_or(false, |v| !v.is_empty()) {
                    self.read_ctx = false;
                    let ix = self.arena_ix(lhs, a, idx);
                    let ixv = self.hoist_force(&Ty::Int(IntTy::U32), &ix);
                    let et = self.s.t.arenas[a].elem.clone();
                    let v = self.val(rhs);
                    let tmp = self.hoist_force(&et, &v);
                    let n = self.s.t.arenas[a].name.clone();
                    let c = self.c("要素まるごと: スロットに残る部分と完全列化の列へ分ける", "the whole element: split into the slot part and the True-SoA columns");
                    if self.s.t.needs_drop(&et) && self.gc_any() {
                        // fascon-gc: 新しい値を先に置いてから古い値を解放する（解放中の #[on_drop] が回収しても、スロットが
                        // 解放済みの own を指さない） / fascon-gc: store the new value first, then drop the old one (if an
                        // #[on_drop] collects during the drop, the slot never points at a freed own)
                        let ect = self.cty(&et);
                        let old = self.new_tmp("old");
                        self.emit(format!("{} {} = arena_{}_load({});", ect, old, n, ixv));
                        self.emit(format!("arena_{}_put({}, &{}); /* {} */", n, ixv, tmp, c));
                        let d = self.gc_drop_temp(&et, &old);
                        self.emit(d);
                        return;
                    }
                    if self.s.t.needs_drop(&et) {
                        let ect = self.cty(&et);
                        let d = self.drop_code(&et, "_old");
                        self.emit(format!("{{ {} _old = arena_{}_load({}); {} }}", ect, n, ixv, d));
                    }
                    self.emit(format!("arena_{}_put({}, &{}); /* {} */", n, ixv, tmp, c));
                    return;
                }
            }
        }
        // 非時間的ストアのループの中の、完全列化の列への上書き / an overwrite of a True-SoA column inside a non-temporal loop
        if op.is_none() {
            if let (Some((na, fs)), ExprKind::Field(base, name, _)) = (self.nt_cur.clone(), &lhs.kind) {
                let f = sanitize_field(name);
                if self.ts_base(base) == Some(na) && fs.contains(&f) {
                    if let Some(k) = self.ts_index(na, &f) {
                        let it = self.tsoa[na][k].it;
                        let fty = self.tsoa[na][k].fty;
                        let place = self.lv(lhs);
                        let v = self.val(rhs);
                        let m = match (fty, it.signed(), it.bits(self.o.ptr_bits) > 32) {
                            (Some(FloatTy::F32), _, _) => "F20_NT_STORE_F32",
                            (Some(FloatTy::F64), _, _) => "F20_NT_STORE_F64",
                            (None, false, true) => "F20_NT_STORE_U64",
                            (None, false, false) => "F20_NT_STORE_U32",
                            (None, true, true) => "F20_NT_STORE_I64",
                            (None, true, false) => "F20_NT_STORE_I32",
                        };
                        self.emit(format!("{}(&{}, {});", m, place, v));
                        return;
                    }
                }
            }
        }
        let cap = self.mirror_target(lhs);
        let place = self.lv(lhs);
        self.arena_cap = None;
        let w = self.arena_cap_out.take();
        self.assign_into(op, &place, &lt, rhs);
        if let (Some((a, mask)), Some(w)) = (cap, w) {
            self.emit_mirror(a, &w, mask, "Id での書き込み → 書き込みが届く列の写しを作り直す", "a write by Id → rebuild the column mirrors it reaches");
        }
    }

    fn assign_into(&mut self, op: Option<BinOp>, place: &str, lt: &Ty, rhs: &Expr) {
        match op {
            None => {
                let v = self.val(rhs);
                if self.s.t.needs_drop(lt) {
                    // 新しい値を先に作り、古い値を解放してから置き換える / build the new value, drop the old one, then store
                    let tmp = self.hoist(lt, &v);
                    let old = self.new_tmp("old");
                    let d = self.cdecl(lt, &old);
                    self.emit(format!("{} = {}; /* {} */", d, place, self.c("差し替え前の値（所有リージョンごと解放する）", "the previous value (its own region is freed)")));
                    self.emit(format!("{} = {};", place, tmp));
                    let dc = self.gc_drop_temp(lt, &old);
                    self.emit(dc);
                } else {
                    self.emit(format!("{} = {};", place, v));
                }
            }
            Some(o) => {
                let r = self.val(rhs);
                let rt = self.ty(rhs);
                let p = if is_simple_c(place) || place_is_stable(place) {
                    place.to_string()
                } else {
                    let ct = self.cty(lt);
                    let pt = self.new_tmp("p");
                    self.emit(format!("{} *{} = &{};", ct, pt, place));
                    format!("(*{})", pt)
                };
                let v = self.binop_c(o, &p, &r, lt, &rt);
                self.emit(format!("{} = {};", p, v));
            }
        }
    }

    // ───────────── 値の式 / value expressions ─────────────

    fn pure(&self, e: &Expr) -> bool {
        // I/O ポートの読み出しは副作用（UART の受信レジスタは読むと次の文字へ進む）: 書いた順のまま評価する
        // reading an I/O port is a side effect (reading a UART's receive register advances it): keep the written order
        if self.s.port_reads.contains_key(&e.id) || self.s.port_index.contains_key(&e.id) {
            return false;
        }
        match &e.kind {
            ExprKind::Int(..) | ExprKind::Float(..) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Unit | ExprKind::Path(_) | ExprKind::ArenaRef(_) => true,
            ExprKind::Field(b, _, _) => self.pure(b),
            ExprKind::Index(b, i) => self.pure(b) && self.pure(i),
            ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => self.pure(x),
            ExprKind::Binary(_, a, b) => self.pure(a) && self.pure(b),
            ExprKind::Call { args, .. } => match self.s.calls.get(&e.id) {
                Some(CallKind::Some) => args.iter().all(|a| self.pure(a)),
                Some(CallKind::Builtin(Builtin::CpuId)) | Some(CallKind::Builtin(Builtin::HeapLiveBlocks)) | Some(CallKind::Builtin(Builtin::HeapLiveBytes)) => true,
                _ => false,
            },
            ExprKind::MethodCall { recv, args, .. } => {
                let k = self.s.methods.get(&e.id).copied();
                let ok = match k {
                    Some(MethodKind::IsSome) | Some(MethodKind::IsNone) | Some(MethodKind::StrLen) | Some(MethodKind::StrByte) | Some(MethodKind::ArrLen) | Some(MethodKind::IdIndex) | Some(MethodKind::IdGeneration) | Some(MethodKind::IdRaw) | Some(MethodKind::ArenaContains(_)) | Some(MethodKind::ArenaLen(_)) | Some(MethodKind::ArenaCapacity(_)) | Some(MethodKind::ArenaValidate(_)) | Some(MethodKind::MmioLen(_)) | Some(MethodKind::MmioWidth(_)) | Some(MethodKind::MmioHeight(_)) => true,
                    Some(MethodKind::Clone) => !self.s.t.needs_drop(&self.ty(e)),
                    _ => false,
                };
                ok && self.pure(recv) && args.iter().all(|a| self.pure(a))
            }
            ExprKind::StructLit { fields, .. } => fields.iter().all(|(_, _, v)| self.pure(v)),
            ExprKind::ArrayLit(v) => v.iter().all(|x| self.pure(x)),
            ExprKind::ArrayRepeat(v, _) => self.pure(v),
            _ => false,
        }
    }

    /// 左から右への評価順を保って複数の値を下ろす / lower several values keeping left-to-right order
    fn operands(&mut self, xs: &[&Expr]) -> Vec<String> {
        let imp: Vec<bool> = xs.iter().map(|x| !self.pure(x)).collect();
        let coll = self.gc_collect_flags(xs);
        let own_mark = self.gc_own_temps.len();
        let mut out = Vec::new();
        for (i, x) in xs.iter().enumerate() {
            let v = self.val(x);
            let later = imp[i + 1..].iter().any(|b| *b);
            let gc_later = coll.get(i + 1..).map_or(false, |c| c.iter().any(|b| *b));
            // fascon-gc: この式の中で作った値（own の確保・移動した own・一時変数に入った値）は、名前だけの C 式でも、後ろの
            // 被演算子が回収しうるなら根にする / fascon-gc: a value made inside this expression (an own allocation, a moved
            // own, a value in a temporary) becomes a root even as a plain C name when a later operand may collect
            let fresh = gc_later && is_simple_c(&v) && self.gc_root_ty(&self.ty(x)) && self.gc_fresh_value(x);
            if (later && !is_simple_c(&v)) || fresh {
                let t = self.ty(x);
                let t = if let Ty::Ref(..) = t { t } else { t };
                if t == Ty::Unit || t == Ty::Never {
                    self.emit(format!("{};", v));
                    out.push("(void)0".to_string());
                } else {
                    let h = self.hoist_operand(&t, &v, gc_later);
                    out.push(h);
                }
            } else {
                out.push(v);
            }
        }
        self.gc_settle_operands(xs, &coll, &mut out, own_mark);
        out
    }

    /// fascon-gc: 並びの中で own の一時値を根にしたなら、そのあとの回収しうる被演算子も先に評価してから（根のまま回収を
    /// 越える）、一時値を受け手へ渡す（根から外す） / fascon-gc: when own temporaries were rooted in the list, the later
    /// operands that may collect are evaluated first as well (the collection happens while they are roots), then the
    /// temporaries are handed to their receiver (removed from the roots)
    fn gc_settle_operands(&mut self, xs: &[&Expr], coll: &[bool], out: &mut [String], own_mark: usize) {
        if self.gc_own_temps.len() <= own_mark {
            return;
        }
        for (j, x) in xs.iter().enumerate() {
            if coll.get(j).copied().unwrap_or(false) && !is_simple_c(&out[j]) && !out[j].trim_start().starts_with('{') && !matches!(self.ty(x), Ty::Array(..)) {
                let t = self.ty(x);
                if t == Ty::Unit || t == Ty::Never {
                    let v = std::mem::replace(&mut out[j], "(void)0".to_string());
                    self.emit(format!("{};", v));
                } else {
                    let v = std::mem::take(&mut out[j]);
                    out[j] = self.hoist_force(&t, &v);
                }
            }
        }
        self.gc_hand_over(own_mark);
    }

    fn hoist_force(&mut self, t: &Ty, v: &str) -> String {
        let n = self.new_tmp("t");
        let d = self.cdecl(t, &n);
        self.emit(format!("{} = {};", d, v));
        n
    }

    /// 値の C 式 / the C expression of a value
    pub fn val(&mut self, e: &Expr) -> String {
        if self.s.own_derefs.contains(&e.id) {
            return self.own_deref_val(e);
        }
        let v = self.val_raw(e);
        // fascon-gc: エスケープ解析による自動昇格（bump_arena → gc_arena） / automatic promotion by escape analysis
        if let Some(&(g, b)) = self.s.auto_promote.get(&e.id) {
            self.gc_used = true;
            return format!("fsc_gc_promote_{}_{}({})", self.s.t.arenas[g].name, self.s.t.arenas[b].name, v);
        }
        // generic-os: 読み取り専用の static の表から読んだ関数ポインタは、読んだ時点で署名する（値はいつも署名済み）
        // generic-os: a function pointer read from a read-only static table is signed when read (values are always signed)
        if self.o.profile == crate::target::Profile::GenericOs && !self.in_static_init {
            let t = self.ty(e);
            if matches!(t, Ty::Fn(..)) && self.rooted_at_static(e) {
                self.report.pac_signs += 1;
                let ct = self.cty(&t);
                return format!("(({})f20_pac_sign((void *){}, {}))", ct, v, pac_modifier(&self.mangle(&t)));
            }
        }
        v
    }

    /// 場所の根が static か / whether a place is rooted at a static
    fn rooted_at_static(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Path(_) => matches!(self.s.res.get(&e.id), Some(Res::Static(_))),
            ExprKind::Index(b, _) | ExprKind::Field(b, _, _) => self.rooted_at_static(b),
            _ => false,
        }
    }

    /// `*b`（own の中身）の値。場所なら `(*p)`（配列は先頭要素へのポインタ）。一時的な own（`*make()`）は中身を写してから
    /// own を解放する / the value of `*b` (the contents of an own): `(*p)` for a place (an array is the pointer to its first
    /// element); a temporary own (`*make()`) is copied out and then freed
    fn own_deref_val(&mut self, e: &Expr) -> String {
        let t = self.ty(e);
        if crate::sema::is_place_expr(e) {
            self.read_ctx = true;
            let p = self.lv_raw(e);
            self.read_ctx = false;
            return if matches!(t, Ty::Array(..)) { p } else { format!("(*{})", p) };
        }
        let own_t = Ty::Own(Box::new(t.clone()));
        let pv = self.val_raw(e);
        let p = self.hoist_force(&own_t, &pv);
        let note = self.c("一時的な own の中身を写してから own を解放する", "copy the contents of a temporary own, then free it");
        let out = if let Ty::Array(..) = t {
            let n = self.new_tmp("arr");
            let d = self.cdecl(&t, &n);
            self.emit(format!("{}; __builtin_memcpy({}, {}, sizeof {}); /* {} */", d, n, p, n, note));
            n
        } else {
            let n = self.new_tmp("t");
            let d = self.cdecl(&t, &n);
            self.emit(format!("{} = *{}; /* {} */", d, p, note));
            n
        };
        let dc = self.gc_drop_temp(&own_t, &p);
        self.emit(dc);
        out
    }

    fn val_raw(&mut self, e: &Expr) -> String {
        // 入れ子の評価には読み出しの文脈を持ち込まない / never carry the read context into nested evaluation
        self.read_ctx = false;
        let t = self.ty(e);
        match &e.kind {
            ExprKind::Int(v, _) => {
                let val = if self.s.neg_lits.contains(&e.id) { -(*v as i128) } else { *v as i128 };
                match t {
                    Ty::Int(it) => self.int_lit_c(val, it, Some(e.span)),
                    _ => format!("{}", val),
                }
            }
            ExprKind::Float(text, _) => float_lit_c(text, &t),
            ExprKind::Bool(b) => if *b { "true".to_string() } else { "false".to_string() },
            ExprKind::Str(s) => format!("FASCON_STR({})", c_escape(s)),
            ExprKind::Unit => "(void)0".to_string(),
            ExprKind::TypeCall { .. } => self.type_call_val(e),
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Local(b)) => {
                    let n = self.names.get(b).cloned().unwrap_or_else(|| "/*?*/0".to_string());
                    if self.s.auto_deref.contains(&e.id) {
                        // 配列への参照の「値」は先頭要素へのポインタそのもの（配列は memcpy で写す。`*p` は先頭要素になってしまう）
                        // the value of a reference to an array is the pointer to its first element itself (arrays are
                        // copied with memcpy; `*p` would be the first element)
                        if matches!(&self.local(*b).ty, Ty::Ref(_, x) if matches!(**x, Ty::Array(..))) {
                            return n;
                        }
                        return format!("(*{})", n);
                    }
                    if let Ty::Ref(_, inner) = &self.local(*b).ty {
                        // 参照の束縛の「値」は参照先（own・配列はペイロード / 先頭要素へのポインタそのもの）
                        // the value of a reference binding is its referent (for own / arrays the pointer itself)
                        return match &**inner {
                            Ty::Own(_) | Ty::DmaOwn(_) | Ty::Array(..) => n,
                            _ => format!("(*{})", n),
                        };
                    }
                    if self.fr.move_exprs.contains(&e.id) && self.s.t.needs_drop(&t) && self.fr.moved_locals.contains(b) {
                        // 移動: 移動元を NULL / 0 にして二重解放を防ぐ / move: clear the source to prevent a double free
                        let tmp = self.new_tmp("mv");
                        let d = self.cdecl(&t, &tmp);
                        let z = self.zero_value(&t);
                        let zs = if matches!(t, Ty::Own(_) | Ty::DmaOwn(_)) || matches!(&t, Ty::Option(i) if Self::opt_niche(i)) { format!("{} = {};", n, z) } else { format!("__builtin_memset(&{}, 0, sizeof {});", n, n) };
                        self.emit(format!("{} = {}; {} /* {} */", d, n, zs, self.c(&format!("`{}` を移動（所有権の受け渡し）", self.local(*b).name), &format!("move `{}` (ownership hand-off)", self.local(*b).name))));
                        return tmp;
                    }
                    n
                }
                Some(Res::Static(si)) => format!("fsc_{}", self.s.t.statics[*si].name),
                Some(Res::NoneLit) => self.zero_value(&t),
                // 関数の名前 = 関数ポインタ（C の関数の番地） / a function name = a function pointer (the C function's address)
                Some(Res::Fn(fi)) => self.fn_c_names.get(fi).cloned().unwrap_or_else(|| "NULL".to_string()),
                // `EINVAL` など（errno と同じ値） / `EINVAL` etc. (the errno values)
                Some(Res::FaultConst(c)) => format!("INT64_C({})", c),
                Some(Res::FloatConst(ft, c)) => float_const_c(*ft, c),
                None => "0".to_string(),
            },
            // big-endian のワイヤ構造体の 2 バイト以上の整数: 読むたびにバイト順を入れ替える
            // an integer of 2 bytes or more in a big-endian wire struct: byte-swapped at every read
            ExprKind::Field(..) if self.be_skip != Some(e.id) && matches!(self.s.wire_int_field(e), Some((_, true))) => {
                let it = self.s.wire_int_field(e).map(|x| x.0).unwrap_or(IntTy::U16);
                self.be_skip = Some(e.id);
                let raw = self.val(e);
                self.be_skip = None;
                format!("(({})fascon_be{}({}))", it.c_name(), it.bits(64), raw)
            }
            ExprKind::Field(..) | ExprKind::Index(..) => {
                if let (Some(&p), ExprKind::Index(_, idx)) = (self.s.port_index.get(&e.id), &e.kind) {
                    return self.port_read(e, p, Some(idx));
                }
                if let Some(&(m, depth)) = self.s.mmio_index.get(&e.id) {
                    let info = self.s.t.mmios[m].clone();
                    if depth == info.dims.len() {
                        if info.framebuffer {
                            let p = self.mmio_ptr(e, m);
                            return format!("(*{})", p);
                        }
                        let before = self.at_count;
                        let a = self.mmio_addr(e, m);
                        let a = self.effect_arg(before, &Ty::Int(IntTy::Usize), a);
                        let site = self.site(e.span);
                        self.commit(&site);
                        return format!("fascon_mmio_read{}({})", info.elem.bits(64), a);
                    }
                }
                // 右辺値として読むだけ: この場所の鎖の上のアリーナ参照は読み出し用でよい
                // read as an rvalue: arena accesses on this place's own chain may use the read form
                self.read_ctx = true;
                let base = self.lv(e);
                self.read_ctx = false;
                base
            }
            ExprKind::ArenaRef(_) => match self.s.port_reads.get(&e.id) {
                Some(&p) => self.port_read(e, p, None),
                None => "0".to_string(),
            },
            ExprKind::OwnAlloc(_) => self.own_alloc_expr(e),
            ExprKind::Call { callee, args, .. } => self.call(e, callee, args),
            ExprKind::CallIndirect { callee, args } => {
                // 呼ばれる式を先に評価する（C では関数の式と引数の評価順が決まっていないため一時変数へ）
                // evaluate the callee first (C leaves the order of the function designator and the arguments unspecified,
                // so it goes into a temporary)
                let ct = self.ty(callee);
                let cv = self.val(callee);
                let cv = if is_simple_c(&cv) { cv } else { self.hoist_force(&ct, &cv) };
                let cv = self.pac_auth(&ct, cv);
                let params = match &ct {
                    Ty::Fn(ps, _) => ps.clone(),
                    _ => Vec::new(),
                };
                let ret = match &ct {
                    Ty::Fn(_, r) => (**r).clone(),
                    _ => Ty::Unit,
                };
                self.emit_call(e, &cv, &params, &ret, args)
            }
            ExprKind::MethodCall { recv, method, args, .. } => self.method(e, recv, method, args),
            ExprKind::StructLit { .. } => self.struct_lit(e, None),
            ExprKind::ArrayLit(_) | ExprKind::ArrayRepeat(..) => {
                // 配列リテラルの値は一時配列にする（配列は C では値で渡せない） / an array literal becomes a temporary array
                let n = self.new_tmp("arr");
                let d = self.cdecl(&t, &n);
                match &e.kind {
                    ExprKind::ArrayLit(elems) => {
                        let refs: Vec<&Expr> = elems.iter().collect();
                        let vals = self.operands(&refs);
                        self.emit(format!("{} = {{ {} }};", d, vals.join(", ")));
                    }
                    ExprKind::ArrayRepeat(v, _) => {
                        if self.is_zero_like(v) {
                            self.emit(format!("{} = {{ 0 }};", d));
                        } else {
                            let val = self.val(v);
                            let val = self.hoist(&self.ty(v), &val);
                            let len = if let Ty::Array(_, k) = &t { *k } else { 0 };
                            self.emit(format!("{};", d));
                            self.emit(format!("for (size_t _i = 0; _i < {}u; _i++) {}[_i] = {};", len, n, val));
                        }
                    }
                    _ => {}
                }
                n
            }
            ExprKind::Own(inner) => self.own_expr(e, inner),
            ExprKind::Unary(op, x) => {
                let v = self.val(x);
                match (op, &t) {
                    (UnOp::Neg, Ty::Int(it)) => {
                        if let ExprKind::Int(..) = x.kind {
                            v
                        } else {
                            format!("fascon_neg_{}({})", arith_suffix(*it), v)
                        }
                    }
                    (UnOp::Not, Ty::Bool) => format!("!{}", paren(&v)),
                    (UnOp::Not, Ty::Int(it)) => format!("(({})~{})", it.c_name(), paren(&v)),
                    // 浮動小数点の符号反転（-0.0 も作る。検査は要らない） / float negation (makes -0.0 too; nothing to check)
                    (UnOp::Neg, Ty::Float(_)) => format!("(-{})", paren(&v)),
                    _ => v,
                }
            }
            ExprKind::Binary(op, a, b) => {
                if matches!(op, BinOp::And | BinOp::Or) {
                    return self.short_circuit(*op, a, b);
                }
                let at = self.ty(a);
                let bt = self.ty(b);
                let vs = self.operands(&[a, b]);
                self.binop_c(*op, &vs[0], &vs[1], &at, &bt)
            }
            ExprKind::Cast(x, _) => {
                let v = self.val(x);
                let xt = self.ty(x);
                if xt == t {
                    v
                } else if let (Ty::Float(_), Ty::Int(it)) = (&xt, &t) {
                    // 浮動小数点 → 整数は飽和（NaN は 0）: C の範囲外の変換（未定義動作）を出さない
                    // float to integer saturates (NaN becomes 0): never the out-of-range conversion of C (undefined behaviour)
                    let (lo, hi) = int_limits_c(*it);
                    if it.signed() {
                        format!("(({})f20_f2s((double){}, {}, {}))", it.c_name(), paren(&v), lo, hi)
                    } else {
                        format!("(({})f20_f2u((double){}, {}))", it.c_name(), paren(&v), hi)
                    }
                } else {
                    format!("({}){}", self.cty(&t), paren(&v))
                }
            }
            ExprKind::Ref { inner, .. } => self.addr_place(inner),
            ExprKind::If { .. } | ExprKind::IfLet { .. } | ExprKind::Match { .. } | ExprKind::Block(_) | ExprKind::Critical(_) | ExprKind::Atomic(_) | ExprKind::RcuRead(_) | ExprKind::CatchFault(_) => {
                if t == Ty::Unit || t == Ty::Never || t.is_err() {
                    self.ctrl(e, None);
                    return "(void)0".to_string();
                }
                let n = self.new_tmp("v");
                let d = self.cdecl(&t, &n);
                self.emit(format!("{};", d));
                self.ctrl(e, Some(&n));
                n
            }
            ExprKind::Assign { op, lhs, rhs } => {
                self.assign(*op, lhs, rhs);
                "(void)0".to_string()
            }
            ExprKind::While { .. } | ExprKind::Loop { .. } | ExprKind::For { .. } | ExprKind::Break | ExprKind::Continue | ExprKind::Return(_) | ExprKind::VolatileBypass(_) => {
                self.expr_stmt(e);
                "(void)0".to_string()
            }
            ExprKind::Unsupported(_) => "0".to_string(),
        }
    }

    /// I/O ポートの読み出し（`let c = com1;` → x86_64: `in al, dx`）。外への作用なのでラッチはここでコミットする
    /// a read of an I/O port (`let c = com1;` → x86_64: `in al, dx`); an external effect, so the latch commits here
    fn port_read(&mut self, e: &Expr, p: usize, idx: Option<&Expr>) -> String {
        let info = self.s.t.ports[p].clone();
        let before = self.at_count;
        let i = match idx {
            Some(x) => {
                let len = info.len.unwrap_or(1);
                let v = self.index_c(e.id, x, len);
                self.effect_arg(before, &Ty::Int(IntTy::Usize), v)
            }
            None => "0u".to_string(),
        };
        let site = self.site(e.span);
        self.commit(&site);
        self.report.port_accesses += 1;
        format!("pio_{}_in({})", info.name, strip_parens(&i))
    }

    fn short_circuit(&mut self, op: BinOp, a: &Expr, b: &Expr) -> String {
        let av = self.val(a);
        let saved = std::mem::take(&mut self.lines);
        let saved_ind = self.ind;
        self.ind = 0;
        let bv = self.val(b);
        let pre = std::mem::replace(&mut self.lines, saved);
        self.ind = saved_ind;
        let cop = if op == BinOp::And { "&&" } else { "||" };
        if pre.is_empty() {
            return format!("({} {} {})", av, cop, bv);
        }
        let t = self.new_tmp("b");
        self.emit(format!("bool {} = {};", t, av));
        self.emit(format!("if ({}{}) {{", if op == BinOp::And { "" } else { "!" }, t));
        self.ind += 1;
        for l in pre {
            self.emit(l);
        }
        self.emit(format!("{} = {};", t, bv));
        self.ind -= 1;
        self.emit("}");
        t
    }

    fn binop_c(&self, op: BinOp, a: &str, b: &str, at: &Ty, bt: &Ty) -> String {
        let it = match at {
            Ty::Int(i) => Some(*i),
            _ => None,
        };
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                let f = match op {
                    BinOp::Add => "add",
                    BinOp::Sub => "sub",
                    BinOp::Mul => "mul",
                    BinOp::Div => "div",
                    _ => "rem",
                };
                match it {
                    Some(i) => format!("fascon_{}_{}({}, {})", f, arith_suffix(i), strip_parens(a), strip_parens(b)),
                    None => format!("({} {} {})", a, op.text(), b),
                }
            }
            BinOp::Shl | BinOp::Shr => {
                let f = if op == BinOp::Shl { "shl" } else { "shr" };
                let bcast = match bt {
                    Ty::Int(IntTy::U32) => strip_parens(b).to_string(),
                    _ => format!("(uint32_t){}", paren(b)),
                };
                match it {
                    Some(i) => format!("fascon_{}_{}({}, {})", f, arith_suffix(i), strip_parens(a), bcast),
                    None => format!("({} {} {})", a, op.text(), b),
                }
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => match it {
                Some(i) if matches!(i, IntTy::I8 | IntTy::I16 | IntTy::U8 | IntTy::U16) => format!("(({})({} {} {}))", i.c_name(), a, op.text(), b),
                _ => format!("({} {} {})", a, op.text(), b),
            },
            BinOp::Eq | BinOp::Ne if matches!(at.deref_all(), Ty::Slice(_)) => {
                let e = format!("f20_slice_eq({}, {})", a, b);
                if op == BinOp::Eq {
                    e
                } else {
                    format!("!{}", e)
                }
            }
            BinOp::Eq | BinOp::Ne if matches!(at, Ty::Array(..)) => {
                let e = format!("(__builtin_memcmp({a}, {b}, sizeof({a})) == 0)", a = a, b = b);
                if op == BinOp::Eq {
                    e
                } else {
                    format!("!{}", e)
                }
            }
            BinOp::Eq | BinOp::Ne if *at == Ty::Str => {
                let e = format!("fascon_str_eq({}, {})", a, b);
                if op == BinOp::Eq {
                    e
                } else {
                    format!("!{}", e)
                }
            }
            BinOp::Eq | BinOp::Ne if matches!(at, Ty::Option(i) if !Self::opt_niche(i)) => {
                let e = format!("({a}.is_some == {b}.is_some && (!{a}.is_some || {a}.value == {b}.value))", a = a, b = b);
                if op == BinOp::Eq {
                    e
                } else {
                    format!("!{}", e)
                }
            }
            _ => format!("({} {} {})", a, op.text(), b),
        }
    }

    // ───────────── 場所 / places ─────────────

    /// 場所の左辺値 / the lvalue of a place
    fn lv(&mut self, e: &Expr) -> String {
        // `*b`: own の中身（配列は先頭要素へのポインタのまま） / `*b`: the contents of an own (an array stays the pointer to
        // its first element)
        if self.s.own_derefs.contains(&e.id) {
            if !crate::sema::is_place_expr(e) {
                return self.own_deref_val(e);
            }
            let p = self.lv_raw(e);
            return if matches!(self.ty(e), Ty::Array(..)) { p } else { format!("(*{})", p) };
        }
        self.lv_raw(e)
    }

    fn lv_raw(&mut self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Path(_) => match self.s.res.get(&e.id) {
                Some(Res::Local(b)) => {
                    let n = self.names.get(b).cloned().unwrap_or_default();
                    if matches!(&self.local(*b).ty, Ty::Ref(_, x) if matches!(**x, Ty::Array(..))) {
                        // 配列への参照は先頭要素へのポインタのまま使う / a reference to an array stays the pointer to its first element
                        return n;
                    }
                    if self.s.auto_deref.contains(&e.id) || matches!(self.local(*b).ty, Ty::Ref(_, ref x) if !matches!(**x, Ty::Own(_) | Ty::DmaOwn(_) | Ty::Array(..)) && !self.s.t.is_copy(x)) {
                        return format!("(*{})", n);
                    }
                    n
                }
                Some(Res::Static(si)) => format!("fsc_{}", self.s.t.statics[*si].name),
                _ => self.val(e),
            },
            ExprKind::Field(base, name, _) => {
                // 完全列化したフィールド: 列の 1 要素（唯一の正本） / a True-SoA field: one column element (the only copy)
                if let Some(a) = self.ts_base(base) {
                    let f = sanitize_field(name);
                    if self.ts_index(a, &f).is_some() {
                        return self.ts_lv(base, a, &f);
                    }
                }
                let (b, arrow) = self.field_base(base);
                format!("{}{}{}", b, if arrow { "->" } else { "." }, sanitize_field(name))
            }
            ExprKind::Index(base, idx) => {
                if let Some(&a) = self.s.arena_index.get(&e.id) {
                    if self.tsoa.get(a).map_or(false, |v| !v.is_empty()) {
                        // 完全列化のアリーナの要素まるごと（読み出し）: スロットに残る部分と列から組み立て直す。書き込みは assign が
                        // arena_<名前>_put で分ける（参照・メソッド・束縛は完全列化の判定で除いてある）
                        // a whole element of a True-SoA arena (a read): rebuilt from the slot part and the columns; writes
                        // are split by assign through arena_<name>_put (references, methods and bindings were ruled out)
                        self.read_ctx = true;
                        let ix = self.arena_ix(e, a, idx);
                        let n = self.s.t.arenas[a].name.clone();
                        let et = self.s.t.arenas[a].elem.clone();
                        return self.hoist_force(&et, &format!("arena_{}_load({})", n, strip_parens(&ix)));
                    }
                    let p = self.arena_ptr(e, a, idx);
                    return format!("(*{})", p);
                }
                if let Some(&(m, _)) = self.s.mmio_index.get(&e.id) {
                    if self.s.t.mmios[m].framebuffer {
                        let p = self.mmio_ptr(e, m);
                        return format!("(*{})", p);
                    }
                }
                if let Some(&d) = self.s.dma_index.get(&e.id) {
                    let info = self.s.t.dmas[d].clone();
                    let i = self.index_c(e.id, idx, info.len);
                    return format!("dma_{}[{}]", info.name, i);
                }
                let bt = self.ty(base);
                if let Ty::Slice(_) = bt.deref_all() {
                    return self.slice_index(base, idx);
                }
                let len = match bt.deref_all() {
                    Ty::Array(_, n) => *n,
                    _ => 0,
                };
                let b = self.lv_array_base(base);
                let i = self.index_c(e.id, idx, len);
                format!("{}[{}]", b, i)
            }
            _ => {
                let t = self.ty(e);
                let v = self.val(e);
                self.hoist_force(&t, &v)
            }
        }
    }

    /// バイト列の式（窓か `[u8; N]`）→（先頭の番地の C 式, 長さの C 式） / a bytes expression → (start address, length) in C
    fn bytes_of(&mut self, recv: &Expr) -> (String, String) {
        let t = self.ty(recv);
        match t.deref_all() {
            Ty::Slice(_) => {
                let s = self.slice_ptr(recv);
                (format!("{}->p", s), format!("{}->n", s))
            }
            Ty::Array(_, n) => {
                let n = *n;
                (self.lv_array_base(recv), format!("UINT64_C({})", n))
            }
            _ => ("NULL".to_string(), "0u".to_string()),
        }
    }

    /// 窓の束縛・引数の C 式（f20_slice_t へのポインタ） / the C expression of a window binding or parameter
    fn slice_ptr(&mut self, e: &Expr) -> String {
        if let ExprKind::Path(_) = &e.kind {
            if let Some(Res::Local(b)) = self.s.res.get(&e.id) {
                return self.names.get(b).cloned().unwrap_or_default();
            }
        }
        self.val(e)
    }

    /// 窓の引数（`&[u8]` / `&mut [u8]`）の C 式: f20_slice_t へのポインタ / a window argument: a pointer to an f20_slice_t
    fn slice_arg(&mut self, a: &Expr, from: crate::sema::SliceFrom) -> String {
        use crate::sema::SliceFrom;
        match from {
            SliceFrom::Slice => self.slice_ptr(a),
            SliceFrom::Array(n) => {
                let p = match &a.kind {
                    ExprKind::Ref { inner, .. } => self.lv_array_base(inner),
                    _ => self.slice_ptr(a),
                };
                let w = self.new_tmp("w");
                self.emit(format!("f20_slice_t {} = {{ (uint8_t *)({}), UINT64_C({}) }};", w, p, n));
                format!("&{}", w)
            }
            SliceFrom::Str => {
                let v = self.val(a);
                let sv = self.new_tmp("s");
                self.emit(format!("const FasconStr {} = {};", sv, v));
                let w = self.new_tmp("w");
                self.emit(format!("f20_slice_t {} = {{ (uint8_t *)(uintptr_t){}.ptr, {}.len }};", w, sv, sv));
                format!("&{}", w)
            }
        }
    }

    fn lv_array_base(&mut self, base: &Expr) -> String {
        // 配列への参照は先頭要素へのポインタ / a reference to an array is a pointer to its first element
        match &base.kind {
            ExprKind::Path(_) => match self.s.res.get(&base.id) {
                Some(Res::Local(b)) => self.names.get(b).cloned().unwrap_or_default(),
                Some(Res::Static(si)) => format!("fsc_{}", self.s.t.statics[*si].name),
                _ => self.lv(base),
            },
            _ => self.lv(base),
        }
    }

    /// フィールドの土台: (C 式, `->` を使うか) / field base: (C expr, whether to use `->`)
    fn field_base(&mut self, base: &Expr) -> (String, bool) {
        let bt = self.ty(base);
        if let Some(&a) = self.s.arena_index.get(&base.id) {
            if let ExprKind::Index(_, idx) = &base.kind {
                let p = self.arena_ptr(base, a, idx);
                return (p, true);
            }
        }
        match &bt {
            // bind_mmio のハンドル: volatile なレジスタ構造体へのポインタ（すべてのフィールドの読み書きが volatile）
            // a bind_mmio handle: a pointer to a volatile register struct (every field access is volatile)
            Ty::MmioBind(sid) => {
                let sn = self.struct_name(*sid);
                let v = self.val(base);
                (format!("((volatile {} *)({}))", sn, v), true)
            }
            Ty::Own(_) | Ty::Ref(..) | Ty::DmaOwn(_) => {
                let v = match &base.kind {
                    ExprKind::Path(_) => match self.s.res.get(&base.id) {
                        Some(Res::Local(b)) => self.names.get(b).cloned().unwrap_or_default(),
                        _ => self.lv(base),
                    },
                    _ => self.lv(base),
                };
                (v, true)
            }
            _ => (self.lv(base), false),
        }
    }

    fn arena_ptr(&mut self, e: &Expr, a: usize, idx: &Expr) -> String {
        let read = std::mem::replace(&mut self.read_ctx, false);
        let id = self.val(idx);
        let n = self.s.t.arenas[a].name.clone();
        let ts_ix = match &idx.kind {
            ExprKind::Path(_) if self.tsoa.get(a).map_or(false, |x| !x.is_empty()) => match self.s.res.get(&idx.id) {
                Some(Res::Local(b)) => self.loop_ix.get(b).cloned(),
                _ => None,
            },
            _ => None,
        };
        let p = if let (true, Some(ix)) = (self.fr.proven.contains(&e.id), ts_ix) {
            // 完全列化の表を回すループの、証明済みのループ変数: スロットの添字をそのまま使う（gen[] を読まない）
            // a proven loop variable of a loop over a True-SoA table: use the slot index directly (no gen[] read)
            self.report.gen_elided += 1;
            format!("(&arena_{}.slot[{}].v)", n, ix)
        } else if self.fr.proven.contains(&e.id) {
            // 生存が証明済み: マスクだけ / proven live: a mask only
            self.report.gen_elided += 1;
            format!("arena_{}_slot({})", n, id)
        } else if self.s.t.needs_drop(&self.s.t.arenas[a].elem) {
            // own を持つ要素: 番兵へ逸らすと own の唯一性が崩れるのでその場で止める / elements holding own stop on the spot
            self.report.gen_checked += 1;
            let site = self.site(e.span);
            format!("arena_{}_at_strict({}, {})", n, id, site)
        } else {
            // 分岐なし: 番兵へ逸らしてラッチ（止めるのは次のコミット点） / branch-free: divert to the sentinel and latch
            self.report.gen_latched += 1;
            self.flt_used = true;
            self.at_count += 1;
            if read {
                format!("arena_{}_at_r({}, &_flt)", n, id)
            } else {
                format!("arena_{}_at({}, &_flt)", n, id)
            }
        };
        if self.arena_cap == Some(e.id) && !read {
            // 書き込みのあとで列の写しを作り直すので、要素ポインタを一時変数にも取る。代入は場所の式の中に置く（評価の順序は
            // 1.0.0 と同じ: 右辺が先に済んでいても、ポインタはこの書き込みの時点のもの）
            // the mirror is rebuilt after the write, so the element pointer is also kept in a temporary; the assignment
            // sits inside the place expression (evaluation order as in 1.0.0: the pointer is the one of this very write)
            self.arena_cap = None;
            // 完全列化のアリーナではスロットに残る部分の型（代入式の型は左辺の型なので、ここが要素まるごとの型だと
            // `(_w = arena_<名前>_at(..))->f` のフィールドの位置がずれる）
            // for a True-SoA arena the type of the part left in the slot (an assignment expression has the type of its
            // left side, so the whole-element type here would shift the field offsets of `(_w = arena_<name>_at(..))->f`)
            let et = if self.tsoa.get(a).map_or(false, |v| !v.is_empty()) { self.ts_part(a) } else { self.cty(&self.s.t.arenas[a].elem.clone()) };
            let w = self.new_tmp("w");
            self.emit(format!("{} *{}; /* {} */", et, w, self.c("書き込む要素（列の写しの作り直しに使う）", "the element written (for rebuilding its column mirror)")));
            self.arena_cap_out = Some(w.clone());
            return format!("({} = {})", w, p);
        }
        p
    }

    /// 書き込み先が列の読むフィールドに届くなら、その `@a[i]` の要素ポインタを取っておくよう印を付ける（アリーナを返す）
    /// when a write target reaches a field a column reads, mark its `@a[i]` so the element pointer is kept (returns the arena)
    fn mirror_target(&mut self, place: &Expr) -> Option<(usize, u64)> {
        let (a, root, path) = self.arena_write_root(place)?;
        let mask = self.col_mask(a, &[path]);
        if mask == 0 {
            return None;
        }
        self.arena_cap = Some(root.id);
        self.arena_cap_out = None;
        Some((a, mask))
    }

    /// 写しを作り直す C の文（mask の列だけ。全部なら arena_<名前>_mirror_p） / the C statement that rebuilds the mirror
    /// (only the columns in mask; arena_<name>_mirror_p when that is all of them)
    fn mirror_call(&mut self, a: usize, w: &str, mask: u64) -> String {
        let n = self.s.t.arenas[a].name.clone();
        if mask == self.col_all(a) || mask == u64::MAX {
            format!("arena_{}_mirror_p({});", n, w)
        } else {
            self.mirror_masks[a].insert(mask);
            format!("arena_{}_mirror_m{:x}({});", n, mask, w)
        }
    }

    /// 書き込みのあとの写しの作り直し / rebuild the mirror after a write
    fn emit_mirror(&mut self, a: usize, w: &str, mask: u64, why_ja: &str, why_en: &str) {
        self.report.mirror_sites += 1;
        if !self.report.mirror_fns.contains(&(self.cur_fn, a)) {
            self.report.mirror_fns.push((self.cur_fn, a));
        }
        // 同じ表を回すループの中の作り直し = 表ごとの作り直し / a rebuild inside a loop over the same table = a table-wide
        // rebuild
        if let Some(&(_, sp)) = self.arena_loops.iter().rev().find(|(x, _)| *x == a) {
            let n = self.s.t.arenas[a].name.clone();
            if !self.report.mirror_loops.iter().any(|(f, s2, _)| *f == self.cur_fn && *s2 == sp) {
                self.report.mirror_loops.push((self.cur_fn, sp, n));
            }
        }
        let call = self.mirror_call(a, w, mask);
        let c = self.c(why_ja, why_en);
        self.emit(format!("{} /* {} */", call, c));
    }

    /// 窓の要素 `s[i]`: 長さは実行時の値なので、境界の検査は毎回（範囲外はパニック 0x000C）
    /// a window element `s[i]`: the length is a run-time value, so the bounds check always runs (panic 0x000C when out)
    fn slice_index(&mut self, base: &Expr, idx: &Expr) -> String {
        let s = self.slice_ptr(base);
        let v = self.val(idx);
        let signed = matches!(self.ty(idx), Ty::Int(i) if i.signed());
        self.report.bounds_checked += 1;
        let site = self.site(idx.span);
        let ns = if self.spectre_idx.contains(&idx.id) {
            self.report.spectre_sites += 1;
            "_ns"
        } else {
            ""
        };
        format!("{}->p[fascon_idx_{}{}({}, {}->n, {})]", s, if signed { "s" } else { "u" }, ns, strip_parens(&v), s, site)
    }

    fn index_c(&mut self, eid: ExprId, idx: &Expr, len: u64) -> String {
        let v = self.val(idx);
        let it = match self.ty(idx) {
            Ty::Int(i) => i,
            _ => IntTy::Usize,
        };
        if self.fr.bounds_elided.contains(&eid) {
            self.report.bounds_elided += 1;
            if it == IntTy::Usize {
                return strip_parens(&v).to_string();
            }
            return format!("(uintptr_t){}", paren(&v));
        }
        self.report.bounds_checked += 1;
        let site = self.site(idx.span);
        // generic-os: 外から来た値の添字は、検査の後に推測実行でも範囲外を読まないマスク（AArch64: CSDB）を掛ける
        // generic-os: an index from outside data also gets a mask that holds under speculation (AArch64: CSDB)
        let ns = if self.spectre_idx.contains(&idx.id) {
            self.report.spectre_sites += 1;
            "_ns"
        } else {
            ""
        };
        if it.signed() {
            format!("fascon_idx_s{}({}, {}u, {})", ns, strip_parens(&v), len, site)
        } else {
            format!("fascon_idx_u{}({}, {}u, {})", ns, strip_parens(&v), len, site)
        }
    }

    fn mmio_indices(&mut self, e: &Expr, m: usize) -> Vec<String> {
        let info = self.s.t.mmios[m].clone();
        let mut out = Vec::new();
        if info.dims.len() == 2 {
            if let ExprKind::Index(b, x) = &e.kind {
                if let ExprKind::Index(_, y) = &b.kind {
                    let yi = self.index_c(b.id, y, info.dims[0]);
                    let xi = self.index_c(e.id, x, info.dims[1]);
                    out.push(yi);
                    out.push(xi);
                }
            }
        } else if let ExprKind::Index(_, i) = &e.kind {
            let ii = self.index_c(e.id, i, info.dims[0]);
            out.push(ii);
        }
        out
    }

    fn mmio_ptr(&mut self, e: &Expr, m: usize) -> String {
        let n = self.s.t.mmios[m].name.clone();
        let idx = self.mmio_indices(e, m);
        format!("mmio_{}_ptr({})", n, idx.join(", "))
    }

    fn mmio_addr(&mut self, e: &Expr, m: usize) -> String {
        let n = self.s.t.mmios[m].name.clone();
        let idx = self.mmio_indices(e, m);
        format!("mmio_{}_addr({})", n, idx.join(", "))
    }

    /// 場所へのポインタ（参照引数として渡す） / a pointer to a place (passed as a reference argument)
    fn addr_place(&mut self, e: &Expr) -> String {
        // 借用した場所は書かれうる / a borrowed place may be written through
        self.read_ctx = false;
        let t = self.ty(e);
        if let Some(&a) = self.s.arena_index.get(&e.id) {
            if let ExprKind::Index(_, idx) = &e.kind {
                return self.arena_ptr(e, a, idx);
            }
        }
        if let ExprKind::Path(_) = &e.kind {
            if let Some(Res::Local(b)) = self.s.res.get(&e.id) {
                let n = self.names.get(b).cloned().unwrap_or_default();
                let lt = self.local(*b).ty.clone();
                return match lt {
                    Ty::Ref(..) | Ty::Own(_) | Ty::DmaOwn(_) | Ty::Array(..) => n,
                    _ => format!("&{}", n),
                };
            }
        }
        if !crate::sema::is_place_expr(e) {
            // 一時値への参照: 値を一時変数に置いて借りる / reference to a temporary
            let v = self.val(e);
            let tmp = self.hoist_force(&t, &v);
            return match t {
                Ty::Own(_) | Ty::DmaOwn(_) | Ty::Array(..) => tmp,
                _ => format!("&{}", tmp),
            };
        }
        match t {
            Ty::Own(_) | Ty::DmaOwn(_) | Ty::Ref(..) => self.lv(e),
            Ty::Array(..) => self.lv(e),
            _ => format!("&{}", self.lv(e)),
        }
    }

    // ───────────── 呼び出し / calls ─────────────

    fn call(&mut self, e: &Expr, callee: &str, args: &[Expr]) -> String {
        if let Some(c) = self.gc_call(e, args) {
            return c;
        }
        let kind = self.s.calls.get(&e.id).copied();
        match kind {
            Some(CallKind::Some) => {
                let t = self.ty(e);
                let v = self.val(&args[0]);
                self.some_value(&t, &v)
            }
            // Result の構築: {is_some = 成功, value, err} / building a Result: {is_some = success, value, err}
            Some(CallKind::Ok) | Some(CallKind::Err) => {
                let t = self.ty(e);
                let c = self.cty(&t);
                let ok = matches!(kind, Some(CallKind::Ok));
                let at = self.ty(&args[0]);
                if matches!(at, Ty::Unit | Ty::Never) {
                    let _ = self.val(&args[0]);
                    return format!("({}){{ .is_some = {} }}", c, ok);
                }
                let v = self.val(&args[0]);
                if ok {
                    if let Ty::Array(..) = at {
                        let n = self.new_tmp("r");
                        self.emit(format!("{} {} = {{ .is_some = true }};", c, n));
                        self.emit(format!("__builtin_memcpy({}.value, {}, sizeof {}.value);", n, v, n));
                        return n;
                    }
                    format!("({}){{ .is_some = true, .value = {} }}", c, v)
                } else {
                    format!("({}){{ .is_some = false, .err = {} }}", c, v)
                }
            }
            Some(CallKind::User(fi)) => {
                let params: Vec<Ty> = self.s.t.fns[fi].params.iter().map(|p| p.ty.clone()).collect();
                let ret = self.s.t.fns[fi].ret.clone();
                let name = self.fn_c_names[&fi].clone();
                let v = self.emit_call(e, &name, &params, &ret, args);
                if self.propagate_fns.contains(&fi) && self.flt_on() {
                    // 呼び出し先のラッチを自分のラッチへ取り込む（直後のコミット点で扱う） / take the callee's latch into ours
                    let r = if matches!(ret, Ty::Unit | Ty::Never) {
                        self.emit(format!("{};", v));
                        "(void)0".to_string()
                    } else {
                        self.hoist_force(&ret, &v)
                    };
                    self.flt_used = true;
                    self.emit(format!("_flt |= fascon__flt_pending; fascon__flt_pending = 0u; /* {} */", self.c("呼び出し先のラッチ", "the callee's latch")));
                    return r;
                }
                v
            }
            // 関数ポインタのローカル・引数・static を名前で呼ぶ / calling a function-pointer local, parameter or static by name
            Some(CallKind::Indirect) => {
                let (cv, ct) = match self.s.call_targets.get(&e.id) {
                    Some(Res::Local(b)) => (self.names.get(b).cloned().unwrap_or_default(), self.local(*b).ty.clone()),
                    Some(Res::Static(si)) => (format!("fsc_{}", self.s.t.statics[*si].name), self.s.t.statics[*si].ty.clone()),
                    _ => ("NULL".to_string(), Ty::Err),
                };
                // static の関数ポインタは読み取り専用の表の生の値（W^X で守られる）なので認証しない / a static function pointer is
                // a raw value in a read-only table (protected by W^X), so it is not authenticated
                let cv = if matches!(self.s.call_targets.get(&e.id), Some(Res::Static(_))) { cv } else { self.pac_auth(&ct, cv) };
                let (params, ret) = match ct {
                    Ty::Fn(ps, r) => (ps, *r),
                    _ => (Vec::new(), Ty::Unit),
                };
                self.emit_call(e, &cv, &params, &ret, args)
            }
            Some(CallKind::Builtin(b)) => self.builtin(e, b, args),
            None => format!("/* unresolved {} */ 0", callee),
        }
    }

    /// 呼び出しを下ろす（関数の直接呼び出しと関数ポインタの呼び出しで共通）: 参照引数は場所のアドレス、値引数は値。
    /// 呼ぶ前にコミット点、&mut @a[id] を渡したら直後に列の写しを作り直す
    /// lower a call (shared by direct calls and calls through function pointers): reference arguments are addresses of
    /// places, value arguments are values; a commit point before the call, and column mirrors rebuilt right after a call
    /// that received &mut @a[id]
    fn emit_call(&mut self, e: &Expr, name: &str, params: &[Ty], ret: &Ty, args: &[Expr]) -> String {
        let before = self.at_count;
        let refs: Vec<&Expr> = args.iter().collect();
        // 参照引数は場所のアドレス、値引数は値 / reference args are addresses, value args are values
        let imp: Vec<bool> = refs.iter().map(|x| !self.pure(x)).collect();
        let coll = self.gc_collect_flags(&refs);
        let own_mark = self.gc_own_temps.len();
        let mut vals = Vec::new();
        let mut temps_to_drop: Vec<(Ty, String)> = Vec::new();
        // &mut @a[id]（列が読むフィールドに届くもの）を渡したら、呼び出しの直後に写しを作り直す
        // a &mut @a[id] argument reaching a field a column reads: rebuild the mirror right after the call
        let mut mirrors: Vec<(usize, u64, String)> = Vec::new();
        for (i, a) in refs.iter().enumerate() {
            let is_ref = matches!(params.get(i), Some(Ty::Ref(..)));
            let v = if let Some(from) = self.s.slice_args.get(&a.id).copied() {
                self.slice_arg(a, from)
            } else if is_ref {
                match &a.kind {
                    ExprKind::Ref { inner, mutable } => {
                        let it = self.ty(inner);
                        let cap = if *mutable { self.mirror_target(inner) } else { None };
                        let r = self.addr_place(inner);
                        self.arena_cap = None;
                        if let (Some((ma, mask)), Some(w)) = (cap, self.arena_cap_out.take()) {
                            mirrors.push((ma, mask, w));
                        }
                        if !crate::sema::is_place_expr(inner) && self.s.t.needs_drop(&it) {
                            let base = r.trim_start_matches('&').to_string();
                            temps_to_drop.push((it, base));
                        }
                        r
                    }
                    ExprKind::Path(_) => match self.s.res.get(&a.id) {
                        Some(Res::Local(b)) => self.names.get(b).cloned().unwrap_or_default(),
                        _ => self.val(a),
                    },
                    _ => self.val(a),
                }
            } else {
                self.val(a)
            };
            let later = imp[i + 1..].iter().any(|b| *b);
            let gc_later = coll.get(i + 1..).map_or(false, |c| c.iter().any(|b| *b));
            let fresh = !is_ref && gc_later && is_simple_c(&v) && self.gc_root_ty(&self.ty(a)) && self.gc_fresh_value(a);
            if (later && !is_simple_c(&v) && !is_ref) || fresh {
                let t = self.ty(a);
                let h = self.hoist_operand(&t, &v, gc_later);
                vals.push(h);
            } else {
                vals.push(v);
            }
        }
        // fascon-gc: 値渡しの own の一時値は、回収しうる被演算子を評価し終えてから呼び出し先の引数へ渡す（根から外す）
        // fascon-gc: by-value own temporaries go to the callee's parameters (leaving the roots) once the operands that may
        // collect have been evaluated
        if self.gc_own_temps.len() > own_mark {
            let ok: Vec<bool> = refs.iter().enumerate().map(|(i, _)| !matches!(params.get(i), Some(Ty::Ref(..)))).collect();
            let coll2: Vec<bool> = coll.iter().enumerate().map(|(i, c)| *c && ok[i]).collect();
            self.gc_settle_operands(&refs, &coll2, &mut vals, own_mark);
        }
        // 呼び出し先はこの関数のラッチを知らないので、呼ぶ前にコミット（引数の評価は先に済ませる）
        // the callee cannot see this function's latch: commit before the call (arguments are evaluated first)
        if self.flt_on() && self.at_count > before {
            for (i, a) in refs.iter().enumerate() {
                let t = self.ty(a);
                let v = std::mem::take(&mut vals[i]);
                vals[i] = self.hoist_any(&t, &v);
            }
        }
        let site = self.site(e.span);
        self.commit(&site);
        let call = format!("{}({})", name, vals.join(", "));
        if temps_to_drop.is_empty() && mirrors.is_empty() {
            call
        } else {
            let rt = ret.clone();
            let res = if rt == Ty::Unit || rt == Ty::Never {
                self.emit(format!("{};", call));
                "(void)0".to_string()
            } else {
                self.hoist_force(&rt, &call)
            };
            for (ma, mask, w) in mirrors {
                self.emit_mirror(ma, &w, mask, "&mut @a[id] を渡した呼び出しの直後 → 届く列の写しを作り直す", "right after a call that received &mut @a[id] → rebuild the column mirrors it reaches");
            }
            for (t, tmp) in temps_to_drop {
                let d = self.gc_drop_temp(&t, &tmp);
                self.emit(format!("{} /* {} */", d, self.c("借用した一時値を呼び出し直後に解放", "drop the borrowed temporary right after the call")));
            }
            res
        }
    }

    fn some_value(&mut self, t: &Ty, v: &str) -> String {
        match t {
            Ty::Option(i) if Self::opt_niche(i) => v.to_string(),
            Ty::Option(i) => {
                let c = self.cty(t);
                if let Ty::Array(..) = **i {
                    let n = self.new_tmp("o");
                    self.emit(format!("{} {} = {{ .is_some = true }};", c, n));
                    self.emit(format!("__builtin_memcpy({}.value, {}, sizeof {}.value);", n, v, n));
                    n
                } else {
                    format!("({}){{ .is_some = true, .value = {} }}", c, v)
                }
            }
            _ => v.to_string(),
        }
    }

    fn builtin(&mut self, e: &Expr, b: Builtin, args: &[Expr]) -> String {
        let refs: Vec<&Expr> = args.iter().collect();
        // 外へ影響が出る組み込み（出力・停止・MMIO）の直前はコミット点 / builtins with external effects are commit points
        let before = self.at_count;
        let site = self.site(e.span);
        if let Some(v) = self.universal_builtin(e, b, args, &site) {
            return v;
        }
        match b {
            Builtin::FloatFromBits(ft) => {
                let v = self.val(&args[0]);
                format!("f20_{}_from_bits({})", ft.name(), strip_parens(&v))
            }
            Builtin::PrintInt | Builtin::PutInt | Builtin::PutHex => {
                let at = self.ty(&args[0]);
                let v = self.val(&args[0]);
                let v = self.effect_arg(before, &at, v);
                self.commit(&site);
                let signed = matches!(at, Ty::Int(i) if i.signed());
                match b {
                    Builtin::PrintInt => {
                        if signed {
                            format!("fascon_print_i64((int64_t){})", paren(&v))
                        } else {
                            format!("fascon_print_u64((uint64_t){})", paren(&v))
                        }
                    }
                    Builtin::PutInt => {
                        if signed {
                            format!("fascon_put_i64((int64_t){})", paren(&v))
                        } else {
                            format!("fascon_put_u64((uint64_t){})", paren(&v))
                        }
                    }
                    _ => format!("fascon_put_hex((uint64_t){})", paren(&v)),
                }
            }
            Builtin::Put => {
                let at = self.ty(&args[0]);
                let v = self.val(&args[0]);
                let v = self.effect_arg(before, &at, v);
                self.commit(&site);
                match at {
                    Ty::Int(i) if i.signed() => format!("fascon_put_i64((int64_t){})", paren(&v)),
                    Ty::Int(_) => format!("fascon_put_u64((uint64_t){})", paren(&v)),
                    Ty::Float(ft) => format!("fascon_put_{}({})", ft.name(), strip_parens(&v)),
                    Ty::Str => format!("fascon_put_str({})", v),
                    Ty::Bool => format!("fascon_put_str({} ? FASCON_STR(\"true\") : FASCON_STR(\"false\"))", paren(&v)),
                    _ => format!("fascon_put_i64((int64_t){})", paren(&v)),
                }
            }
            Builtin::PutBytes => {
                let from = self.s.slice_args.get(&args[0].id).copied().unwrap_or(crate::sema::SliceFrom::Slice);
                let v = self.slice_arg(&args[0], from);
                self.commit(&site);
                format!("fascon_put_bytes({})", v)
            }
            Builtin::PrintStr | Builtin::PutStr => {
                let v = self.val(&args[0]);
                let v = self.effect_arg(before, &Ty::Str, v);
                self.commit(&site);
                if b == Builtin::PrintStr {
                    format!("fascon_print_str({})", v)
                } else {
                    format!("fascon_put_str({})", v)
                }
            }
            Builtin::Panic => {
                let msg = match &args[0].kind {
                    ExprKind::Str(s) => c_escape(s),
                    _ => {
                        let v = self.val(&args[0]);
                        let v = self.effect_arg(before, &Ty::Str, v);
                        format!("{}.ptr", v)
                    }
                };
                self.commit(&site);
                format!("fascon_panic(FASCON_PANIC_USER, {}, {})", msg, site)
            }
            Builtin::Drop => {
                let t = self.ty(&args[0]);
                let v = self.val(&args[0]);
                if self.s.t.needs_drop(&t) {
                    let tmp = self.hoist_force(&t, &v);
                    let d = self.gc_drop_temp(&t, &tmp);
                    self.emit(format!("{} /* drop(): {} */", d, self.c("ここで所有リージョンを一括解放", "bulk-free the own region here")));
                }
                "(void)0".to_string()
            }
            Builtin::CpuRelax => "fascon_cpu_relax()".to_string(),
            Builtin::Min | Builtin::Max => {
                let op = if matches!(b, Builtin::Min) { "<" } else { ">" };
                let t = self.ty(e);
                let x = self.val(&args[0]);
                let x = self.hoist_any(&t, &x);
                let y = self.val(&args[1]);
                let y = self.hoist_any(&t, &y);
                format!("(({x}) {op} ({y}) ? ({x}) : ({y}))", x = x, y = y, op = op)
            }
            Builtin::TimerFreq => "fascon_plat_timer_freq()".to_string(),
            Builtin::TimerNow => "fascon_plat_timer_now()".to_string(),
            Builtin::TimerArm => {
                let t = self.ty(&args[0]);
                let v = self.val(&args[0]);
                let v = self.effect_arg(before, &t, v);
                self.commit(&site);
                format!("fascon_plat_timer_arm({})", v)
            }
            Builtin::IrqOpen => {
                self.commit(&site);
                "fascon_plat_irq_open()".to_string()
            }
            Builtin::WaitIrq => {
                self.commit(&site);
                "fascon_plat_wait_irq()".to_string()
            }
            Builtin::Fence => "fascon_hw_fence()".to_string(),
            Builtin::CompilerFence => "fascon_compiler_fence()".to_string(),
            Builtin::CpuId => "fascon_plat_cpu_id()".to_string(),
            Builtin::CpuCount => "fascon_plat_cpu_count()".to_string(),
            Builtin::SpawnOn => {
                let fi = self.s.spawns.iter().find(|x| x.0 == e.id).map(|x| x.1).unwrap_or(0);
                let fname = self.s.t.fns[fi].name.clone();
                let core = self.val(&args[0]);
                let core = self.hoist_any(&Ty::Int(IntTy::U32), &core);
                let arg = match args.get(2) {
                    Some(a) => {
                        let at = self.ty(a);
                        let v = self.val(a);
                        let v = self.hoist_any(&at, &v);
                        if matches!(at, Ty::Own(_) | Ty::DmaOwn(_)) { format!("(uint64_t)(uintptr_t){}", v) } else { format!("(uint64_t){}", v) }
                    }
                    None => "0u".to_string(),
                };
                self.commit(&site);
                format!("fascon_plat_spawn_on({}, fsc_spawn_{}, {})", core, fname, arg)
            }
            Builtin::HeapLiveBlocks => "fascon_heap_live_blocks()".to_string(),
            Builtin::HeapLiveBytes => "fascon_heap_live_bytes()".to_string(),
            Builtin::Halt => {
                self.commit(&site);
                "fascon_halt()".to_string()
            }
            Builtin::Exit => {
                let t = self.ty(&args[0]);
                let v = self.val(&args[0]);
                let v = self.effect_arg(before, &t, v);
                self.commit(&site);
                format!("fascon_exit({})", v)
            }
            // ユニバーサル OS 拡張（universal_builtin が先に処理する） / universal-OS extension (handled first by universal_builtin)
            Builtin::CopyFromUser | Builtin::CopyToUser | Builtin::SubmitDma | Builtin::CtEq | Builtin::Schedule | Builtin::SleepTicks | Builtin::RcuSynchronize | Builtin::ClockMonoNs | Builtin::CpuThis | Builtin::Fault => "0".to_string(),
            Builtin::MmioRead(bits) => {
                let t = self.ty(&args[0]);
                let v = self.val(&args[0]);
                let v = self.effect_arg(before, &t, v);
                self.commit(&site);
                format!("fascon_mmio_read{}((uintptr_t){})", bits, paren(&v))
            }
            Builtin::MmioWrite(bits) => {
                let mut vs = self.operands(&refs);
                for (i, a) in refs.iter().enumerate() {
                    let t = self.ty(a);
                    let v = std::mem::take(&mut vs[i]);
                    vs[i] = self.effect_arg(before, &t, v);
                }
                self.commit(&site);
                format!("fascon_mmio_write{}((uintptr_t){}, {})", bits, paren(&vs[0]), vs[1])
            }
        }
    }

    fn method(&mut self, e: &Expr, recv: &Expr, _name: &str, args: &[Expr]) -> String {
        if let Some(k) = self.s.methods.get(&e.id).copied() {
            if let Some(v) = self.universal_method(e, recv, k, args) {
                return v;
            }
        }
        let k = match self.s.methods.get(&e.id) {
            Some(k) => *k,
            None => return "0".to_string(),
        };
        let t = self.ty(e);
        let rt = self.ty(recv);
        let opt_inner = match rt.strip_ref() {
            Ty::Option(i) => Some((**i).clone()),
            _ => None,
        };
        match k {
            MethodKind::FloatToBits(ft) => format!("f20_{}_to_bits({})", ft.name(), strip_parens(&self.val(recv))),
            MethodKind::FloatIsNan(ft) => format!("f20_{}_is_nan({})", ft.name(), strip_parens(&self.val(recv))),
            MethodKind::FloatIsFinite(ft) => format!("f20_{}_is_finite({})", ft.name(), strip_parens(&self.val(recv))),
            MethodKind::FloatAbs(ft) => format!("f20_{}_abs({})", ft.name(), strip_parens(&self.val(recv))),
            MethodKind::ResIsOk | MethodKind::ResIsErr | MethodKind::ResUnwrap | MethodKind::ResUnwrapOr | MethodKind::FaultCode | MethodKind::UserLen => "0".to_string(),
            MethodKind::ProcOnCpu(_) | MethodKind::ProcOffCpu(_) | MethodKind::ProcCpuMask(_) | MethodKind::ProcMapPage(_) | MethodKind::ProcSharePage(_) | MethodKind::ProcCowCopy(_) | MethodKind::ProcUnmapPage(_) | MethodKind::ProcTlbFlush(_) | MethodKind::ProcPeek(_) | MethodKind::ProcTlbCount(..) => "0".to_string(),
            MethodKind::IsSome | MethodKind::IsNone => {
                let v = self.val(recv);
                let i = opt_inner.unwrap_or(Ty::Err);
                let some = if Self::opt_niche(&i) {
                    match i {
                        Ty::Id(_) | Ty::GpuBuf(_) => format!("({} != 0u)", v),
                        Ty::Str => format!("({}.ptr != NULL)", v),
                        _ => format!("({} != NULL)", v),
                    }
                } else {
                    format!("{}.is_some", paren(&v))
                };
                if k == MethodKind::IsSome {
                    some
                } else {
                    negate(&some)
                }
            }
            MethodKind::Take => {
                let place = self.lv(recv);
                let n = self.new_tmp("tk");
                let ot = rt.strip_ref().clone();
                let d = self.cdecl(&ot, &n);
                let z = self.zero_value(&ot);
                self.emit(format!("{} = {}; {} = {}; /* .take() */", d, place, place, z));
                n
            }
            MethodKind::Unwrap | MethodKind::UnwrapOr => {
                let v = self.val(recv);
                let ot = rt.strip_ref().clone();
                let i = opt_inner.unwrap_or(Ty::Err);
                let tmp = self.hoist_force(&ot, &v);
                let (test, value) = if Self::opt_niche(&i) {
                    (
                        match i {
                            Ty::Id(_) | Ty::GpuBuf(_) => format!("{} == 0u", tmp),
                            Ty::Str => format!("{}.ptr == NULL", tmp),
                            _ => format!("{} == NULL", tmp),
                        },
                        tmp.clone(),
                    )
                } else {
                    (format!("!{}.is_some", tmp), format!("{}.value", tmp))
                };
                if k == MethodKind::Unwrap {
                    let site = self.site(e.span);
                    self.emit(format!("if (FASCON_UNLIKELY({})) fascon_unwrap_panic({});", test, site));
                    value
                } else {
                    let d = self.val(&args[0]);
                    format!("({} ? {} : {})", test, d, value)
                }
            }
            MethodKind::Clone => {
                let base = rt.strip_ref().clone();
                let v = match &rt {
                    Ty::Ref(..) => match &recv.kind {
                        ExprKind::Path(_) => match self.s.res.get(&recv.id) {
                            Some(Res::Local(b)) if !self.s.auto_deref.contains(&recv.id) => {
                                let n = self.names.get(b).cloned().unwrap_or_default();
                                if matches!(base, Ty::Own(_) | Ty::DmaOwn(_)) {
                                    n
                                } else {
                                    format!("(*{})", n)
                                }
                            }
                            _ => self.val(recv),
                        },
                        _ => self.val(recv),
                    },
                    _ => self.lv_or_val(recv),
                };
                let _ = t;
                self.clone_expr(&base, &v)
            }
            MethodKind::StrLen => {
                let v = self.val(recv);
                format!("(uintptr_t){}.len", paren(&v))
            }
            MethodKind::StrByte => {
                let v = self.val(recv);
                let i = self.val(&args[0]);
                format!("fascon_str_byte({}, (uint64_t){}, {})", v, paren(&i), self.site(args[0].span))
            }
            MethodKind::ArrLen => match rt.deref_all() {
                Ty::Array(_, n) => format!("((uintptr_t){}u)", n),
                _ => "0".to_string(),
            },
            MethodKind::IdIndex | MethodKind::IdGeneration => {
                let (n, bump) = match rt.deref_all() {
                    Ty::Id(a) | Ty::Dma(a) | Ty::GpuBuf(a) => (self.s.t.arenas[*a].name.clone(), self.s.t.arenas[*a].bump),
                    _ => (String::new(), false),
                };
                // バンプアリーナの Id は 64 ビット（F20_BID_*） / Ids of bump arenas are 64 bits (F20_BID_*)
                let m = match (k, bump) {
                    (MethodKind::IdIndex, false) => "F20_ID_INDEX",
                    (MethodKind::IdIndex, true) => "F20_BID_INDEX",
                    (_, false) => "F20_ID_GEN",
                    (_, true) => "F20_BID_GEN",
                };
                format!("{}({}, ARENA_{}_K)", m, self.val(recv), n)
            }
            MethodKind::IdRaw => self.val(recv),
            MethodKind::ArenaAlloc(a) => {
                let n = self.s.t.arenas[a].name.clone();
                let et = self.s.t.arenas[a].elem.clone();
                let v = self.val(&args[0]);
                let arg = if (v.starts_with('(') && v.contains("){")) || (is_simple_c(&v) && v.chars().next().map_or(false, |c| c.is_ascii_alphabetic() || c == '_')) {
                    format!("&{}", v)
                } else {
                    let tmp = self.hoist_force(&et, &v);
                    format!("&{}", tmp)
                };
                if self.s.user_starts.contains(&e.id) {
                    // `@t.start_user(値, 入口の番地, スタックの番地)`: 確保 + EL0 の文脈の用意 / alloc + an EL0 context
                    let (t1, t2) = (self.ty(&args[1]), self.ty(&args[2]));
                    let ev = self.val(&args[1]);
                    let ev = self.hoist_any(&t1, &ev);
                    let sv = self.val(&args[2]);
                    let sv = self.hoist_any(&t2, &sv);
                    let site = self.site(e.span);
                    self.commit(&site);
                    return format!("arena_{}_start_user({}, {}, {})", n, arg, ev, sv);
                }
                if self.s.task_starts.contains(&e.id) {
                    // `@t.start(値, 入口, 引数)`: 確保 + 文脈の用意 / alloc + context setup
                    let entry = self.val(&args[1]);
                    let ta = self.ty(&args[2]);
                    let av = self.val(&args[2]);
                    let av = self.hoist_any(&ta, &av);
                    let site = self.site(e.span);
                    self.commit(&site);
                    return format!("arena_{}_start({}, {}, {})", n, arg, entry, av);
                }
                format!("arena_{}_alloc({})", n, arg)
            }
            MethodKind::ArenaFree(a) => {
                let n = self.s.t.arenas[a].name.clone();
                format!("arena_{}_free({})", n, self.val(&args[0]))
            }
            MethodKind::ArenaRemove(a) => {
                let n = self.s.t.arenas[a].name.clone();
                let id = self.val(&args[0]);
                let ot = t.clone();
                let c = self.cty(&ot);
                let tmp = self.new_tmp("rm");
                self.emit(format!("{} {} = {{ .is_some = false }};", c, tmp));
                self.emit(format!("{}.is_some = arena_{}_remove({}, &{}.value);", tmp, n, id, tmp));
                tmp
            }
            MethodKind::ArenaContains(a) => {
                let n = self.s.t.arenas[a].name.clone();
                format!("(arena_{}_get({}) != NULL)", n, self.val(&args[0]))
            }
            MethodKind::DmaRegionPhys(d) => format!("((uint64_t)(uintptr_t)dma_{})", self.s.t.dmas[d].name),
            MethodKind::DmaRegionLen(d) => format!("UINT64_C({})", self.s.t.dmas[d].len),
            MethodKind::DmaLend(a) => {
                let n = self.s.t.arenas[a].name.clone();
                let h = self.val(&args[0]);
                let site = self.site(e.span);
                self.commit(&site);
                format!("arena_{}_lend({})", n, h)
            }
            MethodKind::DmaReclaim(a) => {
                let n = self.s.t.arenas[a].name.clone();
                let raw = self.val(&args[0]);
                format!("arena_{}_reclaim((uint32_t)({}))", n, raw)
            }
            // 装置へ出す前に、それまでの検査の結果（ラッチ）を確かめる / check the latched results before offloading
            MethodKind::GpuDispatch(a) => {
                let n = self.s.t.arenas[a].name.clone();
                let k = self.val(&args[0]);
                let g = self.val(&args[1]);
                let b = self.val(&args[2]);
                let h = self.val(&args[3]);
                let site = self.site(e.span);
                self.commit(&site);
                format!("arena_{}_dispatch((uint32_t)({}), (uint64_t)({}), (uint64_t)({}), {})", n, k, g, b, h)
            }
            MethodKind::GpuReclaim(a) | MethodKind::GpuReclaimWait(a) => {
                let n = self.s.t.arenas[a].name.clone();
                let f = self.val(&args[0]);
                format!("arena_{}_reclaim({}, {})", n, f, if matches!(k, MethodKind::GpuReclaimWait(_)) { 1 } else { 0 })
            }
            MethodKind::DmaPhys(a) => {
                let n = self.s.t.arenas[a].name.clone();
                let h = self.val(recv);
                format!("((uint64_t)(uintptr_t)arena_{}_slot({}))", n, h)
            }
            MethodKind::SliceLen => {
                let s = self.slice_ptr(recv);
                format!("{}->n", s)
            }
            MethodKind::Window(_) | MethodKind::View(..) | MethodKind::UserMemOf(_) => "NULL".to_string(),
            // チャネル: 送信は空きができるまで、recv_wait は届くまで待つ（どちらもロックなし）。recv は無ければ None
            // channels: send waits for room and recv_wait for a value (both lock-free); recv gives None when empty
            MethodKind::ChanSend(c) => {
                let n = self.s.t.chans[c].name.clone();
                let et = self.s.t.chans[c].elem.clone();
                let v = self.val(&args[0]);
                let v = self.hoist_any(&et, &v);
                let site = self.site(e.span);
                self.commit(&site);
                format!("chan_{}_send({})", n, v)
            }
            MethodKind::ChanRecvWait(c) => {
                let site = self.site(e.span);
                self.commit(&site);
                format!("chan_{}_recv_wait()", self.s.t.chans[c].name)
            }
            MethodKind::ChanRecv(c) => {
                let n = self.s.t.chans[c].name.clone();
                let et = self.s.t.chans[c].elem.clone();
                let ct = self.cty(&et);
                let (tmp, ok) = (self.new_tmp("cv"), self.new_tmp("ck"));
                let d = if ct.ends_with('*') { format!("{}{}", ct, tmp) } else { format!("{} {}", ct, tmp) };
                self.emit(format!("{};", d));
                self.emit(format!("__builtin_memset(&{}, 0, sizeof {});", tmp, tmp));
                self.emit(format!("const bool {} = chan_{}_try_recv(&{});", ok, n, tmp));
                if Self::opt_niche(&et) {
                    format!("({} ? {} : ({})0)", ok, tmp, ct)
                } else {
                    let ot = self.cty(&Ty::Option(Box::new(et)));
                    format!("(({}){{ .is_some = {}, .value = {} }})", ot, ok, tmp)
                }
            } // 対象の位置でだけ使う（lower_match） / scrutinee only
            MethodKind::ReadAt(sid) => {
                let (p, len) = self.bytes_of(recv);
                let off = self.val(&args[1]);
                let st_name = self.struct_name(sid);
                let m = self.new_tmp("r");
                self.emit(format!("const {} *{} = (const {} *)f20_view((uint8_t *)({}), {}, {}, sizeof({}));", st_name, m, st_name, p, len, off, st_name));
                let oc = self.cty(&t);
                let o = self.new_tmp("o");
                self.emit(format!("{} {} = {{ .is_some = false }};", oc, o));
                self.emit(format!("if ({} != NULL) {{ {}.is_some = true; {}.value = *{}; }}", m, o, o, m));
                o
            }
            MethodKind::CopyFrom => {
                let (dp, dl) = self.bytes_of(recv);
                let from = self.s.slice_args.get(&args[0].id).copied().unwrap_or(crate::sema::SliceFrom::Slice);
                let s = self.slice_arg(&args[0], from);
                let s = s.trim_start_matches('&').to_string();
                let (sp, sl) = if s == self.slice_ptr(&args[0]) && from == crate::sema::SliceFrom::Slice { (format!("{}->p", s), format!("{}->n", s)) } else { (format!("{}.p", s), format!("{}.n", s)) };
                format!("(void)f20_bytes_copy((uint8_t *)({}), {}, {}, {})", dp, dl, sp, sl)
            }
            MethodKind::IntOp(code) => {
                let it = match rt.deref_all() {
                    Ty::Int(i) => *i,
                    _ => IntTy::U64,
                };
                let ity = Ty::Int(it);
                let a = self.val(recv);
                let a = self.hoist_any(&ity, &a);
                let b = self.val(&args[0]);
                let b = self.hoist_any(&ity, &b);
                let ct = it.c_name();
                let f = ["add", "sub", "mul"][(code % 3) as usize];
                match code {
                    // 結果を捨てずに切り詰める（符号付きでも未定義動作にならない） / truncating, never undefined even when signed
                    0..=2 => {
                        let r = self.new_tmp("w");
                        self.emit(format!("{} {}; (void)__builtin_{}_overflow({}, {}, &{});", ct, r, f, a, b, r));
                        r
                    }
                    3..=5 => {
                        let r = self.new_tmp("c");
                        let ov = self.new_tmp("ov");
                        self.emit(format!("{} {}; const bool {} = __builtin_{}_overflow({}, {}, &{});", ct, r, ov, f, a, b, r));
                        let some = self.some_value(&t, &r);
                        let oc = self.cty(&t);
                        format!("({} ? ({}){{ .is_some = false }} : {})", ov, oc, some)
                    }
                    _ => {
                        let f = if code == 6 { "add" } else { "sub" };
                        let r = self.new_tmp("s");
                        let (lo, hi) = if it.signed() {
                            (format!("(({ct})(UINT64_C(1) << (sizeof({ct}) * 8u - 1u)))", ct = ct), format!("(({ct})((UINT64_C(1) << (sizeof({ct}) * 8u - 1u)) - 1u))", ct = ct))
                        } else {
                            (format!("(({})0)", ct), format!("(({ct})~({ct})0)", ct = ct))
                        };
                        let sat = if code == 6 {
                            if it.signed() { format!("(({}) < 0 ? {} : {})", b, lo, hi) } else { hi.clone() }
                        } else if it.signed() {
                            format!("(({}) < 0 ? {} : {})", b, hi, lo)
                        } else {
                            lo.clone()
                        };
                        self.emit(format!("{} {}; if (__builtin_{}_overflow({}, {}, &{})) {} = {};", ct, r, f, a, b, r, r, sat));
                        r
                    }
                }
            }
            MethodKind::TaskCurrent(a) => format!("(({})fascon_plat_task_current())", self.id_name(a)),
            MethodKind::TaskYield(a) | MethodKind::TaskResume(a) | MethodKind::TaskEnter(a) => {
                let n = self.s.t.arenas[a].name.clone();
                let it = self.ty(&args[0]);
                let id = self.val(&args[0]);
                let id = self.hoist_any(&it, &id);
                let site = self.site(e.span);
                self.commit(&site);
                let f = match k {
                    MethodKind::TaskYield(_) => "fascon_plat_task_yield",
                    MethodKind::TaskResume(_) => "fascon_plat_task_resume",
                    _ => "fascon_plat_task_enter",
                };
                format!("{}(F20_ID_INDEX({}, ARENA_{}_K), {})", f, id, n, id)
            }
            MethodKind::TaskExit(a) => {
                let n = self.s.t.arenas[a].name.clone();
                let site = self.site(e.span);
                self.commit(&site);
                format!("fsc_task_exit_{}()", n)
            }
            MethodKind::ArenaLen(a) if self.s.t.arenas[a].bump => format!("((uintptr_t)arena_{}.st.top)", self.s.t.arenas[a].name),
            MethodKind::ArenaLen(a) => format!("((uintptr_t)arena_{}.st.live)", self.s.t.arenas[a].name),
            MethodKind::ArenaCapacity(a) => format!("((uintptr_t){}u)", self.s.t.arenas[a].cap),
            MethodKind::ArenaValidate(a) => {
                let n = self.s.t.arenas[a].name.clone();
                let v = self.val(&args[0]);
                if self.s.t.arenas[a].bump {
                    format!("f20_bump_validate(arena_{n}.gen, arena_{n}.st.top, ARENA_{n}_K, {v})", n = n, v = v)
                } else {
                    format!("f20_arena_validate(arena_{n}.gen, arena_{n}.link, ARENA_{n}_CAP, ARENA_{n}_K, {v})", n = n, v = v)
                }
            }
            MethodKind::ArenaGet(a) | MethodKind::ArenaGetMut(a) => {
                let n = self.s.t.arenas[a].name.clone();
                format!("arena_{}_get({})", n, self.val(&args[0]))
            }
            MethodKind::MmioLen(m) => format!("((uintptr_t)MMIO_{}_LEN)", self.s.t.mmios[m].name.to_uppercase()),
            MethodKind::MmioWidth(m) => format!("((uintptr_t)MMIO_{}_W)", self.s.t.mmios[m].name.to_uppercase()),
            MethodKind::MmioHeight(m) => format!("((uintptr_t)MMIO_{}_H)", self.s.t.mmios[m].name.to_uppercase()),
            MethodKind::MmioFill(m) => {
                let info = self.s.t.mmios[m].clone();
                let n = info.name.clone();
                let v = self.val(&args[0]);
                let v = self.hoist(&Ty::Int(info.elem), &v);
                if info.framebuffer {
                    self.emit(format!("for (uintptr_t _i = 0; _i < MMIO_{N}_LEN; _i++) (({et} *)MMIO_{N}_BASE)[_i] = {v}; /* {n}.fill */", N = n.to_uppercase(), et = info.elem.c_name(), v = v, n = n));
                } else {
                    let site = self.site(e.span);
                    self.commit(&site);
                    self.emit(format!("for (uintptr_t _i = 0; _i < MMIO_{N}_LEN; _i++) fascon_mmio_write{b}(MMIO_{N}_BASE + _i * {eb}u, {v}); /* {n}.fill */", N = n.to_uppercase(), b = info.elem.bits(64), eb = info.elem_bytes(), v = v, n = n));
                }
                "(void)0".to_string()
            }
            MethodKind::MmioPut(m) => {
                let info = self.s.t.mmios[m].clone();
                let n = info.name.clone();
                let refs: Vec<&Expr> = args.iter().collect();
                let vs = self.operands(&refs);
                let xt = self.ty(&args[0]);
                let yt = self.ty(&args[1]);
                let x = self.hoist(&xt, &vs[0]);
                let y = self.hoist(&yt, &vs[1]);
                let r = self.new_tmp("ok");
                #[allow(non_snake_case)]
                let (N, w, h) = (n.to_uppercase(), info.dims[1], info.dims[0]);
                let xs = matches!(xt, Ty::Int(i) if i.signed());
                let ys = matches!(yt, Ty::Int(i) if i.signed());
                let xc = if xs { format!("{x} >= 0 && (uint64_t){x} < {w}u", x = x, w = w) } else { format!("(uint64_t){x} < {w}u", x = x, w = w) };
                let yc = if ys { format!("{y} >= 0 && (uint64_t){y} < {h}u", y = y, h = h) } else { format!("(uint64_t){y} < {h}u", y = y, h = h) };
                self.emit(format!("bool {r} = {xc} && {yc}; /* {n}.put: {c} */", r = r, xc = xc, yc = yc, n = n, c = self.c("範囲外は書かずに false（クリップ）", "out of range: no write, returns false (clipped)")));
                if info.framebuffer {
                    self.emit(format!("if ({r}) mmio_{n}_ptr((uintptr_t){y}, (uintptr_t){x})[0] = {v};", r = r, n = n, x = x, y = y, v = vs[2]));
                } else {
                    let vt = self.ty(&args[2]);
                    let v = if self.flt_on() { self.hoist_any(&vt, &vs[2]) } else { vs[2].clone() };
                    let site = self.site(e.span);
                    self.commit(&site);
                    self.emit(format!("if ({r}) fascon_mmio_write{b}(mmio_{n}_addr((uintptr_t){y}, (uintptr_t){x}), {v});", r = r, b = info.elem.bits(64), n = n, x = x, y = y, v = v));
                }
                let _ = N;
                r
            }
            MethodKind::MmioFillRect(m) => {
                let info = self.s.t.mmios[m].clone();
                let n = info.name.clone();
                let refs: Vec<&Expr> = args.iter().collect();
                let vs = self.operands(&refs);
                if info.framebuffer && info.elem.bits(64) == 32 {
                    format!(
                        "fascon_fill_rect32((uint32_t *)MMIO_{N}_BASE, (int32_t)MMIO_{N}_W, (int32_t)MMIO_{N}_H, {}, {}, {}, {}, {})",
                        vs[0],
                        vs[1],
                        vs[2],
                        vs[3],
                        vs[4],
                        N = n.to_uppercase()
                    )
                } else {
                    // 汎用（クリップ付き二重ループ） / generic (clipped double loop)
                    let (w, h) = (info.dims[1], info.dims[0]);
                    let tmp: Vec<String> = vs.iter().take(4).map(|v| self.hoist(&Ty::Int(IntTy::I32), v)).collect();
                    let c = self.hoist(&Ty::Int(info.elem), &vs[4]);
                    if !info.framebuffer {
                        let site = self.site(e.span);
                        self.commit(&site);
                    }
                    self.emit(format!(
                        "for (int64_t _y = ({y} < 0 ? 0 : {y}); _y < (int64_t){y} + {hh} && _y < {h}; _y++) for (int64_t _x = ({x} < 0 ? 0 : {x}); _x < (int64_t){x} + {ww} && _x < {w}; _x++) {wr};",
                        x = tmp[0],
                        y = tmp[1],
                        ww = tmp[2],
                        hh = tmp[3],
                        w = w,
                        h = h,
                        wr = if info.framebuffer {
                            format!("mmio_{}_ptr((uintptr_t)_y, (uintptr_t)_x)[0] = {}", n, c)
                        } else {
                            format!("fascon_mmio_write{}(mmio_{}_addr((uintptr_t)_y, (uintptr_t)_x), {})", info.elem.bits(64), n, c)
                        }
                    ));
                    "(void)0".to_string()
                }
            }
        }
    }

    fn lv_or_val(&mut self, e: &Expr) -> String {
        if crate::sema::is_place_expr(e) {
            self.lv(e)
        } else {
            let t = self.ty(e);
            let v = self.val(e);
            self.hoist_force(&t, &v)
        }
    }

    // ───────────── 構造体リテラルと own / struct literals and own ─────────────

    fn struct_lit(&mut self, e: &Expr, packed_children: Option<&HashMap<ExprId, String>>) -> String {
        let (sid, fields) = match (&e.kind, self.ty(e)) {
            (ExprKind::StructLit { fields, .. }, Ty::Struct(s)) => (s, fields),
            _ => return "0".to_string(),
        };
        let info = self.s.t.structs[sid].clone();
        // ソース順に値を評価 / evaluate values in source order
        let imp: Vec<bool> = fields.iter().map(|(_, _, v)| !self.pure(v) && !packed_children.map_or(false, |m| m.contains_key(&v.id))).collect();
        let fexprs: Vec<&Expr> = fields.iter().map(|(_, _, v)| v).collect();
        let coll = self.gc_collect_flags(&fexprs);
        let own_mark = self.gc_own_temps.len();
        let mut vals: Vec<(String, String)> = Vec::new();
        let mut array_moves: Vec<(String, String)> = Vec::new();
        for (k, (fname, _, v)) in fields.iter().enumerate() {
            let fty = info.field(fname).map(|x| x.1.ty.clone()).unwrap_or(Ty::Err);
            if let Some(p) = packed_children.and_then(|m| m.get(&v.id)) {
                vals.push((fname.clone(), p.clone()));
                continue;
            }
            let val = match (&v.kind, &fty) {
                (ExprKind::ArrayRepeat(x, _), Ty::Array(..)) if self.is_zero_like(x) => "{ 0 }".to_string(),
                (ExprKind::ArrayRepeat(x, _), Ty::Array(et, n)) if *n <= 32 => {
                    let one = self.val(x);
                    let one = self.hoist(&(**et).clone(), &one);
                    format!("{{ {} }}", vec![one; *n as usize].join(", "))
                }
                (ExprKind::ArrayLit(xs), Ty::Array(..)) => {
                    let refs: Vec<&Expr> = xs.iter().collect();
                    let vs = self.operands(&refs);
                    format!("{{ {} }}", vs.join(", "))
                }
                (_, Ty::Array(..)) => {
                    let src = self.val(v);
                    array_moves.push((fname.clone(), src));
                    "{ 0 }".to_string()
                }
                _ => {
                    let x = self.val(v);
                    let later = imp[k + 1..].iter().any(|b| *b);
                    let gc_later = coll.get(k + 1..).map_or(false, |c| c.iter().any(|b| *b));
                    let fresh = gc_later && is_simple_c(&x) && self.gc_root_ty(&fty) && self.gc_fresh_value(v);
                    if (later && !is_simple_c(&x)) || fresh {
                        self.hoist_operand(&fty, &x, gc_later)
                    } else {
                        x
                    }
                }
            };
            vals.push((fname.clone(), val));
        }
        let c = self.struct_name(sid);
        // 構造体の宣言順に並べ替え（読みやすさ） / reorder into declaration order (readability)
        let mut ordered = Vec::new();
        for f in &info.fields {
            if let Some((_, v)) = vals.iter().find(|(n, _)| *n == f.name) {
                ordered.push(format!(".{} = {}", sanitize_field(&f.name), v));
            }
        }
        let lit = format!("({}){{ {} }}", c, ordered.join(", "));
        // fascon-gc: フィールドへ入る own の一時値を根にしたなら、リテラルを一時変数に作り終えてから（後ろのフィールドの回収は
        // 根のまま越える）受け手へ渡す / fascon-gc: when own temporaries going into fields were rooted, the literal is built
        // into a temporary first (collections in later fields happen while they are roots), then they are handed over
        let hand_over = self.gc_own_temps.len() > own_mark;
        if array_moves.is_empty() && !hand_over {
            return lit;
        }
        let tmp = self.new_tmp("s");
        self.emit(format!("{} {} = {};", c, tmp, lit));
        for (f, src) in array_moves {
            self.emit(format!("__builtin_memcpy({t}.{f}, {s}, sizeof {t}.{f});", t = tmp, f = f, s = src));
        }
        if hand_over {
            self.gc_hand_over(own_mark);
        }
        tmp
    }

    /// own 式。構築式の中の `own` の子（Option でないフィールド）は同じブロックへ詰める
    /// an own expression; `own` children inside the construction (non-Option fields) are packed into the same block
    fn own_expr(&mut self, e: &Expr, inner: &Expr) -> String {
        // フレームに置くのはこの式の根だけ（中の own 式はふつうに確保する） / only this expression's root goes to the frame
        let frame = std::mem::replace(&mut self.own_frame, false);
        let alloc_fn = if frame { "fascon_frame_alloc" } else { "fascon_own_alloc" };
        let t = self.ty(e);
        let sid = match &t {
            Ty::Own(i) | Ty::DmaOwn(i) => match &**i {
                Ty::Struct(s) => *s,
                other => {
                    let other = other.clone();
                    return self.own_value_expr(e, inner, &other, alloc_fn);
                }
            },
            _ => return "NULL".to_string(),
        };
        let sn = self.struct_name(sid);
        let line = self.line_of(e.span);
        let site = self.site(e.span);
        if let ExprKind::StructLit { .. } = inner.kind {
            // 詰める子を集める（前順） / collect the packed children (pre-order)
            let mut plan: Vec<(ExprId, ExprId, usize)> = Vec::new(); // (own expr id, struct lit id, struct id)
            self.collect_packed(inner, &mut plan);
            if plan.is_empty() {
                let v = self.struct_lit(inner, None);
                let n = self.new_tmp("o");
                self.emit(format!("{sn} *{n} = ({sn} *){af}(FASCON_OWN_SPAN({sn}), {site}); /* {l} {line}: own {s} {{ .. }} — {c} */", sn = sn, n = n, af = alloc_fn, site = site, l = self.c("line", "line"), line = line, s = self.s.t.structs[sid].name, c = self.c("新しい所有リージョン", "a new own region")));
                self.emit(format!("*{} = {};", n, v));
                self.report.own_sites.push((line, self.s.t.structs[sid].name.clone(), 0));
                return n;
            }
            // 1 回の割り当て: 根 + 子の見出し・ペイロード / one allocation: root + children headers and payloads
            let mut span_terms = vec![format!("FASCON_OWN_SPAN({})", sn)];
            for (_, _, cs) in &plan {
                span_terms.push(format!("FASCON_OWN_CHILD_SPAN({})", self.struct_name(*cs)));
            }
            let blk = self.new_tmp("blk");
            let child_names: Vec<String> = plan.iter().map(|(_, _, cs)| self.s.t.structs[*cs].name.clone()).collect();
            let summary = summarize_names(&child_names);
            self.emit(format!(
                "char *{blk} = (char *){af}({total}, {site}); /* {l} {line}: own {s} {{ .. }} — {c} */",
                af = alloc_fn,
                blk = blk,
                total = span_terms.join(" + "),
                site = site,
                l = self.c("line", "line"),
                line = line,
                s = self.s.t.structs[sid].name,
                c = self.c(&format!("{} + {} を 1 つの所有リージョンに詰める（1 回の確保・1 回の解放）", self.s.t.structs[sid].name, summary), &format!("pack {} + {} into one own region (one allocation, one free)", self.s.t.structs[sid].name, summary))
            ));
            self.report.own_sites.push((line, self.s.t.structs[sid].name.clone(), plan.len()));
            // 子のポインタ（オフセット順） / child pointers (in offset order)
            let mut ptrs: HashMap<ExprId, String> = HashMap::new();
            let mut offset_terms = vec![format!("FASCON_OWN_SPAN({})", sn)];
            for (own_id, _, cs) in &plan {
                let csn = self.struct_name(*cs);
                let pn = self.new_tmp("c");
                self.emit(format!("{csn} *{pn} = ({csn} *)fascon_own_embed({blk}, {off});", csn = csn, pn = pn, blk = blk, off = offset_terms.join(" + ")));
                offset_terms.push(format!("FASCON_OWN_CHILD_SPAN({})", csn));
                ptrs.insert(*own_id, pn);
            }
            // 子の中身（ソース順の評価を保つため、根から順に構築） / child contents, root first to keep source order
            let root_val = self.packed_lit(inner, &plan, &ptrs);
            let root = format!("(({} *){})", sn, blk);
            self.emit(format!("*{} = {};", root, root_val));
            return root;
        }
        // own <値>: 値を新しい所有リージョンへ移す / own <value>: move a value into a new own region
        let v = self.val(inner);
        let n = self.new_tmp("o");
        self.emit(format!("{sn} *{n} = ({sn} *){af}(FASCON_OWN_SPAN({sn}), {site}); /* {l} {line}: own */", sn = sn, n = n, af = alloc_fn, site = site, l = self.c("line", "line"), line = line));
        self.emit(format!("*{} = {};", n, v));
        self.report.own_sites.push((line, self.s.t.structs[sid].name.clone(), 0));
        n
    }

    /// 構造体でない値の own（`own 5` / `own [0u8; 4096]` / `own Some(own Node { .. })`）: 新しい所有リージョンへ値を移す
    /// an own of a non-struct value (`own 5` / `own [0u8; 4096]` / `own Some(own Node { .. })`): move the value into a new own
    /// region
    fn own_value_expr(&mut self, e: &Expr, inner: &Expr, payload: &Ty, alloc_fn: &str) -> String {
        let pc = self.own_cty(payload);
        let pay = self.payload_cty(payload);
        let line = self.line_of(e.span);
        let site = self.site(e.span);
        let disp = self.s.t.display(payload);
        // 中身の値を先に評価する（評価の順序はソースのとおり） / evaluate the contents first (source order)
        let arr = match (&inner.kind, payload) {
            (ExprKind::ArrayRepeat(v, _), Ty::Array(..)) if self.is_zero_like(v) => Some(None),
            (ExprKind::ArrayRepeat(v, _), Ty::Array(et, _)) => {
                let one = self.val(v);
                let one = self.hoist(&(**et).clone(), &one);
                Some(Some(("repeat", one)))
            }
            (_, Ty::Array(..)) => {
                let src = match &inner.kind {
                    ExprKind::ArrayLit(_) => self.val(inner),
                    _ => self.lv(inner),
                };
                Some(Some(("copy", src)))
            }
            _ => None,
        };
        let v = if arr.is_none() { Some(self.val(inner)) } else { None };
        let o = self.new_tmp("o");
        self.emit(format!(
            "{pc}{o} = ({pc}){af}(FASCON_OWN_SPAN({pay}), {site}); /* {l} {line}: own {d} — {c} */",
            pc = pc,
            o = o,
            af = alloc_fn,
            pay = pay,
            site = site,
            l = self.c("line", "line"),
            line = line,
            d = disp,
            c = self.c("新しい所有リージョン（スコープの終わり・差し替え・drop で解放）", "a new own region (freed at scope end, on replacement or by drop)")
        ));
        match (arr, v) {
            (Some(None), _) => self.emit(format!("__builtin_memset({}, 0, sizeof({}));", o, pay)),
            (Some(Some(("repeat", one))), _) => {
                let len = if let Ty::Array(_, n) = payload { *n } else { 0 };
                self.emit(format!("for (size_t _i = 0; _i < {}u; _i++) {}[_i] = {};", len, o, one));
            }
            (Some(Some((_, src))), _) => self.emit(format!("__builtin_memcpy({}, {}, sizeof({}));", o, src, pay)),
            (None, Some(v)) => self.emit(format!("*{} = {};", o, v)),
            (None, None) => {}
        }
        self.report.own_sites.push((line, disp, 0));
        o
    }

    fn collect_packed(&self, lit: &Expr, plan: &mut Vec<(ExprId, ExprId, usize)>) {
        if let (ExprKind::StructLit { fields, .. }, Ty::Struct(sid)) = (&lit.kind, self.ty(lit)) {
            let info = &self.s.t.structs[sid];
            for (fname, _, v) in fields {
                let fty = info.field(fname).map(|x| x.1.ty.clone());
                if let (Some(Ty::Own(_)), ExprKind::Own(inner)) = (fty, &v.kind) {
                    if let (ExprKind::StructLit { .. }, Ty::Struct(cs)) = (&inner.kind, self.ty(inner)) {
                        plan.push((v.id, inner.id, cs));
                        self.collect_packed(inner, plan);
                    }
                }
            }
        }
    }

    /// 詰めた子を含む構造体リテラルを構築する（子は埋め込み先へ直接書く）
    /// build a struct literal containing packed children (children are written straight into their slots)
    fn packed_lit(&mut self, lit: &Expr, plan: &[(ExprId, ExprId, usize)], ptrs: &HashMap<ExprId, String>) -> String {
        // まず子孫を（ソース順に）構築 / first build the descendants (in source order)
        if let ExprKind::StructLit { fields, .. } = &lit.kind {
            for (_, _, v) in fields {
                if let Some(p) = ptrs.get(&v.id) {
                    if let ExprKind::Own(inner) = &v.kind {
                        let cv = self.packed_lit(inner, plan, ptrs);
                        let p = p.clone();
                        self.emit(format!("*{} = {};", p, cv));
                    }
                }
            }
        }
        self.struct_lit(lit, Some(ptrs))
    }
}

enum ArmBody<'a> {
    B(&'a Block),
    E(&'a Expr),
    Empty,
}

fn summarize_names(v: &[String]) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for n in v {
        if let Some(x) = counts.iter_mut().find(|(m, _)| m == n) {
            x.1 += 1;
        } else {
            counts.push((n.clone(), 1));
        }
    }
    counts.iter().map(|(n, c)| if *c > 1 { format!("{}×{}", n, c) } else { n.clone() }).collect::<Vec<_>>().join(" + ")
}

fn sanitize(n: &str) -> String {
    let lower = n.to_string();
    if C_RESERVED.contains(&n) || n.starts_with("fsc_") || n.starts_with("fascon") || n.starts_with("arena_") || n.starts_with("mmio_") || n.starts_with("FASCON") || n.starts_with("MMIO_") {
        format!("{}_", lower)
    } else {
        lower
    }
}

fn sanitize_field(n: &str) -> String {
    if C_RESERVED.contains(&n) {
        format!("{}_", n)
    } else {
        n.to_string()
    }
}

/// C の左辺値か（`&v` を取れるか）: 名前・メンバ・添字・`(*p)`。定数・呼び出し・キャストは違う
/// whether a C expression is an lvalue (`&v` can be taken): names, members, subscripts, `(*p)`; not constants, calls or
/// casts
pub(crate) fn is_c_lvalue(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() || v == "NULL" || v.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '{') {
        return false;
    }
    if v.starts_with("(*") && v.ends_with(')') {
        return true;
    }
    if v.starts_with('(') || v.ends_with(')') {
        return false;
    }
    v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '>' | '[' | ']' | '*' | '(' | ')' | ' '))
}

fn is_simple_c(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() {
        return true;
    }
    // 整数定数のマクロ・キャスト（`INT64_C(5)`、`((uint8_t)5u)`） / integer-constant macros and casts
    for m in ["INT64_C(", "UINT64_C("] {
        if let Some(rest) = v.strip_prefix(m) {
            if let Some(num) = rest.strip_suffix(')') {
                if num.chars().all(|c| c.is_ascii_hexdigit() || c == 'x' || c == '-') {
                    return true;
                }
            }
        }
    }
    if v.starts_with("((") && v.ends_with("u)") || v.starts_with("((") && v.ends_with(')') && v.matches('(').count() == 2 {
        if let Some(i) = v.find(')') {
            let num = &v[i + 1..v.len() - 1];
            if !num.is_empty() && num.trim_end_matches('u').chars().all(|c| c.is_ascii_hexdigit() || c == 'x' || c == '-') {
                return true;
            }
        }
    }
    v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || v.parse::<i64>().is_ok() || (v.ends_with('u') && v[..v.len() - 1].chars().all(|c| c.is_ascii_hexdigit() || c == 'x'))
}

fn place_is_stable(p: &str) -> bool {
    // 世代検査なしのスロット参照（`arena_x_slot(id)->f`）は 2 回書いても安い / an unchecked slot reference is cheap to repeat
    let q = p.replace("_slot(", "");
    let calls = q.matches('(').count();
    let paren_only = q.matches("(*").count();
    (!p.contains("_at(") && !p.contains("fascon_idx") && calls == paren_only) || !p.contains('(')
}

fn paren(v: &str) -> String {
    if is_simple_c(v) || (v.starts_with('(') && v.ends_with(')') && balanced_outer(v)) {
        v.to_string()
    } else {
        format!("({})", v)
    }
}

fn strip_parens(v: &str) -> &str {
    if v.starts_with('(') && v.ends_with(')') && balanced_outer(v) && !v.starts_with("((") {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

fn strip_all_parens(v: &str) -> &str {
    let mut cur = v;
    while cur.starts_with('(') && cur.ends_with(')') && balanced_outer(cur) {
        let inner = &cur[1..cur.len() - 1];
        // キャスト `(T)x` の括弧は外さない / never strip the parentheses of a cast
        if inner.ends_with(" *") || inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            break;
        }
        cur = inner;
    }
    cur
}

/// 最初の `(` が最後の `)` と対応しているか / does the first `(` match the last `)`?
fn balanced_outer(v: &str) -> bool {
    let mut depth = 0i32;
    for (i, c) in v.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && i != v.len() - 1 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn negate(c: &str) -> String {
    if let Some(x) = c.strip_suffix(" != NULL") {
        return format!("{} == NULL", x);
    }
    if let Some(x) = c.strip_suffix(" != 0u") {
        return format!("{} == 0u", x);
    }
    if c.starts_with('(') && c.ends_with(')') && balanced_outer(c) {
        let inner = &c[1..c.len() - 1];
        if let Some(x) = inner.strip_suffix(" != NULL") {
            return format!("({} == NULL)", x);
        }
        if let Some(x) = inner.strip_suffix(" != 0u") {
            return format!("({} == 0u)", x);
        }
    }
    format!("!{}", paren(c))
}

fn const_eval_simple(s: &Sema, e: &Expr) -> Option<i128> {
    match &e.kind {
        ExprKind::Int(v, _) => Some(if s.neg_lits.contains(&e.id) { -(*v as i128) } else { *v as i128 }),
        ExprKind::Path(n) => s.t.static_by_name.get(n).and_then(|&i| s.t.statics[i].int_value),
        ExprKind::Unary(UnOp::Neg, x) => {
            if let ExprKind::Int(..) = x.kind {
                const_eval_simple(s, x)
            } else {
                const_eval_simple(s, x).map(|v| -v)
            }
        }
        ExprKind::Unary(UnOp::Not, x) => const_eval_simple(s, x).map(|v| !v),
        ExprKind::Cast(x, _) => const_eval_simple(s, x),
        ExprKind::Binary(op, a, b) => {
            let x = const_eval_simple(s, a)?;
            let y = const_eval_simple(s, b)?;
            match op {
                BinOp::Add => x.checked_add(y),
                BinOp::Sub => x.checked_sub(y),
                BinOp::Mul => x.checked_mul(y),
                BinOp::Div if y != 0 => Some(x / y),
                BinOp::Rem if y != 0 => Some(x % y),
                BinOp::Shl if (0..64).contains(&y) => x.checked_shl(y as u32),
                BinOp::Shr if (0..128).contains(&y) => Some(x >> y),
                BinOp::BitAnd => Some(x & y),
                BinOp::BitOr => Some(x | y),
                BinOp::BitXor => Some(x ^ y),
                _ => None,
            }
        }
        _ => None,
    }
}

/// アリーナの添字のビット数 K = ceil(log2(容量))（1〜24） / index bits of an arena: K = ceil(log2(capacity)) (1..24)
pub fn arena_k(cap: u32) -> u32 {
    let mut k = 1u32;
    while (1u64 << k) < cap as u64 {
        k += 1;
    }
    k
}

/// 3 桁ごとのカンマ区切り / thousands separators
fn group(v: u64) -> String {
    let s = v.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// ブロックがただ 1 個の式（文または末尾）だけなら、その式 / the single expression of a block (a statement or the tail)
fn single_expr(b: &Block) -> Option<&Expr> {
    match (b.stmts.as_slice(), &b.tail) {
        ([Stmt::Expr(e)], None) => Some(e),
        ([], Some(t)) => Some(t),
        _ => None,
    }
}

/// `a && b && c` を項に分ける / split `a && b && c` into its conjuncts
fn flatten_and<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match &e.kind {
        ExprKind::Binary(BinOp::And, l, r) => {
            flatten_and(l, out);
            flatten_and(r, out);
        }
        _ => out.push(e),
    }
}

/// ブロックの文がすべて式文（let なし）なら、その式の列 / the expressions of a block made only of expression statements
fn block_exprs(b: &Block) -> Option<Vec<&Expr>> {
    let mut v = Vec::new();
    for st in &b.stmts {
        match st {
            Stmt::Expr(e) => v.push(e),
            Stmt::Let(_) => return None,
        }
    }
    if let Some(t) = &b.tail {
        v.push(t);
    }
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// 比較の左右を入れ替えた演算子 / the comparison with its operands swapped
fn mirror_cmp(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Gt => BinOp::Lt,
        BinOp::Le => BinOp::Ge,
        BinOp::Ge => BinOp::Le,
        o => o,
    }
}

/// `a + b + c` を葉に分ける / split `a + b + c` into its leaves
fn flatten_add<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match &e.kind {
        ExprKind::Binary(BinOp::Add, l, r) => {
            flatten_add(l, out);
            flatten_add(r, out);
        }
        _ => out.push(e),
    }
}

/// 世代を収める最小の型（世代は 32-K ビット） / the narrowest type that holds a generation (32-K bits)
pub fn gen_type(k: u32) -> &'static str {
    match 32 - k {
        0..=8 => "f20_gen8_t",
        9..=16 => "f20_gen16_t",
        _ => "f20_gen32_t",
    }
}

/// その型の大きさ（バイト） / its size in bytes
pub fn gen_bytes(k: u32) -> u64 {
    match 32 - k {
        0..=8 => 1,
        9..=16 => 2,
        _ => 4,
    }
}

// ───────────── ユニバーサル OS 拡張の C 生成 / C generation for the universal-OS extension ─────────────

impl<'a, 'p> Cg<'a, 'p> {
    /// この関数の本体に catch_fault があるか / whether this function's body contains a catch_fault
    fn fn_has_catch(&self, fi: usize) -> bool {
        let item = self.s.t.fns[fi].item;
        let Item::Fn(fd) = &self.s.prog.items[item] else { return false };
        let mut found = false;
        crate::sema::walk_block(&fd.body, &mut |x: &Expr| {
            if matches!(x.kind, ExprKind::CatchFault(_)) {
                found = true;
            }
        });
        found
    }

    /// 値か参照の引数から、その中身を指す C のポインタ / a C pointer to the contents of a value or reference argument
    fn ptr_of(&mut self, a: &Expr) -> String {
        match &a.kind {
            // 配列は先頭要素へのポインタに崩れる（リテラルでも場所でも） / an array decays to a pointer to its first element
            ExprKind::Ref { inner, .. } if matches!(self.ty(inner).strip_ref(), Ty::Array(..)) => self.val(inner),
            ExprKind::Ref { inner, .. } => self.addr_place(inner),
            _ => {
                let t = self.ty(a);
                match t.strip_ref() {
                    Ty::Array(..) => self.val(a),
                    _ if matches!(t, Ty::Ref(..)) => self.val(a),
                    _ => {
                        let v = self.val(a);
                        let h = self.hoist_force(&t, &v);
                        format!("&{}", h)
                    }
                }
            }
        }
    }

    fn universal_builtin(&mut self, e: &Expr, b: Builtin, args: &[Expr], site: &str) -> Option<String> {
        Some(match b {
            Builtin::CopyFromUser => self.lower_copy_from_user(e, args),
            Builtin::CopyToUser => self.lower_copy_to_user(e, args),
            Builtin::SubmitDma => {
                let t = self.ty(&args[0]);
                let inner = match &t {
                    Ty::DmaOwn(i) => (**i).clone(),
                    _ => Ty::Int(IntTy::U8),
                };
                let pay = self.payload_cty(&inner);
                let v = self.val(&args[0]);
                self.commit(site);
                let c = self.c(
                    "デバイスへ貸し出す: キャッシュを掃き出し（DC CVAC + DSB SY）、generic-os では IOMMU に IOVA を一時的に写す。値は消える（以後の使用は F3401）",
                    "lend to the device: clean the cache (DC CVAC + DSB SY) and, on generic-os, map an IOVA in the IOMMU; the value is gone (later use is F3401)",
                );
                format!("fascon_dma_submit({}, sizeof({})) /* {} */", v, pay, c)
            }
            Builtin::CtEq => {
                let t = self.ty(&args[0]).strip_ref().clone();
                match t {
                    Ty::Int(_) => {
                        let (x, y) = (self.val(&args[0]), self.val(&args[1]));
                        format!("fascon_ct_eq_u64((uint64_t){}, (uint64_t){})", paren(&x), paren(&y))
                    }
                    other => {
                        let pay = self.payload_cty(&other);
                        let (x, y) = (self.ptr_of(&args[0]), self.ptr_of(&args[1]));
                        format!("fascon_ct_memeq({}, {}, sizeof({}))", x, y, pay)
                    }
                }
            }
            Builtin::Schedule => {
                self.commit(site);
                "fascon_sched_yield()".to_string()
            }
            Builtin::SleepTicks => {
                let v = self.val(&args[0]);
                self.commit(site);
                format!("fascon_sleep_ticks((uint64_t){})", paren(&v))
            }
            Builtin::RcuSynchronize => {
                self.commit(site);
                "fascon_rcu_synchronize()".to_string()
            }
            Builtin::ClockMonoNs => "fascon_clock_mono_ns()".to_string(),
            Builtin::CpuThis => "fascon_cpu_this()".to_string(),
            Builtin::Fault => {
                let v = self.val(&args[0]);
                format!("((int64_t){})", paren(&v))
            }
            _ => return None,
        })
    }

    /// `copy_from_user(ubuf)`: ユーザー空間から 1 回だけ写す（AArch64 の generic-os は PAN を立てたまま LDTR と例外表で写す）
    /// `copy_from_user(ubuf)`: one copy from user space (on generic-os AArch64: LDTR with PAN kept set, plus an exception table)
    fn lower_copy_from_user(&mut self, e: &Expr, args: &[Expr]) -> String {
        let rt = self.ty(e);
        let rc = self.cty(&rt);
        let ut = self.ty(&args[0]);
        let u = self.val(&args[0]);
        let r = self.new_tmp("uc");
        let line = self.line_of(e.span);
        self.emit(format!("{} {}; __builtin_memset(&{}, 0, sizeof {}); /* {} {}: copy_from_user */", rc, r, r, r, self.c("line", "line"), line));
        match ut {
            Ty::User(i) => match *i {
                Ty::Array(_, n) => {
                    let c = self.c("ユーザーメモリを 1 回だけカーネルのスタックへ写す（途中で失敗すれば EFAULT）", "copy user memory once onto the kernel stack (EFAULT if it faults halfway)");
                    self.emit(format!("if (fascon_copy_from_user({r}.value, {u}, {n}u) == 0u) {{ {r}.is_some = true; }} else {{ {r}.err = INT64_C(14); }} /* {c} */", r = r, u = u, n = n, c = c));
                }
                _ => {
                    let max = self.s.user_str_max;
                    let site = self.site(e.span);
                    let c1 = self.c("長さが上限を超える: 写さずに EFAULT とラッチ F20_FAULT_USER", "the length exceeds the limit: no copy, EFAULT and the latch F20_FAULT_USER");
                    let c2 = self.c("1 回だけの写し（own [u8; 上限]）", "the one-time copy (own [u8; limit])");
                    self.flt_used = true;
                    self.emit(format!("if ({u}.len > {max}u) {{ {r}.err = INT64_C(14); _flt |= F20_FAULT_USER; /* {c1} */ }}", u = u, max = max, r = r, c1 = c1));
                    self.emit(format!(
                        "else {{ uint8_t *_k = (uint8_t *)fascon_own_alloc({max}u, {site}); if (fascon_copy_from_user(_k, {u}.addr, {u}.len) == 0u) {{ {r}.is_some = true; {r}.value = _k; }} else {{ fascon_own_free(_k); {r}.err = INT64_C(14); }} }} /* {c2} */",
                        max = max,
                        site = site,
                        u = u,
                        r = r,
                        c2 = c2
                    ));
                }
            },
            _ => {}
        }
        r
    }

    /// `copy_to_user(ubuf, &kbuf)`: カーネルの値をユーザー空間へ写す（STTR と例外表） / copy a kernel value to user space
    fn lower_copy_to_user(&mut self, e: &Expr, args: &[Expr]) -> String {
        let rt = self.ty(e);
        let rc = self.cty(&rt);
        let ut = self.ty(&args[0]);
        let u = self.val(&args[0]);
        let from = self.s.slice_args.get(&args[1].id).copied().unwrap_or(crate::sema::SliceFrom::Slice);
        let w = self.slice_arg(&args[1], from);
        let r = self.new_tmp("ut");
        let ws = self.new_tmp("w");
        self.emit(format!("{} {}; __builtin_memset(&{}, 0, sizeof {});", rc, r, r, r));
        self.emit(format!("const f20_slice_t {} = *({});", ws, w));
        let (addr, cap) = match ut {
            Ty::User(i) => match *i {
                Ty::Array(_, n) => (u.clone(), format!("{}u", n)),
                _ => (format!("{}.addr", u), format!("{}.len", u)),
            },
            _ => ("0".to_string(), "0u".to_string()),
        };
        let c = self.c("書き出す長さがユーザーのバッファを超えれば EINVAL、途中で失敗すれば EFAULT", "EINVAL if the length exceeds the user buffer, EFAULT if it faults halfway");
        self.emit(format!(
            "if ((uint64_t){ws}.n > (uint64_t)({cap})) {{ {r}.err = INT64_C(22); }} else if (fascon_copy_to_user({addr}, {ws}.p, (uint64_t){ws}.n) == 0u) {{ {r}.is_some = true; }} else {{ {r}.err = INT64_C(14); }} /* {c} */",
            ws = ws,
            cap = cap,
            r = r,
            addr = addr,
            c = c
        ));
        r
    }

    fn universal_method(&mut self, e: &Expr, recv: &Expr, k: MethodKind, args: &[Expr]) -> Option<String> {
        Some(match k {
            MethodKind::ResIsOk | MethodKind::ResIsErr => {
                let v = self.val(recv);
                if k == MethodKind::ResIsOk {
                    format!("({}.is_some)", paren(&v))
                } else {
                    format!("(!{}.is_some)", paren(&v))
                }
            }
            MethodKind::ResUnwrap => {
                let t = self.ty(recv);
                let v = self.val(recv);
                let h = self.hoist_force(&t, &v);
                if let Some((label, code, depth)) = self.catch_stack.last().cloned() {
                    // catch_fault の中: 失敗値を持ったまま区間の後始末へ / inside catch_fault: go to the handler carrying the error
                    self.emit(format!("if (FASCON_UNLIKELY(!{}.is_some)) {{", h));
                    self.ind += 1;
                    self.emit(format!("{} = {}.err;", code, h));
                    self.emit_drops_from(depth);
                    self.emit(format!("goto {};", label));
                    self.ind -= 1;
                    self.emit("}");
                } else {
                    let site = self.site(e.span);
                    self.emit(format!("if (FASCON_UNLIKELY(!{}.is_some)) fascon_unwrap_panic({});", h, site));
                }
                format!("{}.value", h)
            }
            MethodKind::ResUnwrapOr => {
                let t = self.ty(recv);
                let v = self.val(recv);
                let h = self.hoist_force(&t, &v);
                let d = self.val(&args[0]);
                format!("({}.is_some ? {}.value : {})", h, h, paren(&d))
            }
            MethodKind::FaultCode => {
                let v = self.val(recv);
                format!("((int64_t){})", paren(&v))
            }
            MethodKind::UserLen => {
                let v = self.val(recv);
                format!("({}.len)", paren(&v))
            }
            MethodKind::ProcOnCpu(a) | MethodKind::ProcOffCpu(a) | MethodKind::ProcCpuMask(a) | MethodKind::ProcMapPage(a) | MethodKind::ProcSharePage(a) | MethodKind::ProcCowCopy(a) | MethodKind::ProcUnmapPage(a) | MethodKind::ProcTlbFlush(a) | MethodKind::ProcPeek(a) | MethodKind::ProcTlbCount(a, _) => {
                let _ = recv;
                return Some(self.process_method(e, k, a, args));
            }
            _ => return None,
        })
    }

    /// プロセス表のメソッド（generic-os）: 隠れ列 cpu_mask と、スロットごとのアドレス空間（ページの所有権の表）
    /// process-table methods (generic-os): the hidden cpu_mask column and a per-slot address space (a page ownership table)
    fn process_method(&mut self, e: &Expr, k: MethodKind, a: usize, args: &[Expr]) -> String {
        let n = self.s.t.arenas[a].name.clone();
        // 古い・偽造の Id は番兵のスロット（何も持たないアドレス空間）に逸れる / stale or forged Ids land on the sentinel slot
        let slot = |cg: &mut Self, x: &Expr| -> String {
            let v = cg.val(x);
            let h = cg.hoist_force(&Ty::Id(a), &v);
            format!("(arena_{n}_get({h}) != NULL ? F20_ID_INDEX({h}, ARENA_{n}_K) : F20_SENTINEL(ARENA_{n}_K))", n = n, h = h)
        };
        let asid = |i: &str| format!("((uint64_t){} + 1u)", i);
        match k {
            MethodKind::ProcTlbCount(_, ipi) => (if ipi { "fascon_tlb_ipi_count()" } else { "fascon_tlb_local_count()" }).to_string(),
            MethodKind::ProcOnCpu(_) | MethodKind::ProcOffCpu(_) => {
                let i = slot(self, &args[0]);
                let c = self.val(&args[1]);
                let (f, m) = if matches!(k, MethodKind::ProcOnCpu(_)) { ("__atomic_fetch_or", format!("(UINT64_C(1) << ({} & 63u))", paren(&c))) } else { ("__atomic_fetch_and", format!("~(UINT64_C(1) << ({} & 63u))", paren(&c))) };
                format!("(void){}(&arena_{}_cpu_mask[{}], {}, __ATOMIC_ACQ_REL)", f, n, i, m)
            }
            MethodKind::ProcCpuMask(_) => {
                let i = slot(self, &args[0]);
                format!("__atomic_load_n(&arena_{}_cpu_mask[{}], __ATOMIC_ACQUIRE)", n, i)
            }
            MethodKind::ProcMapPage(_) => {
                let i = slot(self, &args[0]);
                let ih = self.hoist_force(&Ty::Int(IntTy::U32), &i);
                let va = self.val(&args[1]);
                let pg = self.val(&args[2]);
                let w = self.val(&args[3]);
                let rt = self.ty(e);
                let rc = self.cty(&rt);
                let r = self.new_tmp("pm");
                let c = self.c("ページの所有権をアドレス空間へ移し、PTE を書く（古いページは解放か共有の参照を 1 つ減らす）", "move the page's ownership into the address space and write the PTE (the old page is freed or loses one shared reference)");
                self.emit(format!("const int64_t {r}_e = f20_aspace_map(&arena_{n}_aspace[{ih}], {asid}, (uint64_t){va}, (uint8_t *){pg}, {w}); /* {c} */", r = r, n = n, ih = ih, asid = asid(&ih), va = paren(&va), pg = pg, w = w, c = c));
                format!("(({}){{ .is_some = {r}_e == 0, .err = {r}_e }})", rc, r = r)
            }
            MethodKind::ProcSharePage(_) => {
                let i = slot(self, &args[0]);
                let ih = self.hoist_force(&Ty::Int(IntTy::U32), &i);
                let va = self.val(&args[1]);
                let j = slot(self, &args[2]);
                let jh = self.hoist_force(&Ty::Int(IntTy::U32), &j);
                let rt = self.ty(e);
                let rc = self.cty(&rt);
                let r = self.new_tmp("ps");
                let c = self.c("同じページを両方へ読み取り専用で写す（Copy-on-Write）。元の写しの TLB は cpu_mask で掃く", "map the same page read-only into both (Copy-on-Write); the source's TLB is flushed by cpu_mask");
                self.emit(format!("const int64_t {r}_e = f20_aspace_share(&arena_{n}_aspace[{ih}], {ai}, (uint64_t){va}, &arena_{n}_aspace[{jh}], {aj}, arena_{n}_cpu_mask[{ih}]); /* {c} */", r = r, n = n, ih = ih, ai = asid(&ih), va = paren(&va), jh = jh, aj = asid(&jh), c = c));
                format!("(({}){{ .is_some = {r}_e == 0, .err = {r}_e }})", rc, r = r)
            }
            MethodKind::ProcCowCopy(_) => {
                let i = slot(self, &args[0]);
                let va = self.val(&args[1]);
                let rt = self.ty(e);
                let rc = self.cty(&rt);
                let site = self.site(e.span);
                format!("(({})f20_aspace_cow_copy(&arena_{}_aspace[{}], (uint64_t){}, {}))", rc, n, i, paren(&va), site)
            }
            MethodKind::ProcUnmapPage(_) => {
                let i = slot(self, &args[0]);
                let ih = self.hoist_force(&Ty::Int(IntTy::U32), &i);
                let va = self.val(&args[1]);
                let rt = self.ty(e);
                let rc = self.cty(&rt);
                format!("(({})f20_aspace_unmap(&arena_{}_aspace[{}], {}, (uint64_t){}))", rc, n, ih, asid(&ih), paren(&va))
            }
            MethodKind::ProcTlbFlush(_) => {
                let i = slot(self, &args[0]);
                let ih = self.hoist_force(&Ty::Int(IntTy::U32), &i);
                let va = self.val(&args[1]);
                let c = self.c("自分のコアだけで走っていればローカルの TLBI、ほかのコアのビットがあればそのコアへだけ IPI", "a local TLBI when it runs on this core only; IPIs only to the cores whose bits are set otherwise");
                format!("fascon_tlb_flush_va({}, (uint64_t){}, __atomic_load_n(&arena_{}_cpu_mask[{}], __ATOMIC_ACQUIRE)) /* {} */", asid(&ih), paren(&va), n, ih, c)
            }
            MethodKind::ProcPeek(_) => {
                let i = slot(self, &args[0]);
                let va = self.val(&args[1]);
                let off = self.val(&args[2]);
                format!("f20_aspace_peek(&arena_{}_aspace[{}], (uint64_t){}, (uint64_t){})", n, i, paren(&va), paren(&off))
            }
            _ => "0".to_string(),
        }
    }

    /// `bind_mmio(Regs, 番地)`・`dma_alloc(T)`・`complete_dma(T, iova)` / type calls
    fn type_call_val(&mut self, e: &Expr) -> String {
        let k = self.s.type_calls.get(&e.id).cloned();
        let ExprKind::TypeCall { args, .. } = &e.kind else { return "0".into() };
        match k {
            Some(crate::sema::TypeCallKind::BindMmio(sid)) => {
                let a = self.val(&args[0]);
                let sn = self.struct_name(sid);
                self.flt_used = true;
                let c = self.c(
                    "バス列挙（DTB / ACPI / PCIe）で登録された装置の窓の中か確かめて結ぶ。外れていれば読み出しが全 1・書き込みを捨てる窓へ逸らしてラッチ",
                    "bind only inside a device window registered by bus enumeration (DTB / ACPI / PCIe); otherwise divert to a window that reads all-ones and drops writes, and latch",
                );
                format!("fascon_mmio_bind((uint64_t){}, sizeof({}), &_flt) /* {} */", paren(&a), sn, c)
            }
            Some(crate::sema::TypeCallKind::DmaAlloc(t)) => {
                let pc = self.own_cty(&t);
                let pay = self.payload_cty(&t);
                let site = self.site(e.span);
                format!("(({})fascon_dma_alloc(sizeof({}), {}))", pc.trim_end(), pay, site)
            }
            Some(crate::sema::TypeCallKind::CompleteDma(t)) => {
                let pc = self.own_cty(&t);
                let pay = self.payload_cty(&t);
                let a = self.val(&args[0]);
                format!("(({})fascon_dma_complete((uint64_t){}, sizeof({})))", pc.trim_end(), paren(&a), pay)
            }
            None => "0".to_string(),
        }
    }

    /// `atomic { 表[id].f op= v; }` → __atomic_* の 1 命令（AArch64: LSE の LDADD / LDSET / LDCLR / LDEOR / STLR）
    /// one __atomic_* operation (AArch64: LSE LDADD / LDSET / LDCLR / LDEOR / STLR)
    fn lower_atomic(&mut self, e: &Expr, b: &Block, dest: Option<&str>) {
        let x = match (b.stmts.as_slice(), &b.tail) {
            ([Stmt::Expr(x)], None) => x,
            ([], Some(t)) => &**t,
            _ => return,
        };
        let line = self.line_of(e.span);
        // `atomic { 表[id].f }`: 不可分な読み出し（acquire。AArch64: LDAR）/ an indivisible load (acquire; AArch64: LDAR)
        if let ExprKind::Field(..) = &x.kind {
            let lt = self.ty(x);
            let ct = self.cty(&lt);
            let place = self.lv(x);
            let c = self.c("不可分な読み出し（acquire）", "an indivisible load (acquire)");
            match dest {
                Some(d) => self.emit(format!("{} = ({})__atomic_load_n(&{}, __ATOMIC_ACQUIRE); /* {} {}: atomic — {} */", d, ct, place, self.c("line", "line"), line, c)),
                None => self.emit(format!("(void)__atomic_load_n(&{}, __ATOMIC_ACQUIRE); /* {} {}: atomic — {} */", place, self.c("line", "line"), line, c)),
            }
            return;
        }
        let ExprKind::Assign { op, lhs, rhs } = &x.kind else { return };
        let lt = self.ty(lhs);
        let ct = self.cty(&lt);
        let rv = self.val(rhs);
        let rv = self.hoist(&lt, &rv);
        let place = self.lv(lhs);
        let what = self.c("不可分な読み書き（ほかのコアの atomic と混ざらない。この列は射影列から外してある）", "an indivisible read-modify-write (never torn by other cores' atomics; this field is kept out of columns)");
        // 値の形（更新前の値を返す）: AArch64 LSE の SWPAL / LDADDAL / LDSETAL / LDCLRAL / LDEORAL 1 命令
        // the value form (yields the value before the update): one AArch64 LSE SWPAL / LDADDAL / LDSETAL / LDCLRAL / LDEORAL
        if let Some(d) = dest {
            let f = match op {
                None => "__atomic_exchange_n",
                Some(BinOp::Add) => "__atomic_fetch_add",
                Some(BinOp::Sub) => "__atomic_fetch_sub",
                Some(BinOp::BitOr) => "__atomic_fetch_or",
                Some(BinOp::BitAnd) => "__atomic_fetch_and",
                _ => "__atomic_fetch_xor",
            };
            self.emit(format!("{} = ({}){}(&{}, ({}){}, __ATOMIC_ACQ_REL); /* {} {}: atomic — {} */", d, ct, f, place, ct, paren(&rv), self.c("line", "line"), line, what));
            if matches!(op, Some(BinOp::Add) | Some(BinOp::Sub)) {
                let chk = self.new_tmp("ac");
                self.flt_used = true;
                let bf = if matches!(op, Some(BinOp::Add)) { "__builtin_add_overflow" } else { "__builtin_sub_overflow" };
                self.emit(format!("{} {}; _flt |= (uint32_t){}({}, ({}){}, &{}) << 1; /* F20_FAULT_ARITH */", ct, chk, bf, d, ct, paren(&rv), chk));
            }
            return;
        }
        match op {
            None => self.emit(format!("__atomic_store_n(&{}, ({}){}, __ATOMIC_RELEASE); /* {} {}: atomic — {} */", place, ct, paren(&rv), self.c("line", "line"), line, what)),
            Some(o) => {
                let f = match o {
                    BinOp::Add => "__atomic_fetch_add",
                    BinOp::Sub => "__atomic_fetch_sub",
                    BinOp::BitOr => "__atomic_fetch_or",
                    BinOp::BitAnd => "__atomic_fetch_and",
                    _ => "__atomic_fetch_xor",
                };
                if matches!(o, BinOp::Add | BinOp::Sub) {
                    // あふれ: メモリの値は折り返すが、ラッチを立てて次のコミット点で止める（Fascon の検査付き算術のまま）
                    // overflow: the value in memory wraps, but the latch is set and the next commit point stops (Fascon's
                    // checked arithmetic is kept)
                    let old = self.new_tmp("ao");
                    let chk = self.new_tmp("ac");
                    self.flt_used = true;
                    let bf = if *o == BinOp::Add { "__builtin_add_overflow" } else { "__builtin_sub_overflow" };
                    self.emit(format!("const {} {} = {}(&{}, ({}){}, __ATOMIC_ACQ_REL); /* {} {}: atomic — {} */", ct, old, f, place, ct, paren(&rv), self.c("line", "line"), line, what));
                    self.emit(format!("{} {}; _flt |= (uint32_t){}({}, ({}){}, &{}) << 1; /* F20_FAULT_ARITH */", ct, chk, bf, old, ct, paren(&rv), chk));
                } else {
                    self.emit(format!("(void){}(&{}, ({}){}, __ATOMIC_ACQ_REL); /* {} {}: atomic — {} */", f, place, ct, paren(&rv), self.c("line", "line"), line, what));
                }
            }
        }
    }

    /// `catch_fault { .. }` → Result<T, FaultError>。区間の中のコミット点は止めずに局所的に飛ぶ（大域脱出なし。解放は済ませてから）
    /// inside the section, commit points jump locally instead of stopping (no non-local exit; drops run first)
    fn lower_catch(&mut self, e: &Expr, b: &Block, dest: Option<&str>) {
        let rt = self.ty(e);
        let rc = self.cty(&rt);
        let inner = match &rt {
            Ty::Result(t, _) => (**t).clone(),
            _ => Ty::Unit,
        };
        let n = self.new_tmp("fc");
        let label = format!("{}_fault", n);
        let code = format!("{}_code", n);
        let line = self.line_of(e.span);
        self.flt_used = true;
        self.emit("{");
        self.ind += 1;
        let c = self.c(
            "catch_fault: 区間の中のラッチ異常・ユーザーメモリ例外・unwrap の失敗を Err(FaultError) にする（ローカルな goto だけ。区間で作った値は解放してから飛ぶ）",
            "catch_fault: latched faults, user-memory faults and failed unwraps inside become Err(FaultError) (a local goto only; values made in the section are dropped before the jump)",
        );
        self.emit(format!("const uint32_t {n}_save = _flt; _flt = 0u; int64_t {code} = 0; /* {l} {line}: {c} */", n = n, code = code, l = self.c("line", "line"), line = line, c = c));
        self.emit(format!("{} {}; __builtin_memset(&{}, 0, sizeof {});", rc, n, n, n));
        self.catch_stack.push((label.clone(), code.clone(), self.scopes.len()));
        self.emit("{");
        self.ind += 1;
        self.scopes.push(Vec::new());
        let unit = matches!(inner, Ty::Unit | Ty::Never | Ty::Err);
        let vd = format!("{}.value", n);
        self.block_stmts(b, if unit { None } else { Some(&vd) });
        let site = self.site(e.span);
        self.commit(&site);
        self.scopes.pop();
        self.ind -= 1;
        self.emit("}");
        self.catch_stack.pop();
        self.emit(format!("{}.is_some = true;", n));
        self.emit(format!("goto {}_done;", n));
        self.ind -= 1;
        self.emit(format!("{}:", label));
        self.ind += 1;
        self.emit(format!("{n}.is_some = false; {n}.err = {code} != 0 ? {code} : f20_fault_errno(_flt);", n = n, code = code));
        self.ind -= 1;
        self.emit(format!("{}_done:", n));
        self.ind += 1;
        self.emit(format!("_flt = {}_save;", n));
        if let Some(d) = dest {
            self.emit(format!("{} = {};", d, n));
        }
        self.ind -= 1;
        self.emit("}");
    }
}

impl<'a, 'p> Cg<'a, 'p> {
    /// アリーナ a の生の値の検証の C 式 / the C expression validating a raw value for arena a
    fn validate_c(&self, a: usize, v: &str) -> String {
        let n = self.s.t.arenas[a].name.clone();
        if self.s.t.arenas[a].bump {
            format!("f20_bump_validate(arena_{n}.gen, arena_{n}.st.top, ARENA_{n}_K, {v})", n = n, v = v)
        } else {
            format!("f20_arena_validate(arena_{n}.gen, arena_{n}.link, ARENA_{n}_CAP, ARENA_{n}_K, {v})", n = n, v = v)
        }
    }

    /// ユニバーサル OS 拡張の構文を使うか（使わない今までのプログラムの C には何も足さない）
    /// whether the program uses universal-OS syntax (earlier programs get nothing new in their C)
    fn uses_universal(&self) -> bool {
        let s = self.s;
        if !s.type_calls.is_empty() || !s.atomic_sites.is_empty() || !s.catch_sites.is_empty() || !s.secret_binds.is_empty() {
            return true;
        }
        if s.t.structs.iter().any(|x| x.secret || x.on_drop.is_some() || x.stable_abi) || s.t.arenas.iter().any(|a| a.gc) {
            return true;
        }
        if s.t.fns.iter().any(|f| f.syscall_v2 || f.trap_v2 || f.extern_c.is_some() || f.asm.is_some()) {
            return true;
        }
        use crate::sema::Builtin as B;
        if s.calls.values().any(|k| matches!(k, CallKind::Builtin(B::CopyFromUser | B::CopyToUser | B::SubmitDma | B::CtEq | B::Schedule | B::SleepTicks | B::RcuSynchronize | B::ClockMonoNs | B::CpuThis))) {
            return true;
        }
        let mut found = false;
        for it in &s.prog.items {
            if let Item::Fn(fd) = it {
                crate::sema::walk_block(&fd.body, &mut |x: &Expr| {
                    if matches!(x.kind, ExprKind::RcuRead(_)) {
                        found = true;
                    }
                });
            }
        }
        found
    }

    /// `extern "C" fn 名前(..) -> T;`: C の関数の宣言と、Id を受け取るなら validate を差し込む境界の関数
    /// the declaration of the C function, plus a boundary function that inserts validate when Ids come back
    fn gen_c_import(&mut self, fi: usize) -> String {
        let f = self.s.t.fns[fi].clone();
        let mut ps = Vec::new();
        let mut names = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            let n = format!("a{}", i);
            ps.push(self.cdecl(&p.ty, &n));
            names.push(n);
        }
        let params = if ps.is_empty() { "void".to_string() } else { ps.join(", ") };
        let ret = self.cty(&f.ret);
        let rc = if matches!(f.ret, Ty::Unit) { "void".to_string() } else { ret.clone() };
        let wrapper = self.fn_c_names[&fi].clone();
        let ida = match &f.ret {
            Ty::Id(a) => Some(*a),
            Ty::Option(i) => match **i {
                Ty::Id(a) => Some(a),
                _ => None,
            },
            _ => None,
        };
        let call = format!("{}({})", f.name, names.join(", "));
        let body = match (ida, matches!(f.ret, Ty::Unit)) {
            (Some(a), _) => format!("return ({}){};", ret, self.validate_c(a, &call)),
            (None, true) => format!("{};", call),
            (None, false) => format!("return {};", call),
        };
        let c = self.c(
            "C から取り込む関数（extern \"C\"）。返ってきた生の Id はコンパイラが validate を差し込んで検証する",
            "a function imported from C (extern \"C\"); raw Ids that come back are validated by the validate the compiler inserts",
        );
        format!("/* {} */\nextern {} {}({});\nstatic inline FASCON_MAYBE_UNUSED {} {}({}) {{ {} }}\n", c, rc, f.name, params, rc, wrapper, params, body)
    }

    /// `#[asm(..)] fn`: アーキテクチャごとの命令列を GCC / Clang の拡張 asm にした static inline 関数
    /// a static inline function with one GCC / Clang extended-asm statement per architecture
    fn gen_asm_fn(&mut self, fi: usize) -> String {
        let f = self.s.t.fns[fi].clone();
        let spec = f.asm.clone().unwrap_or_default();
        let name = self.fn_c_names[&fi].clone();
        let mut ps = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            let _ = i;
            ps.push(self.cdecl(&p.ty, &sanitize(&p.name)));
        }
        let params = if ps.is_empty() { "void".to_string() } else { ps.join(", ") };
        let unit = matches!(f.ret, Ty::Unit);
        let rc = if unit { "void".to_string() } else { self.cty(&f.ret) };
        // {r} → %0（結果があるとき）、{名前} / {番号} → 入力のオペランド / {r} → %0 (when there is a result); {name} / {index} → inputs
        let off = if unit { 0 } else { 1 };
        let tr = |t: &str| -> String {
            let mut out = t.replace('%', "%%");
            if !unit {
                out = out.replace("{r}", "%0");
            }
            for (i, p) in f.params.iter().enumerate() {
                out = out.replace(&format!("{{{}}}", p.name), &format!("%{}", i + off));
                out = out.replace(&format!("{{{}}}", i), &format!("%{}", i + off));
            }
            out.replace('"', "\\\"")
        };
        let inputs: Vec<String> = f.params.iter().map(|p| format!("\"r\"({})", sanitize(&p.name))).collect();
        let mut clob: Vec<String> = Vec::new();
        if spec.effects.split(|c: char| c == ',' || c == ' ' || c == '|').any(|e| matches!(e, "barrier" | "mmio" | "irq" | "sleep")) || spec.effects.is_empty() {
            clob.push("\"memory\"".to_string());
        }
        for c in spec.clobbers.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()) {
            clob.push(format!("\"{}\"", c));
        }
        let mk = |tmpl: &str| -> String {
            let outs = if unit { String::new() } else { format!("\"=r\"(_r)") };
            let decl = if unit { String::new() } else { format!("{} _r; ", rc) };
            let ret = if unit { String::new() } else { " return _r;".to_string() };
            format!("    {}__asm__ __volatile__(\"{}\" : {} : {} : {});{}", decl, tr(tmpl), outs, inputs.join(", "), clob.join(", "), ret)
        };
        let mut body = String::new();
        if let Some(t) = &spec.aarch64 {
            let _ = writeln!(body, "#if defined(__aarch64__) && !defined(FASCON2_HOSTED_MMIO)\n{}", mk(t));
        }
        if let Some(t) = &spec.x86_64 {
            let _ = writeln!(body, "{} defined(__x86_64__) && !defined(FASCON2_HOSTED_MMIO)\n{}", if body.is_empty() { "#if" } else { "#elif" }, mk(t));
        }
        let host = match &spec.host {
            Some(h) => {
                let mut h = h.clone();
                for (i, p) in f.params.iter().enumerate() {
                    h = h.replace(&format!("{{{}}}", p.name), &sanitize(&p.name)).replace(&format!("{{{}}}", i), &sanitize(&p.name));
                }
                if unit {
                    format!("    (void)({});", h)
                } else {
                    format!("    return ({})({});", rc, h)
                }
            }
            None => {
                let used: String = f.params.iter().map(|p| format!("(void){}; ", sanitize(&p.name))).collect();
                if unit {
                    format!("    {}/* {} */", used, self.c("このアーキテクチャの命令列は無い（何もしない）", "no template for this architecture (does nothing)"))
                } else {
                    format!("    {}return ({})0; /* {} */", used, rc, self.c("このアーキテクチャの命令列は無い（0 を返す）", "no template for this architecture (returns 0)"))
                }
            }
        };
        if body.is_empty() {
            let _ = writeln!(body, "{}", host);
        } else {
            let _ = writeln!(body, "#else\n{}\n#endif", host);
        }
        let c = self.c(
            &format!("安全インラインアセンブリの境界 #[asm]（効果: {}）。オペランドは整数だけ（メモリを指す値・#[secret] の値は渡せない）", if spec.effects.is_empty() { "none" } else { &spec.effects }),
            &format!("safe inline-assembly boundary #[asm] (effects: {}); operands are integers only (nothing pointing to memory, no #[secret] values)", if spec.effects.is_empty() { "none" } else { &spec.effects }),
        );
        format!("/* {} */\nstatic inline FASCON_MAYBE_UNUSED {} {}({}) {{\n{}}}\n", c, rc, name, params, body)
    }
}

impl<'a, 'p> Cg<'a, 'p> {
    /// catch_fault の区間の中から（推移的に）呼ばれうる関数 / the functions that may be called (transitively) from inside catch_fault
    fn compute_propagate(&self) -> HashSet<usize> {
        let mut set: HashSet<usize> = HashSet::new();
        if self.s.catch_sites.is_empty() {
            return set;
        }
        let mut work: Vec<usize> = Vec::new();
        for it in &self.s.prog.items {
            let Item::Fn(fd) = it else { continue };
            crate::sema::walk_block(&fd.body, &mut |x: &Expr| {
                if let ExprKind::CatchFault(b) = &x.kind {
                    crate::sema::walk_block(b, &mut |y: &Expr| {
                        match self.s.calls.get(&y.id) {
                            Some(CallKind::User(fi)) => work.push(*fi),
                            Some(CallKind::Indirect) => {
                                if let Some(c) = self.s.indirect_cands.get(&y.id) {
                                    work.extend(c.iter().copied());
                                }
                            }
                            _ => {}
                        }
                    });
                }
            });
        }
        while let Some(fi) = work.pop() {
            let f = &self.s.t.fns[fi];
            if f.is_main || f.extern_c == Some(false) || f.asm.is_some() || f.syscall.is_some() || !set.insert(fi) {
                continue;
            }
            work.extend(f.callees.iter().copied());
        }
        set
    }
}

impl<'a, 'p> Cg<'a, 'p> {
    /// 列を作ってはいけない理由（atomic のフィールド・#[no_column]・#[stable_abi]） / why a column must not be made
    fn column_veto(&self, a: usize, paths: &[Vec<String>]) -> Option<(String, String)> {
        let elem = self.s.t.arenas[a].elem.clone();
        if self.mag_arena(a) {
            let n = self.s.t.arenas[a].name.clone();
            return Some((format!("generic-os の共有アリーナ `{}`（ロックフリーの確保・解放と列の同期は両立しない）", n), format!("the generic-os shared arena `{}` (lock-free alloc / free cannot keep columns in step)", n)));
        }
        if let Ty::Struct(sid) = &elem {
            if self.s.t.structs[*sid].stable_abi {
                let n = self.s.t.structs[*sid].name.clone();
                return Some((format!("要素 `{}` が #[stable_abi]（レイアウト固定・射影列なし）", n), format!("the element `{}` is #[stable_abi] (fixed layout, no columns)", n)));
            }
        }
        for p in paths {
            // atomic で書かれるフィールド（道筋の接頭辞が重なれば同じ場所） / a field written under atomic (overlapping prefixes)
            for (x, q) in &self.s.atomic_fields {
                if *x == a && (p.starts_with(q) || q.starts_with(p)) {
                    let f = q.join(".");
                    return Some((format!("`{}` は atomic {{ .. }} で更新される（コア間の不可分な更新と写しの同期は両立しない）", f), format!("`{}` is updated under atomic {{ .. }} (indivisible cross-core updates cannot keep a mirror in step)", f)));
                }
            }
            // #[no_column]: 道筋の途中のどのフィールドでも / #[no_column] on any field along the path
            let mut t = elem.clone();
            for (k, f) in p.iter().enumerate() {
                let Ty::Struct(sid) = t else { break };
                let info = &self.s.t.structs[sid];
                let Some((fi, fd)) = info.field(f) else { break };
                if info.no_column.get(fi).copied().unwrap_or(false) {
                    let fname = p[..=k].join(".");
                    return Some((format!("`{}` に #[no_column]（書き込みの多いフィールド）", fname), format!("`{}` is #[no_column] (a write-heavy field)", fname)));
                }
                t = fd.ty.clone();
            }
        }
        None
    }
}

/// PAC の修飾子: 関数の型の名前から作る 64 ビットの定数（型の違う関数ポインタへすり替えると AUTIA が失敗する）
/// the PAC modifier: a 64-bit constant made from the function type's name (substituting a pointer of another type makes
/// AUTIA fail)
pub fn pac_modifier(type_name: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in type_name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("UINT64_C({:#018x})", h)
}

impl<'a, 'p> Cg<'a, 'p> {
    /// generic-os の共有アリーナ（Per-CPU マガジンと RCU の遅延回収を使う） / a generic-os shared arena (per-CPU magazines and
    /// RCU-deferred reclamation)
    fn mag_arena(&self, a: usize) -> bool {
        let x = &self.s.t.arenas[a];
        self.o.profile == crate::target::Profile::GenericOs && x.shared && !x.bump && x.task_stack.is_none() && !x.dma
    }

    /// 生成したアリーナの C をマガジン版に書き換える: 置き場所（仮想番地の予約）・確保・解放・取り出し
    /// rewrite a generated arena into the magazine version: placement (VA reservation), alloc, free and remove
    fn gen_mag_arena(&mut self, ai: usize, text: String) -> String {
        let a = self.s.t.arenas[ai].clone();
        let n = a.name.clone();
        let et = self.cty(&a.elem);
        let id = self.id_name(ai);
        let nd = self.s.t.needs_drop(&a.elem);
        let drop = if nd { self.drop_code(&a.elem, "(*p)") } else { String::new() };
        // 仮想番地の並び: 前の共有アリーナの後ろ（2 MiB 単位） / VA layout: after the previous shared arena (2 MiB units)
        let prev = (0..ai).rev().find(|&b| self.mag_arena(b)).map(|b| self.s.t.arenas[b].name.clone());
        let off = match prev {
            Some(p) => format!("(ARENA_{p}_VA_OFF + F20_ROUND_2M(sizeof(f20_arena_t({p}))))", p = p),
            None => "UINT64_C(0)".to_string(),
        };
        let storage_old = format!("static f20_arena_t({n}) arena_{n} F20_ARENA_STORAGE(f20_arena_t({n}));", n = n);
        let c1 = self.c(
            "generic-os: 表は最大容量ぶんの仮想番地を固定で予約する（BSP は未マップのまま置き、next_unused が 2 MiB を跨ぐときだけ写す）。番地は定数なので走査の基底の計算に間接参照は増えない",
            "generic-os: the table reserves virtual addresses for its full capacity (the BSP leaves them unmapped and maps 2 MiB only when next_unused crosses it); the address is a constant, so scans gain no indirection",
        );
        let storage_new = format!(
            "/* {c1} */\n#define ARENA_{n}_VA_OFF {off}\n#if defined(FASCON2_GENOS_VA)\n#define arena_{n} (*(f20_arena_t({n}) *)F20_ARENA_VA(ARENA_{n}_VA_OFF))\n#else\nstatic f20_arena_t({n}) arena_{n} F20_ARENA_STORAGE(f20_arena_t({n}));\n#endif\nstatic f20_magd_t arena_{n}_magd;\nstatic FASCON_MAYBE_UNUSED void arena_{n}_mag_init(void) {{\n    f20_magd_t *d = &arena_{n}_magd;\n    d->st = &arena_{n}.st;\n    d->gen = arena_{n}.gen;\n    d->link = arena_{n}.link;\n    d->live = arena_{n}.live;\n    d->cap = ARENA_{n}_CAP;\n    d->K = ARENA_{n}_K;\n    d->base = (uint8_t *)&arena_{n};\n    d->slot_off = (uint64_t)((uint8_t *)&arena_{n}.slot[0] - (uint8_t *)&arena_{n});\n    d->slot_size = sizeof(arena_{n}.slot[0]);\n    d->mapped = F20_ROUND_2M(d->slot_off);\n    fascon_plat_arena_grow(d->base, d->mapped); /* {c2} */\n    fascon_plat_arena_grow((uint8_t *)&arena_{n}.slot[F20_SENTINEL(ARENA_{n}_K)], sizeof(arena_{n}.slot[0])); /* {c3} */\n}}",
            c1 = c1,
            n = n,
            off = off,
            c2 = self.c("管理情報（st・live・gen・link）", "the bookkeeping (st, live, gen, link)"),
            c3 = self.c("番兵スロット（証明できない書き込みの逸らし先）", "the sentinel slot (where unproven writes are diverted)"),
        );
        let mut out = text.replacen(&storage_old, &storage_new, 1);
        // 確保 / alloc
        let fd = if nd { format!("    if (id == 0u) {{ {et} *p = ({et} *)v; {d} }}\n", et = et, d = drop) } else { String::new() };
        let alloc = format!(
            "static inline FASCON_MAYBE_UNUSED {id} arena_{n}_alloc(const {et} *v) {{\n    const f20_id_t id = f20_mag_acquire(&arena_{n}_magd); /* {c} */\n    if (id != 0u) {{\n        const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K);\n        arena_{n}.slot[i].v = *v;\n        f20_mag_publish(&arena_{n}_magd, i); /* {p} */\n    }}\n{fd}    return id; /* 0 = None */\n}}\n",
            id = id,
            n = n,
            et = et,
            fd = fd,
            c = self.c("自分のコアのマガジンから（ロックなし）", "from this core's magazine (no lock)"),
            p = self.c("値を書いてから生存にする（release）", "made live after the value is written (release)"),
        );
        out = replace_fn(&out, &format!("static inline FASCON_MAYBE_UNUSED {} arena_{}_alloc(", id, n), &alloc);
        // 解放（猶予期間の後で中身を解放）と取り出し（猶予期間を待ってから移動） / free (contents dropped after the grace
        // period) and remove (moves out after waiting for the grace period)
        let free = format!(
            "/* {c0} */\nstatic void arena_{n}_reclaim(uintptr_t i) {{\n    {et} *p = &arena_{n}.slot[i].v;\n{d}    __builtin_memset(p, 0, sizeof *p);\n    f20_mag_release(&arena_{n}_magd, (uint32_t)i);\n}}\nstatic inline FASCON_MAYBE_UNUSED bool arena_{n}_free({id} id) {{\n    if (arena_{n}_get(id) == NULL) return false;\n    const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K);\n    if (!f20_mag_retire(&arena_{n}_magd, i)) return false; /* {c1} */\n    fascon_rcu_defer(arena_{n}_reclaim, (uintptr_t)i); /* {c2} */\n    return true;\n}}\n",
            c0 = self.c("RCU の猶予期間の後: 中身の解放 → 0 埋め → 世代 +1 → マガジンへ", "after the RCU grace period: drop → zero → generation + 1 → back to a magazine"),
            n = n,
            et = et,
            id = id,
            d = if nd { format!("    {}\n", drop) } else { String::new() },
            c1 = self.c("LIVE → PENDING（新しいルックアップはもう通らない）", "LIVE → PENDING (new lookups fail from now on)"),
            c2 = self.c("読み手が抜けるまで中身は残る", "the contents stay until the readers have left"),
        );
        out = replace_fn(&out, &format!("static inline FASCON_MAYBE_UNUSED bool arena_{}_free(", n), &free);
        let remove = format!(
            "static void arena_{n}_reclaim_moved(uintptr_t i) {{\n    __builtin_memset(&arena_{n}.slot[i].v, 0, sizeof arena_{n}.slot[i].v);\n    f20_mag_release(&arena_{n}_magd, (uint32_t)i);\n}}\nstatic inline FASCON_MAYBE_UNUSED bool arena_{n}_remove({id} id, {et} *out) {{\n    {et} *p = arena_{n}_get(id);\n    if (p == NULL) return false;\n    const uint32_t i = F20_ID_INDEX(id, ARENA_{n}_K);\n    if (!f20_mag_retire(&arena_{n}_magd, i)) return false;\n    fascon_rcu_synchronize(); /* {c} */\n    *out = *p;\n    arena_{n}_reclaim_moved((uintptr_t)i);\n    return true;\n}}\n",
            n = n,
            id = id,
            et = et,
            c = self.c("所有権ごと取り出す前に読み手が抜けるのを待つ", "wait for the readers to leave before moving the value out"),
        );
        out = replace_fn(&out, &format!("static inline FASCON_MAYBE_UNUSED bool arena_{}_remove(", n), &remove);
        out
    }
}

/// `start` で始まる C の関数（次の行頭の `}` まで）を置き換える / replace the C function starting with `start` (up to the next
/// `}` at the start of a line)
fn replace_fn(text: &str, start: &str, new: &str) -> String {
    let Some(a) = text.find(start) else { return text.to_string() };
    let Some(rel) = text[a..].find("\n}\n") else { return text.to_string() };
    let b = a + rel + 3;
    format!("{}{}{}", &text[..a], new, &text[b..])
}

impl<'a, 'p> Cg<'a, 'p> {
    /// プロセス表の隠れ列: cpu_mask（そのプロセスのタスクが走っているコア）とアドレス空間。free でアドレス空間を畳む
    /// the hidden columns of a process table: cpu_mask (the cores running the process's tasks) and the address space; free
    /// tears the address space down
    fn gen_process_cols(&mut self, ai: usize, text: String) -> String {
        let n = self.s.t.arenas[ai].name.clone();
        let c = self.c(
            "プロセス表の隠れ列（generic-os）: cpu_mask = いまこのプロセスを走らせているコアのビット（スケジューラが on_cpu / off_cpu で立てる）、aspace = ページの所有権の表（PTE の写しの元）",
            "hidden columns of a process table (generic-os): cpu_mask = bits of the cores running this process now (set by the scheduler with on_cpu / off_cpu), aspace = the page ownership table (the source of the PTEs)",
        );
        let cols = format!(
            "/* {c} */\nstatic uint64_t arena_{n}_cpu_mask[F20_SLOTS(ARENA_{n}_K) + 1u] __attribute__((aligned(F20_DESTRUCTIVE_SIZE)));\nstatic f20_aspace_t arena_{n}_aspace[F20_SLOTS(ARENA_{n}_K) + 1u];\n",
            c = c,
            n = n
        );
        // 宣言（get の前に置く必要は無い。free の中で使う） / the declarations (used inside free)
        let anchor = format!("static inline FASCON_MAYBE_UNUSED bool arena_{}_free(", n);
        let mut out = match text.find(&anchor) {
            Some(i) => format!("{}{}{}", &text[..i], cols, &text[i..]),
            None => format!("{}{}", text, cols),
        };
        // free: アドレス空間を畳み（own のページは解放、共有のページは参照を 1 つ減らす）、cpu_mask を 0 に
        // free: tear the address space down (own pages freed, shared pages lose one reference), cpu_mask back to 0
        let needle = format!("    if (p == NULL) return false;\n");
        if let Some(fi) = out.find(&anchor) {
            if let Some(rel) = out[fi..].find(&needle) {
                let at = fi + rel + needle.len();
                let ins = format!(
                    "    {{ const uint32_t i_ = F20_ID_INDEX(id, ARENA_{n}_K); f20_aspace_drop(&arena_{n}_aspace[i_], (uint64_t)i_ + 1u); __atomic_store_n(&arena_{n}_cpu_mask[i_], 0u, __ATOMIC_RELEASE); }} /* {c} */\n",
                    n = n,
                    c = self.c("アドレス空間を畳む", "tear down the address space")
                );
                out.insert_str(at, &ins);
            }
        }
        out
    }
}

/// 浮動小数点のリテラルを C に: 小数点も指数も無ければ `.0` を足し、f32 は `f` を付ける（C コンパイラが型ごとに正しく丸める）
/// a float literal in C: add `.0` when there is neither a point nor an exponent, and `f` for f32 (the C compiler rounds it
/// correctly per type)
pub fn float_lit_c(text: &str, t: &Ty) -> String {
    let mut s = text.to_string();
    if !s.contains('.') && !s.contains('e') && !s.contains('E') {
        s.push_str(".0");
    }
    match t {
        Ty::Float(crate::lexer::FloatTy::F32) => format!("{}f", s),
        _ => s,
    }
}

/// `f32::NAN` などの定数の C（GCC / Clang の組み込みと、往復できる 10 進） / the C of constants such as `f32::NAN` (GCC /
/// Clang builtins and decimals that read back exactly)
pub fn float_const_c(ft: crate::lexer::FloatTy, c: &str) -> String {
    use crate::lexer::FloatTy;
    let s = match (ft, c) {
        (FloatTy::F32, "NAN") => "__builtin_nanf(\"\")",
        (FloatTy::F32, "INFINITY") => "__builtin_inff()",
        (FloatTy::F32, "NEG_INFINITY") => "(-__builtin_inff())",
        (FloatTy::F32, "MAX") => "3.40282347e+38f",
        (FloatTy::F32, "MIN") => "(-3.40282347e+38f)",
        (FloatTy::F32, "MIN_POSITIVE") => "1.17549435e-38f",
        (FloatTy::F32, _) => "1.1920929e-07f",
        (FloatTy::F64, "NAN") => "__builtin_nan(\"\")",
        (FloatTy::F64, "INFINITY") => "__builtin_inf()",
        (FloatTy::F64, "NEG_INFINITY") => "(-__builtin_inf())",
        (FloatTy::F64, "MAX") => "1.7976931348623157e+308",
        (FloatTy::F64, "MIN") => "(-1.7976931348623157e+308)",
        (FloatTy::F64, "MIN_POSITIVE") => "2.2250738585072014e-308",
        (FloatTy::F64, _) => "2.220446049250313e-16",
    };
    s.to_string()
}

/// 整数型の下限・上限の C の定数（飽和変換の範囲） / the C constants of an integer type's bounds (the saturation range)
fn int_limits_c(it: IntTy) -> (&'static str, &'static str) {
    match it {
        IntTy::I8 => ("INT8_MIN", "INT8_MAX"),
        IntTy::I16 => ("INT16_MIN", "INT16_MAX"),
        IntTy::I32 => ("INT32_MIN", "INT32_MAX"),
        IntTy::I64 => ("INT64_MIN", "INT64_MAX"),
        IntTy::Isize => ("INTPTR_MIN", "INTPTR_MAX"),
        IntTy::U8 => ("0", "UINT8_MAX"),
        IntTy::U16 => ("0", "UINT16_MAX"),
        IntTy::U32 => ("0", "UINT32_MAX"),
        IntTy::U64 => ("0", "UINT64_MAX"),
        IntTy::Usize => ("0", "UINTPTR_MAX"),
    }
}

/// `#[fast_math]` の範囲にある `for id in @a` のループ変数を集める（関数の属性・ループの属性はその中の入れ子のループにも
/// 効く。--fast-math はプログラム全体） / collect the loop variables of `for id in @a` loops inside `#[fast_math]` (a
/// function or loop attribute also covers the loops nested in it; --fast-math covers the whole program)
fn fast_math_vars(s: &Sema) -> HashSet<BindId> {
    fn walk(e: &Expr, on: bool, prog: &crate::ast::Program, out: &mut HashSet<BindId>) {
        let here = on || prog.fast_math_loops.contains_key(&e.id);
        if let ExprKind::For { bind, iter: ForIter::Arena(..), .. } = &e.kind {
            if here {
                out.insert(*bind);
            }
        }
        let inner = if matches!(e.kind, ExprKind::For { .. } | ExprKind::While { .. } | ExprKind::Loop { .. }) { here } else { on };
        for c in crate::flow::children(e) {
            match c {
                crate::flow::Child::E(x) => walk(x, inner, prog, out),
                crate::flow::Child::B(b) => {
                    for st in &b.stmts {
                        match st {
                            Stmt::Let(l) => {
                                if let Some(i) = &l.init {
                                    walk(i, inner, prog, out);
                                }
                            }
                            Stmt::Expr(x) => walk(x, inner, prog, out),
                        }
                    }
                    if let Some(t) = &b.tail {
                        walk(t, inner, prog, out);
                    }
                }
            }
        }
    }
    let global = crate::universal::FAST_MATH.load(std::sync::atomic::Ordering::Relaxed);
    let mut out = HashSet::new();
    for f in &s.t.fns {
        if let Item::Fn(fd) = &s.prog.items[f.item] {
            let on = global || fd.attrs.iter().any(|a| a.name == "fast_math");
            for st in &fd.body.stmts {
                match st {
                    Stmt::Let(l) => {
                        if let Some(i) = &l.init {
                            walk(i, on, s.prog, &mut out);
                        }
                    }
                    Stmt::Expr(x) => walk(x, on, s.prog, &mut out),
                }
            }
            if let Some(t) = &fd.body.tail {
                walk(t, on, s.prog, &mut out);
            }
        }
    }
    out
}
