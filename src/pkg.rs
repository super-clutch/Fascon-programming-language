//! `fascon pkg`: 小さなパッケージ管理（fascon.toml の [dependencies] を fsc_modules/ へ入れる。レジストリは使わない）
//! `fascon pkg`: a small package manager (installs fascon.toml [dependencies] into fsc_modules/; no registry)
//!
//!   fascon pkg init [名前]                       fascon.toml を作る / create fascon.toml
//!   fascon pkg new <名前>                        ライブラリの雛形（fascon.toml・src/・tests/）/ scaffold a library package
//!   fascon pkg add <名前> --path <dir>           パスの依存を足して入れる / add and install a path dependency
//!   fascon pkg add <名前> --git <url> [--rev r]  git の依存（git コマンドで clone）/ a git dependency (cloned with git)
//!   fascon pkg install                           fascon.toml のとおりに fsc_modules/ をそろえ、fascon.lock を書く
//!   fascon pkg list                              依存とその中のモジュール / dependencies and their modules
//!   fascon pkg std                               コンパイラに埋め込んだ標準ライブラリ / the embedded standard library
use crate::diag::Lang;
use crate::toml::{quote, Value};
use std::path::{Path, PathBuf};

/// パッケージの安全性の分類（構文の字句を走査して自動で決める） / a package's safety class (decided automatically by scanning
/// the tokens of its sources)
///   Pure Safe: インラインアセンブリ（#[asm]）も extern "C" も無い / no inline assembly (#[asm]) and no extern "C"
///   Low-Level / Hardware: それらか、装置の境界（mmio・port_io・bind_mmio・dma・irq・#[trap]・#[syscall]・user_mem・
///   volatile_bypass）を含む / contains them or a hardware boundary
pub fn classify(dir: &Path) -> (bool, Vec<String>) {
    fn files(d: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            v.sort();
            for p in v {
                let name = p.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                if p.is_dir() {
                    if name != "fsc_modules" && name != "target" && name != "build" && !name.starts_with('.') {
                        files(&p, out);
                    }
                } else if p.extension().map_or(false, |x| x == "fsc") {
                    out.push(p);
                }
            }
        }
    }
    let mut fs = Vec::new();
    files(dir, &mut fs);
    let mut why: Vec<String> = Vec::new();
    let mut low = false;
    for f in fs {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let rel = f.strip_prefix(dir).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        let is_test = rel.starts_with("tests/") || rel.contains("/tests/");
        if is_test {
            continue;
        }
        let w = scan_text(&text, &rel);
        low |= !w.is_empty();
        for x in w {
            if why.len() < 24 {
                why.push(x);
            }
        }
    }
    (!low, why)
}

/// 1 つのソースの装置境界・FFI・アセンブリの場所 / the hardware-boundary, FFI and assembly sites of one source
pub fn scan_text(text: &str, rel: &str) -> Vec<String> {
    let mut why: Vec<String> = Vec::new();
    {
        let lr = crate::lexer::lex(&text);
        let toks: Vec<&crate::lexer::Tok> = lr.tokens.iter().map(|t| &t.tok).collect();
        let line_of = |k: usize| -> usize { let o = lr.tokens[k].span.lo as usize; text[..o.min(text.len())].matches('\n').count() + 1 };
        let mut note = |what: &str, k: usize, why: &mut Vec<String>| {
            if why.len() < 24 {
                why.push(format!("{} ({}:{})", what, rel, line_of(k)));
            }
        };
        use crate::lexer::Tok;
        for k in 0..toks.len() {
            let id = |j: usize| -> Option<&str> {
                match toks.get(j) {
                    Some(Tok::Ident(s)) => Some(s.as_str()),
                    _ => None,
                }
            };
            let hit: Option<&str> = match toks[k] {
                Tok::Hash if matches!(toks.get(k + 1), Some(Tok::LBracket)) => match id(k + 2) {
                    Some("asm") => Some("#[asm] inline assembly"),
                    Some("trap") => Some("#[trap] exception handler"),
                    Some("syscall") => Some("#[syscall] entry"),
                    Some("framebuffer") => Some("#[framebuffer] device memory"),
                    _ => None,
                },
                Tok::Ident(s) if s == "extern" && matches!(toks.get(k + 1), Some(Tok::Str(c)) if c == "C") => Some("extern \"C\" FFI"),
                Tok::Mmio => Some("mmio region"),
                Tok::PortIo => Some("port_io region"),
                Tok::VolatileBypass => Some("volatile_bypass"),
                Tok::Ident(s) => match s.as_str() {
                    "bind_mmio" | "mmio_read32" | "mmio_write32" => Some("MMIO access"),
                    "dma_arena" | "submit_dma" | "complete_dma" | "dma_alloc" | "bus_addr" => Some("DMA"),
                    // アクセラレータへの貸し出し: gpu_arena の宣言・ハンドルの型・`.dispatch(..)` / offloading to accelerators: gpu_arena
                    // declarations, the handle types, `.dispatch(..)`
                    "gpu_arena" | "gpu_dispatch" | "GpuBuf" | "GpuFence" => Some("GPU / Accelerator offload"),
                    "dispatch" if k > 0 && matches!(toks[k - 1], Tok::Dot) && matches!(toks.get(k + 1), Some(Tok::LParen)) => Some("GPU / Accelerator offload"),
                    "dma" if matches!(toks.get(k + 1), Some(Tok::Ident(n)) if n == "own") || matches!(toks.get(k + 2), Some(Tok::Colon)) => Some("DMA"),
                    "irq" if matches!(toks.get(k + 1), Some(Tok::Ident(_)) | Some(Tok::Fn)) => Some("interrupt handler"),
                    "user_mem" => Some("user memory (EL0)"),
                    _ => None,
                },
                _ => None,
            };
            if let Some(w) = hit {
                note(w, k, &mut why);
            }
        }
    }
    why
}

