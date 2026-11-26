//! 言語サーバー（stdio・JSON-RPC）。1.0 の文書はこのエンジンが答え、それ以外は fascon-core（0.1.0）へ中継する
//! Language server (stdio, JSON-RPC). This engine answers for 1.0 documents; everything else is relayed to fascon-core (0.1.0).

use crate::diag::Lang;
use crate::dict;
use crate::driver::{self, Compiled};
use crate::json::{self, J};
use crate::legacy;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

type Out = Arc<Mutex<std::io::Stdout>>;

fn read_msg<R: BufRead>(r: &mut R) -> Option<Vec<u8>> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let l = line.trim_end();
        if l.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().ok();
            }
        }
    }
    let mut buf = vec![0u8; len?];
    r.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn write_raw<W: Write>(w: &mut W, body: &[u8]) {
    let _ = write!(w, "Content-Length: {}\r\n\r\n", body.len());
    let _ = w.write_all(body);
    let _ = w.flush();
}

fn send(out: &Out, v: &J) {
    let body = v.dump();
    let mut o = out.lock().unwrap();
    write_raw(&mut *o, body.as_bytes());
}

fn respond(out: &Out, id: &J, result: J) {
    send(out, &J::obj(vec![("jsonrpc", J::s("2.0")), ("id", id.clone()), ("result", result)]));
}

fn respond_err(out: &Out, id: &J, code: i64, msg: &str) {
    send(out, &J::obj(vec![("jsonrpc", J::s("2.0")), ("id", id.clone()), ("error", J::obj(vec![("code", J::n(code as f64)), ("message", J::s(msg))]))]));
}

fn notify(out: &Out, method: &str, params: J) {
    send(out, &J::obj(vec![("jsonrpc", J::s("2.0")), ("method", J::s(method)), ("params", params)]));
}

/// file:// URI → パス（Windows の `file:///c%3A/...` も扱う） / file:// URI → path (also Windows `file:///c%3A/...`)
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::new();
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                bytes.push(x);
                i += 3;
                continue;
            }
        }
        bytes.push(b[i]);
        i += 1;
    }
    let mut s = String::from_utf8_lossy(&bytes).to_string();
    // `/c:/Users` → `c:/Users`
    let sb = s.as_bytes();
    if sb.len() >= 3 && sb[0] == b'/' && sb[2] == b':' && sb[1].is_ascii_alphabetic() {
        s.remove(0);
    }
    if cfg!(windows) {
        s = s.replace('/', "\\");
    }
    Some(PathBuf::from(s))
}

struct Doc {
    text: String,
    v2: bool,
}

struct Server {
    out: Out,
    docs: HashMap<String, Doc>,
    v2_uris: Arc<Mutex<HashSet<String>>>,
    lang: Lang,
    out_dir: String,
    ptr_bits: u32,
    root: Option<PathBuf>,
    core_in: Option<ChildStdin>,
    core: Option<Child>,
}

/// クライアントに知らせるコマンド。`fascon.build` は拡張機能が同じ名前の VS Code コマンドを持つので知らせない
/// （知らせると vscode-languageclient が同名のコマンドを登録しようとして起動に失敗する）。要求自体はどれも受け付ける。
/// commands announced to the client. `fascon.build` is left out because the extension owns a VS Code command of
/// that name (vscode-languageclient would try to register it again and fail to start). All of them are still served.
const COMMANDS: &[&str] = &["fascon.explain", "fascon.dictionary", "fascon.dictionary.search", "fascon.regions"];

fn capabilities() -> J {
    J::obj(vec![
        ("textDocumentSync", J::obj(vec![("openClose", J::Bool(true)), ("change", J::n(1.0)), ("save", J::obj(vec![("includeText", J::Bool(false))]))])),
        ("hoverProvider", J::Bool(true)),
        ("completionProvider", J::obj(vec![("triggerCharacters", J::Arr(vec![J::s("."), J::s(":"), J::s("#"), J::s("@")]))])),
        ("codeActionProvider", J::obj(vec![("codeActionKinds", J::Arr(vec![J::s("quickfix")]))])),
        ("executeCommandProvider", J::obj(vec![("commands", J::Arr(COMMANDS.iter().map(|c| J::s(*c)).collect()))])),
    ])
}

