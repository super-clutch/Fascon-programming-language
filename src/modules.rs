//! モジュール（`import net::tcp;`）: ライフタイムもジェネリクスも持ち込まない名前空間の分割
//! modules (`import net::tcp;`): namespaces without lifetimes or generics
//!
//! しくみ / how it works
//!   1. 取り込むファイルを探す（このファイルのフォルダ → 上のフォルダの fsc_modules/ → FASCON_PATH → 実行ファイルの横の lib/ →
//!      コンパイラに埋め込んだ標準ライブラリ）。取り込みは推移的で、同じモジュールは 1 回だけ
//!      find the files (this file's folder → fsc_modules/ in it and its parents → FASCON_PATH → lib/ next to the executable →
//!      the standard library embedded in the compiler); imports are transitive and each module is read once
//!   2. 全文をつないだテキストを 1 回だけ字句解析する（診断の位置はつないだテキストの中の位置。モジュールの中なら
//!      メッセージの頭に `モジュール名 (ファイル:行)` を付ける）
//!      the joined text is lexed once (diagnostic positions are positions in it; inside a module the message is prefixed
//!      with `module (file:line)`)
//!   3. トークンを書き換える: `tcp::connect` → `net_tcp__connect`、モジュールの中の自分の項目名も同じ名前へ
//!      （大文字で始まる名前は `Net_Tcp__Conn` のように大文字のまま）。フィールドアクセス（`.x`）と `名前:` は書き換えない
//!      rewrite tokens: `tcp::connect` → `net_tcp__connect`, and the module's own item names inside it likewise (names starting
//!      with a capital stay capitalised, as `Net_Tcp__Conn`); field accesses (`.x`) and `name:` are never rewritten
//!   4. モジュールごとの単独の検査を並列に走らせ、通ったものは内容のハッシュで .fascon-cache/ に覚える
//!      each module is checked on its own, in parallel; the ones that pass are remembered in .fascon-cache/ by content hash
use crate::diag::Diag;
use crate::lexer::{Tok, Token};
use crate::span::Span;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// つないだテキストの中のモジュールの区間 / a module's range in the joined text
#[derive(Clone, Debug)]
pub struct Segment {
    pub lo: u32,
    pub hi: u32,
    /// `net::tcp`
    pub module: String,
    /// 見つけたファイル（埋め込みなら `<std>/net/tcp.fsc`） / the file found (`<std>/net/tcp.fsc` when embedded)
    pub file: String,
    /// モジュールのテキスト / the module's text
    pub text: String,
}

#[derive(Default, Clone, Debug)]
pub struct Expanded {
    pub text: String,
    pub segments: Vec<Segment>,
    pub diags: Vec<Diag>,
}

/// 1 つの `import` 文 / one `import` statement
#[derive(Clone, Debug)]
struct ImportStmt {
    path: Vec<String>,
    alias: String,
    span: Span,
}