/// fascon.toml の [package] に safety / safety_reasons を書く（既にあれば書き換え） / write safety / safety_reasons into
/// [package] of fascon.toml (replacing existing lines)
pub fn label_manifest(dir: &Path, lang: Lang) -> Result<String, String> {
    let mpath = dir.join("fascon.toml");
    let text = std::fs::read_to_string(&mpath).map_err(|_| lang.pick("fascon.toml がありません", "no fascon.toml").to_string())?;
    let (pure, why) = classify(dir);
    let label = if pure { "Pure Safe" } else { "Low-Level / Hardware" };
    let mut out = String::new();
    let mut in_pkg = false;
    let mut done = false;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let t = l.trim();
        if t.starts_with('[') {
            if in_pkg && !done {
                out.push_str(&safety_lines(label, &why));
                out.push('\n');
                done = true;
            }
            in_pkg = t == "[package]";
        }
        if in_pkg && (t.starts_with("safety ") || t.starts_with("safety=") || t.starts_with("safety_reasons")) {
            // 書き換え: 複数行の配列なら閉じ括弧まで飛ばす / replace: skip a multi-line array up to its closing bracket
            if t.starts_with("safety_reasons") && t.contains('[') && !t.contains(']') {
                while i < lines.len() && !lines[i].contains(']') {
                    i += 1;
                }
            }
            i += 1;
            continue;
        }
        out.push_str(l);
        out.push('\n');
        i += 1;
    }
    if in_pkg && !done {
        out.push_str(&safety_lines(label, &why));
    }
    std::fs::write(&mpath, out).map_err(|e| e.to_string())?;
    Ok(label.to_string())
}

fn safety_lines(label: &str, why: &[String]) -> String {
    let mut s = format!("safety = {}  # fascon pkg が構文を走査して自動で付ける / set automatically by fascon pkg from the syntax\n", quote(label));
    if why.is_empty() {
        s.push_str("safety_reasons = []\n");
    } else {
        s.push_str("safety_reasons = [\n");
        for w in why {
            s.push_str(&format!("    {},\n", quote(w)));
        }
        s.push_str("]\n");
    }
    s
}

fn say(lang: Lang, ja: &str, en: &str) {
    println!("{}", lang.pick(ja, en));
}

fn manifest() -> PathBuf {
    PathBuf::from("fascon.toml")
}

fn read_manifest(lang: Lang) -> Result<(String, Value), String> {
    let text = std::fs::read_to_string(manifest()).map_err(|_| lang.pick("fascon.toml がありません（`fascon pkg init` で作れます）", "no fascon.toml (create one with `fascon pkg init`)").to_string())?;
    let v = crate::toml::parse(&text).map_err(|e| format!("fascon.toml: {}", e))?;
    Ok((text, v))
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<u64> {
    let mut n = 0;
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        if name == ".git" || name == "fsc_modules" || name == ".fascon-cache" || name == "build" {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            n += copy_dir(&p, &to.join(&name))?;
        } else {
            std::fs::copy(&p, to.join(&name))?;
            n += 1;
        }
    }
    Ok(n)
}

/// 中の .fsc の内容のハッシュ（fascon.lock に書く） / a hash of the .fsc contents (written to fascon.lock)
fn tree_hash(dir: &Path) -> u64 {
    let mut files: Vec<PathBuf> = Vec::new();
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().map_or(false, |n| n != ".git") {
                        walk(&p, out);
                    }
                } else if p.extension().map_or(false, |x| x == "fsc" || x == "toml") {
                    out.push(p);
                }
            }
        }
    }
    walk(dir, &mut files);
    files.sort();
    let mut buf = Vec::new();
    for f in files {
        buf.extend_from_slice(f.strip_prefix(dir).unwrap_or(&f).to_string_lossy().replace('\\', "/").as_bytes());
        buf.extend(std::fs::read(&f).unwrap_or_default());
    }
    crate::modules::fnv64(&buf)
}