impl Server {
    fn forward(&mut self, body: &[u8]) {
        if let Some(w) = self.core_in.as_mut() {
            write_raw(w, body);
        }
    }

    fn compile_doc(&self, uri: &str, gen_c: bool) -> Option<Compiled> {
        let d = self.docs.get(uri)?;
        let path = uri_to_path(uri).map(|p| p.display().to_string()).unwrap_or_else(|| uri.to_string());
        let mut o = driver::Opts::new(self.lang, self.ptr_bits);
        o.gen_c = gen_c;
        // C を生成するときだけ、頼まれていれば厳格形も作る（環境変数 FASCON_EMIT_STRICT = VS Code の fascon.emitStrictOnBuild）
        // only when generating C, also produce the strict form if asked (FASCON_EMIT_STRICT = VS Code's fascon.emitStrictOnBuild)
        o.emit_strict = gen_c && matches!(std::env::var("FASCON_EMIT_STRICT").ok().as_deref(), Some("1") | Some("on") | Some("true"));
        Some(driver::compile_opts(&path, &d.text, &o))
    }

    fn native(&self, uri: &str) -> bool {
        self.core_in.is_none() || self.docs.get(uri).map_or(false, |d| d.v2)
    }

    fn set_doc(&mut self, uri: &str, text: String) {
        let v2 = driver::wants_v2(&text);
        let mut set = self.v2_uris.lock().unwrap();
        if v2 {
            set.insert(uri.to_string());
        } else {
            set.remove(uri);
        }
        drop(set);
        self.docs.insert(uri.to_string(), Doc { text, v2 });
    }

    fn publish(&self, uri: &str) {
        let Some(cm) = self.compile_doc(uri, false) else { return };
        let mut arr = Vec::new();
        for d in &cm.diags {
            let (l0, c0) = cm.src.line_col_utf16(d.span.lo);
            let (l1, c1) = cm.src.line_col_utf16(d.span.hi.max(d.span.lo));
            let mut msg = d.msg(self.lang).to_string();
            if !d.notes.is_empty() {
                let mut steps: Vec<String> = d.notes.iter().map(|n| format!("{}{} {}", cm.src.line_of(n.span.lo) + 1, self.lang.pick("行目:", ":"), self.lang.pick(&n.ja, &n.en))).collect();
                steps.push(format!("{}{} {}", l0 + 1, self.lang.pick("行目で", ": →"), d.code));
                msg.push_str(&format!("\n{}: {}", self.lang.pick("根本原因", "root cause"), steps.join(" ➔ ")));
            }
            if let Some(h) = d.hint_text(self.lang) {
                msg.push_str(&format!("\n{}: {}", self.lang.pick("ヒント", "hint"), h));
            }
            let related: Vec<J> = d
                .notes
                .iter()
                .map(|n| {
                    let (a, b) = cm.src.line_col_utf16(n.span.lo);
                    let (c, e) = cm.src.line_col_utf16(n.span.hi.max(n.span.lo));
                    J::obj(vec![("location", J::obj(vec![("uri", J::s(uri)), ("range", range(a, b, c, e))])), ("message", J::s(self.lang.pick(&n.ja, &n.en)))])
                })
                .collect();
            arr.push(J::obj(vec![
                ("range", range(l0, c0, l1, c1)),
                ("severity", J::n(if d.is_error() { 1.0 } else { 2.0 })),
                ("code", J::s(d.code)),
                ("source", J::s("fascon")),
                ("message", J::s(msg)),
                ("relatedInformation", J::Arr(related)),
            ]));
        }
        notify(&self.out, "textDocument/publishDiagnostics", J::obj(vec![("uri", J::s(uri)), ("diagnostics", J::Arr(arr))]));
    }

    fn resolve_out_dir(&self, file: &Path) -> PathBuf {
        let od = PathBuf::from(&self.out_dir);
        if od.is_absolute() {
            return od;
        }
        if let Some(r) = &self.root {
            return r.join(od);
        }
        file.parent().map(|p| p.join(&od)).unwrap_or(od)
    }

