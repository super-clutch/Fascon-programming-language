//! 意味論上の型と、プログラム全体の宣言表 / semantic types and program-wide declaration tables

use crate::ast::BindId;
use crate::lexer::{FloatTy, IntTy};
use crate::span::Span;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Int(IntTy),
    /// `f32` / `f64`（IEEE-754）。演算は C の float / double そのもの（縮約なし）、整数への `as` は飽和
    /// `f32` / `f64` (IEEE-754); arithmetic is C float / double as is (no contraction), `as` to integers saturates
    Float(FloatTy),
    /// まだ決まっていない浮動小数点のリテラルの型（最後に f64 へ既定化） / undecided float-literal type (defaults to f64)
    FloatVar(u32),
    /// まだ決まっていない整数リテラルの型（最後に i32 へ既定化） / undecided integer-literal type (defaults to i32)
    IntVar(u32),
    /// まだ決まっていない型（`let x;` / `let x = None;` など。後の代入・比較・呼び出しから推論する）
    /// a type not known yet (`let x;` / `let x = None;` ...; inferred from later assignments, comparisons and calls)
    Var(u32),
    Bool,
    Unit,
    Str,
    /// `[u8]`: 長さ付きの窓。`&[u8]` / `&mut [u8]` の中にだけ現れる（第二級。保存できない）
    /// a window with a length; appears only inside `&[u8]` / `&mut [u8]` (second-class, never stored)
    Slice(Box<Ty>),
    /// `Dma<@b>`: dma_arena のスロットの移動専用のハンドル（lend で消え、reclaim で戻る）
    /// the move-only handle of a dma_arena slot (consumed by lend, returned by reclaim)
    Dma(usize),
    /// `GpuBuf<@g>`: gpu_arena のスロットの移動専用のハンドル（dispatch で消え、reclaim で戻る）
    /// the move-only handle of a gpu_arena slot (consumed by dispatch, returned by reclaim)
    GpuBuf(usize),
    /// `GpuFence<@g>`: dispatch の完了を待つ引換券（コピーできる。reclaim が所有権を返すのは 1 回だけ）
    /// a ticket for a dispatch's completion (copyable; reclaim returns ownership only once)
    GpuFence(usize),
    Never,
    Struct(usize),
    Option(Box<Ty>),
    Own(Box<Ty>),
    /// `Id<@a>`: アリーナ a に着色されたハンドル / a handle coloured by arena a
    Id(usize),
    Array(Box<Ty>, u64),
    /// 第二級参照（引数とルックアップ束縛だけ） / second-class reference (parameters and lookup bindings only)
    Ref(bool, Box<Ty>),
    /// 関数ポインタ `fn(引数..) -> 戻り値`（引数は参照でもよい。値としてコピーでき、データは持たない）
    /// a function pointer `fn(params..) -> ret` (parameters may be references; it is a copyable value that holds no data)
    Fn(Vec<Ty>, Box<Ty>),
    /// `Result<T, E>`: 成功値 T か失敗値 E（E はコピーできる型）。C では {is_some, value, err} の構造体（Option と同じ並び + err）
    /// a success value T or a failure E (E is copyable); in C an {is_some, value, err} struct (Option's layout + err)
    Result(Box<Ty>, Box<Ty>),
    /// `FaultError`: ラッチ異常・ユーザーメモリ例外・errno 風の失敗の符号（コピーできる。C では int64_t）
    /// the code of a latched fault, a user-memory fault or an errno-style failure (copyable; int64_t in C)
    Fault,
    /// `user [u8; N]` / `user Str`: ユーザー空間のバッファ（#[syscall] の引数だけ。copy_from_user で 1 回だけ写す）
    /// a user-space buffer (#[syscall] parameters only; copied once with copy_from_user)
    User(Box<Ty>),
    /// `dma own T`: 4 KiB 境界の独立した物理ページに置いた所有値 / an owned value on its own 4 KiB-aligned physical pages
    DmaOwn(Box<Ty>),
    /// `mmio T`: bind_mmio で結んだレジスタ構造体（構造体の番号）のハンドル / the handle of a bound register struct (struct id)
    MmioBind(usize),
    Err,
}