fn install_one(name: &str, spec: &Value, lang: Lang) -> Result<(String, u64), String> {
    let dest = Path::new("fsc_modules").join(name);
    if let Some(p) = spec.get("path").and_then(|x| x.as_str()) {
        let _ = std::fs::remove_dir_all(&dest);
        let n = copy_dir(Path::new(p), &dest).map_err(|e| format!("{}: {}", p, e))?;
        say(lang, &format!("  {} ← {}（{} ファイル）", name, p, n), &format!("  {} ← {} ({} files)", name, p, n));
        return Ok((format!("path+{}", p), tree_hash(&dest)));
    }
    if let Some(url) = spec.get("git").and_then(|x| x.as_str()) {
        let _ = std::fs::remove_dir_all(&dest);
        let mut c = std::process::Command::new("git");
        c.args(["clone", "--depth", "1"]);
        if let Some(r) = spec.get("rev").and_then(|x| x.as_str()) {
            c.args(["--branch", r]);
        }
        c.arg(url).arg(&dest);
        let st = c.status().map_err(|e| format!("git: {}", e))?;
        if !st.success() {
            return Err(format!("git clone {} failed", url));
        }
        say(lang, &format!("  {} ← {}", name, url), &format!("  {} ← {}", name, url));
        return Ok((format!("git+{}", url), tree_hash(&dest)));
    }
    Err(format!("{}: `path = \"..\"` or `git = \"..\"` is required", name))
}

fn install_all(lang: Lang) -> Result<(), String> {
    let (_, v) = read_manifest(lang)?;
    let mut lock = String::from("# fascon.lock — generated by `fascon pkg install`\n");
    if let Some(deps) = v.get("dependencies").and_then(|x| x.as_table()) {
        for (name, spec) in deps {
            let (src, h) = install_one(name, spec, lang)?;
            let (pure, _) = classify(&Path::new("fsc_modules").join(name));
            let label = if pure { "Pure Safe" } else { "Low-Level / Hardware" };
            lock.push_str(&format!("\n[[package]]\nname = {}\nsource = {}\nhash = \"{:016x}\"\nsafety = {}\n", quote(name), quote(&src), h, quote(label)));
            println!("  {} — {}", name, label);
        }
    }
    std::fs::write("fascon.lock", lock).map_err(|e| e.to_string())?;
    if Path::new("fascon.toml").exists() {
        let _ = label_manifest(Path::new("."), lang);
    }
    say(lang, "fascon.lock を書きました（依存ごとの安全性のラベル付き）", "wrote fascon.lock (with a safety label per dependency)");
    Ok(())
}

fn add_dep(name: &str, spec: &str, lang: Lang) -> Result<(), String> {
    let (mut text, v) = read_manifest(lang)?;
    if v.get(&format!("dependencies.{}", name)).is_some() {
        return Err(lang.pick(&format!("`{}` はもう依存にあります", name), &format!("`{}` is already a dependency", name)).to_string());
    }
    if v.get("dependencies").is_none() {
        text.push_str("\n[dependencies]\n");
    }
    // [dependencies] の表の最後に足す（他の表が後ろにあってもよいように、表の見出しの直後へ）
    // append to the [dependencies] table (right after its header, so later tables do not matter)
    let line = format!("{} = {}\n", name, spec);
    match text.find("[dependencies]") {
        Some(i) => {
            let at = text[i..].find('\n').map_or(text.len(), |e| i + e + 1);
            text.insert_str(at, &line);
        }
        None => text.push_str(&line),
    }
    std::fs::write(manifest(), text).map_err(|e| e.to_string())?;
    install_all(lang)
}