    fn cmd_build(&self, uri: &str) -> J {
        let Some(cm) = self.compile_doc(uri, true) else { return J::obj(vec![("ok", J::Bool(false)), ("message", J::s("document not open"))]) };
        let Some(c) = cm.c.as_ref() else {
            let first = cm.diags.iter().find(|d| d.is_error()).map(|d| format!("{}: {}", d.code, d.msg(self.lang))).unwrap_or_default();
            return J::obj(vec![("ok", J::Bool(false)), ("message", J::s(first))]);
        };
        let file = uri_to_path(uri).unwrap_or_else(|| PathBuf::from("untitled.fsc"));
        let dir = self.resolve_out_dir(&file);
        let stem = file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "out".into());
        let stem = stem.trim_end_matches(".fsc").to_string();
        let target = dir.join(format!("{}.fascon.c", stem));
        let mut r = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&target, c)).and_then(|_| std::fs::write(dir.join("fascon2_rt.h"), crate::assets::RUNTIME_H));
        if let (Ok(()), Some(st)) = (&r, &cm.strict) {
            r = std::fs::write(dir.join(format!("{}.strict.fsc", stem.trim_end_matches(".rs"))), st);
        }
        match r {
            Ok(()) => J::obj(vec![("ok", J::Bool(true)), ("path", J::s(target.display().to_string()))]),
            Err(e) => J::obj(vec![("ok", J::Bool(false)), ("message", J::s(e.to_string()))]),
        }
    }

    fn cmd_regions(&self, uri: &str) -> J {
        let Some(cm) = self.compile_doc(uri, false) else { return J::s("") };
        if cm.has_errors() {
            return J::s(driver::render_diags(&cm, self.lang));
        }
        J::s(driver::regions_text(&cm, self.lang))
    }

    fn cmd_explain(&self, uri: &str, line0: Option<i64>) -> J {
        let Some(cm) = self.compile_doc(uri, false) else { return J::Null };
        match driver::explain(&cm, line0.map(|l| l as usize + 1), None, self.lang) {
            Some((md, fixes)) => J::obj(vec![
                ("markdown", J::s(md)),
                ("fixes", J::Arr(fixes.into_iter().map(|f| J::obj(vec![("title", J::s(f.title)), ("verified", J::Bool(f.verified)), ("fixedSource", J::s(f.fixed_source))])).collect())),
            ]),
            None => J::Null,
        }
    }

    fn word_at(&self, uri: &str, line: usize, ch: usize) -> Option<String> {
        let d = self.docs.get(uri)?;
        let src = crate::span::SourceFile::new(uri, &d.text);
        let off = src.offset_of_utf16(line, ch) as usize;
        let b = d.text.as_bytes();
        let is_w = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let mut lo = off.min(b.len());
        let mut hi = lo;
        while lo > 0 && is_w(b[lo - 1]) {
            lo -= 1;
        }
        while hi < b.len() && is_w(b[hi]) {
            hi += 1;
        }
        if lo == hi {
            // `@` の上 / on the `@` itself
            if hi < b.len() && b[hi] == b'@' {
                hi += 1;
                lo = hi;
                while hi < b.len() && is_w(b[hi]) {
                    hi += 1;
                }
                return Some(format!("@{}", &d.text[lo..hi]));
            }
            return None;
        }
        let at = lo > 0 && b[lo - 1] == b'@';
        Some(format!("{}{}", if at { "@" } else { "" }, &d.text[lo..hi]))
    }

    fn hover(&self, uri: &str, line: usize, ch: usize) -> J {
        let Some(w) = self.word_at(uri, line, ch) else { return J::Null };
        let cm = self.compile_doc(uri, false);
        if let Some(cm) = &cm {
            // `@` は書いても書かなくても同じリージョン / with or without `@` it is the same region
            if let Some(sym) = cm.symbols.iter().find(|s| s.name == w || s.name == w.trim_start_matches('@')) {
                let md = format!("{}\n\n*{} {}{}*", sym.md, self.lang.pick("定義:", "defined at line"), sym.line + 1, self.lang.pick("行目", ""));
                return J::obj(vec![("contents", J::obj(vec![("kind", J::s("markdown")), ("value", J::s(md))]))]);
            }
        }
        let key = w.trim_start_matches('@');
        let hits = dict::find(if w.starts_with('@') { &w } else { key });
        let exact = hits.iter().find(|e| e.name.eq_ignore_ascii_case(&w) || e.name.eq_ignore_ascii_case(key) || e.aliases.iter().any(|a| a.eq_ignore_ascii_case(key) || a.eq_ignore_ascii_case(&w)));
        if let Some(e) = exact {
            let md = format!(
                "**{}** — {}\n\n{}\n\n{}",
                e.name,
                self.lang.pick(e.cat.0, e.cat.1),
                self.lang.pick(e.summary.0, e.summary.1),
                self.lang.pick(e.detail.0, e.detail.1)
            );
            return J::obj(vec![("contents", J::obj(vec![("kind", J::s("markdown")), ("value", J::s(md))]))]);
        }
        J::Null
    }

    fn completion(&self, uri: &str, line: usize, ch: usize) -> J {
        let mut items = Vec::new();
        let mut push = |label: &str, kind: f64, detail: &str| {
            items.push(J::obj(vec![("label", J::s(label)), ("kind", J::n(kind)), ("detail", J::s(detail))]));
        };
        let prev = self.docs.get(uri).and_then(|d| {
            let src = crate::span::SourceFile::new(uri, &d.text);
            let off = src.offset_of_utf16(line, ch) as usize;
            let b = d.text.as_bytes();
            let mut i = off.min(b.len());
            while i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
                i -= 1;
            }
            if i > 0 {
                Some(b[i - 1])
            } else {
                None
            }
        });
        if prev == Some(b'.') {
            for (m, d) in [
                ("alloc", "a.alloc(v) -> Option<Id<a>>"),
                ("free", "a.free(id) -> bool（bump_arena には無い / not on bump_arena）"),
                ("remove", "a.remove(id) -> Option<T>"),
                ("get", "if let Some(x) = a.get(id) { .. } / a.get(id).unwrap()"),
                ("get_mut", "if let Some(x) = a.get_mut(id) { .. } / a.get_mut(id).unwrap()"),
                ("contains", "a.contains(id) -> bool"),
                ("len", "len() -> usize"),
                ("capacity", "a.capacity() -> usize"),
                ("validate", "a.validate(raw) -> Option<Id<a>>"),
                ("index", "id.index() -> u32"),
                ("generation", "id.generation() -> u32 (bump_arena: u64)"),
                ("raw", "id.raw() -> u32 (bump_arena: u64)"),
                ("read", "port.read() — in al, dx"),
                ("write", "port.write(v) — out dx, al"),
                ("read_at", "ports.read_at(i)"),
                ("write_at", "ports.write_at(i, v)"),
                ("is_some", "Option::is_some"),
                ("is_none", "Option::is_none"),
                ("take", "Option::take (own の取り出し / move out)"),
                ("unwrap", "Option::unwrap"),
                ("unwrap_or", "Option::unwrap_or"),
                ("clone", "deep clone"),
                ("width", "fb.width()"),
                ("height", "fb.height()"),
                ("fill", "fb.fill(v)"),
                ("put", "fb.put(x, y, v)"),
                ("fill_rect", "fb.fill_rect(x, y, w, h, v)"),
                ("byte", "s.byte(i) -> u8"),
            ] {
                push(m, 2.0, d);
            }
            return J::Arr(items);
        }
        for k in ["fn", "struct", "let", "mut", "if", "else", "match", "while", "loop", "for", "in", "return", "break", "continue", "arena", "bump_arena", "mmio", "port_io", "at_port", "own", "type", "at", "static", "true", "false", "None", "Some", "volatile_bypass"] {
            push(k, 14.0, "keyword");
        }
        for t in ["u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize", "bool", "Str", "Option", "Id"] {
            push(t, 25.0, "type");
        }
        for b in ["print_int", "print_str", "put_int", "put_hex", "put_str", "panic", "drop", "own_alloc", "cpu_relax", "fence", "compiler_fence", "cpu_id", "heap_live_blocks", "heap_live_bytes", "halt", "exit", "mmio_read32", "mmio_write32"] {
            push(b, 3.0, "builtin");
        }
        if let Some(cm) = self.compile_doc(uri, false) {
            for s in &cm.symbols {
                let first = s.md.lines().nth(1).unwrap_or("").to_string();
                push(&s.name, s.kind as f64, &first);
            }
        }
        J::Arr(items)
    }

    fn code_actions(&self, uri: &str, params: &J) -> J {
        let Some(cm) = self.compile_doc(uri, false) else { return J::Arr(vec![]) };
        let mut acts = Vec::new();
        let wanted: Vec<String> = match params.get("context").get("diagnostics") {
            J::Arr(a) => a.iter().filter_map(|d| d.get("code").as_str().map(|s| s.to_string())).collect(),
            _ => vec![],
        };
        if cm.apostrophes > 0 && (wanted.is_empty() || wanted.iter().any(|c| c == "F0001")) {
            if let Some((_, fixes)) = driver::explain(&cm, None, Some("F0001"), self.lang) {
                for f in fixes {
                    let (el, ec) = cm.src.line_col_utf16(cm.src.text.len() as u32);
                    let mut changes = std::collections::BTreeMap::new();
                    changes.insert(uri.to_string(), J::Arr(vec![J::obj(vec![("range", range(0, 0, el, ec)), ("newText", J::s(f.fixed_source))])]));
                    acts.push(J::obj(vec![
                        ("title", J::s(format!("{}{}", f.title, if f.verified { self.lang.pick("（検証済み）", " (verified)") } else { "" }))),
                        ("kind", J::s("quickfix")),
                        ("isPreferred", J::Bool(f.verified)),
                        ("edit", J::obj(vec![("changes", J::Obj(changes))])),
                    ]));
                }
            }
        }
        // F2601: 該当の文を `critical { .. }` で囲む（囲んだソースをコンパイルし直して、そのエラーが消えたら「検証済み」）
        // F2601: wrap the statement in `critical { .. }` (the wrapped source is recompiled; "verified" when the error is gone)
        let mut done: HashSet<(u32, u32)> = HashSet::new();
        for d in cm.diags.iter().filter(|d| d.code == "F2601" || d.code == "F2613") {
            let Some((lo, hi)) = critical_fix_range(&cm.src.text, d.span.lo, d.span.hi) else { continue };
            if !done.insert((lo, hi)) {
                continue;
            }
            let text = &cm.src.text;
            let fixed = format!("{}critical {{ {} }}{}", &text[..lo as usize], &text[lo as usize..hi as usize], &text[hi as usize..]);
            let mut o = driver::Opts::new(self.lang, self.ptr_bits);
            o.gen_c = false;
            let again = driver::compile_opts(&cm.src.path, &fixed, &o);
            let shift = "critical { ".len() as u32;
            let verified = !again.diags.iter().any(|x| (x.code == "F2601" || x.code == "F2613") && x.span.lo >= lo && x.span.hi <= hi + shift + 2);
            let (l0, c0) = cm.src.line_col_utf16(lo);
            let (l1, c1) = cm.src.line_col_utf16(hi);
            let mut changes = std::collections::BTreeMap::new();
            changes.insert(uri.to_string(), J::Arr(vec![J::obj(vec![("range", range(l0, c0, l1, c1)), ("newText", J::s(format!("critical {{ {} }}", &text[lo as usize..hi as usize])))])]));
            acts.push(J::obj(vec![
                ("title", J::s(format!("{}{}", self.lang.pick("`critical { .. }` で囲む（閉じた区間にする）", "Wrap in `critical { .. }` (make it a closed section)"), if verified { self.lang.pick("（検証済み）", " (verified)") } else { "" }))),
                ("kind", J::s("quickfix")),
                ("isPreferred", J::Bool(verified)),
                ("edit", J::obj(vec![("changes", J::Obj(changes))])),
            ]));
        }
        // data/fixes.json の宣言的な修正（F2601 / F2613 は上の専用の形） / declarative fixes from data/fixes.json (F2601 / F2613
        // have the dedicated form above)
        let mut o = driver::Opts::new(self.lang, self.ptr_bits);
        o.gen_c = false;
        // 修正のある警告（F3901: #[allow(hpc_io)]、F2707: ふつうの arena）も / warnings that have fixes too (F3901:
        // #[allow(hpc_io)], F2707: an ordinary arena)
        for d in cm.diags.iter().filter(|d| (d.is_error() || matches!(d.code, "F3901" | "F2707")) && d.code != "F2601" && d.code != "F2613") {
            for (f, verified) in crate::fixrules::fixes_for(d, &cm.src, &o) {
                let (l0, c0) = cm.src.line_col_utf16(f.lo);
                let (l1, c1) = cm.src.line_col_utf16(f.hi);
                let mut changes = std::collections::BTreeMap::new();
                changes.insert(uri.to_string(), J::Arr(vec![J::obj(vec![("range", range(l0, c0, l1, c1)), ("newText", J::s(f.new_text.clone()))])]));
                acts.push(J::obj(vec![
                    ("title", J::s(format!("{}{}", f.title, if verified { self.lang.pick("（検証済み）", " (verified)") } else { "" }))),
                    ("kind", J::s("quickfix")),
                    ("isPreferred", J::Bool(verified)),
                    ("edit", J::obj(vec![("changes", J::Obj(changes))])),
                ]));
            }
        }
        J::Arr(acts)
    }
}

