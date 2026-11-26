//! 構文解析（再帰下降・演算子優先順位法） / parser (recursive descent with precedence climbing)

use crate::ast::*;
use crate::diag::Diag;
use crate::lexer::{Tok, Token};
use crate::prescan::SymTab;
use crate::span::Span;

pub struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
    pub diags: Vec<Diag>,
    next_expr: u32,
    next_bind: u32,
    /// `if` / `while` / `match` の条件部では構造体リテラルを書けない（Rust と同じ）
    /// struct literals are not allowed in `if` / `while` / `match` heads (same as Rust)
    no_struct: bool,
    /// `>>` を 2 つの `>` として読んだ残り / the second half of a split `>>`
    pending_gt: bool,
    /// 1 周目で集めたトップレベルの名前（`Id<名前>` の名前がアリーナかどうかを、宣言の位置に関係なく決める）
    /// the top-level names from pass 1 (decide whether the name in `Id<name>` is an arena, wherever it is declared)
    syms: &'a SymTab,
    /// 型の別名の本体を読んでいる間のアリーナ引数 / arena parameters while the body of a type alias is parsed
    alias_params: Vec<String>,
    /// リフレーミング（Rust 構文）モード: `while let` を `loop` + `if let` に脱糖衣する
    /// reframing (Rust syntax) mode: `while let` is desugared into `loop` + `if let`
    pub reframe: bool,
    /// 前置 `*` の付いた式 / expressions written with a prefix `*`
    derefs: std::collections::HashSet<ExprId>,
    par_fors: Vec<ExprId>,
    /// `?` から作った match の式（式の番号, 位置） / the match expressions made from `?` (expression id, span)
    pub question_marks: Vec<(ExprId, Span)>,
    /// `#[fast_math]` を付けたループ / loops written with `#[fast_math]`
    fast_math_loops: std::collections::HashMap<ExprId, Span>,
}

type PResult<T> = Result<T, ()>;

