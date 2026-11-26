//! 1.0 エンジンの入口（コンパイル・リージョン報告・解説） / entry points of the 1.0 engine (compile, region report, explain)

use crate::codegen::{Cg, CgOpts, CgReport};
use crate::diag::{self, Diag, Lang};
use crate::flow::{self, Flow};
use crate::lexer;
use crate::parser;
use crate::prescan;
use crate::reframe;
use crate::resolve;
use crate::sema::Sema;
use crate::span::SourceFile;
use crate::target::{Profile, Target};
use crate::types::{Tables, Ty};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Compiled {
    pub src: SourceFile,
    pub diags: Vec<Diag>,
    pub c: Option<String>,
    pub report: Option<CgReport>,
    pub summary: String,
    pub apostrophes: usize,
    /// ホバー用の宣言表（名前 → Markdown） / declarations for hover (name → Markdown)
    pub symbols: Vec<Symbol>,
    /// リフレーミング（Rust 構文）を使ったか / whether reframing (Rust syntax) was applied
    pub reframed: bool,
    /// リフレーミングで整数へ変えた文字リテラル・取り除いたライフタイムの数 / char literals and lifetimes reframed away
    pub reframed_marks: usize,
    pub reframe_report: reframe::Report,
    /// `--emit-strict-fascon` の本文（頼まれたときだけ） / the `--emit-strict-fascon` text (only when asked for)
    pub strict: Option<String>,
    /// ユーザープログラム（EL0）なら (仮想番地, 大きさ) / (virtual address, size) for a user program (EL0)
    pub user_layout: Option<(u64, u64)>,
    /// --target hpc の節（fascon regions） / the --target hpc section (fascon regions)
    pub hpc_summary: String,
}

/// コンパイルの設定 / compile settings
#[derive(Clone, Copy)]
pub struct Opts {
    pub lang: Lang,
    pub ptr_bits: u32,
    pub gen_c: bool,
    pub columns: bool,
    /// 完全列化（True SoA）。`--columns mirrored` で切る（写しの列だけ） / True SoA; `--columns mirrored` turns it off
    /// (mirrored columns only)
    pub true_soa: bool,
    /// 非時間的ストア（`--nontemporal=auto|on|off`） / non-temporal stores (`--nontemporal=auto|on|off`)
    pub nontemporal: crate::codegen::NtMode,
    /// None = ファイル先頭の `#![reframing(rust)]` に従う / None = follow `#![reframing(rust)]` at the top of the file
    pub reframing: Option<bool>,
    pub target: Target,
    /// OS のプロファイル（`--target rtos` / `--target generic-os`。既定は今までの 1.0 = Classic）
    /// the OS profile (`--target rtos` / `--target generic-os`; the default is plain 1.0 = Classic)
    pub profile: Profile,
    pub emit_strict: bool,
    /// モジュールの単独の検査の中（入れ子の検査はしない） / inside a per-module check (no nested checks)
    pub nested: bool,
    /// ボード定義から来た `user Str` の上限など / limits that came from a board definition (`user Str` etc.)
    pub user_str_max: Option<u64>,
    /// `--board` が付いている（fascon_board.h を読む C を作る） / `--board` was given (the C reads fascon_board.h)
    pub board: bool,
    /// `--topology`（`--target hpc` の計算ノードの形） / `--topology` (the shape of the `--target hpc` compute node)
    pub topology: Option<crate::target::Topology>,
}

impl Opts {
    pub fn new(lang: Lang, ptr_bits: u32) -> Opts {
        Opts { lang, ptr_bits, gen_c: false, columns: columns_default(), true_soa: true_soa_default(), nontemporal: nontemporal_default(), reframing: reframing_default(), target: target_default(), profile: profile_default(), emit_strict: false, nested: false, user_str_max: None, board: false, topology: None }
    }
}

/// ターゲットの既定（環境変数 FASCON_TARGET=x86_64 / aarch64 / rtos / generic-os+aarch64 …。VS Code の設定 fascon.target が
/// 言語サーバーへこれで渡す）
/// the default target (FASCON_TARGET=x86_64 / aarch64 / rtos / generic-os+aarch64 ...; the VS Code setting fascon.target
/// reaches the language server this way)
pub fn target_default() -> Target {
    std::env::var("FASCON_TARGET").ok().and_then(|v| crate::target::parse_spec(&v)).and_then(|(a, _)| a).unwrap_or(Target::Portable)
}

/// プロファイルの既定（FASCON_TARGET か FASCON_PROFILE。無ければ Classic） / the default profile (FASCON_TARGET or
/// FASCON_PROFILE; Classic when neither is set)
pub fn profile_default() -> Profile {
    for k in ["FASCON_PROFILE", "FASCON_TARGET"] {
        if let Some((_, Some(p))) = std::env::var(k).ok().and_then(|v| crate::target::parse_spec(&v)) {
            return p;
        }
    }
    Profile::Classic
}

/// リフレーミングの既定（環境変数 FASCON_REFRAMING=rust / off。VS Code の設定 fascon.reframing が言語サーバーへこれで渡す。
/// 無ければファイル先頭の `#![reframing(rust)]` に従う）
/// the default for reframing (FASCON_REFRAMING=rust / off; the VS Code setting fascon.reframing reaches the language server
/// this way; when unset, `#![reframing(rust)]` at the top of the file decides)
pub fn reframing_default() -> Option<bool> {
    match std::env::var("FASCON_REFRAMING").ok().map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("rust") | Some("on") | Some("1") | Some("true") | Some("yes") => Some(true),
        Some("off") | Some("0") | Some("false") | Some("no") | Some("fascon") => Some(false),
        _ => None,
    }
}

pub struct Symbol {
    /// 構造体・関数・static は名前そのまま、アリーナと MMIO は `@名前` / plain name for structs, fns, statics; `@name` for arenas and MMIO
    pub name: String,
    pub kind: u32,
    pub md: String,
    pub line: usize,
}

impl Compiled {
    pub fn has_errors(&self) -> bool {
        self.diags.iter().any(|d| d.is_error())
    }
}

/// このファイルを 1.0 エンジンで扱うか（1.0 構文の目印か `#![fascon(1)]` / `#![reframing(rust)]`）
/// should this file go to the 1.0 engine? (1.0 syntax markers, `#![fascon(1)]` or `#![reframing(rust)]`)
pub fn wants_v2(text: &str) -> bool {
    if reframe::detect(text) == Some(true) || reframing_default() == Some(true) {
        return true;
    }
    let lr = lexer::lex(text);
    lr.had_sigil || lexer::has_v2_markers(&lr.tokens)
}

pub fn compile(path: &str, text: &str, lang: Lang, ptr_bits: u32, gen_c: bool) -> Compiled {
    compile_with(path, text, lang, ptr_bits, gen_c, columns_default())
}

/// 列の既定（環境変数 FASCON_COLUMNS=off で切る。VS Code の設定 fascon.columns が言語サーバーへこれで渡す）
/// the default for columns (FASCON_COLUMNS=off turns them off; the VS Code setting fascon.columns reaches the language
/// server this way)
pub fn columns_default() -> bool {
    !matches!(std::env::var("FASCON_COLUMNS").ok().as_deref().map(|v| v.trim().to_ascii_lowercase()).as_deref(), Some("off") | Some("0") | Some("no") | Some("false"))
}

/// 完全列化の既定（FASCON_COLUMNS=mirrored で写しの列だけ） / the default for True SoA (FASCON_COLUMNS=mirrored keeps
/// mirrored columns only)
pub fn true_soa_default() -> bool {
    !matches!(std::env::var("FASCON_COLUMNS").ok().as_deref().map(|v| v.trim().to_ascii_lowercase()).as_deref(), Some("mirrored") | Some("mirror"))
}

/// 非時間的ストアの既定（FASCON_NONTEMPORAL=auto|on|off、既定 auto） / the default for non-temporal stores
/// (FASCON_NONTEMPORAL=auto|on|off, default auto)
pub fn nontemporal_default() -> crate::codegen::NtMode {
    parse_nontemporal(std::env::var("FASCON_NONTEMPORAL").ok().as_deref().unwrap_or("auto")).unwrap_or(crate::codegen::NtMode::Auto)
}

/// `auto` / `on` / `off` を読む / read `auto` / `on` / `off`
pub fn parse_nontemporal(v: &str) -> Option<crate::codegen::NtMode> {
    match v.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(crate::codegen::NtMode::Auto),
        "on" | "1" | "yes" | "true" => Some(crate::codegen::NtMode::On),
        "off" | "0" | "no" | "false" => Some(crate::codegen::NtMode::Off),
        _ => None,
    }
}