/// F2601 の修正で囲む範囲: エラーの場所を含むいちばん内側の文（`let x = 式;` なら式だけ。変数は外から見えたままになる）
/// the range the F2601 fix wraps: the innermost statement containing the error (for `let x = expr;` only the expression,
/// so the variable stays visible outside)
pub fn critical_fix_range(text: &str, lo: u32, hi: u32) -> Option<(u32, u32)> {
    use crate::ast::{Block, Expr, Item, Stmt};
    use crate::flow::{children, Child};
    let lr = crate::lexer::lex_mode(text, false);
    let syms = crate::prescan::prescan(&lr.tokens);
    let (prog, _) = crate::parser::parse(&lr.tokens, &syms, false);
    fn in_expr(e: &Expr, lo: u32, hi: u32, best: &mut Option<(u32, u32)>) {
        for c in children(e) {
            match c {
                Child::E(x) => in_expr(x, lo, hi, best),
                Child::B(b) => in_block(b, lo, hi, best),
            }
        }
    }
    fn in_block(b: &Block, lo: u32, hi: u32, best: &mut Option<(u32, u32)>) {
        let visit = |e: &Expr, best: &mut Option<(u32, u32)>| {
            if e.span.lo <= lo && hi <= e.span.hi {
                *best = Some((e.span.lo, e.span.hi));
                in_expr(e, lo, hi, best);
            }
        };
        for st in &b.stmts {
            match st {
                Stmt::Let(l) => {
                    if let Some(i) = &l.init {
                        visit(i, best);
                    }
                }
                Stmt::Expr(e) => visit(e, best),
            }
        }
        if let Some(t) = &b.tail {
            visit(t, best);
        }
    }
    let mut best = None;
    for it in &prog.items {
        if let Item::Fn(f) = it {
            if f.body.span.lo <= lo && hi <= f.body.span.hi {
                in_block(&f.body, lo, hi, &mut best);
            }
        }
    }
    best
}