impl<'a> Parser<'a> {
    pub fn new(toks: &'a [Token], syms: &'a SymTab) -> Parser<'a> {
        Parser { toks, pos: 0, diags: Vec::new(), next_expr: 0, next_bind: 0, no_struct: false, pending_gt: false, syms, alias_params: Vec::new(), reframe: false, derefs: std::collections::HashSet::new(), par_fors: Vec::new(), question_marks: Vec::new(), fast_math_loops: std::collections::HashMap::new() }
    }

    /// リージョンの名前（`名前` か、古い書き方の `@名前`）。`@` は要らない（書いても同じ意味）
    /// a region name (`name`, or the legacy `@name`); the `@` is never needed (it means the same thing when written)
    fn region_name(&mut self, ja: &str, en: &str) -> PResult<(String, Span)> {
        match self.peek().clone() {
            Tok::Ident(n) | Tok::ArenaName(n) => {
                let sp = self.bump().span;
                Ok((n, sp))
            }
            _ => {
                self.err_here(ja, en);
                Err(())
            }
        }
    }

    /// 型引数の位置の名前がアリーナ（Id を持つリージョン）か / is a name in a type-argument position an arena (a region with Ids)?
    fn arena_arg_name(&self, n: &str) -> bool {
        self.alias_params.iter().any(|p| p == n) || self.syms.has_ids(n)
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn peek_at(&self, k: usize) -> &Tok {
        let i = (self.pos + k).min(self.toks.len() - 1);
        &self.toks[i].tok
    }
    fn span(&self) -> Span {
        self.toks[self.pos].span
    }
    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            self.toks[0].span
        } else {
            self.toks[self.pos - 1].span
        }
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == t {
            self.bump();
            true
        } else {
            false
        }
    }
    fn err_here(&mut self, ja: &str, en: &str) {
        let sp = self.span();
        let found = self.peek().describe();
        self.diags.push(Diag::error(
            "F0001",
            sp,
            format!("構文エラー: {}（見つかったもの: {}）", ja, found),
            format!("syntax error: {} (found {})", en, found),
        ));
    }
    fn expect(&mut self, t: Tok, ja: &str, en: &str) -> PResult<Span> {
        if self.peek() == &t {
            Ok(self.bump().span)
        } else {
            self.err_here(ja, en);
            Err(())
        }
    }
    fn ident(&mut self, ja: &str, en: &str) -> PResult<(String, Span)> {
        if let Tok::Ident(s) = self.peek().clone() {
            let sp = self.bump().span;
            Ok((s, sp))
        } else {
            self.err_here(ja, en);
            Err(())
        }
    }
    fn new_expr(&mut self, kind: ExprKind, span: Span) -> Expr {
        let id = self.next_expr;
        self.next_expr += 1;
        Expr { id, kind, span }
    }
    fn new_bind(&mut self) -> BindId {
        let b = self.next_bind;
        self.next_bind += 1;
        b
    }

    /// エラー後の回復: 次の項目の先頭まで読み飛ばす / recovery: skip to the start of the next item
    fn recover_item(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::LBrace => depth += 1,
                Tok::RBrace => {
                    depth -= 1;
                    if depth <= 0 {
                        self.bump();
                        if depth <= 0 {
                            return;
                        }
                        continue;
                    }
                }
                Tok::Fn | Tok::Struct | Tok::Static | Tok::ArenaKw | Tok::BumpArena | Tok::PortIo | Tok::Mmio | Tok::Type | Tok::Hash if depth == 0 => return,
                Tok::Semi if depth == 0 => {
                    self.bump();
                    return;
                }
                _ => {}
            }
            self.bump();
        }
    }

    pub fn parse_program(&mut self) -> Program {
        let mut items = Vec::new();
        let mut v2_attr = false;
        let mut reframing_attr = false;
        // #![fascon(1)] / #![reframing(rust)] などの内部属性 / inner attributes such as #![fascon(1)] / #![reframing(rust)]
        while self.peek() == &Tok::HashBang {
            self.bump();
            let ok = self.eat(&Tok::LBracket);
            let mut depth = 1;
            let mut first = true;
            while ok && depth > 0 && self.peek() != &Tok::Eof {
                match self.bump().tok {
                    Tok::LBracket => depth += 1,
                    Tok::RBracket => depth -= 1,
                    Tok::Ident(s) if first && s == "fascon" => v2_attr = true,
                    Tok::Ident(s) if first && s == "reframing" => reframing_attr = true,
                    _ => {}
                }
                first = false;
            }
        }
        while self.peek() != &Tok::Eof {
            match self.parse_item() {
                Ok(it) => items.push(it),
                Err(()) => self.recover_item(),
            }
        }
        let question_marks = self.question_marks.iter().map(|(e, _)| *e).collect();
        Program { items, n_exprs: self.next_expr, n_binds: self.next_bind, v2_attr, reframing_attr, par_fors: std::mem::take(&mut self.par_fors), derefs: std::mem::take(&mut self.derefs), question_marks, fast_math_loops: std::mem::take(&mut self.fast_math_loops) }
    }

    fn parse_attrs(&mut self) -> PResult<Vec<Attr>> {
        let mut attrs = Vec::new();
        while self.peek() == &Tok::Hash {
            let start = self.bump().span;
            self.expect(Tok::LBracket, "`#` の後には `[` が必要です", "`[` expected after `#`")?;
            let (name, _) = self.ident("属性名が必要です", "attribute name expected")?;
            // `#[repr(align(N))]`: N を取り出す（形が違えば F2031） / extract N (F2031 when the shape is wrong)
            let mut repr_align = None;
            let mut int_arg = None;
            let mut ident_arg = None;
            let mut kv: Vec<(String, AttrVal, Span)> = Vec::new();
            if name == "repr" && self.peek() == &Tok::LParen {
                let open = self.bump().span;
                let mut ok = false;
                if let Tok::Ident(w) = self.peek().clone() {
                    if w == "align" {
                        self.bump();
                        if self.eat(&Tok::LParen) {
                            if let Tok::Int(v, _) = self.peek().clone() {
                                let vs = self.bump().span;
                                if self.eat(&Tok::RParen) && self.eat(&Tok::RParen) {
                                    repr_align = Some((v, vs));
                                    ok = true;
                                }
                            }
                        }
                    }
                }
                if !ok {
                    self.diags.push(Diag::error(
                        "F2031",
                        open,
                        "使える repr は `#[repr(align(N))]`（N は 1〜4096 の 2 の累乗）だけです",
                        "the only supported repr is `#[repr(align(N))]` (N a power of two from 1 to 4096)",
                    ));
                    // 閉じ括弧まで読み飛ばす / skip to the closing bracket
                    while !matches!(self.peek(), Tok::RBracket | Tok::Eof) {
                        self.bump();
                    }
                }
            } else if self.peek() == &Tok::LParen && self.attr_has_kv() {
                // `#[asm(aarch64 = "mrs {r}, cntvct_el0", effects = "none")]` のような `名前 = 値` の並び
                // a list of `key = value` pairs such as `#[asm(aarch64 = "mrs {r}, cntvct_el0", effects = "none")]`
                self.bump();
                while !matches!(self.peek(), Tok::RParen | Tok::Eof) {
                    match (self.peek().clone(), self.peek_at(1).clone()) {
                        (Tok::Ident(k), Tok::Eq) => {
                            let ksp = self.bump().span;
                            self.bump();
                            let v = match self.peek().clone() {
                                Tok::Str(x) => AttrVal::Str(x),
                                Tok::Int(x, _) => AttrVal::Int(x),
                                Tok::Ident(x) => AttrVal::Ident(x),
                                _ => {
                                    self.err_here("属性の値（文字列・整数・名前）が必要です", "an attribute value (a string, an integer or a name) is expected");
                                    return Err(());
                                }
                            };
                            let vsp = self.bump().span;
                            kv.push((k, v, ksp.to(vsp)));
                        }
                        (Tok::Int(v, _), _) => {
                            let vs = self.bump().span;
                            int_arg = Some((v, vs));
                        }
                        (Tok::Ident(w), _) => {
                            self.bump();
                            ident_arg = Some(w);
                        }
                        (Tok::Str(x), _) => {
                            let vs = self.bump().span;
                            kv.push((String::new(), AttrVal::Str(x), vs));
                        }
                        _ => {
                            self.err_here("属性の引数が読めません", "cannot read the attribute arguments");
                            return Err(());
                        }
                    }
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(Tok::RParen, "`)` が必要です", "`)` expected")?;
            } else if self.peek() == &Tok::LParen {
                // それ以外の属性の引数は読み飛ばす（整数 1 個だけなら取っておく: `#[mmio_map(0x0900_0000)]`）
                // skip the arguments of other attributes (a single integer is kept: `#[mmio_map(0x0900_0000)]`)
                if let (Tok::Int(v, _), Tok::RParen) = (self.peek_at(1).clone(), self.peek_at(2).clone()) {
                    self.bump();
                    let vs = self.bump().span;
                    self.bump();
                    int_arg = Some((v, vs));
                } else if let (Tok::Ident(w), Tok::RParen) = (self.peek_at(1).clone(), self.peek_at(2).clone()) {
                    // 名前 1 個だけの引数は取っておく: `#[wire(le)]` / a single identifier argument is kept
                    self.bump();
                    self.bump();
                    self.bump();
                    ident_arg = Some(w);
                } else {
                    let mut depth = 0;
                    loop {
                        match self.bump().tok {
                            Tok::LParen => depth += 1,
                            Tok::RParen => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            Tok::Eof => break,
                            _ => {}
                        }
                    }
                }
            }
            let end = self.expect(Tok::RBracket, "属性は `]` で閉じます", "`]` expected to close the attribute")?;
            attrs.push(Attr { name, span: start.to(end), repr_align, int_arg, ident_arg, kv });
        }
        Ok(attrs)
    }

    /// 属性の括弧の中に `名前 = 値` か文字列があるか（`(` の位置で呼ぶ） / whether the attribute parentheses hold `key = value`
    /// or a string (called at the `(`)
    fn attr_has_kv(&self) -> bool {
        let mut k = 1;
        let mut depth = 0;
        loop {
            match self.peek_at(k) {
                Tok::Eof => return false,
                Tok::LParen => depth += 1,
                Tok::RParen => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                Tok::Eq | Tok::Str(_) if depth == 0 => return true,
                _ => {}
            }
            k += 1;
            if k > 64 {
                return false;
            }
        }
    }

    /// 文脈依存キーワード: 位置 k のトークンが名前 `w` か / contextual keyword: is the token at offset k the identifier `w`?
    fn ident_at(&self, k: usize, w: &str) -> bool {
        matches!(self.peek_at(k), Tok::Ident(s) if s == w)
    }

    /// `irq 名前 at INTID { .. }`: 割り込みハンドラ（引数も戻り値も無い関数として扱う）
    /// an interrupt handler, handled as a function without parameters or a result
    fn parse_irq(&mut self, attrs: Vec<Attr>) -> PResult<Item> {
        let start = self.bump().span; // irq
        let (name, name_span) = self.ident("割り込みハンドラの名前が必要です（`irq 名前 at INTID { .. }`）", "an interrupt handler name is expected (`irq name at INTID { .. }`)")?;
        self.bump(); // at
        let saved = self.no_struct;
        self.no_struct = true;
        let intid = self.parse_expr();
        self.no_struct = saved;
        let intid = intid?;
        let body = self.parse_block()?;
        let span = start.to(body.span);
        Ok(Item::Fn(FnDef { name, name_span, params: Vec::new(), ret: None, body, is_async: false, attrs, span, kind: FnKind::Irq(intid) }))
    }

    fn parse_item(&mut self) -> PResult<Item> {
        let attrs = self.parse_attrs()?;
        // アリーナの `#[numa_node(n)]`（--target hpc で使う。ほかのプロファイルでは C を変えない） / `#[numa_node(n)]` on
        // arenas (used by --target hpc; other profiles leave the C unchanged)
        let numa = attrs.iter().find(|a| a.name == "numa_node").map(|a| (a.int_arg.map_or(u128::MAX, |x| x.0), a.span));
        let it = self.parse_item_inner(attrs)?;
        Ok(match it {
            Item::Arena(mut a) if numa.is_some() => {
                a.numa_node = numa;
                Item::Arena(a)
            }
            other => other,
        })
    }

    fn parse_item_inner(&mut self, attrs: Vec<Attr>) -> PResult<Item> {
        let start = self.span();
        // 文脈依存キーワード（項目の先頭でだけ意味を持つ） / contextual keywords (meaningful only at the start of an item)
        if self.ident_at(0, "critical") && self.peek_at(1) == &Tok::Fn {
            // `critical fn f(..) { B }` = `fn f(..) { critical { B } }`
            self.bump();
            let it = self.parse_fn(attrs, false)?;
            return Ok(match it {
                Item::Fn(mut fd) => {
                    let body = std::mem::replace(&mut fd.body, Block { stmts: Vec::new(), tail: None, span: fd.span });
                    let sp = body.span;
                    let ce = self.new_expr(ExprKind::Critical(body), sp);
                    fd.body = Block { stmts: Vec::new(), tail: Some(Box::new(ce)), span: sp };
                    Item::Fn(fd)
                }
                other => other,
            });
        }
        if self.ident_at(0, "irq") && matches!(self.peek_at(1), Tok::Ident(_)) && self.ident_at(2, "at") {
            return self.parse_irq(attrs);
        }
        if self.ident_at(0, "syscall") && matches!(self.peek_at(1), Tok::Ident(_)) && self.ident_at(2, "at") {
            return self.parse_syscall(attrs);
        }
        if self.ident_at(0, "trap") && matches!(self.peek_at(1), Tok::Ident(_)) && self.peek_at(2) == &Tok::LParen {
            return self.parse_trap(attrs);
        }
        if self.ident_at(0, "user_mem") && matches!(self.peek_at(1), Tok::ArenaName(_) | Tok::Ident(_)) && self.peek_at(2) == &Tok::For {
            return self.parse_user_mem();
        }
        if self.ident_at(0, "dma_arena") {
            return self.parse_arena(false, false, true, false);
        }
        // `dma @r: [T; N];`
        if self.ident_at(0, "dma") && matches!(self.peek_at(1), Tok::ArenaName(_) | Tok::Ident(_)) && self.peek_at(2) == &Tok::Colon {
            let start = self.bump().span;
            let (name, name_span) = self.region_name("`dma` の後にはリージョン名が必要です（例: `dma vq_desc: [VqDesc; 16];`）", "a region name is expected after `dma` (e.g. `dma vq_desc: [VqDesc; 16];`)")?;
            self.expect(Tok::Colon, "`:` が必要です", "`:` expected")?;
            let ty = self.parse_type()?;
            let end = self.expect(Tok::Semi, "`;` が必要です", "`;` expected")?;
            return Ok(Item::Dma(DmaDef { name, name_span, ty, span: start.to(end) }));
        }
        if self.ident_at(0, "task_arena") {
            return self.parse_arena(false, true, false, false);
        }
        // `gpu_arena particles: Particle[N];`: アクセラレータへ貸し出す表（文脈依存キーワード） / a table lent to an
        // accelerator (a contextual keyword)
        if self.ident_at(0, "gpu_arena") && matches!(self.peek_at(1), Tok::ArenaName(_) | Tok::Ident(_)) {
            let mut it = self.parse_arena(false, false, false, false)?;
            if let Item::Arena(a) = &mut it {
                a.gpu = true;
            }
            return Ok(it);
        }
        // `gc_arena nodes: Node[N];`・`shared gc_arena g: T[N];`: fascon-gc が回収するアリーナ / arenas reclaimed by fascon-gc
        if self.ident_at(0, "gc_arena") || (self.ident_at(0, "shared") && self.ident_at(1, "gc_arena")) {
            let shared = self.ident_at(0, "shared");
            if shared {
                self.bump();
            }
            let mut it = self.parse_arena(false, false, false, shared)?;
            if let Item::Arena(a) = &mut it {
                a.gc = true;
            }
            return Ok(it);
        }
        // `shared arena g: T[N];`: 全コア共有のアリーナ / an arena shared by every core
        if self.ident_at(0, "shared") && self.peek_at(1) == &Tok::ArenaKw {
            self.bump();
            return self.parse_arena(false, false, false, true);
        }
        // `chan ch: own T[CAP];`: コア間のチャネル / a channel between cores
        if self.ident_at(0, "chan") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::ArenaName(_)) && self.peek_at(2) == &Tok::Colon {
            let start = self.bump().span;
            let (name, name_span) = self.region_name("チャネルの名前が必要です（例: `chan work: own Packet[16];`）", "a channel name is expected (e.g. `chan work: own Packet[16];`)")?;
            self.bump(); // :
            let elem = self.parse_type_no_array_suffix()?;
            self.expect(Tok::LBracket, "容量 `[N]` が必要です（2 の累乗）", "a capacity `[N]` is expected (a power of two)")?;
            let cap = self.parse_expr()?;
            self.expect(Tok::RBracket, "`]` が必要です", "`]` expected")?;
            let end = self.expect(Tok::Semi, "`;` が必要です", "`;` expected")?;
            return Ok(Item::Chan(ChanDef { name, name_span, elem, cap, span: start.to(end) }));
        }
        // `import net::tcp;`: モジュールの取り込み（字句の段階で展開済み。ここへ来るのは展開しない読み方のとき）
        // a module import (already expanded at the token level; it only reaches here when parsing without expansion)
        if self.ident_at(0, "import") && matches!(self.peek_at(1), Tok::Ident(_)) && matches!(self.peek_at(2), Tok::ColonColon | Tok::Semi) {
            let start = self.bump().span;
            let mut path = Vec::new();
            loop {
                let (seg, _) = self.ident("モジュールの名前が必要です（`import net::tcp;`）", "a module name is expected (`import net::tcp;`)")?;
                path.push(seg);
                if !self.eat(&Tok::ColonColon) {
                    break;
                }
            }
            let end = self.expect(Tok::Semi, "`;` が必要です", "`;` expected")?;
            return Ok(Item::Import(ImportDef { path, span: start.to(end) }));
        }
        // `extern "C" fn 名前(..) -> T;`（C から取り込む）/ `extern "C" fn 名前(..) -> T { .. }`（C へ公開）
        // an import from C / a function exported to C
        if self.peek() == &Tok::Reserved("extern") && matches!(self.peek_at(1), Tok::Str(_)) && self.peek_at(2) == &Tok::Fn {
            self.bump();
            if let Tok::Str(abi) = self.peek().clone() {
                if abi != "C" {
                    self.err_here("使える ABI は `extern \"C\"` だけです", "the only supported ABI is `extern \"C\"`");
                }
            }
            self.bump();
            return self.parse_fn_ext(attrs, FnKind::ExternC { body: false }, true);
        }
        if self.peek() == &Tok::Fn {
            // `#[syscall(N)] fn ..`・`#[trap] fn ..`・`#[asm(..)] fn ..;` / attribute forms of system calls, traps and asm
            if let Some(a) = attrs.iter().find(|a| a.name == "syscall") {
                let (n, nsp) = a.int_arg.unwrap_or((u128::MAX, a.span));
                let nr = self.new_expr(ExprKind::Int(n, None), nsp);
                return self.parse_fn_ext(attrs, FnKind::Syscall(nr), false);
            }
            if attrs.iter().any(|a| a.name == "trap") {
                return self.parse_fn_ext(attrs, FnKind::Trap, false);
            }
            if attrs.iter().any(|a| a.name == "asm") {
                return self.parse_fn_ext(attrs, FnKind::Asm, true);
            }
        }
        match self.peek().clone() {
            Tok::Fn => self.parse_fn(attrs, false),
            Tok::Async => {
                self.bump();
                if self.peek() != &Tok::Fn {
                    self.err_here("`async` の後には `fn` が必要です", "`fn` expected after `async`");
                    return Err(());
                }
                self.parse_fn(attrs, true)
            }
            Tok::Struct => self.parse_struct(attrs),
            Tok::Static | Tok::Const => self.parse_static(),
            Tok::ArenaKw => self.parse_arena(false, false, false, false),
            Tok::BumpArena => self.parse_arena(true, false, false, false),
            Tok::PortIo => self.parse_port(attrs),
            Tok::Mmio => self.parse_mmio(attrs),
            Tok::Type => self.parse_alias(),
            Tok::Reserved(w) => {
                self.diags.push(Diag::error(
                    "F0001",
                    start,
                    format!("`{}` は Fascon 1.0 では使えません（メソッドは自由関数で、列挙は整数定数か Option で書きます）", w),
                    format!("`{}` is not supported in Fascon 1.0 (write methods as free functions, enums as integer constants or Option)", w),
                ));
                Err(())
            }
            _ => {
                self.err_here(
                    "項目（fn / struct / static / arena / bump_arena / mmio / port_io / type）が必要です",
                    "an item (fn / struct / static / arena / bump_arena / mmio / port_io / type) is expected",
                );
                Err(())
            }
        }
    }

    fn parse_fn(&mut self, attrs: Vec<Attr>, is_async: bool) -> PResult<Item> {
        let start = self.bump().span; // fn
        let (name, name_span) = self.ident("関数名が必要です", "function name expected")?;
        if self.peek() == &Tok::Lt {
            // ジェネリクスは未対応（ライフタイム `'a` は字句解析で報告済み） / generics are not supported
            let sp = self.span();
            self.diags.push(Diag::error(
                "F0001",
                sp,
                "ジェネリクスは Fascon 1.0 では使えません（ライフタイム注釈も不要です）",
                "generics are not supported in Fascon 1.0 (and lifetime annotations are never needed)",
            ));
            let mut depth = 0;
            loop {
                match self.bump().tok {
                    Tok::Lt => depth += 1,
                    Tok::Gt => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Tok::Shr => {
                        depth -= 2;
                        if depth <= 0 {
                            break;
                        }
                    }
                    Tok::Eof | Tok::LParen => break,
                    _ => {}
                }
            }
        }
        let params = self.parse_param_list()?;
        let ret = if self.eat(&Tok::Arrow) { Some(self.parse_type()?) } else { None };
        let body = self.parse_block()?;
        let span = start.to(body.span);
        Ok(Item::Fn(FnDef { name, name_span, params, ret, body, is_async, attrs, span, kind: FnKind::Normal }))
    }

    /// 種類つきの関数（`#[syscall(N)] fn`・`#[trap] fn`・`#[asm(..)] fn ..;`・`extern "C" fn`）。semi_ok なら本体の代わりに
    /// `;` を書ける（asm と C からの取り込み）
    /// a function of a given kind; with semi_ok a `;` may replace the body (asm and imports from C)
    fn parse_fn_ext(&mut self, attrs: Vec<Attr>, kind: FnKind, semi_ok: bool) -> PResult<Item> {
        let start = self.bump().span; // fn
        let (name, name_span) = self.ident("関数名が必要です", "function name expected")?;
        if self.peek() == &Tok::Lt {
            self.err_here("ジェネリクスは Fascon 1.0 では使えません", "generics are not supported in Fascon 1.0");
            return Err(());
        }
        let params = self.parse_param_list()?;
        let ret = if self.eat(&Tok::Arrow) { Some(self.parse_type()?) } else { None };
        if semi_ok && self.peek() == &Tok::Semi {
            let end = self.bump().span;
            let body = Block { stmts: Vec::new(), tail: None, span: end };
            return Ok(Item::Fn(FnDef { name, name_span, params, ret, body, is_async: false, attrs, span: start.to(end), kind }));
        }
        let body = self.parse_block()?;
        let span = start.to(body.span);
        let kind = match kind {
            FnKind::ExternC { .. } => FnKind::ExternC { body: true },
            k => k,
        };
        Ok(Item::Fn(FnDef { name, name_span, params, ret, body, is_async: false, attrs, span, kind }))
    }

    /// `(名前: 型, ..)` / a parenthesised parameter list
    fn parse_param_list(&mut self) -> PResult<Vec<Param>> {
        self.expect(Tok::LParen, "`(` が必要です", "`(` expected")?;
        let mut params = Vec::new();
        while self.peek() != &Tok::RParen {
            let pstart = self.span();
            let mutable = self.eat(&Tok::Mut);
            let (pname, _) = self.ident("引数名が必要です", "parameter name expected")?;
            self.expect(Tok::Colon, "引数の型（`: 型`）が必要です", "parameter type (`: Type`) expected")?;
            let ty = self.parse_type()?;
            let bind = self.new_bind();
            params.push(Param { name: pname, bind, mutable, ty, span: pstart.to(self.prev_span()) });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen, "`)` が必要です", "`)` expected")?;
        Ok(params)
    }

    /// `syscall 名前 at 番号 (引数) -> i64 { .. }`（カーネル側）/ `syscall 名前 at 番号 (引数) -> i64;`（ユーザー側の宣言）
    /// the kernel side of a system call, or its user-side declaration (no body)
    fn parse_syscall(&mut self, attrs: Vec<Attr>) -> PResult<Item> {
        let start = self.bump().span; // syscall
        let (name, name_span) = self.ident("システムコールの名前が必要です（`syscall 名前 at 番号 (引数) -> i64 { .. }`）", "a system call name is expected (`syscall name at number (params) -> i64 { .. }`)")?;
        self.bump(); // at
        let saved = self.no_struct;
        self.no_struct = true;
        let nr = self.parse_primary(); // 番号は整数リテラルか定数の名前 1 つ（後ろの `(` は引数の並び） / one literal or constant name
        self.no_struct = saved;
        let nr = nr?;
        let params = self.parse_param_list()?;
        let ret = if self.eat(&Tok::Arrow) { Some(self.parse_type()?) } else { None };
        if self.peek() == &Tok::Semi {
            let end = self.bump().span;
            let body = Block { stmts: Vec::new(), tail: None, span: end };
            return Ok(Item::Fn(FnDef { name, name_span, params, ret, body, is_async: false, attrs, span: start.to(end), kind: FnKind::SyscallStub(nr) }));
        }
        let body = self.parse_block()?;
        let span = start.to(body.span);
        Ok(Item::Fn(FnDef { name, name_span, params, ret, body, is_async: false, attrs, span, kind: FnKind::Syscall(nr) }))
    }

    /// `trap user_fault(esr: u64, far: u64, pc: u64) { .. }`
    fn parse_trap(&mut self, attrs: Vec<Attr>) -> PResult<Item> {
        let start = self.bump().span; // trap
        let (name, name_span) = self.ident("トラップの名前が必要です（`trap user_fault(esr: u64, far: u64, pc: u64) { .. }`）", "a trap name is expected (`trap user_fault(esr: u64, far: u64, pc: u64) { .. }`)")?;
        let params = self.parse_param_list()?;
        let body = self.parse_block()?;
        let span = start.to(body.span);
        Ok(Item::Fn(FnDef { name, name_span, params, ret: None, body, is_async: false, attrs, span, kind: FnKind::Trap }))
    }

    /// `user_mem @u for @t: バイト数 at 仮想番地;`
    fn parse_user_mem(&mut self) -> PResult<Item> {
        let start = self.bump().span; // user_mem
        let (name, name_span) = self.region_name("`user_mem` の後にはリージョン名が必要です（例: `user_mem umem for task: 0x20_0000 at 0x1_0000_0000;`）", "a region name is expected after `user_mem` (e.g. `user_mem umem for task: 0x20_0000 at 0x1_0000_0000;`)")?;
        self.expect(Tok::For, "`for タスク表` が必要です", "`for <task_arena>` is expected")?;
        let (task, task_span) = self.region_name("`for` の後には task_arena の名前が必要です", "a task_arena name is expected after `for`")?;
        self.expect(Tok::Colon, "`:` が必要です", "`:` expected")?;
        let saved = self.no_struct;
        self.no_struct = true;
        let size = self.parse_expr();
        self.no_struct = saved;
        let size = size?;
        if !self.ident_at(0, "at") {
            self.err_here("`at 仮想番地` が必要です", "`at <virtual address>` is expected");
            return Err(());
        }
        self.bump();
        let at = self.parse_expr()?;
        let end = self.expect(Tok::Semi, "`;` が必要です", "`;` expected")?;
        Ok(Item::UserMem(UserMemDef { name, name_span, task, task_span, size, at, span: start.to(end) }))
    }

    fn parse_struct(&mut self, attrs: Vec<Attr>) -> PResult<Item> {
        let start = self.bump().span;
        let (name, name_span) = self.ident("構造体名が必要です", "struct name expected")?;
        if self.peek() == &Tok::Lt {
            self.err_here("構造体のジェネリクス・ライフタイム引数は書けません", "struct generics / lifetime parameters are not allowed");
            return Err(());
        }
        self.expect(Tok::LBrace, "`{` が必要です", "`{` expected")?;
        let mut fields = Vec::new();
        while self.peek() != &Tok::RBrace {
            let fstart = self.span();
            // `#[no_column]` / `#[secret]` などのフィールドの属性 / field attributes such as `#[no_column]` / `#[secret]`
            let fattrs = self.parse_attrs()?;
            if let Tok::Reserved("pub") = self.peek() {
                self.bump();
            }
            let (fname, _) = self.ident("フィールド名が必要です", "field name expected")?;
            self.expect(Tok::Colon, "`:` が必要です", "`:` expected")?;
            let ty = self.parse_type()?;
            fields.push(FieldDef { name: fname, ty, span: fstart.to(self.prev_span()), attrs: fattrs });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        let end = self.expect(Tok::RBrace, "`}` が必要です", "`}` expected")?;
        Ok(Item::Struct(StructDef { name, name_span, fields, attrs, span: start.to(end) }))
    }

    fn parse_static(&mut self) -> PResult<Item> {
        let start = self.bump().span;
        if self.peek() == &Tok::Mut {
            let sp = self.span();
            self.diags.push(
                Diag::error(
                    "F0001",
                    sp,
                    "`static mut` は使えません（共有の可変状態はアリーナか MMIO リージョンで表します）",
                    "`static mut` is not allowed (express shared mutable state with an arena or an MMIO region)",
                )
                .hint("`arena @name: T[N];` で静的なリージョンを宣言してください", "declare a static region with `arena @name: T[N];`"),
            );
            self.bump();
        }
        let (name, name_span) = self.ident("static の名前が必要です", "static name expected")?;
        self.expect(Tok::Colon, "static には型が必要です（`: 型`）", "a static needs a type (`: Type`)")?;
        let ty = self.parse_type()?;
        self.expect(Tok::Eq, "`=` が必要です", "`=` expected")?;
        let value = self.parse_expr()?;
        let end = self.expect(Tok::Semi, "`;` が必要です", "`;` expected")?;
        Ok(Item::Static(StaticDef { name, name_span, ty, value, span: start.to(end) }))
    }

    /// `arena process: Process[256];` / `bump_arena scratch: Node[1024];`（`@` は要らない / no `@` needed）
    fn parse_arena(&mut self, bump: bool, task: bool, dma: bool, shared: bool) -> PResult<Item> {
        let start = self.bump().span;
        let (name, name_span) = self.region_name("アリーナ名が必要です（例: `arena process: Process[64];`）", "an arena name is expected (e.g. `arena process: Process[64];`)")?;
        let kw = if dma { "dma_arena" } else if task { "task_arena" } else if bump { "bump_arena" } else { "arena" };
        self.expect(
            Tok::Colon,
            &format!("`:` が必要です（`{} 名前: 型[容量];`）", kw),
            &format!("`:` expected (`{} name: Type[capacity];`)", kw),
        )?;
        let elem = self.parse_type_no_array_suffix()?;
        self.expect(Tok::LBracket, "容量 `[N]` が必要です", "a capacity `[N]` is expected")?;
        let cap = self.parse_expr()?;
        self.expect(Tok::RBracket, "`]` が必要です", "`]` expected")?;
        let mut task_stack = None;
        if task {
            if !self.ident_at(0, "stack") {
                self.err_here("`stack バイト数` が必要です（例: `task_arena task: Task[64] stack 16384;`）", "`stack <bytes>` is expected (e.g. `task_arena task: Task[64] stack 16384;`)");
                return Err(());
            }
            self.bump();
            task_stack = Some(self.parse_expr()?);
        }
        let end = self.expect(Tok::Semi, "`;` が必要です", "`;` expected")?;
        Ok(Item::Arena(ArenaDef { name, name_span, elem, cap, span: start.to(end), bump, task_stack, dma, gc: false, shared, gpu: false, numa_node: None }))
    }

    /// `port_io com1 at_port 0x3f8;` / `port_io com1: [u8; 8] at_port 0x3f8;`
    fn parse_port(&mut self, attrs: Vec<Attr>) -> PResult<Item> {
        let start = self.bump().span;
        let (name, name_span) = self.region_name("ポート名が必要です（例: `port_io com1 at_port 0x3f8;`）", "a port name is expected (e.g. `port_io com1 at_port 0x3f8;`)")?;
        let ty = if self.eat(&Tok::Colon) { Some(self.parse_type()?) } else { None };
        match self.peek().clone() {
            Tok::Ident(s) if s == "at_port" => {
                self.bump();
            }
            _ => {
                self.err_here("`at_port ポート番号` が必要です（例: `at_port 0x3f8`）", "`at_port <port number>` is expected (e.g. `at_port 0x3f8`)");
                return Err(());
            }
        }
        let port = self.parse_expr()?;
        let end = self.expect(Tok::Semi, "`;` が必要です", "`;` expected")?;
        Ok(Item::Port(PortDef { name, name_span, ty, port, attrs, span: start.to(end) }))
    }

    /// `#[framebuffer] mmio fb: [[u32; 800]; 600] at 0x4800_0000;`
    fn parse_mmio(&mut self, attrs: Vec<Attr>) -> PResult<Item> {
        let start = self.bump().span;
        let (name, name_span) = self.region_name("リージョン名が必要です（例: `mmio uart: [u32; 18] at 0x0900_0000;`）", "a region name is expected (e.g. `mmio uart: [u32; 18] at 0x0900_0000;`)")?;
        self.expect(Tok::Colon, "`:` が必要です（`mmio 名前: [T; N] at 番地;`）", "`:` expected (`mmio name: [T; N] at address;`)")?;
        let ty = self.parse_type()?;
        match self.peek().clone() {
            Tok::Ident(s) if s == "at" => {
                self.bump();
            }
            _ => {
                self.err_here("`at 物理番地` が必要です", "`at <physical address>` is expected");
                return Err(());
            }
        }
        let addr = self.parse_expr()?;
        let end = self.expect(Tok::Semi, "`;` が必要です", "`;` expected")?;
        Ok(Item::Mmio(MmioDef { name, name_span, ty, addr, attrs, span: start.to(end) }))
    }

    /// `type FileId<@a: File> = Id<@a>;`
    fn parse_alias(&mut self) -> PResult<Item> {
        let start = self.bump().span;
        let (name, name_span) = self.ident("型の別名が必要です", "type alias name expected")?;
        let mut params = Vec::new();
        if self.eat(&Tok::Lt) {
            loop {
                let psp = self.span();
                // ジェネリクスは無いので、引数はいつもアリーナ引数（`a` / 古い書き方の `@a`）
                // there are no generics, so a parameter is always an arena parameter (`a`, or the legacy `@a`)
                let (pname, _) = self.region_name(
                    "型の別名の引数はアリーナ引数（例: `type FileId<a: File> = Id<a>;`）だけです",
                    "type alias parameters can only be arena parameters (e.g. `type FileId<a: File> = Id<a>;`)",
                )?;
                let bound = if self.eat(&Tok::Colon) { Some(self.parse_type()?) } else { None };
                params.push(AliasParam { name: pname, bound, span: psp.to(self.prev_span()) });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect_gt()?;
        }
        self.expect(Tok::Eq, "`=` が必要です", "`=` expected")?;
        let saved = std::mem::replace(&mut self.alias_params, params.iter().map(|p| p.name.clone()).collect());
        let ty = self.parse_type();
        self.alias_params = saved;
        let ty = ty?;
        let end = self.expect(Tok::Semi, "`;` が必要です", "`;` expected")?;
        Ok(Item::Alias(AliasDef { name, name_span, params, ty, span: start.to(end) }))
    }

    fn expect_gt(&mut self) -> PResult<()> {
        if self.pending_gt {
            self.pending_gt = false;
            self.bump(); // `>>` の 2 つ目 / the second `>` of `>>`
            return Ok(());
        }
        match self.peek() {
            Tok::Gt => {
                self.bump();
                Ok(())
            }
            Tok::Shr => {
                // `>>` を分割: 1 つ目をここで消費したことにする / split `>>`: consume the first half here
                self.pending_gt = true;
                Ok(())
            }
            Tok::Ge | Tok::ShrEq => {
                self.err_here("`>` が必要です", "`>` expected");
                Err(())
            }
            _ => {
                self.err_here("`>` が必要です", "`>` expected");
                Err(())
            }
        }
    }

    pub fn parse_type(&mut self) -> PResult<TypeExpr> {
        let t = self.parse_type_no_array_suffix()?;
        Ok(t)
    }

    fn parse_type_no_array_suffix(&mut self) -> PResult<TypeExpr> {
        let start = self.span();
        // 修飾子で書く型（ネストした型引数を増やさない）: `user T`・`dma own T`・`mmio T`
        // types written with a modifier (no nested type arguments): `user T`, `dma own T`, `mmio T`
        if self.ident_at(0, "user") && matches!(self.peek_at(1), Tok::LBracket | Tok::Ident(_)) {
            self.bump();
            let inner = self.parse_type()?;
            let sp = start.to(inner.span());
            return Ok(TypeExpr::User(Box::new(inner), sp));
        }
        if self.ident_at(0, "dma") && self.peek_at(1) == &Tok::Own {
            self.bump();
            self.bump();
            let inner = self.parse_type()?;
            let sp = start.to(inner.span());
            return Ok(TypeExpr::DmaOwn(Box::new(inner), sp));
        }
        if self.peek() == &Tok::Mmio && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.bump();
            let inner = self.parse_type()?;
            let sp = start.to(inner.span());
            return Ok(TypeExpr::MmioBind(Box::new(inner), sp));
        }
        match self.peek().clone() {
            Tok::Amp | Tok::AmpAmp => {
                let double = self.peek() == &Tok::AmpAmp;
                self.bump();
                let mutable = self.eat(&Tok::Mut);
                let inner = self.parse_type()?;
                let sp = start.to(inner.span());
                if double {
                    self.diags.push(Diag::error("F0515", sp, "参照の参照は書けません（第二級参照）", "a reference to a reference is not allowed (second-class references)"));
                }
                Ok(TypeExpr::Ref { mutable, inner: Box::new(inner), span: sp })
            }
            Tok::Own => {
                self.bump();
                let inner = self.parse_type()?;
                let sp = start.to(inner.span());
                Ok(TypeExpr::Own(Box::new(inner), sp))
            }
            Tok::LParen => {
                self.bump();
                let end = self.expect(Tok::RParen, "タプル型は使えません（`()` だけ書けます）", "tuple types are not supported (only `()`)")?;
                Ok(TypeExpr::Unit(start.to(end)))
            }
            Tok::Fn => {
                // 関数ポインタ型 `fn(T, &mut U) -> R`（引数名は書かない。ジェネリクス・ライフタイムは無い）
                // a function pointer type `fn(T, &mut U) -> R` (no parameter names, no generics or lifetimes)
                self.bump();
                if self.peek() != &Tok::LParen {
                    self.err_here(
                        "型の位置の `fn` は関数ポインタ型 `fn(引数の型, ..) -> 戻り値の型` です",
                        "`fn` in a type position is a function pointer type `fn(param types, ..) -> return type`",
                    );
                    return Err(());
                }
                self.bump();
                let mut params = Vec::new();
                while self.peek() != &Tok::RParen {
                    // Rust と同じく `fn(x: i32)` の名前は読み飛ばす / like Rust, a name in `fn(x: i32)` is skipped
                    if let (Tok::Ident(_), Tok::Colon) = (self.peek().clone(), self.peek_at(1).clone()) {
                        self.bump();
                        self.bump();
                    }
                    params.push(self.parse_type()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                let mut end = self.expect(Tok::RParen, "`)` が必要です", "`)` expected")?;
                let ret = if self.eat(&Tok::Arrow) {
                    let r = self.parse_type()?;
                    end = r.span();
                    Some(Box::new(r))
                } else {
                    None
                };
                Ok(TypeExpr::Fn { params, ret, span: start.to(end) })
            }
            Tok::LBracket => {
                self.bump();
                let elem = self.parse_type()?;
                // `[u8]`: 長さ付きの窓（`&[u8]` / `&mut [u8]` の中でだけ意味を持つ。検査は型の解決で）
                // `[u8]`: a window with a length (meaningful only inside `&[u8]` / `&mut [u8]`; checked when the type is resolved)
                if self.peek() == &Tok::RBracket {
                    let end = self.bump().span;
                    return Ok(TypeExpr::Slice { elem: Box::new(elem), span: start.to(end) });
                }
                self.expect(Tok::Semi, "配列型は `[T; N]` と書きます", "an array type is written `[T; N]`")?;
                let len = self.parse_expr()?;
                let end = self.expect(Tok::RBracket, "`]` が必要です", "`]` expected")?;
                Ok(TypeExpr::Array { elem: Box::new(elem), len: Box::new(len), span: start.to(end) })
            }
            Tok::Ident(name) => {
                self.bump();
                let mut args = Vec::new();
                if self.peek() == &Tok::Lt {
                    self.bump();
                    loop {
                        // `Id<名前>` の名前がアリーナかどうかは 1 周目の表で決める（宣言がファイルの後ろにあってもよい）
                        // whether the name in `Id<name>` is an arena comes from the pass-1 table (it may be declared later)
                        let after = self.peek_at(1).clone();
                        match self.peek().clone() {
                            Tok::ArenaName(a) => {
                                let sp = self.bump().span;
                                args.push(GenericArg::Arena(a, sp));
                            }
                            Tok::Ident(a) if self.arena_arg_name(&a) && matches!(after, Tok::Comma | Tok::Gt | Tok::Shr | Tok::Ge | Tok::ShrEq) => {
                                let sp = self.bump().span;
                                args.push(GenericArg::Arena(a, sp));
                            }
                            _ => args.push(GenericArg::Type(self.parse_type()?)),
                        }
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect_gt()?;
                }
                Ok(TypeExpr::Path { name, args, span: start.to(self.prev_span()) })
            }
            _ => {
                self.err_here("型が必要です", "a type is expected");
                Err(())
            }
        }
    }

    pub fn parse_block(&mut self) -> PResult<Block> {
        let start = self.expect(Tok::LBrace, "`{` が必要です", "`{` expected")?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mut stmts = Vec::new();
        let mut tail: Option<Box<Expr>> = None;
        loop {
            match self.peek() {
                Tok::RBrace => break,
                Tok::Eof => {
                    self.err_here("`}` が必要です", "`}` expected");
                    self.no_struct = saved;
                    return Err(());
                }
                Tok::Semi => {
                    self.bump();
                    continue;
                }
                _ => {}
            }
            if let Some(t) = tail.take() {
                // 直前の式は `;` 無しの式文だった / the previous expression was a statement without `;`
                stmts.push(Stmt::Expr(*t));
            }
            if self.peek() == &Tok::Let {
                match self.parse_let() {
                    Ok(l) => stmts.push(Stmt::Let(l)),
                    Err(()) => self.recover_stmt(),
                }
                continue;
            }
            // `#[secret] let k = ..;`: 属性付きの let / a let with attributes
            if self.peek() == &Tok::Hash {
                let attrs = match self.parse_attrs() {
                    Ok(a) => a,
                    Err(()) => {
                        self.recover_stmt();
                        continue;
                    }
                };
                // `#[fast_math] for .. { .. }`（while・loop も）: ループの浮動小数点の集計に再結合を許す
                // `#[fast_math] for .. { .. }` (while and loop too): allow reassociation in the loop's float reductions
                if matches!(self.peek(), Tok::For | Tok::While | Tok::Loop) {
                    if let Some(a) = attrs.iter().find(|a| a.name != "fast_math") {
                        self.diags.push(Diag::error("F0001", a.span, format!("ループに付けられる属性は `#[fast_math]` だけです（`{}` は付けられません）", a.name), format!("the only loop attribute is `#[fast_math]` (not `{}`)", a.name)));
                    }
                    let asp = attrs[0].span;
                    match self.parse_expr_stmt() {
                        Ok((e, had_semi)) => {
                            self.fast_math_loops.insert(e.id, asp);
                            if had_semi || self.peek() != &Tok::RBrace {
                                stmts.push(Stmt::Expr(e));
                            } else {
                                tail = Some(Box::new(e));
                            }
                        }
                        Err(()) => self.recover_stmt(),
                    }
                    continue;
                }
                if self.peek() != &Tok::Let {
                    self.err_here("属性の後には `let` が必要です（文に付けられる属性は `#[secret] let ..` と、ループの `#[fast_math]` だけ）", "`let` expected after the attribute (statement attributes are `#[secret] let ..` and `#[fast_math]` on a loop)");
                    self.recover_stmt();
                    continue;
                }
                match self.parse_let() {
                    Ok(mut l) => {
                        l.attrs = attrs;
                        stmts.push(Stmt::Let(l))
                    }
                    Err(()) => self.recover_stmt(),
                }
                continue;
            }
            match self.parse_expr_stmt() {
                Ok((e, had_semi)) => {
                    if had_semi {
                        stmts.push(Stmt::Expr(e));
                    } else if e.kind.is_block_like() && self.peek() != &Tok::RBrace {
                        stmts.push(Stmt::Expr(e));
                    } else {
                        tail = Some(Box::new(e));
                    }
                }
                Err(()) => self.recover_stmt(),
            }
        }
        let end = self.expect(Tok::RBrace, "`}` が必要です", "`}` expected")?;
        self.no_struct = saved;
        Ok(Block { stmts, tail, span: start.to(end) })
    }

    fn recover_stmt(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::Semi if depth == 0 => {
                    self.bump();
                    return;
                }
                Tok::LBrace => depth += 1,
                Tok::RBrace => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn parse_let(&mut self) -> PResult<LetStmt> {
        let start = self.bump().span;
        let mutable = self.eat(&Tok::Mut);
        let (name, name_span) = match self.peek().clone() {
            Tok::Ident(s) => (s, self.bump().span),
            Tok::LParen => {
                self.err_here("タプルの分解は使えません", "tuple destructuring is not supported");
                return Err(());
            }
            _ => {
                self.err_here("変数名が必要です", "variable name expected");
                return Err(());
            }
        };
        let ty = if self.eat(&Tok::Colon) { Some(self.parse_type()?) } else { None };
        let init = if self.eat(&Tok::Eq) { Some(self.parse_expr()?) } else { None };
        let end = self.expect(Tok::Semi, "`;` が必要です", "`;` expected")?;
        let bind = self.new_bind();
        Ok(LetStmt { name, name_span, bind, mutable, ty, init, span: start.to(end), attrs: Vec::new() })
    }

    /// 式文: 代入を含む。戻り値の bool は `;` があったか / expression statement incl. assignment; bool = had `;`
    fn parse_expr_stmt(&mut self) -> PResult<(Expr, bool)> {
        // 文頭の if / match / ループ / ブロックは文（後ろの `-1` などを二項演算として続けない。Rust と同じ）
        // a statement starting with if / match / a loop / a block is a statement (a following `-1` is not an operand; same as Rust)
        // `critical { .. }`: 文の先頭でだけ閉じた区間になる（`if critical { .. }` の変数とは曖昧にならない）
        // a closed section only at the start of a statement (never ambiguous with a variable in `if critical { .. }`)
        if self.ident_at(0, "critical") && self.peek_at(1) == &Tok::LBrace {
            let start = self.bump().span;
            let b = self.parse_block()?;
            let sp = start.to(b.span);
            let e = self.new_expr(ExprKind::Critical(b), sp);
            let had_semi = self.eat(&Tok::Semi);
            return Ok((e, had_semi));
        }
        // `atomic { .. }`・`rcu_read { .. }`・`catch_fault { .. }`: 文の先頭の区間（`critical` と同じ扱い）
        // sections at the start of a statement (handled like `critical`)
        if let Some(e) = self.scope_expr()? {
            let had_semi = self.eat(&Tok::Semi);
            return Ok((e, had_semi));
        }
        if matches!(self.peek(), Tok::If | Tok::Match | Tok::While | Tok::Loop | Tok::For | Tok::LBrace | Tok::VolatileBypass) {
            let e = self.parse_primary()?;
            let had_semi = self.eat(&Tok::Semi);
            return Ok((e, had_semi));
        }
        let e = self.parse_assign()?;
        let had_semi = self.eat(&Tok::Semi);
        if !had_semi && !e.kind.is_block_like() && self.peek() != &Tok::RBrace {
            self.err_here("`;` が必要です", "`;` expected");
            return Err(());
        }
        Ok((e, had_semi))
    }

    /// `atomic {`・`rcu_read {`・`catch_fault {` なら区間の式を読む / read a section expression at `atomic {` etc.
    fn scope_expr(&mut self) -> PResult<Option<Expr>> {
        for w in ["atomic", "rcu_read", "catch_fault"] {
            if self.ident_at(0, w) && self.peek_at(1) == &Tok::LBrace {
                let start = self.bump().span;
                let b = self.parse_block()?;
                let sp = start.to(b.span);
                let k = match w {
                    "atomic" => ExprKind::Atomic(b),
                    "rcu_read" => ExprKind::RcuRead(b),
                    _ => ExprKind::CatchFault(b),
                };
                return Ok(Some(self.new_expr(k, sp)));
            }
        }
        Ok(None)
    }

    fn parse_assign(&mut self) -> PResult<Expr> {
        let lhs = self.parse_expr()?;
        let op = match self.peek() {
            Tok::Eq => None,
            Tok::PlusEq => Some(BinOp::Add),
            Tok::MinusEq => Some(BinOp::Sub),
            Tok::StarEq => Some(BinOp::Mul),
            Tok::SlashEq => Some(BinOp::Div),
            Tok::PercentEq => Some(BinOp::Rem),
            Tok::AmpEq => Some(BinOp::BitAnd),
            Tok::PipeEq => Some(BinOp::BitOr),
            Tok::CaretEq => Some(BinOp::BitXor),
            Tok::ShlEq => Some(BinOp::Shl),
            Tok::ShrEq => Some(BinOp::Shr),
            _ => return Ok(lhs),
        };
        self.bump();
        let rhs = self.parse_expr()?;
        let sp = lhs.span.to(rhs.span);
        Ok(self.new_expr(ExprKind::Assign { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, sp))
    }

    pub fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_bin(0)
    }

    fn bin_op(&self) -> Option<(BinOp, u8)> {
        Some(match self.peek() {
            Tok::PipePipe => (BinOp::Or, 1),
            Tok::AmpAmp => (BinOp::And, 2),
            Tok::EqEq => (BinOp::Eq, 3),
            Tok::Ne => (BinOp::Ne, 3),
            Tok::Lt => (BinOp::Lt, 3),
            Tok::Le => (BinOp::Le, 3),
            Tok::Gt => (BinOp::Gt, 3),
            Tok::Ge => (BinOp::Ge, 3),
            Tok::Pipe => (BinOp::BitOr, 4),
            Tok::Caret => (BinOp::BitXor, 5),
            Tok::Amp => (BinOp::BitAnd, 6),
            Tok::Shl => (BinOp::Shl, 7),
            Tok::Shr => (BinOp::Shr, 7),
            Tok::Plus => (BinOp::Add, 8),
            Tok::Minus => (BinOp::Sub, 8),
            Tok::Star => (BinOp::Mul, 9),
            Tok::Slash => (BinOp::Div, 9),
            Tok::Percent => (BinOp::Rem, 9),
            _ => return None,
        })
    }

    fn parse_bin(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut lhs = self.parse_cast()?;
        loop {
            let (op, prec) = match self.bin_op() {
                Some(x) => x,
                None => break,
            };
            if prec < min_prec || prec == 0 {
                break;
            }
            self.bump();
            // 比較は非結合 / comparisons are non-associative
            let next_min = prec + 1;
            let rhs = self.parse_bin(next_min)?;
            if op.is_cmp() {
                if let Some((op2, p2)) = self.bin_op() {
                    if p2 == 3 {
                        let sp = self.span();
                        self.diags.push(Diag::error(
                            "F0001",
                            sp,
                            format!("比較演算子 `{}` と `{}` は連結できません（括弧で区切ってください）", op.text(), op2.text()),
                            format!("comparison operators `{}` and `{}` cannot be chained (use parentheses)", op.text(), op2.text()),
                        ));
                        return Err(());
                    }
                }
            }
            let sp = lhs.span.to(rhs.span);
            lhs = self.new_expr(ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), sp);
        }
        Ok(lhs)
    }

    fn parse_cast(&mut self) -> PResult<Expr> {
        let mut e = self.parse_unary()?;
        while self.peek() == &Tok::As {
            self.bump();
            let ty = self.parse_type()?;
            let sp = e.span.to(ty.span());
            e = self.new_expr(ExprKind::Cast(Box::new(e), ty), sp);
        }
        Ok(e)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        let start = self.span();
        match self.peek().clone() {
            Tok::Minus => {
                self.bump();
                let inner = self.parse_unary()?;
                let sp = start.to(inner.span);
                Ok(self.new_expr(ExprKind::Unary(UnOp::Neg, Box::new(inner)), sp))
            }
            Tok::Bang => {
                self.bump();
                let inner = self.parse_unary()?;
                let sp = start.to(inner.span);
                Ok(self.new_expr(ExprKind::Unary(UnOp::Not, Box::new(inner)), sp))
            }
            Tok::Amp | Tok::AmpAmp => {
                let double = self.peek() == &Tok::AmpAmp;
                self.bump();
                let mutable = self.eat(&Tok::Mut);
                let inner = self.parse_unary()?;
                let sp = start.to(inner.span);
                if double {
                    self.diags.push(Diag::error("F0515", sp, "参照の参照は作れません（第二級参照）", "a reference to a reference cannot be created (second-class references)"));
                }
                Ok(self.new_expr(ExprKind::Ref { mutable, inner: Box::new(inner) }, sp))
            }
            Tok::Star => {
                self.bump();
                let inner = self.parse_unary()?;
                // 参照は自動で辿られるので `*` は不要（そのまま中身を使う）。own の値に付いたときだけ own の中身を指す
                // （`*ctx.buffer` / `*ctx.buffer = 5`）。どちらになるかは意味解析が型を見て決める
                // references are auto-dereferenced, so `*` is a no-op for them; on an own value it denotes the contents of
                // the own (`*ctx.buffer` / `*ctx.buffer = 5`). Semantic analysis decides from the type
                self.derefs.insert(inner.id);
                Ok(inner)
            }
            Tok::Own => {
                self.bump();
                let inner = self.parse_unary()?;
                let sp = start.to(inner.span);
                Ok(self.new_expr(ExprKind::Own(Box::new(inner)), sp))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_args(&mut self) -> PResult<Vec<Expr>> {
        self.expect(Tok::LParen, "`(` が必要です", "`(` expected")?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mut args = Vec::new();
        while self.peek() != &Tok::RParen {
            args.push(self.parse_expr()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.no_struct = saved;
        self.expect(Tok::RParen, "`)` が必要です", "`)` expected")?;
        Ok(args)
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            match self.peek().clone() {
                Tok::Dot => {
                    self.bump();
                    match self.peek().clone() {
                        Tok::Ident(name) => {
                            let msp = self.bump().span;
                            if self.peek() == &Tok::LParen {
                                let args = self.parse_args()?;
                                let sp = e.span.to(self.prev_span());
                                e = self.new_expr(ExprKind::MethodCall { recv: Box::new(e), method: name, method_span: msp, args }, sp);
                            } else {
                                let sp = e.span.to(msp);
                                e = self.new_expr(ExprKind::Field(Box::new(e), name, msp), sp);
                            }
                        }
                        Tok::Await => {
                            let sp = e.span.to(self.bump().span);
                            e = self.new_expr(ExprKind::Unsupported("await"), sp);
                        }
                        Tok::Int(..) => {
                            self.err_here("タプルは使えません", "tuples are not supported");
                            return Err(());
                        }
                        _ => {
                            self.err_here("`.` の後にフィールド名かメソッド名が必要です", "a field or method name is expected after `.`");
                            return Err(());
                        }
                    }
                }
                Tok::LBracket => {
                    self.bump();
                    let saved = self.no_struct;
                    self.no_struct = false;
                    let idx = self.parse_expr()?;
                    self.no_struct = saved;
                    let end = self.expect(Tok::RBracket, "`]` が必要です", "`]` expected")?;
                    let sp = e.span.to(end);
                    e = self.new_expr(ExprKind::Index(Box::new(e), Box::new(idx)), sp);
                }
                Tok::Question => {
                    // `e?` = `match e { Ok(v) => v, Err(x) => return Err(x) }`（Result だけ。早期リターンは見た目どおりの 1 か所）
                    // `e?` = `match e { Ok(v) => v, Err(x) => return Err(x) }` (Result only; the early return is this one place)
                    let qsp = self.bump().span;
                    let sp = e.span.to(qsp);
                    let n = self.next_bind;
                    let (vb, xb) = (self.new_bind(), self.new_bind());
                    let (vn, xn) = (format!("__q{}", n), format!("__e{}", n));
                    let v_use = self.new_expr(ExprKind::Path(vn.clone()), sp);
                    let x_use = self.new_expr(ExprKind::Path(xn.clone()), sp);
                    let err_call = self.new_expr(ExprKind::Call { callee: "Err".into(), callee_span: qsp, args: vec![x_use], turbofish: None }, sp);
                    let ret = self.new_expr(ExprKind::Return(Some(Box::new(err_call))), sp);
                    let arms = vec![Arm { pat: Pattern::Ok(Some((vn, vb, false)), qsp), body: v_use, span: sp }, Arm { pat: Pattern::Err(Some((xn, xb, false)), qsp), body: ret, span: sp }];
                    self.question_marks.push((self.next_expr, sp));
                    e = self.new_expr(ExprKind::Match { scrut: Box::new(e), arms }, sp);
                }
                Tok::LParen => {
                    // 式を呼ぶ（関数ポインタ）: `(h.on_click)(&mut b)` / `ops[i](x)` / `make()(x)`
                    // calling an expression (a function pointer): `(h.on_click)(&mut b)` / `ops[i](x)` / `make()(x)`
                    let args = self.parse_args()?;
                    let sp = e.span.to(self.prev_span());
                    e = self.new_expr(ExprKind::CallIndirect { callee: Box::new(e), args }, sp);
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let start = self.span();
        match self.peek().clone() {
            Tok::Int(v, suf) => {
                self.bump();
                Ok(self.new_expr(ExprKind::Int(v, suf), start))
            }
            Tok::Float(t, suf) => {
                self.bump();
                Ok(self.new_expr(ExprKind::Float(t, suf), start))
            }
            Tok::True => {
                self.bump();
                Ok(self.new_expr(ExprKind::Bool(true), start))
            }
            Tok::False => {
                self.bump();
                Ok(self.new_expr(ExprKind::Bool(false), start))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(self.new_expr(ExprKind::Str(s), start))
            }
            Tok::ArenaName(n) => {
                self.bump();
                Ok(self.new_expr(ExprKind::ArenaRef(n), start))
            }
            Tok::LParen => {
                self.bump();
                if self.peek() == &Tok::RParen {
                    let end = self.bump().span;
                    return Ok(self.new_expr(ExprKind::Unit, start.to(end)));
                }
                let saved = self.no_struct;
                self.no_struct = false;
                let e = self.parse_expr()?;
                self.no_struct = saved;
                if self.peek() == &Tok::Comma {
                    self.err_here("タプルは使えません", "tuples are not supported");
                    return Err(());
                }
                let end = self.expect(Tok::RParen, "`)` が必要です", "`)` expected")?;
                let mut e = e;
                e.span = start.to(end);
                Ok(e)
            }
            Tok::LBracket => {
                self.bump();
                let saved = self.no_struct;
                self.no_struct = false;
                if self.peek() == &Tok::RBracket {
                    self.no_struct = saved;
                    self.err_here("空の配列リテラルは使えません（`[値; 個数]` で書いてください）", "empty array literals are not supported (write `[value; count]`)");
                    return Err(());
                }
                let first = self.parse_expr()?;
                let r = if self.eat(&Tok::Semi) {
                    let count = self.parse_expr()?;
                    let end = self.expect(Tok::RBracket, "`]` が必要です", "`]` expected")?;
                    self.new_expr(ExprKind::ArrayRepeat(Box::new(first), Box::new(count)), start.to(end))
                } else {
                    let mut elems = vec![first];
                    while self.eat(&Tok::Comma) {
                        if self.peek() == &Tok::RBracket {
                            break;
                        }
                        elems.push(self.parse_expr()?);
                    }
                    let end = self.expect(Tok::RBracket, "`]` が必要です", "`]` expected")?;
                    self.new_expr(ExprKind::ArrayLit(elems), start.to(end))
                };
                self.no_struct = saved;
                Ok(r)
            }
            Tok::LBrace => {
                let b = self.parse_block()?;
                let sp = b.span;
                Ok(self.new_expr(ExprKind::Block(b), sp))
            }
            Tok::VolatileBypass => {
                self.bump();
                let b = self.parse_block()?;
                let sp = start.to(b.span);
                Ok(self.new_expr(ExprKind::VolatileBypass(b), sp))
            }
            Tok::If => self.parse_if(),
            Tok::Match => {
                self.bump();
                let saved = self.no_struct;
                self.no_struct = true;
                let scrut = self.parse_expr()?;
                self.no_struct = saved;
                self.expect(Tok::LBrace, "`match` の後に `{` が必要です", "`{` expected after the `match` head")?;
                let mut arms = Vec::new();
                while self.peek() != &Tok::RBrace && self.peek() != &Tok::Eof {
                    let astart = self.span();
                    let pat = self.parse_pattern()?;
                    if self.peek() == &Tok::If {
                        self.err_here("match ガード（`if`）は使えません", "match guards (`if`) are not supported");
                        return Err(());
                    }
                    self.expect(Tok::FatArrow, "`=>` が必要です", "`=>` expected")?;
                    let saved2 = self.no_struct;
                    self.no_struct = false;
                    let body = self.parse_assign()?;
                    self.no_struct = saved2;
                    let block_like = body.kind.is_block_like();
                    let sp = astart.to(body.span);
                    arms.push(Arm { pat, body, span: sp });
                    if !self.eat(&Tok::Comma) && !block_like && self.peek() != &Tok::RBrace {
                        self.err_here("アームの区切り `,` が必要です", "`,` expected between match arms");
                        return Err(());
                    }
                }
                let end = self.expect(Tok::RBrace, "`}` が必要です", "`}` expected")?;
                Ok(self.new_expr(ExprKind::Match { scrut: Box::new(scrut), arms }, start.to(end)))
            }
            Tok::While => {
                self.bump();
                if self.peek() == &Tok::Let && self.reframe {
                    // リフレーミング: `while let P = e { 本体 }` → `loop { if let P = e { 本体 } else { break; } }`
                    // reframing: `while let P = e { body }` → `loop { if let P = e { body } else { break; } }`
                    self.bump();
                    let pat = self.parse_pattern()?;
                    self.expect(Tok::Eq, "`while let パターン = 式` の `=` が必要です", "`=` expected in `while let PATTERN = EXPR`")?;
                    let saved = self.no_struct;
                    self.no_struct = true;
                    let scrut = self.parse_expr()?;
                    self.no_struct = saved;
                    let then = self.parse_block()?;
                    let sp = start.to(then.span);
                    let brk = self.new_expr(ExprKind::Break, sp);
                    let els_block = Block { stmts: vec![Stmt::Expr(brk)], tail: None, span: sp };
                    let els = self.new_expr(ExprKind::Block(els_block), sp);
                    let iflet = self.new_expr(ExprKind::IfLet { pat, scrut: Box::new(scrut), then, els: Some(Box::new(els)) }, sp);
                    let body = Block { stmts: vec![Stmt::Expr(iflet)], tail: None, span: sp };
                    return Ok(self.new_expr(ExprKind::Loop { body }, sp));
                }
                if self.peek() == &Tok::Let {
                    self.err_here("`while let` は使えません（`loop` と `if let` で書いてください）", "`while let` is not supported (use `loop` with `if let`)");
                    return Err(());
                }
                let saved = self.no_struct;
                self.no_struct = true;
                let cond = self.parse_expr()?;
                self.no_struct = saved;
                let body = self.parse_block()?;
                let sp = start.to(body.span);
                Ok(self.new_expr(ExprKind::While { cond: Box::new(cond), body }, sp))
            }
            Tok::Loop => {
                self.bump();
                let body = self.parse_block()?;
                let sp = start.to(body.span);
                Ok(self.new_expr(ExprKind::Loop { body }, sp))
            }
            Tok::For => {
                self.bump();
                let (var, var_span) = self.ident("ループ変数が必要です", "loop variable expected")?;
                self.expect(Tok::In, "`in` が必要です", "`in` expected")?;
                let saved = self.no_struct;
                self.no_struct = true;
                let mut par = false;
                // `for id in 名前 { .. }`: 名前 1 つの後ろが範囲・フィールド・添字・呼び出しでなければ、アリーナを回す
                // `for id in name { .. }`: a lone name not followed by a range, a field, an index or a call iterates an arena
                let iter = match self.peek().clone() {
                    // `for id in 名前.par() { .. }`: 読み取り専用の並列走査 / a read-only parallel scan
                    Tok::ArenaName(a) | Tok::Ident(a) if self.peek_at(1) == &Tok::Dot && self.ident_at(2, "par") && self.peek_at(3) == &Tok::LParen && self.peek_at(4) == &Tok::RParen => {
                        let asp = self.bump().span;
                        for _ in 0..4 {
                            self.bump();
                        }
                        par = true;
                        ForIter::Arena(a, asp)
                    }
                    Tok::ArenaName(a) | Tok::Ident(a) if !matches!(self.peek_at(1), Tok::DotDot | Tok::DotDotEq | Tok::Dot | Tok::LBracket | Tok::LParen | Tok::ColonColon) => {
                        let asp = self.bump().span;
                        ForIter::Arena(a, asp)
                    }
                    _ => self.parse_range_iter()?,
                };
                self.no_struct = saved;
                let body = self.parse_block()?;
                let sp = start.to(body.span);
                let bind = self.new_bind();
                let e = self.new_expr(ExprKind::For { var, var_span, bind, iter, body }, sp);
                if par {
                    self.par_fors.push(e.id);
                }
                Ok(e)
            }
            Tok::Break => {
                self.bump();
                if !matches!(self.peek(), Tok::Semi | Tok::RBrace | Tok::Comma) {
                    self.err_here("`break` に値は付けられません", "`break` cannot carry a value");
                    return Err(());
                }
                Ok(self.new_expr(ExprKind::Break, start))
            }
            Tok::Continue => {
                self.bump();
                Ok(self.new_expr(ExprKind::Continue, start))
            }
            Tok::Return => {
                self.bump();
                let v = if matches!(self.peek(), Tok::Semi | Tok::RBrace | Tok::Comma) { None } else { Some(Box::new(self.parse_expr()?)) };
                let sp = start.to(self.prev_span());
                Ok(self.new_expr(ExprKind::Return(v), sp))
            }
            Tok::Spawn => {
                self.bump();
                let b = self.parse_block()?;
                let sp = start.to(b.span);
                Ok(self.new_expr(ExprKind::Unsupported("spawn"), sp))
            }
            Tok::Ident(name) if name == "own_alloc" && matches!(self.peek_at(1), Tok::LParen) => {
                // `own_alloc(Block)`: 型名を引数のように渡す組み込み（ジェネリクスは使わない）
                // `own_alloc(Block)`: a builtin that takes a type name like an argument (no generics)
                self.bump();
                self.bump();
                let ty = self.parse_type()?;
                if self.peek() == &Tok::Comma {
                    self.err_here("`own_alloc` の引数は型名 1 つです（例: `own_alloc(Block)`）", "`own_alloc` takes exactly one type name (e.g. `own_alloc(Block)`)");
                    return Err(());
                }
                let end = self.expect(Tok::RParen, "`)` が必要です", "`)` expected")?;
                Ok(self.new_expr(ExprKind::OwnAlloc(ty), start.to(end)))
            }
            Tok::Ident(name) if name == "own_alloc" && matches!(self.peek_at(1), Tok::ColonColon) => {
                self.err_here(
                    "ジェネリクスは使えません。`own_alloc(Block)` のように型名を引数として渡してください",
                    "generics are not supported; pass the type name as an argument, as in `own_alloc(Block)`",
                );
                Err(())
            }
            // 式の位置の `critical { .. }`（`let x = critical { .. };`）。構造体リテラルが書ける位置でだけ認識する
            // （構造体の名前は大文字で始まるので曖昧にならない。`if critical { .. }` の変数はそのまま）
            // `critical { .. }` in expression position (`let x = critical { .. };`), recognised only where a struct literal
            // could appear (struct names start with an upper-case letter, so it is never ambiguous; a variable in
            // `if critical { .. }` is untouched)
            Tok::Ident(w) if w == "critical" && self.peek_at(1) == &Tok::LBrace && !self.no_struct => {
                let start = self.bump().span;
                let b = self.parse_block()?;
                let sp = start.to(b.span);
                Ok(self.new_expr(ExprKind::Critical(b), sp))
            }
            // `let r = catch_fault { .. };` など式の位置の区間 / sections in expression position
            Tok::Ident(w) if (w == "atomic" || w == "rcu_read" || w == "catch_fault") && self.peek_at(1) == &Tok::LBrace && !self.no_struct => Ok(self.scope_expr()?.unwrap()),
            // 型名を第一引数に取る組み込み（ジェネリクスなし）: `bind_mmio(Regs, 番地)`・`dma_alloc(T)`・`complete_dma(T, iova)`
            // builtins taking a type name first (no generics)
            Tok::Ident(name) if (name == "bind_mmio" || name == "dma_alloc" || name == "complete_dma") && matches!(self.peek_at(1), Tok::LParen) => {
                let nsp = self.bump().span;
                self.bump();
                let ty = self.parse_type()?;
                let mut args = Vec::new();
                while self.eat(&Tok::Comma) {
                    if self.peek() == &Tok::RParen {
                        break;
                    }
                    let saved = self.no_struct;
                    self.no_struct = false;
                    let a = self.parse_expr();
                    self.no_struct = saved;
                    args.push(a?);
                }
                let end = self.expect(Tok::RParen, "`)` が必要です", "`)` expected")?;
                Ok(self.new_expr(ExprKind::TypeCall { name, name_span: nsp, ty, args }, start.to(end)))
            }
            // 浮動小数点の型の定数と関数: `f32::NAN`・`f64::INFINITY`・`f32::from_bits(b)`（パスはこの 2 つの型だけ）
            // constants and functions of the float types: `f32::NAN`, `f64::INFINITY`, `f32::from_bits(b)` (the only paths)
            Tok::Ident(name) if (name == "f32" || name == "f64") && self.peek_at(1) == &Tok::ColonColon && matches!(self.peek_at(2), Tok::Ident(_)) => {
                self.bump();
                self.bump();
                let Tok::Ident(item) = self.bump().tok.clone() else { return Err(()) };
                let full = format!("{}::{}", name, item);
                if self.peek() == &Tok::LParen {
                    let args = self.parse_args()?;
                    let sp = start.to(self.prev_span());
                    return Ok(self.new_expr(ExprKind::Call { callee: full, callee_span: start, args, turbofish: None }, sp));
                }
                let sp = start.to(self.prev_span());
                Ok(self.new_expr(ExprKind::Path(full), sp))
            }
            Tok::Ident(name) => {
                self.bump();
                // ターボフィッシュ `chan::<T>(..)` / turbofish
                let mut turbofish = None;
                if self.peek() == &Tok::ColonColon {
                    self.bump();
                    if self.eat(&Tok::Lt) {
                        turbofish = Some(self.parse_type()?);
                        self.expect_gt()?;
                    } else {
                        self.err_here("パス（`a::b`）は使えません", "paths (`a::b`) are not supported");
                        return Err(());
                    }
                }
                if self.peek() == &Tok::LParen {
                    let args = self.parse_args()?;
                    let sp = start.to(self.prev_span());
                    return Ok(self.new_expr(ExprKind::Call { callee: name, callee_span: start, args, turbofish }, sp));
                }
                if self.peek() == &Tok::LBrace && !self.no_struct && self.looks_like_struct_lit() {
                    return self.parse_struct_lit(name, start);
                }
                Ok(self.new_expr(ExprKind::Path(name), start))
            }
            Tok::Reserved(w) => {
                self.diags.push(Diag::error(
                    "F0001",
                    start,
                    format!("`{}` は Fascon 1.0 では使えません", w),
                    format!("`{}` is not supported in Fascon 1.0", w),
                ));
                Err(())
            }
            _ => {
                self.err_here("式が必要です", "an expression is expected");
                Err(())
            }
        }
    }

    fn parse_range_iter(&mut self) -> PResult<ForIter> {
        let lo = self.parse_expr()?;
        let inclusive = match self.peek() {
            Tok::DotDot => false,
            Tok::DotDotEq => true,
            _ => {
                self.err_here(
                    "`for` は範囲（`a..b` / `a..=b`）かアリーナ（`名前`）を回します",
                    "`for` iterates over a range (`a..b` / `a..=b`) or an arena (`name`)",
                );
                return Err(());
            }
        };
        self.bump();
        let hi = self.parse_expr()?;
        Ok(ForIter::Range { lo: Box::new(lo), hi: Box::new(hi), inclusive })
    }

    fn looks_like_struct_lit(&self) -> bool {
        // `Name {}` / `Name { field: ...` / `Name { field, ...`（省略記法は不可だが構造体と判断）
        match (self.peek_at(1), self.peek_at(2)) {
            (Tok::RBrace, _) => true,
            (Tok::Ident(_), Tok::Colon) => true,
            _ => false,
        }
    }

    fn parse_struct_lit(&mut self, name: String, name_span: Span) -> PResult<Expr> {
        self.expect(Tok::LBrace, "`{` が必要です", "`{` expected")?;
        let mut fields = Vec::new();
        while self.peek() != &Tok::RBrace {
            let (fname, fsp) = self.ident("フィールド名が必要です", "field name expected")?;
            if self.peek() != &Tok::Colon {
                self.err_here("`フィールド: 値` と書いてください（省略記法は使えません）", "write `field: value` (shorthand is not supported)");
                return Err(());
            }
            self.bump();
            let v = self.parse_expr()?;
            fields.push((fname, fsp, v));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        if self.peek() == &Tok::DotDot {
            self.err_here("構造体更新構文（`..base`）は使えません", "struct update syntax (`..base`) is not supported");
            return Err(());
        }
        let end = self.expect(Tok::RBrace, "`}` が必要です", "`}` expected")?;
        Ok(self.new_expr(ExprKind::StructLit { name, name_span, fields }, name_span.to(end)))
    }

    fn parse_if(&mut self) -> PResult<Expr> {
        let start = self.bump().span; // if
        if self.eat(&Tok::Let) {
            let pat = self.parse_pattern()?;
            self.expect(Tok::Eq, "`if let パターン = 式` の `=` が必要です", "`=` expected in `if let PATTERN = EXPR`")?;
            let saved = self.no_struct;
            self.no_struct = true;
            let scrut = self.parse_expr()?;
            self.no_struct = saved;
            let then = self.parse_block()?;
            let els = self.parse_else()?;
            let end = els.as_ref().map(|e| e.span).unwrap_or(then.span);
            return Ok(self.new_expr(ExprKind::IfLet { pat, scrut: Box::new(scrut), then, els }, start.to(end)));
        }
        let saved = self.no_struct;
        self.no_struct = true;
        let cond = self.parse_expr()?;
        self.no_struct = saved;
        let then = self.parse_block()?;
        let els = self.parse_else()?;
        let end = els.as_ref().map(|e| e.span).unwrap_or(then.span);
        Ok(self.new_expr(ExprKind::If { cond: Box::new(cond), then, els }, start.to(end)))
    }

    fn parse_else(&mut self) -> PResult<Option<Box<Expr>>> {
        if !self.eat(&Tok::Else) {
            return Ok(None);
        }
        if self.peek() == &Tok::If {
            return Ok(Some(Box::new(self.parse_if()?)));
        }
        let b = self.parse_block()?;
        let sp = b.span;
        Ok(Some(Box::new(self.new_expr(ExprKind::Block(b), sp))))
    }

    fn parse_pattern(&mut self) -> PResult<Pattern> {
        let start = self.span();
        match self.peek().clone() {
            Tok::Ident(s) if s == "_" => {
                self.bump();
                Ok(Pattern::Wild(start))
            }
            Tok::Ident(s) if s == "None" => {
                self.bump();
                Ok(Pattern::None(start))
            }
            Tok::Ident(s) if (s == "Ok" || s == "Err") && self.peek_at(1) == &Tok::LParen => {
                self.bump();
                self.bump();
                let inner = match self.peek().clone() {
                    Tok::Ident(n) if n == "_" => {
                        self.bump();
                        None
                    }
                    Tok::Mut => {
                        self.bump();
                        let (n, _) = self.ident("束縛名が必要です", "binding name expected")?;
                        let b = self.new_bind();
                        Some((n, b, true))
                    }
                    Tok::Ident(n) => {
                        self.bump();
                        let b = self.new_bind();
                        Some((n, b, false))
                    }
                    _ => {
                        self.err_here("`Ok(` / `Err(` の中には束縛名か `_` を書きます", "a binding name or `_` is expected inside `Ok(` / `Err(`");
                        return Err(());
                    }
                };
                let end = self.expect(Tok::RParen, "`)` が必要です", "`)` expected")?;
                Ok(if s == "Ok" { Pattern::Ok(inner, start.to(end)) } else { Pattern::Err(inner, start.to(end)) })
            }
            Tok::Ident(s) if s == "Some" => {
                self.bump();
                self.expect(Tok::LParen, "`Some(` が必要です", "`Some(` expected")?;
                let inner = match self.peek().clone() {
                    Tok::Ident(n) if n == "_" => {
                        self.bump();
                        None
                    }
                    Tok::Mut => {
                        self.bump();
                        let (n, _) = self.ident("束縛名が必要です", "binding name expected")?;
                        let b = self.new_bind();
                        Some((n, b, true))
                    }
                    Tok::Ident(n) => {
                        self.bump();
                        let b = self.new_bind();
                        Some((n, b, false))
                    }
                    _ => {
                        self.err_here("`Some(` の中には束縛名か `_` を書きます", "a binding name or `_` is expected inside `Some(`");
                        return Err(());
                    }
                };
                let end = self.expect(Tok::RParen, "`)` が必要です", "`)` expected")?;
                Ok(Pattern::Some(inner, start.to(end)))
            }
            Tok::Ident(s) => {
                self.bump();
                let b = self.new_bind();
                Ok(Pattern::Bind(s, b, start))
            }
            Tok::True => {
                self.bump();
                Ok(Pattern::Bool(true, start))
            }
            Tok::False => {
                self.bump();
                Ok(Pattern::Bool(false, start))
            }
            Tok::Minus => {
                self.bump();
                if let Tok::Int(v, _) = self.peek().clone() {
                    let end = self.bump().span;
                    Ok(Pattern::Int(-(v as i128), start.to(end)))
                } else {
                    self.err_here("整数が必要です", "an integer is expected");
                    Err(())
                }
            }
            Tok::Int(v, _) => {
                self.bump();
                if matches!(self.peek(), Tok::DotDot | Tok::DotDotEq | Tok::Pipe) {
                    self.err_here("範囲パターン・`|` パターンは使えません", "range and `|` patterns are not supported");
                    return Err(());
                }
                Ok(Pattern::Int(v as i128, start))
            }
            _ => {
                self.err_here("パターン（整数・true/false・Some(x)・None・_）が必要です", "a pattern (integer, true/false, Some(x), None, _) is expected");
                Err(())
            }
        }
    }
}

/// 2 周目: 1 周目の表（syms）を見ながら構文を読む / pass 2: parse, consulting the pass-1 table (syms)
pub fn parse(toks: &[Token], syms: &SymTab, reframe: bool) -> (Program, Vec<Diag>) {
    let mut p = Parser::new(toks, syms);
    p.reframe = reframe;
    let prog = p.parse_program();
    (prog, p.diags)
}