/// columns: `for id in @a` の集計ごとに列を足すか（`--columns off` で 1.0.0 と同じ表と走査）
/// columns: whether reductions get columns (`--columns off` gives the 1.0.0 tables and scans)
pub fn compile_with(path: &str, text: &str, lang: Lang, ptr_bits: u32, gen_c: bool, columns: bool) -> Compiled {
    let mut o = Opts::new(lang, ptr_bits);
    o.gen_c = gen_c;
    o.columns = columns;
    compile_opts(path, text, &o)
}

/// 全体の流れ / the whole pipeline
///   字句解析 →（リフレーミング）→ 1 周目: 宣言の名前の事前登録 → 2 周目: 構文解析 → 名前解決（`@` なしの名前・厳格形の脱糖衣）
///   → 意味解析（型推論は関数ごとに必要なら 2 周）→ 流れ解析 → C 生成 /（厳格形の書き出し）
///   lexing → (reframing) → pass 1: pre-registration of declared names → pass 2: parsing → name resolution (bare names,
///   desugaring of strict forms) → semantic analysis (type inference runs twice per function when needed) → flow analysis
///   → C generation / (strict-form output)
pub fn compile_opts(path: &str, text: &str, o: &Opts) -> Compiled {
    compile_user(path, text, o, false, None)
}