pub fn run(args: &[String], lang: Lang) -> i32 {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
    let arg = |k: &str| -> Option<String> { args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned() };
    let r: Result<(), String> = match sub {
        "init" => {
            if manifest().exists() {
                Err(lang.pick("fascon.toml はもうあります", "fascon.toml already exists").to_string())
            } else {
                let name = args.get(1).cloned().unwrap_or_else(|| std::env::current_dir().ok().and_then(|d| d.file_name().map(|n| n.to_string_lossy().to_string())).unwrap_or_else(|| "app".into()));
                let t = format!("[package]\nname = {}\nversion = \"0.1.0\"\nfascon = \"1.0\"\n\n[dependencies]\n", quote(&name));
                std::fs::write(manifest(), t).map_err(|e| e.to_string()).map(|_| say(lang, "fascon.toml を作りました", "created fascon.toml"))
            }
        }
        "new" => match args.get(1) {
            None => Err("fascon pkg new <name>".into()),
            Some(n) => {
                let d = PathBuf::from(n);
                let res = (|| -> std::io::Result<()> {
                    std::fs::create_dir_all(d.join("src"))?;
                    std::fs::create_dir_all(d.join("tests"))?;
                    std::fs::write(d.join("fascon.toml"), format!("[package]\nname = {}\nversion = \"0.1.0\"\nfascon = \"1.0\"\n\n[dependencies]\n", quote(n)))?;
                    std::fs::write(
                        d.join("src").join("lib.fsc"),
                        format!("// {n}: Fascon 1.0 library. `import {n};` makes `{n}::answer()` available\n// ライブラリの項目は `{n}::名前` で使えます（main は書きません）\nfn answer() -> u32 {{ 42 }}\n", n = n),
                    )?;
                    std::fs::write(d.join("tests").join("smoke.fsc"), format!("import {n};\nfn main() {{\n    print_int({n}::answer());\n}}\n", n = n))?;
                    Ok(())
                })();
                res.map_err(|e| e.to_string()).map(|_| say(lang, &format!("{} を作りました（src/lib.fsc・tests/smoke.fsc）", n), &format!("created {} (src/lib.fsc, tests/smoke.fsc)", n)))
            }
        },
        "add" => match args.get(1) {
            None => Err("fascon pkg add <name> --path <dir> | --git <url> [--rev <rev>]".into()),
            Some(n) => {
                if let Some(p) = arg("--path") {
                    add_dep(n, &format!("{{ path = {} }}", quote(&p)), lang)
                } else if let Some(u) = arg("--git") {
                    let rev = arg("--rev").map(|r| format!(", rev = {}", quote(&r))).unwrap_or_default();
                    add_dep(n, &format!("{{ git = {}{} }}", quote(&u), rev), lang)
                } else {
                    Err("--path <dir> or --git <url> is required".into())
                }
            }
        },
        "install" => install_all(lang),
        // 安全性のラベル: このパッケージ（と入れた依存）を走査して fascon.toml へ書く / safety labels: scan this package (and
        // the installed dependencies) and write them into fascon.toml
        "label" | "check" => {
            let dir = arg("--dir").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
            label_manifest(&dir, lang).map(|label| {
                println!("{}: {}", dir.display(), label);
                let (_, why) = classify(&dir);
                for w in why {
                    println!("  - {}", w);
                }
                let mods = dir.join("fsc_modules");
                if let Ok(rd) = std::fs::read_dir(&mods) {
                    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
                    v.sort();
                    for d in v {
                        let (pure, _) = classify(&d);
                        println!("  {} {}: {}", lang.pick("依存", "dependency"), d.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(), if pure { "Pure Safe" } else { "Low-Level / Hardware" });
                    }
                }
            })
        }
        "list" => read_manifest(lang).map(|(_, v)| {
            if let Some(deps) = v.get("dependencies").and_then(|x| x.as_table()) {
                for (name, _) in deps {
                    let d = Path::new("fsc_modules").join(name);
                    let mut mods: Vec<String> = Vec::new();
                    fn walk(base: &Path, d: &Path, out: &mut Vec<String>) {
                        if let Ok(rd) = std::fs::read_dir(d) {
                            for e in rd.flatten() {
                                let p = e.path();
                                if p.is_dir() {
                                    walk(base, &p, out);
                                } else if p.extension().map_or(false, |x| x == "fsc") && !p.to_string_lossy().contains("tests") {
                                    out.push(p.strip_prefix(base).unwrap_or(&p).with_extension("").to_string_lossy().replace(['/', '\\'], "::"));
                                }
                            }
                        }
                    }
                    walk(&d, &d, &mut mods);
                    mods.sort();
                    println!("{}{}", name, if d.exists() { "" } else { lang.pick("（未インストール）", " (not installed)") });
                    for m in mods {
                        println!("  import {}::{};", name, m.trim_start_matches("src::"));
                    }
                }
            }
        }),
        "std" => {
            for (p, t) in crate::stdlib::FILES {
                let first = t.lines().find(|l| l.starts_with("//")).unwrap_or("").trim_start_matches('/').trim();
                let label = if scan_text(t, p).is_empty() { "[Pure Safe]" } else { "[Low-Level / Hardware]" };
                println!("import {};{}{} {}", p.replace('/', "::"), " ".repeat(26usize.saturating_sub(p.len() + 9)), label, first);
            }
            Ok(())
        }
        _ => {
            println!("fascon pkg init [name] | new <name> | add <name> --path <dir> | add <name> --git <url> [--rev r] | install | list | label [--dir d] | std");
            Ok(())
        }
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("fascon pkg: {}", e);
            1
        }
    }
}
