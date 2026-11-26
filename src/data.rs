//! 外へ出したデータ（辞書・解説・自動修正の規則）の読み込み。コンパイラに埋め込んだ data/*.json を土台に、上書きの
//! ファイルを重ねる（同じ名前・同じ番号は後から読んだものが勝つ）
//! loading the externalised data (dictionary, explanations, quick-fix rules): the data/*.json embedded in the compiler is
//! the base, and override files are layered on top (a later file wins for the same name / code)
//!
//! 上書きの場所（この順に重ねる）/ override locations (layered in this order)
//!   1. 実行ファイルの横の data/ / data/ next to the executable
//!   2. FASCON_DATA（フォルダ。区切り文字で複数可）/ FASCON_DATA (folders, separated like PATH)
//!   3. いまのフォルダの .fascon/ / .fascon/ in the current folder
//! ファイル名: dictionary.json・rules.json・fixes.json（形は data/ の同名のファイルと同じ。多言語の訳は "ja" / "en" のほかの
//! キーを足せる。コンパイラは ja / en を読み、VS Code 拡張は表示言語のキーを読む）
//! file names: dictionary.json, rules.json, fixes.json (same shape as the files in data/; translations may add keys besides
//! "ja" / "en": the compiler reads ja / en, the VS Code extension reads the display language's key)
use crate::diag::Lang;
use crate::json::{parse, J};
use std::path::PathBuf;
use std::sync::OnceLock;

pub const DICTIONARY_JSON: &str = include_str!("../data/dictionary.json");
pub const RULES_JSON: &str = include_str!("../data/rules.json");
pub const FIXES_JSON: &str = include_str!("../data/fixes.json");

fn override_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            v.push(d.join("data"));
        }
    }
    if let Ok(p) = std::env::var("FASCON_DATA") {
        v.extend(std::env::split_paths(&p));
    }
    v.push(PathBuf::from(".fascon"));
    v
}

/// 埋め込みの配列に上書きのファイルを重ねる（key のフィールドが同じなら置き換え、無ければ足す）
/// layer override files onto the embedded array (an element with the same key field replaces it, others are appended)
fn layered(base: &str, file: &str, key: &str) -> Vec<J> {
    let mut out: Vec<J> = match parse(base) {
        Ok(J::Arr(a)) => a,
        _ => Vec::new(),
    };
    for d in override_dirs() {
        let p = d.join(file);
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        match parse(&text) {
            Ok(J::Arr(a)) => {
                for x in a {
                    let k = x.get(key).as_str().map(|s| s.to_string());
                    match k.and_then(|k| out.iter().position(|y| y.get(key).as_str() == Some(k.as_str()))) {
                        Some(i) => out[i] = x,
                        None => out.push(x),
                    }
                }
            }
            _ => eprintln!("fascon: {}: not a JSON array (ignored)", p.display()),
        }
    }
    out
}

/// 2 言語の文 `{ "ja": .., "en": .. }` / a bilingual text
pub fn pick(j: &J, lang: Lang) -> String {
    let k = if lang == Lang::Ja { "ja" } else { "en" };
    j.get(k).as_str().or_else(|| j.get("en").as_str()).unwrap_or("").to_string()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// 辞書の項目（data/dictionary.json） / dictionary entries (data/dictionary.json)
pub fn dictionary() -> &'static [crate::dict::Entry] {
    static D: OnceLock<Vec<crate::dict::Entry>> = OnceLock::new();
    D.get_or_init(|| {
        let pair = |j: &J| (leak(pick(j, Lang::Ja)), leak(pick(j, Lang::En)));
        layered(DICTIONARY_JSON, "dictionary.json", "name")
            .iter()
            .map(|x| crate::dict::Entry {
                name: leak(x.get("name").as_str().unwrap_or("").to_string()),
                cat: pair(x.get("category")),
                summary: pair(x.get("summary")),
                detail: pair(x.get("detail")),
                constraints: pair(x.get("constraints")),
                remedy: pair(x.get("remedy")),
                aliases: Box::leak(
                    match x.get("aliases") {
                        J::Arr(a) => a.iter().filter_map(|s| s.as_str()).map(|s| leak(s.to_string())).collect::<Vec<_>>(),
                        _ => Vec::new(),
                    }
                    .into_boxed_slice(),
                ),
            })
            .collect()
    })
}

/// 解説（見出しと数理的な説明）。無い番号は "*" の既定 / an explanation (title and mathematics); "*" is the default
pub fn rule(code: &str, lang: Lang) -> (String, String) {
    static R: OnceLock<Vec<J>> = OnceLock::new();
    let rs = R.get_or_init(|| layered(RULES_JSON, "rules.json", "code"));
    let find = |c: &str| rs.iter().find(|x| x.get("code").as_str() == Some(c));
    match find(code).or_else(|| find("*")) {
        Some(x) => (pick(x.get("title"), lang), pick(x.get("math"), lang)),
        None => (code.to_string(), String::new()),
    }
}

/// 自動修正の規則（data/fixes.json）。同じ番号に複数あってよい（上書きのファイルは足すだけ）
/// quick-fix rules (data/fixes.json); a code may have several (override files only add)
pub fn fixes() -> &'static [J] {
    static F: OnceLock<Vec<J>> = OnceLock::new();
    F.get_or_init(|| {
        let mut v: Vec<J> = match parse(FIXES_JSON) {
            Ok(J::Arr(a)) => a,
            _ => Vec::new(),
        };
        for d in override_dirs() {
            if let Ok(text) = std::fs::read_to_string(d.join("fixes.json")) {
                if let Ok(J::Arr(a)) = parse(&text) {
                    v.extend(a);
                }
            }
        }
        v
    })
}