/// ファイルの先頭のコメント `// fascon-abi: kernel.fsc`: このファイルはユーザープログラム（EL0）で、システムコールの宣言を
/// そのカーネルのソースから読む（パスはこのファイルのフォルダから）
/// the comment `// fascon-abi: kernel.fsc` near the top of a file: the file is a user program (EL0) and takes its system
/// call declarations from that kernel source (the path is relative to this file's folder)
pub fn abi_directive(text: &str) -> Option<String> {
    for line in text.lines().take(40) {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let Some(rest) = l.strip_prefix("//") else { break };
        if let Some(p) = rest.trim().strip_prefix("fascon-abi:") {
            let p = p.trim();
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

/// カーネルのソースから、ユーザープログラムへ渡す宣言（本体の無い `syscall`・ワイヤ構造体）とユーザーメモリの配置を取り出す
/// extract from a kernel source the declarations handed to user programs (bodiless `syscall` items, wire structs) and the
/// placement of the user memory
fn abi_import(kernel_path: &str, kernel: &str, user_text: &str, ptr_bits: u32) -> (String, Option<(u64, u64)>) {
    use crate::ast::{FnKind, Item};
    let reframing = reframe::detect(kernel).unwrap_or(false);
    let lr = lexer::lex_mode(kernel, reframing);
    let toks = if reframing { reframe::rewrite(&lr.tokens).0 } else { lr.tokens.clone() };
    let syms = prescan::prescan(&toks);
    let (mut prog, _) = parser::parse(&toks, &syms, reframing);
    let _ = resolve::resolve(&mut prog, &syms);
    let mut s = Sema::new(&prog, ptr_bits);
    s.base_dir = std::path::Path::new(kernel_path).parent().map(|p| p.to_path_buf());
    s.run();
    // ユーザー側がすでに持っている名前は取り込まない / names the user program already defines are not imported
    let ulex = lexer::lex_mode(user_text, false);
    let usyms = prescan::prescan(&ulex.tokens);
    let (uprog, _) = parser::parse(&ulex.tokens, &usyms, false);
    let mut have: Vec<&str> = Vec::new();
    for it in &uprog.items {
        match it {
            Item::Struct(st) => have.push(&st.name),
            Item::Fn(f) => have.push(&f.name),
            _ => {}
        }
    }
    let mut out = String::new();
    for it in &prog.items {
        match it {
            Item::Struct(st) if !have.contains(&st.name.as_str()) => {
                if let Some(Some(be)) = s.t.struct_by_name.get(&st.name).map(|&i| s.t.structs[i].wire) {
                    out.push_str(&format!("#[wire({})] {}\n", if be { "be" } else { "le" }, &kernel[st.span.lo as usize..st.span.hi as usize]));
                }
            }
            Item::Fn(f) if matches!(f.kind, FnKind::Syscall(_)) && !have.contains(&f.name.as_str()) => {
                let Some(nr) = s.t.fn_by_name.get(&f.name).and_then(|&i| s.t.fns[i].syscall) else { continue };
                let ps: Vec<&str> = f.params.iter().map(|p| &kernel[p.span.lo as usize..p.span.hi as usize]).collect();
                out.push_str(&format!("syscall {} at {} ({}) -> i64;\n", f.name, nr, ps.join(", ")));
            }
            _ => {}
        }
    }
    let layout = s.t.umems.first().map(|u| (u.at, u.size));
    (out, layout)
}

/// ファイルの先頭 20 行の `// fascon-target: ..` / the `// fascon-target: ..` directive in the first 20 lines
pub fn target_directive(text: &str) -> Option<(Option<Target>, Option<Profile>)> {
    for l in text.lines().take(20) {
        let t = l.trim();
        if let Some(v) = t.strip_prefix("// fascon-target:") {
            return crate::target::parse_spec(v.trim());
        }
    }
    None
}

/// ファイルの先頭 20 行の `// fascon-topology: cores=64 smt=2 numa=4` / the `// fascon-topology:` directive in the first
/// 20 lines
pub fn topology_directive(text: &str) -> Option<crate::target::Topology> {
    for l in text.lines().take(20) {
        if let Some(v) = l.trim().strip_prefix("// fascon-topology:") {
            return crate::target::Topology::parse(v.trim(), 1);
        }
    }
    None
}

/// user = ユーザープログラム（EL0）として翻訳する。abi = システムコールの宣言を読むカーネルのソース
/// user = compile as a user program (EL0); abi = the kernel source its system call declarations come from
pub fn compile_user(path: &str, text: &str, o: &Opts, user: bool, abi: Option<&str>) -> Compiled {
    // `// fascon-target: rtos` / `// fascon-target: generic-os+aarch64`: ファイルの中でプロファイルを決める（--target が
    // 優先）/ choose the profile inside the file (--target wins)
    let mut o2 = o.clone();
    if let Some((a, p)) = target_directive(text) {
        if o.profile == Profile::Classic {
            if let Some(p) = p {
                o2.profile = p;
            }
        }
        if o.target == Target::Portable {
            if let Some(a) = a {
                o2.target = a;
            }
        }
    }
    let o = &o2;
    let (lang, ptr_bits, gen_c, columns) = (o.lang, o.ptr_bits, o.gen_c, o.columns);
    // 標準ライブラリのモジュール（<std>/net/tcp.fsc）にはフォルダが無い。キャッシュを今いるフォルダの <std>/ に作らない
    // a standard library module (<std>/net/tcp.fsc) has no folder: no cache under <std>/ in the current folder
    let base_dir = if path.starts_with("<std>") { None } else { std::path::Path::new(path).parent().map(|p| p.to_path_buf()) };
    let abi_path: Option<std::path::PathBuf> = match abi {
        Some(a) => Some(std::path::PathBuf::from(a)),
        None => abi_directive(text).map(|a| match &base_dir {
            Some(d) if !std::path::Path::new(&a).is_absolute() => d.join(a),
            _ => std::path::PathBuf::from(a),
        }),
    };
    let mut joined = String::new();
    let mut abi_err: Option<String> = None;
    let mut layout: Option<(u64, u64)> = None;
    if let Some(ap) = &abi_path {
        match std::fs::read_to_string(ap) {
            Ok(k) => {
                let (decls, lay) = abi_import(&ap.to_string_lossy(), &k, text, ptr_bits);
                layout = lay;
                joined = format!("{}\n// ───── ABI: {} ─────\n{}", text, ap.display(), decls);
            }
            Err(_) => abi_err = Some(ap.display().to_string()),
        }
    }
    let user = user || abi_path.is_some();
    let text: &str = if joined.is_empty() { text } else { &joined };
    // モジュール（`import net::tcp;`）の展開: つないだテキストを 1 回だけ字句解析し、トークンを名前空間へ書き換える
    // modules (`import net::tcp;`): the joined text is lexed once and its tokens are rewritten into namespaces
    let expanded = crate::modules::expand(path, text);
    let main_len = text.len() as u32;
    let text: &str = match &expanded {
        Some(ex) => &ex.text,
        None => text,
    };
    let src = SourceFile::new(path, text);
    let reframing = o.reframing.unwrap_or_else(|| reframe::detect(text).unwrap_or(false));
    let mut lr = lexer::lex_mode(text, reframing);
    let mut mod_diags: Vec<Diag> = Vec::new();
    if let Some(ex) = &expanded {
        mod_diags.extend(ex.diags.iter().cloned());
        mod_diags.extend(crate::modules::rewrite(&mut lr.tokens, ex, main_len));
        if !o.nested {
            for (m, f, ds) in crate::modules::check_modules(ex, base_dir.as_deref(), o) {
                let first = ds.first().cloned();
                if let Some(d0) = first {
                    let sp = expanded_import_span(text, &m);
                    mod_diags.push(
                        Diag::error(
                            "F3702",
                            sp,
                            format!("モジュール `{}`（{}）に {} 件のエラーがあります。最初: [{}] {}", m, f, ds.len(), d0.code, d0.ja),
                            format!("module `{}` ({}) has {} error(s); the first: [{}] {}", m, f, ds.len(), d0.code, d0.en),
                        )
                        .hint(format!("`fascon check {}` で全部を見られます", f), format!("`fascon check {}` shows all of them", f)),
                    );
                }
            }
        }
    }
    let mut diags: Vec<Diag> = lr.diags.clone();
    diags.extend(mod_diags);
    if let Some(p) = abi_err {
        diags.push(
            Diag::error("F2803", crate::span::Span::new(0, 1), format!("ABI のカーネルのソース `{}` が読めません", p), format!("cannot read the kernel source of the ABI, `{}`", p))
                .hint("`// fascon-abi: kernel.fsc`（このファイルのフォルダからのパス）か `--abi kernel.fsc` を確かめてください", "check `// fascon-abi: kernel.fsc` (a path relative to this file's folder) or `--abi kernel.fsc`"),
        );
    }
    let (toks, rep) = if reframing {
        let (t, d, r) = reframe::rewrite(&lr.tokens);
        diags.extend(d);
        (t, r)
    } else {
        (lr.tokens.clone(), reframe::Report::default())
    };
    // 1 周目: すべての構造体・アリーナ・関数・ポートの名前を先に登録 / pass 1: register every struct, arena, fn and port name first
    let syms = prescan::prescan(&toks);
    // 2 周目: 中身を読む / pass 2: read the bodies
    let (mut prog, pd) = parser::parse(&toks, &syms, reframing);
    diags.extend(pd);
    let shared = resolve::resolve(&mut prog, &syms);
    let parse_failed = diags.iter().any(|d| d.is_error());
    let mut s = Sema::new(&prog, ptr_bits);
    s.shared_places = shared;
    s.user_mode = user;
    s.base_dir = base_dir;
    s.profile = o.profile;
    s.t.id64 = o.profile.id64();
    // --target hpc: 計算ノードの形（--topology > ソースの `// fascon-topology:` > 既定） / the compute node's shape
    // (--topology > `// fascon-topology:` in the source > the default)
    if o.profile == Profile::Hpc {
        s.topology = Some(o.topology.or_else(|| topology_directive(text)).unwrap_or(crate::target::Topology::HPC_DEFAULT));
    }
    if let Some(m) = o.user_str_max {
        s.user_str_max = m;
    }
    s.run();
    s.check_smp();
    flow::compute_effects(&mut s);
    // ユニバーサル OS 拡張: Sleep 効果・rtos の再帰・rcu_read / atomic / #[trap] / #[secret] の規則（F3001・F32xx・F33xx・F34xx・F35xx）
    // universal-OS extension: the Sleep effect, recursion under rtos, the rules of rcu_read / atomic / #[trap] / #[secret]
    crate::universal::analyze(&mut s);
    // 実行ルートと文脈（割り込み・タスク・閉じた区間）: F2601 / F2603 と、切り替えうる呼び出しの効果
    // execution roots and contexts (interrupts, tasks, closed sections): F2601 / F2603 and the effects of calls that may switch
    crate::context::analyze(&mut s);
    // --target hpc: 計算コアの検査（F3901 / F3902 / F3903） / compute-core checks (F3901 / F3902 / F3903)
    crate::hpc::analyze(&mut s);
    diags.extend(std::mem::take(&mut s.diags));
    let sema_failed = diags.iter().any(|d| d.is_error());
    let fr = Flow::new(&s).run();
    diags.extend(fr.diags.clone());
    // コールバックの参照のエスケープ解析（F0520）。同じ場所の F0515 / F0308 は F0520 のタイムラインにまとめる
    // escape analysis of references given to callbacks (F0520); F0515 / F0308 at the same places are folded into the
    // F0520 timeline
    let esc = crate::escape::analyze(&s);
    if !esc.diags.is_empty() {
        diags.retain(|d| !esc.is_folded(d));
        diags.extend(esc.diags.iter().cloned());
    }
    if let Some(ex) = &expanded {
        crate::modules::locate(&mut diags, ex);
    }
    diag::sort_dedup(&mut diags);
    let mut c = None;
    let mut report = None;
    let mut strict = None;
    let failed = diags.iter().any(|d| d.is_error()) || parse_failed || sema_failed;
    if !failed {
        // 報告だけほしい場合も、世代検査・境界検査の数は C 生成で数える / counts come from a codegen run even when only a report is wanted
        let cg = Cg::new(&s, &fr, &src, CgOpts { lang, ptr_bits, version: VERSION, columns, target: o.target, profile: o.profile, board: o.board, true_soa: o.true_soa, nontemporal: o.nontemporal });
        let (code, cgrep) = cg.generate();
        // --target hpc: 計算コアから届く `for id in @a` の中で列の写し（Mirrored-SoA）を作り直す書き込み（F3902）
        // --target hpc: writes inside `for id in @a` reachable from compute cores that rebuild column mirrors
        // (Mirrored-SoA; F3902)
        let hd = crate::hpc::mirror_diags(&s, &cgrep);
        let hpc_failed = hd.iter().any(|d| d.is_error());
        diags.extend(hd);
        if gen_c && !hpc_failed {
            c = Some(code);
        }
        report = Some(cgrep);
        if o.emit_strict {
            strict = Some(crate::strict::print(&s, &src, reframing, &rep, lr.reframed_marks, lang));
        }
    }
    let user_layout = if s.user_mode { Some(layout.unwrap_or((0x1_0000_0000, 0x20_0000))) } else { None };
    let ts_layout: Vec<(String, Vec<String>)> = report.as_ref().map(|r| r.tsoa_layout.clone()).unwrap_or_default();
    let summary = layout_summary(&s.t, lang, ptr_bits, &ts_layout);
    let symbols = symbol_table(&s.t, &src, lang, ptr_bits);
    let hpc_summary = hpc_summary(&s, &src, lang);
    Compiled { src, diags, c, report, summary, apostrophes: lr.apostrophes, symbols, reframed: reframing, reframed_marks: lr.reframed_marks, reframe_report: rep, strict, user_layout, hpc_summary }
}

/// --target hpc の節: 計算ノードの形・コアごとの役割と置き場所・計算コアから届く関数・I/O と確保の検査の結果
/// the --target hpc section: the compute node's shape, each core's role and placement, functions reachable from compute
/// cores, and the results of the I/O and allocation checks
fn hpc_summary(s: &Sema, src: &SourceFile, lang: Lang) -> String {
    let Some(h) = &s.hpc else { return String::new() };
    let t = &h.topology;
    let line = |sp: crate::span::Span| src.line_col(sp.lo).0 + 1;
    let from = match t.source {
        2 => "--topology",
        1 => "// fascon-topology:",
        _ => lang.pick("プロファイルの既定", "the profile default"),
    };
    let mut out = String::new();
    out.push_str(&format!("  --target hpc: {} {} ({}){}\n", lang.pick("計算ノード", "compute node"), t.spec(), from, if h.no_smt { lang.pick("、#[no_smt]", ", #[no_smt]") } else { "" }));
    let main = s.t.fns.iter().position(|f| f.is_main);
    out.push_str(&format!(
        "    {:<8} {}: {} — {} {}, NUMA {} {} ({})\n",
        "core 0",
        lang.pick("管理コア", "management"),
        main.map_or("main".to_string(), |m| s.t.fns[m].name.clone()),
        lang.pick("物理コア", "physical core"),
        t.phys(0),
        lang.pick("ノード", "node"),
        t.node(0),
        lang.pick("main の入口でピン留め", "pinned at main's entry")
    ));
    for (f, sp) in &h.mgmt_spawns {
        out.push_str(&format!("    {:<8} {}: `{}` ({} {})\n", "core 0", lang.pick("管理コアで走る spawn_on", "spawn_on onto the management core"), s.t.fns[*f].name, lang.pick("行", "line"), line(*sp)));
    }
    for r in &h.roots {
        match r.core {
            Some(c) => out.push_str(&format!(
                "    {:<8} {}: `{}` ({} {}) — {} {}, NUMA {} {} ({})\n",
                format!("core {}", c),
                lang.pick("計算コアのルート", "compute root"),
                s.t.fns[r.func].name,
                lang.pick("行", "line"),
                line(r.span),
                lang.pick("物理コア", "physical core"),
                t.phys(c),
                lang.pick("ノード", "node"),
                t.node(c),
                lang.pick("spawn_on の入口でピン留めし、コアごとのアリーナ・fascon__cpu・own ヒープをこのノードへ固定", "pinned at the spawn_on entry; per-core arenas, fascon__cpu and the own heap bound to this node")
            )),
            None => out.push_str(&format!(
                "    {:<8} {}: `{}` ({} {}) — {}\n",
                "core ?",
                lang.pick("計算コアのルート", "compute root"),
                s.t.fns[r.func].name,
                lang.pick("行", "line"),
                line(r.span),
                lang.pick("コア番号は実行時に決まる（F3903 の検査は定数だけ）", "the core number is decided at run time (F3903 checks constants only)")
            )),
        }
    }
    for (a, nd) in &h.numa_arenas {
        out.push_str(&format!("    #[numa_node({})] {} → NUMA {} {} ({})\n", nd, s.t.arenas[*a].name, lang.pick("ノード", "node"), nd, lang.pick("main の入口でコア 0 が固定", "bound by core 0 at main's entry")));
    }
    let mut reach: Vec<usize> = h.compute.keys().copied().collect();
    reach.sort();
    let names: Vec<String> = reach.iter().map(|f| s.t.fns[*f].name.clone()).collect();
    out.push_str(&format!("    {}: {}\n", lang.pick("計算コアから届く関数", "functions reachable from compute cores"), if names.is_empty() { lang.pick("なし", "none").to_string() } else { names.join(", ") }));
    let mut any_io = false;
    for x in &h.io {
        any_io = true;
        out.push_str(&format!(
            "    F3901 {}: {} — `{}` ({} {}){}\n",
            lang.pick("計算コアから届く I/O", "I/O reachable from compute cores"),
            lang.pick(&x.what_ja, &x.what_en),
            s.t.fns[x.func].name,
            lang.pick("行", "line"),
            line(x.span),
            if x.allowed { lang.pick("（#[allow(hpc_io)] で外した）", " (lifted by #[allow(hpc_io)])") } else { "" }
        ));
    }
    if !any_io {
        out.push_str(&format!("    F3901 {}: {}\n", lang.pick("計算コアから届く I/O", "I/O reachable from compute cores"), lang.pick("なし", "none")));
    }
    out
}

fn symbol_table(t: &Tables, src: &SourceFile, lang: Lang, ptr_bits: u32) -> Vec<Symbol> {
    let ptr = (ptr_bits / 8) as u64;
    let mut v = Vec::new();
    let names = |xs: &Vec<usize>| xs.iter().map(|a| t.arenas[*a].name.clone()).collect::<Vec<_>>().join(", ");
    for st in &t.structs {
        let (sz, al) = size_align(t, &Ty::Struct(t.struct_by_name[&st.name]), ptr);
        let fields: Vec<String> = st.fields.iter().map(|f| format!("    {}: {},", f.name, t.display(&f.ty))).collect();
        let mut md = format!("```rust\nstruct {} {{\n{}\n}}\n```\nC: `fsc_{}` — {} {} B / {} {} B", st.name, fields.join("\n"), st.name, lang.pick("サイズ", "size"), sz, lang.pick("整列", "align"), al);
        if st.needs_drop {
            md.push_str(lang.pick("\n\nown を含む: スコープの終わりで解放処理（drop glue）が走る", "\n\ncontains own: drop glue runs at the end of its scope"));
        }
        v.push(Symbol { name: st.name.clone(), kind: 23, md, line: src.line_of(st.span.lo) });
    }
    for f in &t.fns {
        let ps: Vec<String> = f.params.iter().map(|p| format!("{}{}: {}", if p.mutable { "mut " } else { "" }, p.name, t.display(&p.ty))).collect();
        let ret = if f.ret == Ty::Unit { String::new() } else { format!(" -> {}", t.display(&f.ret)) };
        let mut md = format!("```rust\nfn {}({}){}\n```", f.name, ps.join(", "), ret);
        let e = &f.effects;
        if !e.reads.is_empty() || !e.writes.is_empty() || !e.frees.is_empty() {
            md.push_str(&format!(
                "\n\n{}: {} {{{}}} · {} {{{}}} · {} {{{}}}",
                lang.pick("アリーナ効果", "arena effects"),
                lang.pick("読む", "reads"),
                names(&e.reads),
                lang.pick("書く", "writes"),
                names(&e.writes),
                lang.pick("解放", "frees"),
                names(&e.frees)
            ));
        }
        v.push(Symbol { name: f.name.clone(), kind: 12, md, line: src.line_of(f.span.lo) });
    }
    for a in &t.arenas {
        let (sz, al) = size_align(t, &a.elem, ptr);
        let md = if a.bump {
            let l = bump_layout(sz, al, a.cap);
            format!(
                "```rust\nbump_arena {}: {}[{}];\n```\n{}: {} B × (2^{} + 1) + {} {} B → {} B — `Id<{}>` = {} {} bit | {} {} bit\n\n{}",
                a.name,
                t.display(&a.elem),
                a.cap,
                lang.pick("静的な表（.bss）", "static table (.bss)"),
                l.slot,
                l.k,
                lang.pick("世代（64 ビット）", "generations (64-bit)"),
                l.gen_link,
                l.total,
                a.name,
                lang.pick("世代", "generation"),
                64 - l.k,
                lang.pick("添字", "index"),
                l.k,
                lang.pick(
                    "確保は先頭を 1 進めるだけ。確保した関数（呼び出しリージョン）が戻るとき、先頭を入口の印へ戻す 1 回の格納でまとめて解放（O(1)）。Id を返す関数・&mut 引数で Id を渡す関数の確保は呼び出し元のリージョンへ昇格。古い Id は「添字 < 先頭」と世代で検出（None / ラッチ）。要素は own を持てない（F2103）、長く生きる表・static には Id を置けない（F2402）、1 つずつの free は無い（F2401）。",
                    "Allocation just advances the top; when the allocating function (the call region) returns, one store resets the top to the mark taken at entry and releases everything at once (O(1)). Allocations of functions that return Ids or pass them out through &mut parameters are promoted to the caller's region. A stale Id is caught by \"index < top\" plus the generation (None / latch). Elements cannot hold own (F2103), Ids cannot be stored in long-lived tables or statics (F2402), and there is no per-slot free (F2401)."
                )
            )
        } else {
            let l = arena_layout(sz, al, a.cap);
            format!(
                "```rust\narena {}: {}[{}];\n```\n{}: {} B × (2^{} + 1) + {} {} B → {} B — `Id<{}>` = {} {} bit | {} {} bit\n\n{}",
                a.name,
                t.display(&a.elem),
                a.cap,
                lang.pick("静的な表（.bss）", "static table (.bss)"),
                l.slot,
                l.k,
                lang.pick("生存ビットマップ・世代・リンク", "live bitmap/generation/link"),
                l.gen_link,
                l.total,
                a.name,
                lang.pick("世代", "generation"),
                32 - l.k,
                lang.pick("添字", "index"),
                l.k,
                lang.pick(
                    &format!("`{n}[id]` は世代タグで検証するルックアップ（`{n}.get_mut(id).unwrap()` と同じ）。別のアリーナの Id は型が違うので渡せない（F2101）。解放済みの Id は世代が合わず None になる（ABA 防止）。1 スロット {g} 世代、アリーナ全体で {l} 回の確保まで（世代は一周しない）。", n = a.name, g = l.gens, l = l.lifetime),
                    &format!("`{n}[id]` is a generation-verified lookup (the same as `{n}.get_mut(id).unwrap()`). An Id of another arena has a different type and cannot be passed (F2101). A freed Id no longer matches the generation and yields None (ABA prevention). {g} generations per slot, {l} allocations over the arena's life (generations never wrap).", n = a.name, g = l.gens, l = l.lifetime)
                )
            )
        };
        v.push(Symbol { name: a.name.clone(), kind: 9, md, line: src.line_of(a.span.lo) });
    }
    for m in &t.mmios {
        let shape = m.dims.iter().rev().fold(m.elem.name().to_string(), |acc, d| format!("[{}; {}]", acc, d));
        let md = format!(
            "```rust\n{}mmio {}: {} at {:#x};\n```\n{} B — {}\n\n{}",
            if m.framebuffer { "#[framebuffer] " } else { "" },
            m.name,
            shape,
            m.addr,
            m.size_bytes(),
            if m.framebuffer { lang.pick("通常メモリ（書き込みの結合・ベクトル化可）", "normal memory (write combining, vectorization allowed)") } else { lang.pick("デバイスメモリ（volatile + I/O バリア）", "device memory (volatile + I/O barriers)") },
            lang.pick("添字は静的に範囲を証明できれば検査なし、できなければ 1 回の比較（定数の範囲外は F2201）。", "An index proven in range statically has no check; otherwise one compare (a constant out of range is F2201).")
        );
        v.push(Symbol { name: m.name.clone(), kind: 9, md, line: src.line_of(m.span.lo) });
    }
    for p in &t.ports {
        let shape = match p.len {
            Some(l) => format!("[{}; {}]", p.elem.name(), l),
            None => p.elem.name().to_string(),
        };
        let a64 = crate::target::describe(&crate::target::aarch64_map(p.port, p.elem.bits(64), p.port, p.mmio_map));
        let md = format!(
            "```rust\nport_io {}: {} at_port {:#x};\n```\n{}\n\n- x86_64: `{}`\n- AArch64: {}\n- PC: {}",
            p.name,
            shape,
            p.port,
            lang.pick("I/O ポート（x86 の I/O 空間。MMIO の物理アドレス空間とは別）", "an I/O port (the x86 I/O space, separate from the MMIO physical address space)"),
            match p.elem.bits(64) {
                8 => "out dx, al / in al, dx",
                16 => "out dx, ax / in ax, dx",
                _ => "out dx, eax / in eax, dx",
            },
            a64,
            lang.pick("プラットフォーム層が真似る（COM1 の送信は標準出力）", "emulated by the platform layer (COM1 transmit goes to stdout)")
        );
        v.push(Symbol { name: p.name.clone(), kind: 9, md, line: src.line_of(p.span.lo) });
    }
    for st in &t.statics {
        let val = st.int_value.map(|x| format!(" = {}", x)).unwrap_or_default();
        v.push(Symbol { name: st.name.clone(), kind: 21, md: format!("```rust\nstatic {}: {}{}\n```", st.name, t.display(&st.ty), val), line: src.line_of(st.span.lo) });
    }
    v
}

// ───────────── C のレイアウト計算（報告用） / C layout computation (for reports) ─────────────

pub fn size_align(t: &Tables, ty: &Ty, ptr: u64) -> (u64, u64) {
    size_align_d(t, ty, ptr, 0)
}

/// 再帰する構造体（F2030 のエラーになるもの）でも止まるように深さを数える / count depth so recursive structs (F2030 errors) terminate
fn size_align_d(t: &Tables, ty: &Ty, ptr: u64, depth: u32) -> (u64, u64) {
    if depth > 64 {
        return (0, 1);
    }
    let size_align = |t: &Tables, ty: &Ty, ptr: u64| size_align_d(t, ty, ptr, depth + 1);
    match ty {
        Ty::Int(i) => {
            let b = (i.bits(ptr as u32 * 8) / 8) as u64;
            (b, b)
        }
        Ty::IntVar(_) | Ty::Var(_) => (4, 4),
        Ty::Float(f) => ((f.bits() / 8) as u64, (f.bits() / 8) as u64),
        Ty::FloatVar(_) | Ty::GpuFence(_) => (8, 8),
        Ty::Bool => (1, 1),
        Ty::Unit | Ty::Never | Ty::Err => (0, 1),
        Ty::Slice(_) => (16, 8),
        Ty::Str => {
            let a = ptr;
            (round(ptr + 4, a), a)
        }
        Ty::Own(_) | Ty::Ref(..) | Ty::Fn(..) | Ty::DmaOwn(_) | Ty::MmioBind(_) => (ptr, ptr),
        Ty::Fault => (8, 8),
        // ユーザーの番地（と長さ）: レジスタの値 / a user address (and length): register values
        Ty::User(i) => if matches!(**i, Ty::Str) { (16, 8) } else { (8, 8) },
        Ty::Result(i, e) => {
            let (s, a) = size_align(t, i, ptr);
            let (es, ea) = size_align(t, e, ptr);
            let a = a.max(ea).max(1);
            (round(round(round(1, a) + s, ea.max(1)) + es, a), a)
        }
        Ty::Id(a) | Ty::Dma(a) | Ty::GpuBuf(a) => {
            let b = (t.id_bits(*a) / 8) as u64;
            (b, b)
        }
        Ty::Option(i) => match &**i {
            Ty::Own(_) | Ty::Ref(..) | Ty::Id(_) | Ty::Str | Ty::Fn(..) => size_align(t, i, ptr),
            other => {
                let (s, a) = size_align(t, other, ptr);
                let a = a.max(1);
                (round(round(1, a) + s, a), a)
            }
        },
        Ty::Array(e, n) => {
            let (s, a) = size_align(t, e, ptr);
            (s * n, a)
        }
        Ty::Struct(sid) => {
            let mut off = 0u64;
            // #[repr(align(N))] は整列を N 以上に上げる / #[repr(align(N))] raises the alignment to at least N
            let mut al = (t.structs[*sid].align as u64).max(1);
            for f in &t.structs[*sid].fields {
                let (s, a) = size_align(t, &f.ty, ptr);
                off = round(off, a) + s;
                al = al.max(a);
            }
            (round(off.max(if t.structs[*sid].fields.is_empty() { 1 } else { 0 }), al), al)
        }
    }
}

fn round(x: u64, a: u64) -> u64 {
    if a <= 1 {
        x
    } else {
        (x + a - 1) / a * a
    }
}

/// アリーナの表の大きさ（生成 C の f20_arena_t と同じ並び） / the size of an arena's table (same layout as f20_arena_t in the C)
struct ArenaLayout {
    k: u32,
    slot: u64,
    gen_link: u64,
    total: u64,
    gens: String,
    lifetime: String,
}

/// バンプアリーナの表: st（128 B）| gen[2^K]（64 ビット）| slot[2^K + 1] / a bump arena's table
fn bump_layout(elem_size: u64, elem_align: u64, cap: u32) -> ArenaLayout {
    let k = crate::codegen::arena_k(cap);
    let n = 1u64 << k;
    let slot = round(elem_size.max(1), elem_align.max(16));
    let gen = round(8 * n, 128);
    let total = round(round((n + 1) * slot, 128) + 128 + gen, 128);
    let gens = if k == 0 { u64::MAX } else { (1u64 << (64 - k)) - 1 };
    ArenaLayout { k, slot, gen_link: gen, total, gens: group3(gens), lifetime: String::new() }
}

fn arena_layout(elem_size: u64, elem_align: u64, cap: u32) -> ArenaLayout {
    arena_layout_n(elem_size, elem_align, cap, false)
}

/// natural: 完全列化の表（スロットは残った部分の自然な整列。16 バイトに上げない） / natural: a True-SoA table (the slot keeps
/// the natural alignment of the part left in it, not raised to 16 bytes)
fn arena_layout_n(elem_size: u64, elem_align: u64, cap: u32, natural: bool) -> ArenaLayout {
    let k = crate::codegen::arena_k(cap);
    let n = 1u64 << k;
    let slot = if natural { round(elem_size.max(1), elem_align.max(1)) } else { round(elem_size.max(1), elem_align.max(16)) };
    // st（128 B）| live[] | gen[] | link[] | slot[2^K + 1]（各 128 B 境界） / each on a 128-byte boundary
    let live = round(8 * ((n + 63) / 64), 128);
    let gen = round(crate::codegen::gen_bytes(k) * n, 128);
    let link = round(4 * n, 128);
    let total = round(round((n + 1) * slot, 128) + 128 + live + gen + link, 128);
    let gens = (1u64 << (32 - k)) - 1;
    ArenaLayout { k, slot, gen_link: live + gen + link, total, gens: group3(gens), lifetime: group3(gens * cap as u64) }
}

/// 表示幅（全角は 2）で右を空白で埋める / pad on the right to a display width (full-width characters count 2)
fn pad_display(s: &str, width: usize) -> String {
    let w: usize = s.chars().map(|c| if (c as u32) >= 0x1100 { 2 } else { 1 }).sum();
    if w >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - w))
    }
}