fn range(l0: usize, c0: usize, l1: usize, c1: usize) -> J {
    J::obj(vec![
        ("start", J::obj(vec![("line", J::n(l0 as f64)), ("character", J::n(c0 as f64))])),
        ("end", J::obj(vec![("line", J::n(l1 as f64)), ("character", J::n(c1 as f64))])),
    ])
}

#[cfg(test)]
mod tests {
    use super::critical_fix_range;

    #[test]
    fn f2601_fix_wraps_the_innermost_statement() {
        let src = "struct S { v: u64 }\narena @a: S[2];\nfn f(id: Id<@a>) -> u64 {\n    let x = @a[id].v;\n    if x > 0 {\n        @a[id].v += 1;\n    }\n    @a[id].v\n}\n";
        // `let x = 式;` は式だけを囲む / only the initializer of a let is wrapped
        let lo = src.find("@a[id].v;").unwrap() as u32;
        let (a, b) = critical_fix_range(src, lo, lo + 6).unwrap();
        assert_eq!(&src[a as usize..b as usize], "@a[id].v");
        // if の中の文 / a statement inside an if
        let lo = src.find("@a[id].v += 1").unwrap() as u32;
        let (a, b) = critical_fix_range(src, lo, lo + 6).unwrap();
        assert_eq!(&src[a as usize..b as usize], "@a[id].v += 1");
        // 関数の末尾の式 / the tail expression of a function
        let lo = src.rfind("@a[id].v").unwrap() as u32;
        let (a, b) = critical_fix_range(src, lo, lo + 6).unwrap();
        assert_eq!(&src[a as usize..b as usize], "@a[id].v");
    }
}

