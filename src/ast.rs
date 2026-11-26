//! 構文木 / abstract syntax tree

use crate::lexer::{FloatTy, IntTy};
use crate::span::Span;

pub type ExprId = u32;
/// 束縛（let・引数・for 変数・パターン変数）ごとの番号 / one number per binding site
pub type BindId = u32;

#[derive(Clone, Debug)]
pub struct Program {
    pub items: Vec<Item>,
    pub n_exprs: u32,
    pub n_binds: u32,
    pub v2_attr: bool,
    /// `#![reframing(rust)]` が書かれていたか / whether `#![reframing(rust)]` was written
    pub reframing_attr: bool,
    /// `for id in a.par() { .. }` の式（読み取り専用の並列走査） / `for id in a.par()` expressions (read-only parallel scans)
    pub par_fors: Vec<ExprId>,
    /// 前置 `*` が付いた式。参照は自動で辿られるので `*` は今までどおり何もしないが、`own T` の値に付いたときは own の
    /// 中身（`*ctx.buffer`）を指す（意味解析が型を見て決める）
    /// expressions written with a prefix `*`. References are auto-dereferenced, so `*` still does nothing for them, but on
    /// an `own T` value it denotes the contents of the own (`*ctx.buffer`); semantic analysis decides from the type
    pub derefs: std::collections::HashSet<ExprId>,
    /// `e?` から作った match の式 / the match expressions desugared from `e?`
    pub question_marks: std::collections::HashSet<ExprId>,
    /// `#[fast_math]` を付けたループ（for / while / loop）の式と属性の位置 / loops (for / while / loop) written with
    /// `#[fast_math]`, and where the attribute is
    pub fast_math_loops: std::collections::HashMap<ExprId, Span>,
}

#[derive(Clone, Debug)]
pub struct Attr {
    pub name: String,
    pub span: Span,
    /// `#[repr(align(N))]` の N と、その位置 / N of `#[repr(align(N))]` and where it is
    pub repr_align: Option<(u128, Span)>,
    /// `#[mmio_map(0x0900_0000)]` などの整数引数 / an integer argument such as `#[mmio_map(0x0900_0000)]`
    pub int_arg: Option<(u128, Span)>,
    /// `#[wire(le)]` などの名前の引数 / an identifier argument such as `#[wire(le)]`
    pub ident_arg: Option<String>,
    /// `#[asm(aarch64 = "..", effects = "none")]` のような `名前 = 文字列/整数` の引数 / `key = "string" | integer` arguments
    pub kv: Vec<(String, AttrVal, Span)>,
}

/// 属性の `名前 = 値` の値 / the value of a `key = value` attribute argument
#[derive(Clone, Debug)]
pub enum AttrVal {
    Str(String),
    Int(u128),
    Ident(String),
}

impl Attr {
    /// `名前 = "文字列"` の値 / the value of `key = "string"`
    pub fn kv_str(&self, key: &str) -> Option<&str> {
        self.kv.iter().find_map(|(k, v, _)| match v {
            AttrVal::Str(s) | AttrVal::Ident(s) if k == key => Some(s.as_str()),
            _ => None,
        })
    }
}

/// `import net::tcp;`（モジュールの取り込み。字句の段階で展開される） / module import (expanded at the token level)
#[derive(Clone, Debug)]
pub struct ImportDef {
    pub path: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Item {
    Struct(StructDef),
    Fn(FnDef),
    Static(StaticDef),
    /// `arena 名前: T[N];` と `bump_arena 名前: T[N];`（ArenaDef::bump で区別） / both kinds of arena (ArenaDef::bump)
    Arena(ArenaDef),
    Dma(DmaDef),
    UserMem(UserMemDef),
    Chan(ChanDef),
    Mmio(MmioDef),
    Alias(AliasDef),
    /// `port_io 名前 at_port 0x3f8;` / `port_io 名前: [u8; 8] at_port 0x3f8;`
    Port(PortDef),
    /// `import net::tcp;`
    Import(ImportDef),
}

#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: String,
    pub name_span: Span,
    pub fields: Vec<FieldDef>,
    pub attrs: Vec<Attr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
    /// `#[no_column]` / `#[secret]` などフィールドの属性 / field attributes such as `#[no_column]` / `#[secret]`
    pub attrs: Vec<Attr>,
}

#[derive(Clone, Debug)]
pub struct FnDef {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Block,
    pub is_async: bool,
    pub attrs: Vec<Attr>,
    pub span: Span,
    /// 項目の種類（ふつうの関数・割り込みハンドラ） / the kind of item (plain function, interrupt handler)
    pub kind: FnKind,
}