fn group3(v: u64) -> String {
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

fn layout_summary(t: &Tables, lang: Lang, ptr_bits: u32, ts_layout: &[(String, Vec<String>)]) -> String {
    let ptr = (ptr_bits / 8) as u64;
    let mut out = String::new();
    for a in &t.arenas {
        let (mut sz, mut al) = size_align(t, &a.elem, ptr);
        // 完全列化の表: スロットに残る部分（列に出たフィールドを除く）を自然な整列で / a True-SoA table: the part left in the
        // slot (without the fields that moved to columns), naturally aligned
        let ts = ts_layout.iter().find(|(n, _)| *n == a.name).map(|(_, f)| f.clone());
        if let (Some(fs), Ty::Struct(sid)) = (&ts, &a.elem) {
            let mut off = 0u64;
            let mut pal = 1u64;
            for f in t.structs[*sid].fields.iter().filter(|f| !fs.contains(&f.name)) {
                let (s1, a1) = size_align(t, &f.ty, ptr);
                off = round(off, a1) + s1;
                pal = pal.max(a1);
            }
            sz = round(off.max(1), pal);
            al = pal;
        }
        if a.bump {
            let l = bump_layout(sz, al, a.cap);
            out.push_str(&format!(
                "  bump  {:<11} {}[{}]  {} = {} B × (2^{} + {}) + gen {} B → {} B\n",
                a.name,
                t.display(&a.elem),
                a.cap,
                lang.pick("スロット", "slot"),
                l.slot,
                l.k,
                lang.pick("番兵 1", "1 sentinel"),
                l.gen_link,
                l.total
            ));
            out.push_str(&format!(
                "  {:<17} {}\n",
                "",
                lang.pick(
                    &format!("Id = 世代 {} ビット | 添字 {} ビット（64 ビット）: 確保は先頭を進めるだけ、解放は呼び出しリージョンの出口で 1 回の格納（O(1)）", 64 - l.k, l.k),
                    &format!("Id = generation {} bits | index {} bits (64-bit): allocation advances the top, release is one store at the call region's exit (O(1))", 64 - l.k, l.k)
                )
            ));
            continue;
        }
        let l = arena_layout_n(sz, al, a.cap, ts.is_some());
        out.push_str(&format!(
            "  arena {:<11} {}[{}]  {} = {} B × (2^{} + {}) + live/gen/link {} B → {} B{}\n",
            a.name,
            t.display(&a.elem),
            a.cap,
            lang.pick("スロット", "slot"),
            l.slot,
            l.k,
            lang.pick("番兵 1", "1 sentinel"),
            l.gen_link,
            l.total,
            match &ts {
                Some(fs) => lang.pick(&format!("（完全列化: {} は列、列の大きさは下の一覧）", fs.join(", ")), &format!(" (True SoA: {} in columns, sized below)", fs.join(", "))).to_string(),
                None => String::new(),
            }
        ));
        if t.id64 {
            // 64 ビット Id（generic-os・hpc）: [世代 32 | 添字 32]。表の大きさは K で決まる / 64-bit Ids (generic-os, hpc):
            // [generation 32 | index 32]; the table size still follows K
            out.push_str(&format!(
                "  {:<17} {}\n",
                "",
                lang.pick(
                    &format!("Id = 64 ビット [世代 32 ビット | 添字 32 ビット]（表は 2^{} スロット）: 1 スロット 4,294,967,295 世代", l.k),
                    &format!("Id = 64 bits [generation 32 bits | index 32 bits] (the table has 2^{} slots): 4,294,967,295 generations per slot", l.k)
                )
            ));
        } else {
            out.push_str(&format!(
                "  {:<17} {}\n",
                "",
                lang.pick(
                    &format!("Id = 世代 {} ビット | 添字 {} ビット（K = {}）: 1 スロット {} 世代・アリーナ全体で {} 回の確保まで", 32 - l.k, l.k, l.k, l.gens, l.lifetime),
                    &format!("Id = generation {} bits | index {} bits (K = {}): {} generations per slot, {} allocations over the arena's life", 32 - l.k, l.k, l.k, l.gens, l.lifetime)
                )
            ));
        }
        // gpu_arena: 置き場所と貸し出しの道筋 / gpu_arena: where it lives and how slots are lent
        if a.gpu {
            let off = crate::universal::GPU_OFF.load(std::sync::atomic::Ordering::Relaxed);
            out.push_str(&format!(
                "  {:<17} {}\n",
                "",
                if off {
                    lang.pick(
                        "gpu_arena: .bss.fascon_gpu（2 MiB 境界）。--gpu=off: dispatch は呼んだコアでその場で走り、すぐ完了する（fascon_plat_gpu_* は呼ばない）",
                        "gpu_arena: .bss.fascon_gpu (2 MiB aligned); --gpu=off: dispatch runs in place on the calling core and completes at once (no fascon_plat_gpu_* calls)",
                    )
                } else {
                    lang.pick(
                        "gpu_arena: .bss.fascon_gpu（2 MiB 境界・ピン留め）。dispatch / reclaim / reclaim_wait は fascon_plat_gpu_register / dispatch / poll / wait だけを通す",
                        "gpu_arena: .bss.fascon_gpu (2 MiB aligned, pinned); dispatch / reclaim / reclaim_wait go through fascon_plat_gpu_register / dispatch / poll / wait only",
                    )
                }
            ));
        }
    }
    for p in &t.ports {
        let shape = match p.len {
            Some(l) => format!("[{}; {}]", p.elem.name(), l),
            None => p.elem.name().to_string(),
        };
        let a64 = crate::target::describe(&crate::target::aarch64_map(p.port, p.elem.bits(64), p.port, p.mmio_map));
        out.push_str(&format!(
            "  port  {:<11} {} at_port {:#x}  (x86_64: in / out, AArch64: {})\n",
            p.name, shape, p.port, a64
        ));
    }
    for m in &t.mmios {
        let shape = m.dims.iter().rev().fold(m.elem.name().to_string(), |acc, d| format!("[{}; {}]", acc, d));
        out.push_str(&format!(
            "  mmio  {:<11} {} at {:#x}  {} B  ({})\n",
            m.name,
            shape,
            m.addr,
            m.size_bytes(),
            if m.framebuffer { lang.pick("フレームバッファ: 通常メモリ", "framebuffer: normal memory") } else { lang.pick("デバイス: volatile + I/O バリア", "device: volatile + I/O barriers") }
        ));
    }
    let mut owns = Vec::new();
    for s in &t.structs {
        for f in &s.fields {
            if let Ty::Own(_) = f.ty {
                owns.push(format!("{}.{}", s.name, f.name));
            }
        }
    }
    if !owns.is_empty() {
        out.push_str(&format!("  own {}: {}\n", lang.pick("フィールド（構築時に親の所有リージョンへ詰める）", "fields (packed into the parent's own region at construction)"), owns.join(", ")));
    }
    out
}

/// `fascon regions` の本文 / the body of `fascon regions`
pub fn regions_text(cm: &Compiled, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}: {}\n", lang.pick("1.0 エンジン（リージョン・アリーナ・own・MMIO の確定結果）", "1.0 engine (regions, arenas, own, MMIO)"), cm.src.path));
    out.push_str(&cm.summary);
    out.push_str(&cm.hpc_summary);
    if let Some(r) = &cm.report {
        for (line, name, packed) in &r.own_sites {
            // 構造体の own は `own T { .. }`、構造体でない own（`own i32` / `own [u8; 64]`）は `own T` と表示する
            // struct owns print as `own T { .. }`, non-struct owns (`own i32` / `own [u8; 64]`) as `own T`
            let shown = if name.starts_with(|c: char| c.is_ascii_uppercase()) && name != "Str" && !name.starts_with("Option<") && !name.starts_with("Id<") {
                format!("{} {{ .. }}", name)
            } else {
                name.clone()
            };
            let name = &shown;
            if *packed > 0 {
                out.push_str(&format!(
                    "  {:>5}{}  own {}  → {}\n",
                    line,
                    lang.pick("行目", ""),
                    name,
                    lang.pick(&format!("所有リージョン 1 個（子 {} 個を詰めて 1 回の確保・1 回の解放）", packed), &format!("one own region ({} children packed: one allocation, one free)", packed))
                ));
            } else {
                out.push_str(&format!("  {:>5}{}  own {}  → {}\n", line, lang.pick("行目", ""), name, lang.pick("所有リージョン 1 個", "one own region")));
            }
        }
        out.push_str(&format!(
            "  {}: {} {} / {} {} / {} {} / {} {}\n",
            lang.pick("世代検査", "generation checks"),
            r.gen_elided,
            lang.pick("回は生存を静的に証明して省略", "elided by a static liveness proof"),
            r.gen_latched,
            lang.pick("回は分岐なし（番兵 + ラッチ、止めるのはコミット点）", "branch-free (sentinel + latch, stopped at commit points)"),
            r.gen_lookups,
            lang.pick("回は Option のルックアップ（None の腕はソースが書く）", "Option lookups (the source handles None)"),
            r.gen_checked,
            lang.pick("回はその場で止める（own を持つ要素）", "stop on the spot (elements holding own)")
        ));
        for (line, name) in &r.frame_sites {
            out.push_str(&format!(
                "  {:>5}{}  {}  → {}\n",
                line,
                lang.pick("行目", ""),
                name,
                lang.pick("フレームリージョン（どこへも移動しない: 先頭ポインタを進めるだけ、スコープの終わりに戻す）", "frame region (never moved: a pointer bump, popped at scope end)")
            ));
        }
        if r.reductions > 0 {
            out.push_str(&format!(
                "  {}: {}{}\n",
                lang.pick("集計の特殊化（`for id in a` を分岐なしの表の直線走査に）", "reduction specializations (`for id in a` as a branch-free straight table scan)"),
                r.reductions,
                if r.masked_reductions > 0 { lang.pick(&format!("（うちマスク付き {}: 条件・個数・最小 / 最大）", r.masked_reductions), &format!(" ({} masked: filters, counts, min / max)", r.masked_reductions)).to_string() } else { String::new() }
            ));
        }
        if !r.columns.is_empty() {
            let added: u64 = r.columns.iter().map(|c| c.bytes).sum();
            out.push_str(&format!(
                "  {}: {} {}\n",
                lang.pick("列を読む集計（スロットを読まず、その集計が読む値だけの列を走査）", "reductions that read columns (they scan a column of just the values they read, never the slots)"),
                r.column_reductions,
                lang.pick(&format!("（列は表に {} B を足す）", group3(added)), &format!("(the columns add {} B to the tables)", group3(added)))
            ));
            for c in &r.columns {
                let lines = c.lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(", ");
                let name = format!("{}.{}", c.arena, c.name);
                let size = format!("{}{}", c.per_slot, lang.pick("/スロット", "/slot"));
                out.push_str(&format!(
                    "    {} {} {} {} {:>10} B  ({} {})\n",
                    pad_display(&name, 28),
                    pad_display(lang.pick(&c.mode_ja, &c.mode_en), 26),
                    pad_display(lang.pick(&c.kind_ja, &c.kind_en), 26),
                    pad_display(&size, 14),
                    group3(c.bytes),
                    lang.pick("行", "line"),
                    lines
                ));
            }
            out.push_str(&format!(
                "  {}: {}\n",
                lang.pick("列の写しを作り直す場所（Id での書き込み・&mut の借用の終わり・&mut を渡した呼び出し）", "places that rebuild column mirrors (writes by Id, ends of &mut borrows, calls given &mut)"),
                r.mirror_sites
            ));
        }
        if !r.column_skips.is_empty() {
            out.push_str(&format!("  {}:\n", lang.pick("列を作らずフェーズ 1 の走査（生存ビットマップ + スロット）に戻した集計", "reductions kept on the phase-1 scan (live bitmap + slots) without columns")));
            for (a, line, ja, en) in &r.column_skips {
                out.push_str(&format!("    {} {} {}: {}\n", a, lang.pick("行", "line"), line, lang.pick(ja, en)));
            }
        }
        if !r.tsoa_skips.is_empty() {
            out.push_str(&format!(
                "  {}:\n",
                lang.pick(
                    "完全列化（True SoA）にしなかったフィールド（スロットに残り、列があればそれは Mirrored-SoA (dual-write)）",
                    "fields kept off True SoA (they stay in the slot; any column they have is Mirrored-SoA (dual-write))"
                )
            ));
            for (a, f, ja, en) in &r.tsoa_skips {
                out.push_str(&format!("    {}.{}: {}\n", a, f, lang.pick(ja, en)));
            }
        }
        if !r.nt_loops.is_empty() {
            out.push_str(&format!("  {}:\n", lang.pick("完全列化の列への一括上書きのループ", "bulk-overwrite loops over True-SoA columns")));
            for (a, f, line, on, ja, en) in &r.nt_loops {
                let how = if *on { lang.pick("非時間的ストア（F20_NT_STORE）", "non-temporal stores (F20_NT_STORE)") } else { lang.pick("普通のストア", "plain stores") };
                out.push_str(&format!("    {}.{} {} {}: {} — {}\n", a, f, lang.pick("行", "line"), line, how, lang.pick(ja, en)));
            }
        }
        if !r.rmw_loops.is_empty() {
            out.push_str(&format!("  {}:\n", lang.pick("条件付き一括更新（Masked Column RMW: 条件のビット列を語ごとに読む分岐なしのベクトル更新）", "masked column RMW (branch-free vector updates reading the filter bit column word by word)")));
            for (a, f, line, k, m, nt) in &r.rmw_loops {
                let how = if *nt { lang.pick("全部 1 の語は非時間的ストア", "all-one words use non-temporal stores") } else { lang.pick("普通のストア", "plain stores") };
                out.push_str(&format!("    {}.{} {} {}: {}{}ts_{} & {}{} — {}\n", a, f, lang.pick("行", "line"), line, k, lang.pick("（", " ("), f, m, lang.pick("）", ")"), how));
            }
        }
        if r.bitmap_loops > 0 {
            out.push_str(&format!(
                "  {}: {}\n",
                lang.pick("生存ビットマップ + ctz の `for id in a`（スロットごとの分岐なし）", "`for id in a` driven by the live bitmap + ctz (no per-slot branch)"),
                r.bitmap_loops
            ));
        }
        if r.hoisted_commits + r.folded_commits > 0 {
            out.push_str(&format!(
                "  {}: {} {} / {} {}\n",
                lang.pick("ループのコミット点", "loop commit points"),
                r.hoisted_commits,
                lang.pick("個はループの出口へ移動（回数の決まったループ）", "moved to the loop exit (fixed trip count)"),
                r.folded_commits,
                lang.pick("個は終了条件に混ぜた（while / loop）", "folded into the exit condition (while / loop)")
            ));
        }
        for l in &r.ctx_lines {
            out.push_str(&format!("  {}\n", l));
        }
        // バンプアリーナ・own_alloc・I/O ポート / bump arenas, own_alloc and I/O ports
        for (f, a) in &r.call_regions {
            out.push_str(&format!(
                "  {}  fn {}  → {}\n",
                lang.pick("呼び出しリージョン", "call region"),
                f,
                lang.pick(&format!("`{}` の先頭を入口の印へ戻す（出口ごとに 1 回の格納 = O(1) の一括解放）", a), &format!("resets the top of `{}` to its entry mark (one store per exit = an O(1) bulk release)", a))
            ));
        }
        if r.bump_loops > 0 {
            out.push_str(&format!("  {}: {}\n", lang.pick("バンプアリーナの `for id in b`（0..top の直線ループ）", "`for id in b` over bump arenas (straight 0..top loops)"), r.bump_loops));
        }
        for (line, name) in &r.own_alloc_sites {
            out.push_str(&format!(
                "  {:>5}{}  own_alloc({})  → {}\n",
                line,
                lang.pick("行目", ""),
                name,
                lang.pick("0 で埋めた所有リージョン（own の子も同じブロックに詰める・スコープの終わりで自動解放）", "a zero-filled own region (own children packed into the same block, freed automatically at scope end)")
            ));
        }
        if r.port_accesses > 0 {
            out.push_str(&format!(
                "  {}: {}\n",
                lang.pick("I/O ポートの読み書き（x86_64: in / out 命令、AArch64: コンパイラが MMIO へ写す）", "I/O port reads and writes (x86_64: in / out instructions, AArch64: remapped to MMIO by the compiler)"),
                r.port_accesses
            ));
        }
        out.push_str(&format!(
            "  {}: {} {} / {} {}\n",
            lang.pick("境界検査", "bounds checks"),
            r.bounds_checked,
            lang.pick("回（実行時）", "at run time"),
            r.bounds_elided,
            lang.pick("回は範囲を静的に証明して省略", "elided by a static range proof")
        ));
    }
    out.push_str(&format!(
        "  {}: {}\n",
        lang.pick("ソース中のライフタイム注釈（`'` の数）", "lifetime annotations in the source (count of `'`)"),
        cm.apostrophes
    ));
    if cm.reframed {
        out.push_str(&format!(
            "  {}: {} / {} {}\n",
            lang.pick("自動リフレーミング（Rust の見た目 → Fascon）", "automatic reframing (Rust look → Fascon)"),
            lang.pick(&format!("{} 箇所を書き換え", cm.reframe_report.lines.len()), &format!("{} rewrites", cm.reframe_report.lines.len())),
            cm.reframed_marks,
            lang.pick("個の文字リテラル・ライフタイムを整理", "char literals / lifetimes reframed")
        ));
    }
    out
}