impl Ty {
    pub fn is_err(&self) -> bool {
        matches!(self, Ty::Err)
    }
    pub fn is_int(&self) -> bool {
        matches!(self, Ty::Int(_) | Ty::IntVar(_))
    }
    pub fn is_float(&self) -> bool {
        matches!(self, Ty::Float(_) | Ty::FloatVar(_))
    }
    /// 推論しきれていない部分を含むか（整数の型変数は含めない） / whether a still-unknown part remains (integer variables excluded)
    pub fn has_var(&self) -> bool {
        match self {
            Ty::Var(_) => true,
            Ty::Option(i) | Ty::Own(i) | Ty::Array(i, _) | Ty::Ref(_, i) | Ty::User(i) | Ty::DmaOwn(i) => i.has_var(),
            Ty::Fn(ps, r) => ps.iter().any(|p| p.has_var()) || r.has_var(),
            Ty::Result(a, b) => a.has_var() || b.has_var(),
            _ => false,
        }
    }
    /// 参照を（Option・配列・関数ポインタの引数を除いて）値として含むか / whether a reference is held as a value
    /// (inside Options, arrays or own; the parameters of a function pointer type do not count)
    pub fn holds_ref(&self) -> bool {
        match self {
            Ty::Ref(..) => true,
            Ty::Option(i) | Ty::Own(i) | Ty::Array(i, _) | Ty::Result(i, _) | Ty::DmaOwn(i) => i.holds_ref(),
            _ => false,
        }
    }
    /// Option / Result の中身（成功値） / the payload of an Option or the success value of a Result
    pub fn opt_inner(&self) -> Option<&Ty> {
        match self {
            Ty::Option(i) | Ty::Result(i, _) => Some(i),
            _ => None,
        }
    }
    pub fn strip_ref(&self) -> &Ty {
        match self {
            Ty::Ref(_, t) => t,
            t => t,
        }
    }
    /// own / 参照を辿った中身 / the value behind own / references
    pub fn deref_all(&self) -> &Ty {
        match self {
            Ty::Ref(_, t) => t.deref_all(),
            Ty::Own(t) | Ty::DmaOwn(t) => t.deref_all(),
            t => t,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct StructInfo {
    pub name: String,
    pub fields: Vec<FieldInfo>,
    pub span: Span,
    /// own を（推移的に）含む = 解放処理（drop glue）が要る / contains own (transitively) = needs drop glue
    pub needs_drop: bool,
    /// `#[repr(align(N))]` の N（0 = 指定なし） / N of `#[repr(align(N))]` (0 = none)
    pub align: u32,
    /// ワイヤ構造体（`#[wire(le)]` = Some(false)、`#[wire(be)]` = Some(true)）: 整列 1・詰め物なし・バイト順固定の POD
    /// a wire struct (`#[wire(le)]` = Some(false), `#[wire(be)]` = Some(true)): alignment 1, no padding, fixed byte order, POD
    pub wire: Option<bool>,
    /// `#[secret]`: 複製・出力・分岐を禁止し、破棄のすべての経路で揮発性の 0 埋め + バリア
    /// `#[secret]`: no copies, output or branches; volatile zeroing + a barrier on every drop path
    pub secret: bool,
    /// `#[on_drop(関数)]`: 破棄のときに最初に呼ぶ関数 / `#[on_drop(fn)]`: the function called first when the value is dropped
    pub on_drop: Option<String>,
    /// `#[stable_abi]`: レイアウト固定（宣言順・C の自然な整列・列を作らない） / fixed layout (declaration order, natural C
    /// alignment, no columns)
    pub stable_abi: bool,
    /// フィールドごとの `#[no_column]` / per-field `#[no_column]`
    pub no_column: Vec<bool>,
}

impl StructInfo {
    pub fn field(&self, name: &str) -> Option<(usize, &FieldInfo)> {
        self.fields.iter().enumerate().find(|(_, f)| f.name == name)
    }
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: String,
    pub bind: BindId,
    pub ty: Ty,
    pub mutable: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Default)]
pub struct Effects {
    /// スロットの中身を読む / reads slot contents
    pub reads: Vec<usize>,
    /// スロットの中身を書く（get_mut・代入・&mut） / writes slot contents
    pub writes: Vec<usize>,
    /// スロットを解放する（free / remove）= 生存証明を消す / frees slots = kills liveness proofs
    pub frees: Vec<usize>,
    /// スロットを確保する（alloc）= 生存ビットマップを変える / allocates slots = changes the live bitmap
    pub allocs: Vec<usize>,
}

impl Effects {
    pub fn add(v: &mut Vec<usize>, a: usize) -> bool {
        if !v.contains(&a) {
            v.push(a);
            true
        } else {
            false
        }
    }
    pub fn touches(&self, a: usize) -> bool {
        self.reads.contains(&a) || self.writes.contains(&a) || self.frees.contains(&a)
    }
    pub fn mutates(&self, a: usize) -> bool {
        self.writes.contains(&a) || self.frees.contains(&a)
    }
}

#[derive(Clone, Debug)]
pub struct FnInfo {
    pub name: String,
    pub params: Vec<ParamInfo>,
    pub ret: Ty,
    pub item: usize,
    pub span: Span,
    pub is_main: bool,
    pub effects: Effects,
    pub callees: Vec<usize>,
    /// 割り込みハンドラなら INTID / the INTID of an interrupt handler
    pub irq: Option<u32>,
    /// システムコールなら番号 / the number of a system call
    pub syscall: Option<u32>,
    /// 本体の無いシステムコール（ユーザー側: svc の呼び出しになる） / a bodiless system call (user side: becomes an svc)
    pub stub: bool,
    /// `trap user_fault(..)`
    pub trap: bool,
    /// 属性の形 `#[syscall(N)] fn`（Result を返し、`user T`・bool・Id の引数を受ける） / the attribute form `#[syscall(N)] fn`
    /// (returns a Result and takes `user T`, bool and Id parameters)
    pub syscall_v2: bool,
    /// 属性の形 `#[trap] fn user_fault(pid: Id<p>, addr: u64, is_write: bool)` / the attribute form of a trap
    pub trap_v2: bool,
    /// `extern "C"`: Some(true) = C へ公開（本体あり）、Some(false) = C から取り込む / Some(true) = exported to C (has a body),
    /// Some(false) = imported from C
    pub extern_c: Option<bool>,
    /// `#[asm(..)] fn`: 安全インラインアセンブリの境界 / the safe inline-assembly boundary
    pub asm: Option<AsmSpec>,
    /// Sleep 効果（眠りうる: schedule()・sleep_ticks()・`#[sleep]` を推移的に呼ぶ） / the Sleep effect (may sleep: calls
    /// schedule(), sleep_ticks() or a `#[sleep]` function, transitively)
    pub sleeps: bool,
}

/// `#[asm(aarch64 = "..", x86_64 = "..", host = "..", effects = "..", clobbers = "..")]`
#[derive(Clone, Debug, Default)]
pub struct AsmSpec {
    pub aarch64: Option<String>,
    pub x86_64: Option<String>,
    /// PC 実行用の C の式（引数は a0, a1, ..） / a C expression for PC runs (parameters are a0, a1, ..)
    pub host: Option<String>,
    /// none | sleep | barrier | irq | mmio
    pub effects: String,
    pub clobbers: String,
}

#[derive(Clone, Debug)]
pub struct StaticInfo {
    pub name: String,
    pub ty: Ty,
    pub item: usize,
    pub span: Span,
    /// 整数の static は定数評価した値 / the constant value of an integer static
    pub int_value: Option<i128>,
}

#[derive(Clone, Debug)]
pub struct ArenaInfo {
    pub name: String,
    pub elem: Ty,
    pub cap: u32,
    pub span: Span,
    /// バンプアリーナ（呼び出しリージョンごとに O(1) で一括解放。Id は 64 ビット = [世代 64-K | 添字 K]）
    /// a bump arena (bulk-freed in O(1) per call region; its Ids are 64 bits = [generation 64-K | index K])
    pub bump: bool,
    /// task_arena のスタックの大きさ（バイト） / the stack size of a task_arena (bytes)
    pub task_stack: Option<u32>,
    /// dma_arena か / whether this is a dma_arena
    pub dma: bool,
    /// `shared arena`: 全コアで 1 つの実体 / one instance for every core
    pub shared: bool,
    /// `gc_arena`: fascon-gc が回収する / reclaimed by fascon-gc
    pub gc: bool,
    /// `gpu_arena`: アクセラレータへ貸し出すピン留めの表（ハンドルは移動専用の `GpuBuf<@g>`） / a pinned table lent to
    /// an accelerator (handles are move-only `GpuBuf<@g>`)
    pub gpu: bool,
}

/// `chan @ch: own T[CAP];`
#[derive(Clone, Debug)]
pub struct ChanInfo {
    pub name: String,
    pub elem: Ty,
    pub cap: u64,
    pub span: Span,
}

/// `user_mem @u for @t: バイト数 at 仮想番地;`
#[derive(Clone, Debug)]
pub struct UserMemInfo {
    pub name: String,
    pub task: usize,
    pub size: u64,
    pub at: u64,
    pub span: Span,
}

/// `dma @r: [T; N];`
#[derive(Clone, Debug)]
pub struct DmaInfo {
    pub name: String,
    pub elem: Ty,
    pub len: u64,
    pub span: Span,
}

/// `port_io 名前[: 型] at_port 番号;` — x86 の I/O ポート空間（MMIO の物理アドレス空間とは別）
/// `port_io name[: type] at_port number;` — the x86 I/O port space (separate from the MMIO physical address space)
#[derive(Clone, Debug)]
pub struct PortInfo {
    pub name: String,
    /// 1 回の in / out の幅（u8 = `in al, dx` / `out dx, al`） / the width of one in / out (u8 = `in al, dx` / `out dx, al`)
    pub elem: IntTy,
    /// 配列のポート（`[u8; 8]` = 連続する 8 個のレジスタ）なら個数 / the count for an array of ports (`[u8; 8]` = 8 consecutive registers)
    pub len: Option<u64>,
    pub port: u64,
    /// `#[mmio_map(番地)]`: ポート I/O の無いターゲット（AArch64）でこの番地の MMIO に写す / remap to MMIO at this address on
    /// targets without port I/O (AArch64)
    pub mmio_map: Option<u64>,
    pub span: Span,
}

impl PortInfo {
    pub fn width_bytes(&self) -> u64 {
        (self.elem.bits(64) / 8) as u64
    }
}

#[derive(Clone, Debug)]
pub struct MmioInfo {
    pub name: String,
    pub elem: IntTy,
    /// 外側から内側への各次元の長さ（`[[u32; 800]; 600]` → [600, 800]）
    /// lengths from the outermost dimension inward (`[[u32; 800]; 600]` → [600, 800])
    pub dims: Vec<u64>,
    pub addr: u64,
    pub framebuffer: bool,
    pub span: Span,
}

impl MmioInfo {
    pub fn elem_bytes(&self) -> u64 {
        (self.elem.bits(64) / 8) as u64
    }
    pub fn total_elems(&self) -> u64 {
        self.dims.iter().product()
    }
    pub fn size_bytes(&self) -> u64 {
        self.total_elems() * self.elem_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionRef {
    /// 表のアリーナとバンプアリーナ（ArenaInfo::bump で区別） / table arenas and bump arenas (ArenaInfo::bump)
    Arena(usize),
    Mmio(usize),
    Port(usize),
    /// `dma @r` / a `dma` region
    Dma(usize),
    /// `user_mem @u` / per-task user memory
    UserMem(usize),
    /// `chan @ch` / a channel between cores
    Chan(usize),
}

#[derive(Clone, Debug, Default)]
pub struct Tables {
    pub structs: Vec<StructInfo>,
    pub struct_by_name: HashMap<String, usize>,
    pub fns: Vec<FnInfo>,
    pub fn_by_name: HashMap<String, usize>,
    pub statics: Vec<StaticInfo>,
    pub static_by_name: HashMap<String, usize>,
    pub arenas: Vec<ArenaInfo>,
    pub mmios: Vec<MmioInfo>,
    pub ports: Vec<PortInfo>,
    pub dmas: Vec<DmaInfo>,
    pub umems: Vec<UserMemInfo>,
    pub chans: Vec<ChanInfo>,
    pub regions: HashMap<String, RegionRef>,
    pub ptr_bits: u32,
    /// `--target generic-os`: 表のアリーナの Id も 64 ビット（上位 32 = 世代・下位 32 = 添字、0 = None）
    /// `--target generic-os`: table-arena Ids are 64 bits too (upper 32 = generation, lower 32 = index, 0 = None)
    pub id64: bool,
}

impl Tables {
    pub fn display(&self, t: &Ty) -> String {
        match t {
            Ty::Int(i) => i.name().to_string(),
            Ty::IntVar(_) => "{integer}".to_string(),
            Ty::Float(f) => f.name().to_string(),
            Ty::FloatVar(_) => "{float}".to_string(),
            Ty::Var(_) => "_".to_string(),
            Ty::Bool => "bool".to_string(),
            Ty::Unit => "()".to_string(),
            Ty::Str => "Str".to_string(),
            Ty::Slice(e) => format!("[{}]", self.display(e)),
            Ty::Dma(a) => format!("Dma<{}>", self.arenas[*a].name),
            Ty::GpuBuf(a) => format!("GpuBuf<{}>", self.arenas[*a].name),
            Ty::GpuFence(a) => format!("GpuFence<{}>", self.arenas[*a].name),
            Ty::Never => "!".to_string(),
            Ty::Struct(s) => self.structs[*s].name.clone(),
            Ty::Option(i) => format!("Option<{}>", self.display(i)),
            Ty::Own(i) => format!("own {}", self.display(i)),
            Ty::Id(a) => format!("Id<{}>", self.arenas[*a].name),
            Ty::Array(e, n) => format!("[{}; {}]", self.display(e), n),
            Ty::Ref(m, i) => format!("&{}{}", if *m { "mut " } else { "" }, self.display(i)),
            Ty::Fn(ps, r) => {
                let p: Vec<String> = ps.iter().map(|x| self.display(x)).collect();
                match &**r {
                    Ty::Unit => format!("fn({})", p.join(", ")),
                    other => format!("fn({}) -> {}", p.join(", "), self.display(other)),
                }
            }
            Ty::Result(a, b) => format!("Result<{}, {}>", self.display(a), self.display(b)),
            Ty::Fault => "FaultError".to_string(),
            Ty::User(i) => format!("user {}", self.display(i)),
            Ty::DmaOwn(i) => format!("dma own {}", self.display(i)),
            Ty::MmioBind(s) => format!("mmio {}", self.structs[*s].name),
            Ty::Err => "{error}".to_string(),
        }
    }

    /// 暗黙にコピーできる型（整数・bool・Str・Id と、それだけの Option / 配列）
    /// implicitly copyable types (integers, bool, Str, Id and Option / arrays of them)
    pub fn is_copy(&self, t: &Ty) -> bool {
        match t {
            Ty::Int(_) | Ty::IntVar(_) | Ty::Var(_) | Ty::Bool | Ty::Unit | Ty::Str | Ty::Never | Ty::Id(_) | Ty::Err => true,
            Ty::Float(_) | Ty::FloatVar(_) | Ty::GpuFence(_) => true,
            // 関数ポインタはコードの番地だけ（データを所有しない） / a function pointer is just a code address (owns no data)
            Ty::Fn(..) => true,
            Ty::Option(i) => self.is_copy(i),
            Ty::Array(e, _) => self.is_copy(e),
            Ty::Ref(m, _) => !*m,
            // FaultError と MMIO のハンドルは値（データを所有しない） / FaultError and MMIO handles are plain values (own no data)
            Ty::Fault | Ty::MmioBind(_) => true,
            Ty::Result(a, b) => self.is_copy(a) && self.is_copy(b),
            Ty::Struct(_) | Ty::Own(_) | Ty::Slice(_) | Ty::Dma(_) | Ty::GpuBuf(_) | Ty::User(_) | Ty::DmaOwn(_) => false,
        }
    }

    /// `own T` の中身として置ける型か（参照・()・own の own・関数ポインタは置けない）
    /// whether a type can be the contents of an `own T` (references, (), own of own and function pointers cannot)
    pub fn ownable(t: &Ty) -> bool {
        !matches!(t, Ty::Ref(..) | Ty::Unit | Ty::Never | Ty::Own(_) | Ty::Fn(..) | Ty::User(_) | Ty::DmaOwn(_) | Ty::MmioBind(_))
    }

    /// 解放処理（own の解放）が要る型 / types whose values must run drop glue (they own boxes)
    /// `#[repr(align(N))]` で上げた整列（値として埋め込んだ構造体・配列・Option を推移的に。0 = 指定なし）
    /// the alignment raised by `#[repr(align(N))]`, transitively through inline structs, arrays and Options (0 = none)
    pub fn type_align_attr(&self, t: &Ty) -> u32 {
        self.type_align_attr_d(t, 0)
    }
    fn type_align_attr_d(&self, t: &Ty, depth: u32) -> u32 {
        if depth > 64 {
            return 0;
        }
        match t {
            Ty::Struct(s) => {
                let info = &self.structs[*s];
                info.fields.iter().map(|f| self.type_align_attr_d(&f.ty, depth + 1)).fold(info.align, |a, b| a.max(b))
            }
            Ty::Array(e, _) => self.type_align_attr_d(e, depth + 1),
            Ty::Option(i) => match &**i {
                Ty::Own(_) | Ty::Id(_) | Ty::Str | Ty::Ref(..) => 0,
                other => self.type_align_attr_d(other, depth + 1),
            },
            _ => 0,
        }
    }

    pub fn needs_drop(&self, t: &Ty) -> bool {
        match t {
            Ty::Own(_) | Ty::Dma(_) | Ty::GpuBuf(_) | Ty::DmaOwn(_) => true,
            Ty::Struct(s) => self.structs[*s].needs_drop,
            Ty::Option(i) | Ty::Result(i, _) => self.needs_drop(i),
            Ty::Array(e, _) => self.needs_drop(e),
            _ => false,
        }
    }

    pub fn arena_of_name(&self, n: &str) -> Option<usize> {
        match self.regions.get(n) {
            Some(RegionRef::Arena(a)) => Some(*a),
            _ => None,
        }
    }

    /// 型が（値として推移的に）バンプアリーナの Id を含むなら、そのアリーナ / the bump arenas whose Ids a type contains (by value, transitively)
    pub fn bump_ids_in(&self, t: &Ty, out: &mut Vec<usize>) {
        self.bump_ids_in_d(t, out, 0)
    }
    fn bump_ids_in_d(&self, t: &Ty, out: &mut Vec<usize>, depth: u32) {
        if depth > 64 {
            return;
        }
        match t {
            Ty::Id(a) if self.arenas[*a].bump => {
                if !out.contains(a) {
                    out.push(*a);
                }
            }
            Ty::Option(i) | Ty::Array(i, _) | Ty::Own(i) | Ty::Result(i, _) => self.bump_ids_in_d(i, out, depth + 1),
            Ty::Struct(s) => {
                for f in &self.structs[*s].fields {
                    self.bump_ids_in_d(&f.ty, out, depth + 1);
                }
            }
            _ => {}
        }
    }

    /// Id の幅（表のアリーナは 32 ビット、バンプアリーナは 64 ビット） / the width of an Id (32 bits for table arenas, 64 for bump arenas)
    pub fn id_bits(&self, a: usize) -> u32 {
        if self.arenas[a].bump || self.id64 {
            64
        } else {
            32
        }
    }
}