/// 関数として扱う項目の種類 / kinds of items that are handled as functions
#[derive(Clone, Debug)]
pub enum FnKind {
    Normal,
    /// `irq 名前 at INTID { .. }`
    Irq(Expr),
    /// `syscall 名前 at 番号 (引数) -> i64 { .. }`（カーネル側）/ the kernel side of a system call
    Syscall(Expr),
    /// `syscall 名前 at 番号 (引数) -> i64;`（ユーザー側: svc の呼び出しになる）/ the user side: becomes an svc
    SyscallStub(Expr),
    /// `trap user_fault(esr: u64, far: u64, pc: u64) { .. }` / `#[trap] fn user_fault(pid: Id<p>, addr: u64, is_write: bool) { .. }`
    Trap,
    /// `extern "C" fn 名前(..) -> T;`（body = false: C から取り込む）/ `extern "C" fn 名前(..) -> T { .. }`（body = true: C へ公開）
    /// an import from C (body = false) or a function exported to C (body = true)
    ExternC { body: bool },
    /// `#[asm(aarch64 = "..", x86_64 = "..", effects = "..")] fn 名前(..) -> T;`: 安全インラインアセンブリの境界
    /// the safe inline-assembly boundary
    Asm,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub bind: BindId,
    pub mutable: bool,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct StaticDef {
    pub name: String,
    pub name_span: Span,
    pub ty: TypeExpr,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ArenaDef {
    pub name: String,
    pub name_span: Span,
    pub elem: TypeExpr,
    pub cap: Expr,
    pub span: Span,
    /// `bump_arena`: 呼び出しリージョン単位のバンプアロケータ（関数の出口で先頭を 1 命令で戻す）
    /// `bump_arena`: a bump allocator per call region (its top is rewound in one instruction at function exit)
    pub bump: bool,
    /// `task_arena @t: T[N] stack バイト数;` のスタックの大きさ / the stack size of a task_arena
    pub task_stack: Option<Expr>,
    /// `dma_arena @b: T[N];`: DMA バッファの表（ハンドルは移動専用の `Dma<@b>`） / a table of DMA buffers (handles are move-only `Dma<@b>`)
    pub dma: bool,
    /// `gc_arena @n: T[N];`: fascon-gc が回収するアリーナ（手動の free / remove は F2621） / an arena reclaimed by fascon-gc
    /// (manual free / remove is F2621)
    pub gc: bool,
    /// `shared arena @g: T[N];`: 全コアで 1 つの実体（ふつうのアリーナはコアごとに 1 つ） / one instance for every core (plain arenas are per core)
    pub shared: bool,
    /// `gpu_arena @g: T[N];`: アクセラレータへ貸し出すピン留めの表（ハンドルは移動専用の `GpuBuf<@g>`） / a pinned table
    /// lent to an accelerator (handles are move-only `GpuBuf<@g>`)
    pub gpu: bool,
    /// `#[numa_node(n)]`（--target hpc: 共有の表を NUMA ノード n の物理メモリへ固定する） / `#[numa_node(n)]` (--target hpc:
    /// bind a shared table to the physical memory of NUMA node n)
    pub numa_node: Option<(u128, Span)>,
}

/// `chan @ch: own T[CAP];`: コア間のロックフリーなチャネル / a lock-free channel between cores
#[derive(Clone, Debug)]
pub struct ChanDef {
    pub name: String,
    pub name_span: Span,
    pub elem: TypeExpr,
    pub cap: Expr,
    pub span: Span,
}

/// `user_mem @u for @t: バイト数 at 仮想番地;`: タスクごとのユーザーメモリ / per-task user memory
#[derive(Clone, Debug)]
pub struct UserMemDef {
    pub name: String,
    pub name_span: Span,
    pub task: String,
    pub task_span: Span,
    pub size: Expr,
    pub at: Expr,
    pub span: Span,
}

/// `dma @r: [T; N];`: デバイスと共有する RAM（読み書きは常に volatile） / RAM shared with a device (every access is volatile)
#[derive(Clone, Debug)]
pub struct DmaDef {
    pub name: String,
    pub name_span: Span,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct PortDef {
    pub name: String,
    pub name_span: Span,
    /// 省略時は u8（`out dx, al`）。`u16` / `u32` / `[u8; N]` など / defaults to u8 (`out dx, al`); `u16` / `u32` / `[u8; N]` ...
    pub ty: Option<TypeExpr>,
    pub port: Expr,
    pub attrs: Vec<Attr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct MmioDef {
    pub name: String,
    pub name_span: Span,
    pub ty: TypeExpr,
    pub addr: Expr,
    pub attrs: Vec<Attr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct AliasParam {
    pub name: String,
    pub bound: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct AliasDef {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<AliasParam>,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum GenericArg {
    Type(TypeExpr),
    Arena(String, Span),
}

#[derive(Clone, Debug)]
pub enum TypeExpr {
    Path { name: String, args: Vec<GenericArg>, span: Span },
    Own(Box<TypeExpr>, Span),
    Ref { mutable: bool, inner: Box<TypeExpr>, span: Span },
    Array { elem: Box<TypeExpr>, len: Box<Expr>, span: Span },
    /// `[u8]`（`&[u8]` / `&mut [u8]` の中だけ）: 長さ付きの窓 / a window with a length (only inside `&[u8]` / `&mut [u8]`)
    Slice { elem: Box<TypeExpr>, span: Span },
    Unit(Span),
    /// 関数ポインタ型 `fn(&mut i32) -> i64`。引数の位置なので参照型を書ける（戻り値には書けない）
    /// a function pointer type `fn(&mut i32) -> i64`; its parameters are parameter positions, so they may be references
    /// (the return type may not)
    Fn { params: Vec<TypeExpr>, ret: Option<Box<TypeExpr>>, span: Span },
    /// `user [u8; N]` / `user Str`: ユーザー空間のバッファ（#[syscall] の引数だけ。copy_from_user で 1 回だけ写す）
    /// a user-space buffer (#[syscall] parameters only; copied once with copy_from_user)
    User(Box<TypeExpr>, Span),
    /// `dma own T`: 4 KiB 境界の独立した物理ページに置く所有値（デバイスへ貸せる） / an owned value on its own 4 KiB-aligned
    /// physical pages (lendable to a device)
    DmaOwn(Box<TypeExpr>, Span),
    /// `mmio T`: bind_mmio で結んだレジスタ構造体のハンドル / the handle of a register struct bound with bind_mmio
    MmioBind(Box<TypeExpr>, Span),
}

impl TypeExpr {
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Path { span, .. } => *span,
            TypeExpr::Own(_, s) => *s,
            TypeExpr::Ref { span, .. } => *span,
            TypeExpr::Array { span, .. } => *span,
            TypeExpr::Slice { span, .. } => *span,
            TypeExpr::Unit(s) => *s,
            TypeExpr::Fn { span, .. } => *span,
            TypeExpr::User(_, s) | TypeExpr::DmaOwn(_, s) | TypeExpr::MmioBind(_, s) => *s,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let(LetStmt),
    /// 末尾に `;` が付いた式、または `;` の要らない式文（if / while など）
    /// an expression with a trailing `;`, or a block-like statement (if / while ...)
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct LetStmt {
    pub name: String,
    pub name_span: Span,
    pub bind: BindId,
    pub mutable: bool,
    pub ty: Option<TypeExpr>,
    pub init: Option<Expr>,
    pub span: Span,
    /// `#[secret] let k = ..;` / let の属性 / let attributes
    pub attrs: Vec<Attr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn text(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }
    pub fn is_cmp(self) -> bool {
        matches!(self, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
    }
    pub fn is_arith(self) -> bool {
        matches!(self, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem)
    }
    pub fn is_bit(self) -> bool {
        matches!(self, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
    }
    pub fn is_shift(self) -> bool {
        matches!(self, BinOp::Shl | BinOp::Shr)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub id: ExprId,
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ForIter {
    Range { lo: Box<Expr>, hi: Box<Expr>, inclusive: bool },
    Arena(String, Span),
}

#[derive(Clone, Debug)]
pub enum Pattern {
    Wild(Span),
    Int(i128, Span),
    Bool(bool, Span),
    None(Span),
    /// `Some(x)` / `Some(_)`: (束縛名, 束縛番号, mut) / (name, bind id, mut)
    Some(Option<(String, BindId, bool)>, Span),
    Bind(String, BindId, Span),
    /// `Ok(x)` / `Ok(_)`（Result の成功値。表現は Option の Some と同じ） / the success value of a Result (same layout as Some)
    Ok(Option<(String, BindId, bool)>, Span),
    /// `Err(e)` / `Err(_)`（Result の失敗値。e はコピーできる型） / the failure value of a Result (e is a copyable type)
    Err(Option<(String, BindId, bool)>, Span),
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wild(s) | Pattern::Int(_, s) | Pattern::Bool(_, s) | Pattern::None(s) | Pattern::Some(_, s) | Pattern::Bind(_, _, s) | Pattern::Ok(_, s) | Pattern::Err(_, s) => *s,
        }
    }
    /// 値を束縛するか（束縛名と番号） / the binding, if any (name and bind id)
    pub fn binding(&self) -> Option<(&str, BindId, bool)> {
        match self {
            Pattern::Some(Some((n, b, m)), _) | Pattern::Ok(Some((n, b, m)), _) | Pattern::Err(Some((n, b, m)), _) => Some((n.as_str(), *b, *m)),
            Pattern::Bind(n, b, _) => Some((n.as_str(), *b, false)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pat: Pattern,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(u128, Option<IntTy>),
    /// 浮動小数点のリテラル（`_` を除いた 10 進の文字列・接尾辞） / a floating-point literal (decimal text without `_`, suffix)
    Float(String, Option<FloatTy>),
    Bool(bool),
    Str(String),
    Unit,
    /// 変数・static・`None` / a variable, a static, `None`
    Path(String),
    /// `@name`（アリーナ・MMIO リージョン） / `@name` (an arena or an MMIO region)
    ArenaRef(String),
    Call { callee: String, callee_span: Span, args: Vec<Expr>, turbofish: Option<TypeExpr> },
    /// 式を呼ぶ: `(h.on_click)(&mut b)` / `ops[i](x)` / `make()(x)`（呼ばれる式は関数ポインタ `fn(..)`）
    /// calling an expression: `(h.on_click)(&mut b)` / `ops[i](x)` / `make()(x)` (the callee is a function pointer `fn(..)`)
    CallIndirect { callee: Box<Expr>, args: Vec<Expr> },
    MethodCall { recv: Box<Expr>, method: String, method_span: Span, args: Vec<Expr> },
    Field(Box<Expr>, String, Span),
    Index(Box<Expr>, Box<Expr>),
    StructLit { name: String, name_span: Span, fields: Vec<(String, Span, Expr)> },
    ArrayLit(Vec<Expr>),
    ArrayRepeat(Box<Expr>, Box<Expr>),
    Own(Box<Expr>),
    /// `own_alloc(Block)`: 型名を引数に取る組み込み（ジェネリクスなし）。0 で初期化した own Block を返す
    /// `own_alloc(Block)`: a builtin taking a type name (no generics); returns a zero-initialized own Block
    OwnAlloc(TypeExpr),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Cast(Box<Expr>, TypeExpr),
    Ref { mutable: bool, inner: Box<Expr> },
    Assign { op: Option<BinOp>, lhs: Box<Expr>, rhs: Box<Expr> },
    If { cond: Box<Expr>, then: Block, els: Option<Box<Expr>> },
    IfLet { pat: Pattern, scrut: Box<Expr>, then: Block, els: Option<Box<Expr>> },
    Match { scrut: Box<Expr>, arms: Vec<Arm> },
    While { cond: Box<Expr>, body: Block },
    Loop { body: Block },
    For { var: String, var_span: Span, bind: BindId, iter: ForIter, body: Block },
    Break,
    Continue,
    Return(Option<Box<Expr>>),
    Block(Block),
    VolatileBypass(Block),
    /// `critical { .. }`: 閉じた区間（割り込まれない） / a closed section (not interrupted)
    Critical(Block),
    /// `atomic { 場所 op= 値; }`: 1 つのスロットの整数フィールドへの読み書き 1 回（RMW）を不可分にする
    /// `atomic { place op= value; }`: one read-modify-write of an integer field of one slot, made indivisible
    Atomic(Block),
    /// `rcu_read { .. }`: プリエンプション禁止の読み取り専用区間（&T の読み出しだけ。書き込み・解放は F3202）
    /// a read-only section with preemption disabled (&T reads only; writes and frees are F3202)
    RcuRead(Block),
    /// `catch_fault { .. }`: 区間内のラッチ異常とユーザーメモリ例外を Result<T, FaultError> として受ける（大域脱出なし）
    /// catches latched faults and user-memory faults inside as Result<T, FaultError> (no non-local exit)
    CatchFault(Block),
    /// 型名を第一引数に取る組み込み: `bind_mmio(Regs, 番地)`・`dma_alloc(T)`・`complete_dma(T, iova)`（ジェネリクスなし）
    /// builtins taking a type name first: `bind_mmio(Regs, addr)`, `dma_alloc(T)`, `complete_dma(T, iova)` (no generics)
    TypeCall { name: String, name_span: Span, ty: TypeExpr, args: Vec<Expr> },
    /// 0.1 専用（1.0 エンジンは F2900 で報告） / 0.1 only (reported as F2900 by the 1.0 engine)
    Unsupported(&'static str),
}

impl ExprKind {
    /// `;` 無しで文として書ける式（ブロックで終わる式） / expressions usable as statements without `;`
    pub fn is_block_like(&self) -> bool {
        matches!(
            self,
            ExprKind::If { .. }
                | ExprKind::IfLet { .. }
                | ExprKind::Match { .. }
                | ExprKind::While { .. }
                | ExprKind::Loop { .. }
                | ExprKind::For { .. }
                | ExprKind::Block(_)
                | ExprKind::VolatileBypass(_)
                | ExprKind::Critical(_)
                | ExprKind::Atomic(_)
                | ExprKind::RcuRead(_)
                | ExprKind::CatchFault(_)
        )
    }
}