// ───────────── 解説（fascon explain） / explanations (fascon explain) ─────────────

pub struct Fix {
    pub title: String,
    pub verified: bool,
    pub fixed_source: String,
}

fn rule_text(code: &str, lang: Lang) -> (String, String) {
    // (見出し, 数理的解説) は data/rules.json から / (title, mathematical explanation) come from data/rules.json
    crate::data::rule(code, lang)
}

/// ライフタイム注釈を取り除いた修正版（`<'a>` と `'a `） / the source with lifetime annotations removed
pub fn strip_lifetimes(text: &str) -> String {
    let mut out = String::new();
    let b: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '<' {
            // `<'a>` / `<'a, 'b>` を丸ごと削る / remove a whole `<'a, 'b>` list
            let mut j = i + 1;
            let mut only_lt = true;
            let mut saw = false;
            while j < b.len() && b[j] != '>' {
                if b[j] == '\'' {
                    saw = true;
                } else if !(b[j].is_alphanumeric() || b[j] == '_' || b[j] == ',' || b[j] == ' ') {
                    only_lt = false;
                    break;
                }
                j += 1;
            }
            if saw && only_lt && j < b.len() {
                i = j + 1;
                continue;
            }
        }
        if b[i] == '\'' {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_alphanumeric() || b[j] == '_') {
                j += 1;
            }
            if j < b.len() && b[j] == ' ' {
                j += 1;
            }
            i = j;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

pub fn explain(cm: &Compiled, line: Option<usize>, code: Option<&str>, lang: Lang) -> Option<(String, Vec<Fix>)> {
    let d = cm
        .diags
        .iter()
        .filter(|d| d.is_error())
        .find(|d| {
            let l = cm.src.line_of(d.span.lo) + 1;
            line.map_or(true, |x| x == l || x == l - 1) && code.map_or(true, |c| c == d.code)
        })
        .or_else(|| cm.diags.iter().find(|d| d.is_error()))?;
    let (l, c) = cm.src.line_col(d.span.lo);
    let (title, math) = rule_text(d.code, lang);
    let mut md = String::new();
    md.push_str(&format!("## {} — {}\n\n", d.code, title));
    md.push_str(&format!("**{}**（{}{} {}{}）\n\n", d.msg(lang), l + 1, lang.pick("行目", ":"), c + 1, lang.pick("列", "")));
    md.push_str(&format!("### {}\n\n> ", lang.pick("ステップ1: 根本原因タイムライン", "Step 1: root-cause timeline")));
    let mut steps = Vec::new();
    for n in &d.notes {
        let nl = cm.src.line_of(n.span.lo) + 1;
        steps.push(format!("{}{}: {}", nl, lang.pick("行目", ""), lang.pick(&n.ja, &n.en)));
    }
    steps.push(format!("{}{}: {} ➔ {}", l + 1, lang.pick("行目", ""), d.msg(lang), d.code));
    md.push_str(&steps.join(" ➔ "));
    md.push_str("\n\n");
    let mut fixes = Vec::new();
    md.push_str(&format!("### {}\n\n", lang.pick("ステップ2: 直し方", "Step 2: how to fix it")));
    if let Some(h) = d.hint_text(lang) {
        md.push_str(&format!("{}\n\n", h));
    }
    if d.code == "F0001" && cm.apostrophes > 0 {
        let fixed = strip_lifetimes(&cm.src.text);
        let re = compile(&cm.src.path, &fixed, lang, 64, false);
        let verified = re.apostrophes == 0 && !re.diags.iter().any(|x| x.code == "F0001" && x.is_error());
        md.push_str(&format!("```rust\n{}\n```\n\n", fixed.trim_end()));
        fixes.push(Fix { title: lang.pick("ライフタイム注釈を削除する", "remove the lifetime annotations").to_string(), verified, fixed_source: fixed });
    }
    // data/fixes.json の宣言的な修正（当ててコンパイルし直し、検証済みかを付ける） / declarative fixes from data/fixes.json
    let mut o = Opts::new(lang, 64);
    o.gen_c = false;
    for (f, verified) in crate::fixrules::fixes_for(d, &cm.src, &o) {
        md.push_str(&format!("- {}{}\n\n```rust\n{}\n```\n\n", f.title, if verified { lang.pick("（検証済み）", " (verified)") } else { "" }, f.new_text.trim()));
        fixes.push(Fix { title: f.title, verified, fixed_source: f.source });
    }
    md.push_str(&format!("### {}\n\n{}\n", lang.pick("ステップ3: なぜこの規則で安全になるのか（数理的解説）", "Step 3: why this rule makes it safe (mathematical explanation)"), math));
    Some((md, fixes))
}

pub fn render_diags(cm: &Compiled, lang: Lang) -> String {
    let mut s = String::new();
    for d in &cm.diags {
        s.push_str(&diag::render(d, &cm.src, lang));
        s.push('\n');
    }
    s
}

pub fn diags_json(cm: &Compiled, lang: Lang) -> crate::json::J {
    use crate::json::J;
    let mut arr = Vec::new();
    for d in &cm.diags {
        let (l, c) = cm.src.line_col(d.span.lo);
        let mut timeline: Vec<String> = d.notes.iter().map(|n| format!("{}: {}", cm.src.line_of(n.span.lo) + 1, lang.pick(&n.ja, &n.en))).collect();
        timeline.push(format!("{}: {} ➔ {}", l + 1, d.msg(lang), d.code));
        arr.push(J::obj(vec![
            ("code", J::s(d.code)),
            ("message", J::s(d.msg(lang))),
            ("line", J::n((l + 1) as f64)),
            ("column", J::n((c + 1) as f64)),
            ("severity", J::s(if d.is_error() { "error" } else { "warning" })),
            ("timeline", J::s(timeline.join(" ➔ "))),
            ("engine", J::s("1.0")),
        ]));
    }
    J::Arr(arr)
}

/// `import` 文の位置（つないだテキストの中で、名前の最後の段を含む最初の import） / the span of an `import` statement
fn expanded_import_span(text: &str, module: &str) -> crate::span::Span {
    let last = module.rsplit("::").next().unwrap_or(module);
    let pat = format!("import ");
    let mut from = 0usize;
    while let Some(i) = text[from..].find(&pat) {
        let at = from + i;
        let line_end = text[at..].find('\n').map_or(text.len(), |e| at + e);
        if text[at..line_end].contains(last) {
            return crate::span::Span::new(at as u32, line_end as u32);
        }
        from = at + pat.len();
    }
    crate::span::Span::new(0, 1)
}

/// 解説（数理的な説明）を両言語の JSON に（data/rules.json の形） / the explanations as bilingual JSON (data/rules.json)
pub fn dump_rules() -> crate::json::J {
    use crate::json::J;
    let codes = ["F0001", "F0382", "F0499", "F0502", "F0505", "F0507", "F0515", "F0520", "F2030", "F2101", "F2102", "F2201", "F2203", "F2502", "*"];
    J::Arr(
        codes
            .iter()
            .map(|c| {
                let (tj, mj) = rule_text(c, Lang::Ja);
                let (te, me) = rule_text(c, Lang::En);
                J::obj(vec![("code", J::s(*c)), ("title", J::obj(vec![("ja", J::s(tj)), ("en", J::s(te))])), ("math", J::obj(vec![("ja", J::s(mj)), ("en", J::s(me))]))])
            })
            .collect(),
    )
}