pub fn serve(default_lang: Lang) -> i32 {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let v2_uris = Arc::new(Mutex::new(HashSet::new()));
    let init_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut srv = Server { out: out.clone(), docs: HashMap::new(), v2_uris: v2_uris.clone(), lang: default_lang, out_dir: "build".into(), ptr_bits: 64, root: None, core_in: None, core: None };

    // 0.1 の言語サーバー（fascon-core lsp）を子プロセスで起動 / start the 0.1 language server (fascon-core lsp) as a child process
    if std::env::var_os("FASCON_LSP_STANDALONE").is_none() {
        if let Some(core) = legacy::core_path() {
            if let Ok(mut child) = Command::new(core).arg("lsp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn() {
                srv.core_in = child.stdin.take();
                let cout = child.stdout.take().unwrap();
                srv.core = Some(child);
                let out2 = out.clone();
                let uris2 = v2_uris.clone();
                let init2 = init_id.clone();
                std::thread::spawn(move || {
                    let mut r = BufReader::new(cout);
                    while let Some(body) = read_msg(&mut r) {
                        let text = String::from_utf8_lossy(&body).to_string();
                        if let Ok(mut v) = json::parse(&text) {
                            if v.get("method").as_str() == Some("textDocument/publishDiagnostics") {
                                if let Some(u) = v.get("params").get("uri").as_str() {
                                    if uris2.lock().unwrap().contains(u) {
                                        continue; // 1.0 の文書は 1.0 エンジンが診断する / 1.0 documents are diagnosed by the 1.0 engine
                                    }
                                }
                            }
                            let is_init = { init2.lock().unwrap().as_deref() == Some(v.get("id").dump().as_str()) && v.get("method").is_null() };
                            if is_init {
                                if let J::Obj(m) = &mut v {
                                    if let Some(J::Obj(res)) = m.get_mut("result") {
                                        res.insert("serverInfo".into(), J::obj(vec![("name", J::s("fascon-lsp")), ("version", J::s(format!("{} (core 0.1.0)", driver::VERSION)))]));
                                        if let Some(J::Obj(caps)) = res.get_mut("capabilities") {
                                            caps.insert("completionProvider".into(), J::obj(vec![("triggerCharacters", J::Arr(vec![J::s("."), J::s(":"), J::s("#"), J::s("@")]))]));
                                            caps.insert("executeCommandProvider".into(), J::obj(vec![("commands", J::Arr(COMMANDS.iter().map(|c| J::s(*c)).collect()))]));
                                        }
                                    }
                                }
                                send(&out2, &v);
                                continue;
                            }
                        }
                        let mut o = out2.lock().unwrap();
                        write_raw(&mut *o, &body);
                    }
                });
            }
        }
    }

    let stdin = std::io::stdin();
    let mut r = BufReader::new(stdin.lock());
    let mut shutdown = false;
    while let Some(body) = read_msg(&mut r) {
        let text = String::from_utf8_lossy(&body).to_string();
        let Ok(msg) = json::parse(&text) else { continue };
        let method = msg.get("method").as_str().map(|s| s.to_string());
        let id = msg.get("id").clone();
        let params = msg.get("params").clone();
        let Some(method) = method else {
            // クライアントからの応答（0.1 サーバーの要求への） / a client response (to a request of the 0.1 server)
            srv.forward(&body);
            continue;
        };
        let uri = params.get("textDocument").get("uri").as_str().map(|s| s.to_string());
        match method.as_str() {
            "initialize" => {
                let io = params.get("initializationOptions");
                if let Some(l) = io.get("language").as_str().and_then(Lang::parse) {
                    srv.lang = l;
                }
                if let Some(o) = io.get("outDir").as_str() {
                    if !o.trim().is_empty() {
                        srv.out_dir = o.to_string();
                    }
                }
                if let Some(p) = io.get("ptrBits").as_i64() {
                    if p == 32 || p == 64 {
                        srv.ptr_bits = p as u32;
                    }
                }
                srv.root = params.get("rootUri").as_str().and_then(uri_to_path).or_else(|| params.get("workspaceFolders").at(0).get("uri").as_str().and_then(uri_to_path));
                if srv.core_in.is_some() {
                    *init_id.lock().unwrap() = Some(id.dump());
                    srv.forward(&body);
                } else {
                    respond(&out, &id, J::obj(vec![("capabilities", capabilities()), ("serverInfo", J::obj(vec![("name", J::s("fascon-lsp")), ("version", J::s(driver::VERSION))]))]));
                }
            }
            "textDocument/didOpen" => {
                let td = params.get("textDocument");
                let u = td.get("uri").as_str().unwrap_or("").to_string();
                srv.set_doc(&u, td.get("text").as_str().unwrap_or("").to_string());
                srv.forward(&body);
                if srv.native(&u) {
                    srv.publish(&u);
                }
            }
            "textDocument/didChange" => {
                let u = uri.clone().unwrap_or_default();
                let was_v2 = srv.docs.get(&u).map_or(false, |d| d.v2);
                if let J::Arr(ch) = params.get("contentChanges") {
                    if let Some(last) = ch.last() {
                        if let Some(t) = last.get("text").as_str() {
                            srv.set_doc(&u, t.to_string());
                        }
                    }
                }
                srv.forward(&body);
                if srv.native(&u) {
                    srv.publish(&u);
                } else if was_v2 {
                    // 1.0 → 0.1 に戻った: 0.1 サーバーが改めて診断を出す / back to 0.1: the 0.1 server publishes again
                }
            }
            "textDocument/didClose" => {
                let u = uri.clone().unwrap_or_default();
                let native = srv.native(&u);
                srv.docs.remove(&u);
                srv.v2_uris.lock().unwrap().remove(&u);
                srv.forward(&body);
                if native {
                    notify(&out, "textDocument/publishDiagnostics", J::obj(vec![("uri", J::s(u)), ("diagnostics", J::Arr(vec![]))]));
                }
            }
            "textDocument/hover" | "textDocument/completion" | "textDocument/codeAction" if uri.as_deref().map_or(false, |u| srv.native(u)) => {
                let u = uri.unwrap();
                let line = params.get("position").get("line").as_i64().unwrap_or(0) as usize;
                let ch = params.get("position").get("character").as_i64().unwrap_or(0) as usize;
                let res = match method.as_str() {
                    "textDocument/hover" => srv.hover(&u, line, ch),
                    "textDocument/completion" => srv.completion(&u, line, ch),
                    _ => srv.code_actions(&u, &params),
                };
                respond(&out, &id, res);
            }
            "workspace/executeCommand" => {
                let cmd = params.get("command").as_str().unwrap_or("").to_string();
                let args = params.get("arguments");
                let u = args.at(0).as_str().map(|s| s.to_string());
                let native_doc = u.as_deref().map_or(false, |u| srv.native(u) && srv.docs.contains_key(u));
                match cmd.as_str() {
                    "fascon.build" if native_doc => respond(&out, &id, srv.cmd_build(u.as_deref().unwrap())),
                    "fascon.regions" if native_doc => respond(&out, &id, srv.cmd_regions(u.as_deref().unwrap())),
                    "fascon.explain" if native_doc => respond(&out, &id, srv.cmd_explain(u.as_deref().unwrap(), args.at(1).as_i64())),
                    "fascon.dictionary" | "fascon.dictionary.search" if srv.core_in.is_none() => {
                        let term = args.at(0).as_str().unwrap_or("");
                        let all = dict::entries_json(srv.lang);
                        let res: Vec<J> = if term.is_empty() {
                            all
                        } else {
                            let names: Vec<&str> = dict::find(term).iter().map(|e| e.name).collect();
                            all.into_iter().filter(|e| e.get("name").as_str().map_or(false, |n| names.contains(&n))).collect()
                        };
                        respond(&out, &id, J::Arr(res));
                    }
                    _ if srv.core_in.is_some() => srv.forward(&body),
                    _ => respond_err(&out, &id, -32601, "unknown command"),
                }
            }
            "shutdown" => {
                shutdown = true;
                if srv.core_in.is_some() {
                    srv.forward(&body);
                } else {
                    respond(&out, &id, J::Null);
                }
            }
            "exit" => {
                srv.forward(&body);
                if let Some(mut c) = srv.core.take() {
                    for _ in 0..20 {
                        if let Ok(Some(_)) = c.try_wait() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    let _ = c.kill();
                }
                return if shutdown { 0 } else { 1 };
            }
            _ => {
                if srv.core_in.is_some() {
                    srv.forward(&body);
                } else if !id.is_null() {
                    respond_err(&out, &id, -32601, "method not found");
                }
            }
        }
    }
    if let Some(mut c) = srv.core.take() {
        let _ = c.kill();
    }
    0
}