/// テキストの `import a::b [as c];`（波括弧の外）を探す / find `import a::b [as c];` outside braces
fn find_imports(text: &str) -> Vec<ImportStmt> {
    let lr = crate::lexer::lex_mode(text, false);
    let t = &lr.tokens;
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < t.len() {
        match &t[i].tok {
            Tok::LBrace => depth += 1,
            Tok::RBrace => depth -= 1,
            Tok::Ident(w) if w == "import" && depth == 0 => {
                let mut k = i + 1;
                let mut path = Vec::new();
                while let Some(Tok::Ident(seg)) = t.get(k).map(|x| &x.tok) {
                    path.push(seg.clone());
                    k += 1;
                    if matches!(t.get(k).map(|x| &x.tok), Some(Tok::ColonColon)) {
                        k += 1;
                    } else {
                        break;
                    }
                }
                let mut alias = path.last().cloned().unwrap_or_default();
                if matches!(t.get(k).map(|x| &x.tok), Some(Tok::Ident(a)) if a == "as") {
                    if let Some(Tok::Ident(a)) = t.get(k + 1).map(|x| &x.tok) {
                        alias = a.clone();
                        k += 2;
                    }
                }
                if !path.is_empty() && matches!(t.get(k).map(|x| &x.tok), Some(Tok::Semi)) {
                    out.push(ImportStmt { path, alias, span: t[i].span.to(t[k].span) });
                    i = k;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// テキストに `import` があるか（速い前検査） / whether a text has an `import` (a quick pre-check)
pub fn has_imports(text: &str) -> bool {
    text.contains("import ") && !find_imports(text).is_empty()
}

/// モジュールのファイルを探す / find a module's file
pub fn resolve(base_dir: Option<&Path>, path: &[String]) -> Option<(String, String)> {
    let rel: PathBuf = path.iter().collect::<PathBuf>().with_extension("fsc");
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(d) = base_dir {
        cands.push(d.join(&rel));
        let mut cur = Some(d.to_path_buf());
        while let Some(c) = cur {
            cands.push(c.join("fsc_modules").join(&rel));
            // パッケージの形: fsc_modules/<最初の段>/src/<残り>.fsc / package layout: fsc_modules/<first>/src/<rest>.fsc
            if path.len() >= 2 {
                let rest: PathBuf = path[1..].iter().collect::<PathBuf>().with_extension("fsc");
                cands.push(c.join("fsc_modules").join(&path[0]).join("src").join(rest));
            } else {
                cands.push(c.join("fsc_modules").join(&path[0]).join("src").join("lib.fsc"));
            }
            cur = c.parent().map(|p| p.to_path_buf());
        }
    }
    if let Ok(fp) = std::env::var("FASCON_PATH") {
        for p in std::env::split_paths(&fp) {
            cands.push(p.join(&rel));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            cands.push(d.join("lib").join(&rel));
            cands.push(d.join("..").join("lib").join(&rel));
        }
    }
    for c in cands {
        if let Ok(t) = std::fs::read_to_string(&c) {
            return Some((c.to_string_lossy().to_string(), t));
        }
    }
    // コンパイラに埋め込んだ標準ライブラリ / the standard library embedded in the compiler
    let key = path.join("/");
    crate::stdlib::FILES.iter().find(|(p, _)| *p == key).map(|(p, t)| (format!("<std>/{}.fsc", p), t.to_string()))
}

/// 名前の付け替えの接頭辞（小文字で始まる名前 / 大文字で始まる名前） / the renaming prefixes (lowercase / capitalised names)
fn prefixes(path: &[String]) -> (String, String) {
    let lower = format!("{}__", path.join("_"));
    let upper = format!(
        "{}__",
        path.iter()
            .map(|s| {
                let mut c = s.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join("_")
    );
    (lower, upper)
}

pub fn mangle(path: &[String], name: &str) -> String {
    let (lo, up) = prefixes(path);
    if name.starts_with(|c: char| c.is_ascii_uppercase()) {
        format!("{}{}", up, name)
    } else {
        format!("{}{}", lo, name)
    }
}

/// 取り込みを展開したテキストを作る（取り込みが無ければ None） / build the joined text (None when there is no import)
pub fn expand(path: &str, text: &str) -> Option<Expanded> {
    if !has_imports(text) {
        return None;
    }
    let mut ex = Expanded { text: text.to_string(), ..Default::default() };
    let base = Path::new(path).parent().map(|p| p.to_path_buf());
    // (モジュール名, 取り込んだ側のフォルダ, 取り込み文の位置) の作業リスト / worklist of (module, importer's folder, span)
    let mut work: Vec<(Vec<String>, Option<PathBuf>, Span)> = find_imports(text).into_iter().map(|im| (im.path, base.clone(), im.span)).collect();
    let mut seen: HashSet<String> = HashSet::new();
    while let Some((mp, dir, sp)) = work.pop() {
        let key = mp.join("::");
        if !seen.insert(key.clone()) {
            continue;
        }
        match resolve(dir.as_deref(), &mp) {
            Some((file, mtext)) => {
                let mdir = if file.starts_with("<std>") { None } else { Path::new(&file).parent().map(|p| p.to_path_buf()) };
                for im in find_imports(&mtext) {
                    work.push((im.path, mdir.clone(), sp));
                }
                ex.text.push('\n');
                ex.text.push_str(&format!("// ───── module {} ({}) ─────\n", key, file));
                let lo = ex.text.len() as u32;
                ex.text.push_str(&mtext);
                let hi = ex.text.len() as u32;
                ex.segments.push(Segment { lo, hi, module: key, file, text: mtext });
            }
            None => {
                ex.diags.push(
                    Diag::error(
                        "F3701",
                        sp,
                        format!("モジュール `{}` が見つかりません（{}.fsc を探しました）", key, mp.join("/")),
                        format!("module `{}` not found (looked for {}.fsc)", key, mp.join("/")),
                    )
                    .hint(
                        "このファイルのフォルダ・fsc_modules/（`fascon pkg add` で入る）・FASCON_PATH・標準ライブラリ（`fascon pkg std` で一覧）を探します",
                        "searched: this file's folder, fsc_modules/ (filled by `fascon pkg add`), FASCON_PATH and the standard library (listed by `fascon pkg std`)",
                    ),
                );
            }
        }
    }
    Some(ex)
}

/// 区間の中の取り込み文（別名 → モジュールの道筋） / the imports of a range (alias → module path)
fn aliases_in(text: &str, base: u32, diags: &mut Vec<Diag>) -> HashMap<String, Vec<String>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    for im in find_imports(text) {
        if let Some(prev) = m.get(&im.alias) {
            if *prev != im.path {
                diags.push(
                    Diag::error(
                        "F3703",
                        Span::new(im.span.lo + base, im.span.hi + base),
                        format!("別名 `{}` が 2 つのモジュール（{} と {}）を指しています", im.alias, prev.join("::"), im.path.join("::")),
                        format!("the alias `{}` names two modules ({} and {})", im.alias, prev.join("::"), im.path.join("::")),
                    )
                    .hint("`import a::b as 別名;` で別の名前を付けてください", "give one of them another name with `import a::b as alias;`"),
                );
            }
        }
        m.insert(im.alias, im.path);
    }
    m
}

/// つないだテキストのトークンを書き換える / rewrite the tokens of the joined text
pub fn rewrite(toks: &mut Vec<Token>, ex: &Expanded, main_len: u32) -> Vec<Diag> {
    let mut diags = Vec::new();
    // 区間ごとの別名と、モジュールごとの項目名 / aliases per range and item names per module
    let mut ranges: Vec<(u32, u32, HashMap<String, Vec<String>>, Option<Vec<String>>)> = Vec::new();
    ranges.push((0, main_len, aliases_in(&ex.text[..main_len as usize], 0, &mut diags), None));
    for s in &ex.segments {
        let path: Vec<String> = s.module.split("::").map(|x| x.to_string()).collect();
        ranges.push((s.lo, s.hi, aliases_in(&s.text, s.lo, &mut diags), Some(path)));
    }
    let range_of = |pos: u32| ranges.iter().position(|(lo, hi, _, _)| pos >= *lo && pos < *hi.max(&(lo + 1)));
    // モジュールの項目名（prescan で集める） / each module's item names (collected with prescan)
    let mut items: Vec<HashSet<String>> = vec![HashSet::new(); ranges.len()];
    for (ri, (lo, hi, _, mp)) in ranges.iter().enumerate() {
        if mp.is_none() {
            continue;
        }
        let seg: Vec<Token> = toks.iter().filter(|t| t.span.lo >= *lo && t.span.lo < *hi).cloned().collect();
        let syms = crate::prescan::prescan(&seg);
        items[ri] = syms.map.keys().cloned().collect();
        if items[ri].contains("main") {
            items[ri].remove("main");
        }
    }
    // 1. import 文のトークンを取り除く / drop the tokens of import statements
    let mut drop: Vec<(u32, u32)> = Vec::new();
    for (lo, hi, _, _) in &ranges {
        let text = &ex.text[*lo as usize..*hi as usize];
        for im in find_imports(text) {
            drop.push((im.span.lo + lo, im.span.hi + lo));
        }
    }
    toks.retain(|t| !drop.iter().any(|(a, b)| t.span.lo >= *a && t.span.hi <= *b));
    // 2. `別名::名前` を 1 つの名前に / `alias::name` becomes one name
    let mut out: Vec<Token> = Vec::with_capacity(toks.len());
    let mut i = 0usize;
    while i < toks.len() {
        let t = &toks[i];
        let ri = range_of(t.span.lo);
        let alias = match &t.tok {
            Tok::Ident(a) | Tok::ArenaName(a) => Some(a.clone()),
            _ => None,
        };
        if let (Some(a), Some(ri)) = (alias, ri) {
            if let Some(mp) = ranges[ri].2.get(&a) {
                if matches!(toks.get(i + 1).map(|x| &x.tok), Some(Tok::ColonColon)) {
                    if let Some(Tok::Ident(name)) = toks.get(i + 2).map(|x| &x.tok) {
                        let m = mangle(mp, name);
                        let sp = t.span.to(toks[i + 2].span);
                        let tok = if matches!(t.tok, Tok::ArenaName(_)) { Tok::ArenaName(m) } else { Tok::Ident(m) };
                        out.push(Token { tok, span: sp });
                        i += 3;
                        continue;
                    }
                }
            }
        }
        out.push(t.clone());
        i += 1;
    }
    // 3. モジュールの中の自分の項目名を付け替える。同じ名前の引数・let・パターンの束縛も一緒に付け替える（名前の
    //    付け替えは一貫しているので意味は変わらない）。付け替えないのは `.名前`（フィールド・メソッド）と、構造体の定義・
    //    リテラルのフィールド名（`名前:` で、引数の並びの中でも let の後でもないもの）だけ
    // 3. rename the module's own item names inside it; parameters, lets and pattern bindings of the same name are renamed
    //    with them (the renaming is consistent, so the meaning does not change). Only `.name` (fields, methods) and field
    //    names in struct definitions and literals (`name:` outside parameter lists and not after let) are kept
    let n = out.len();
    let mut param_depth: i32 = 0;
    let mut in_params = false;
    for k in 0..n {
        // 引数の並び `fn 名前(...)` の中か / inside the parameter list of `fn name(...)`
        match &out[k].tok {
            Tok::LParen if k >= 2 && matches!(out[k - 2].tok, Tok::Fn) && !in_params => {
                in_params = true;
                param_depth = 1;
                continue;
            }
            Tok::LParen if in_params => param_depth += 1,
            Tok::RParen if in_params => {
                param_depth -= 1;
                if param_depth == 0 {
                    in_params = false;
                }
            }
            _ => {}
        }
        let Some(ri) = range_of(out[k].span.lo) else { continue };
        let Some(mp) = ranges[ri].3.clone() else { continue };
        let name = match &out[k].tok {
            Tok::Ident(s) | Tok::ArenaName(s) => s.clone(),
            _ => continue,
        };
        if !items[ri].contains(&name) {
            continue;
        }
        let prev_dot = k > 0 && matches!(out[k - 1].tok, Tok::Dot | Tok::ColonColon);
        let next_colon = matches!(out.get(k + 1).map(|x| &x.tok), Some(Tok::Colon));
        // 宣言の名前（let・const・static・arena・bump_arena・mmio・chan・task_arena・dma_arena・dma・user_mem）の後の `:` は
        // フィールド名ではない / a `:` after a declared name (let, const, static, arena, ...) is not a field name
        let after_binder = k > 0
            && (matches!(out[k - 1].tok, Tok::Let | Tok::Mut | Tok::Const | Tok::Static | Tok::ArenaKw | Tok::BumpArena | Tok::Mmio | Tok::PortIo)
                || matches!(&out[k - 1].tok, Tok::Ident(w) if matches!(w.as_str(), "chan" | "task_arena" | "gc_arena" | "gpu_arena" | "dma_arena" | "dma" | "user_mem")));
        if prev_dot || (next_colon && !in_params && !after_binder) {
            continue;
        }
        let m = mangle(&mp, &name);
        out[k].tok = match &out[k].tok {
            Tok::ArenaName(_) => Tok::ArenaName(m),
            _ => Tok::Ident(m),
        };
    }
    *toks = out;
    diags
}

/// モジュールの中の診断に `モジュール (ファイル:行)` を付ける / prefix diagnostics inside modules with `module (file:line)`
pub fn locate(diags: &mut [Diag], ex: &Expanded) {
    for d in diags.iter_mut() {
        if let Some(s) = ex.segments.iter().find(|s| d.span.lo >= s.lo && d.span.lo < s.hi) {
            let off = (d.span.lo - s.lo) as usize;
            let line = s.text[..off.min(s.text.len())].matches('\n').count() + 1;
            let tag = format!("{} ({}:{}) ", s.module, s.file, line);
            d.prefix_message(&tag);
        }
    }
}

// ───────────── モジュールごとの並列検査とキャッシュ / per-module parallel checks with a cache ─────────────

/// FNV-1a 64 ビット / FNV-1a 64-bit
pub fn fnv64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 取り込んだモジュールを 1 つずつ単独で検査する（並列。通ったものはキャッシュ）。失敗したモジュールの診断を返す
/// check every imported module on its own (in parallel; passing ones are cached); returns the diagnostics of failing modules
pub fn check_modules(ex: &Expanded, base_dir: Option<&Path>, o: &crate::driver::Opts) -> Vec<(String, String, Vec<Diag>)> {
    let cache = base_dir.map(|d| d.join(".fascon-cache").join("modules"));
    let profile = format!("{:?}", o.profile);
    let results: Vec<Option<(String, String, Vec<Diag>)>> = std::thread::scope(|sc| {
        let hs: Vec<_> = ex
            .segments
            .iter()
            .map(|s| {
                let cache = cache.clone();
                let profile = profile.clone();
                sc.spawn(move || {
                    let key = format!("{:016x}", fnv64(format!("{}\n{}\n{}\n{}", crate::driver::VERSION, profile, s.file, s.text).as_bytes()));
                    if let Some(c) = &cache {
                        if c.join(&key).exists() {
                            return None;
                        }
                    }
                    let mut oo = *o;
                    oo.gen_c = false;
                    let cm = crate::driver::compile_user(&s.file, &s.text, &oo, false, None);
                    // モジュールに main は要らない（F0601 は数えない） / a module needs no main (F0601 is not counted)
                    let errs: Vec<Diag> = cm.diags.iter().filter(|d| d.is_error() && d.code != "F0601").cloned().collect();
                    if errs.is_empty() {
                        if let Some(c) = &cache {
                            let _ = std::fs::create_dir_all(c);
                            let _ = std::fs::write(c.join(&key), &s.module);
                        }
                        None
                    } else {
                        Some((s.module.clone(), s.file.clone(), errs))
                    }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or(None)).collect()
    });
    results.into_iter().flatten().collect()
}
