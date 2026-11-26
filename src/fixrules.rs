//! 宣言的な自動修正（data/fixes.json）: 規則をデータで書き、当てたソースをコンパイルし直して、そのエラーが消えたら
//! 「検証済み」にする。コントリビュータは JSON に 1 件足すだけで修正を増やせる（コンパイラを変えない）
//! declarative quick fixes (data/fixes.json): rules are data; the fixed source is recompiled and the fix is "verified" when
//! the error is gone. Contributors add a fix by adding one JSON object (no compiler change)
//!
//! 規則の種類 / rule kinds
//!   wrap_statement      : エラーを含むいちばん内側の文を prefix + 文 + suffix にする / wrap the innermost statement
//!   replace_in_span     : 診断の範囲の文字列を pattern（{名前} が穴）に合わせて replace にする / rewrite the diagnostic's text
//!   replace_in_line     : 診断のある行で pattern を探して replace にする / rewrite within the diagnostic's line
//!   prefix_line_token   : 診断のある行の token（例 "let "）の前に insert を差し込む / insert before a token on that line
use crate::diag::{Diag, Lang};
use crate::json::J;
use crate::span::SourceFile;

pub struct RuleFix {
    pub title: String,
    pub source: String,
    /// 書き換えた範囲（元のテキストの番地）と新しい文字列（LSP の編集に使う） / the replaced range and its new text (LSP edits)
    pub lo: u32,
    pub hi: u32,
    pub new_text: String,
}

/// pattern の {名前} を穴として text に当てはめる（穴は空でない最短一致。前後の空白は除く）
/// match a pattern whose {name} parts are holes against text (holes are the shortest non-empty match, trimmed)
fn match_pattern(pat: &str, text: &str) -> Option<Vec<(String, String)>> {
    // pat を「固定の文字列」と「穴」に分ける / split pat into literal pieces and holes
    let mut parts: Vec<(bool, String)> = Vec::new();
    let mut rest = pat;
    while let Some(i) = rest.find('{') {
        if i > 0 {
            parts.push((false, rest[..i].to_string()));
        }
        let j = rest[i..].find('}')? + i;
        parts.push((true, rest[i + 1..j].to_string()));
        rest = &rest[j + 1..];
    }
    if !rest.is_empty() {
        parts.push((false, rest.to_string()));
    }
    fn go(parts: &[(bool, String)], text: &str, out: &mut Vec<(String, String)>) -> bool {
        match parts.first() {
            None => text.trim().is_empty(),
            Some((false, lit)) => {
                let t = text.trim_start();
                let l = lit.trim_start();
                if let Some(r) = t.strip_prefix(l) {
                    go(&parts[1..], r, out)
                } else {
                    false
                }
            }
            Some((true, name)) => {
                // 次の固定の文字列が現れる位置ごとに試す / try every position where the next literal occurs
                let next = parts.get(1).map(|p| p.1.trim().to_string());
                match next {
                    None => {
                        let v = text.trim();
                        if v.is_empty() {
                            return false;
                        }
                        out.push((name.clone(), v.to_string()));
                        true
                    }
                    Some(lit) => {
                        let mut from = 0usize;
                        while let Some(k) = text[from..].find(&lit) {
                            let at = from + k;
                            let v = text[..at].trim();
                            if !v.is_empty() {
                                let mut o2 = out.clone();
                                o2.push((name.clone(), v.to_string()));
                                if go(&parts[1..], &text[at..], &mut o2) {
                                    *out = o2;
                                    return true;
                                }
                            }
                            from = at + lit.len().max(1);
                        }
                        false
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    if go(&parts, text, &mut out) {
        Some(out)
    } else {
        None
    }
}

fn fill(tmpl: &str, caps: &[(String, String)]) -> String {
    let mut s = tmpl.to_string();
    for (k, v) in caps {
        s = s.replace(&format!("{{{}}}", k), v);
    }
    s
}

/// 1 つの規則を 1 つの診断に当てる / apply one rule to one diagnostic
fn apply(rule: &J, d: &Diag, src: &SourceFile, lang: Lang) -> Option<RuleFix> {
    let text = &src.text;
    let kind = rule.get("kind").as_str()?;
    let title = crate::data::pick(rule.get("title"), lang);
    let line = src.line_of(d.span.lo);
    let ls = src.line_start(line) as usize;
    let le = ls + src.line_text(line).len();
    let (lo, hi, new) = match kind {
        "wrap_statement" => {
            let (lo, hi) = crate::lsp::critical_fix_range(text, d.span.lo, d.span.hi)?;
            let body = &text[lo as usize..hi as usize];
            (lo, hi, format!("{}{}{}", rule.get("prefix").as_str().unwrap_or(""), body, rule.get("suffix").as_str().unwrap_or("")))
        }
        "replace_in_span" => {
            let (lo, hi) = (d.span.lo as usize, (d.span.hi as usize).min(text.len()));
            let caps = match_pattern(rule.get("pattern").as_str()?, &text[lo..hi])?;
            (lo as u32, hi as u32, fill(rule.get("replace").as_str()?, &caps))
        }
        "replace_in_line" => {
            let lt = &text[ls..le];
            let pat = rule.get("pattern").as_str()?;
            // 行の中の部分文字列ごとに試す（いちばん短い一致） / try substrings of the line (shortest match)
            let first_lit = pat.split('{').next().unwrap_or("");
            let mut found = None;
            let starts: Vec<usize> = if first_lit.is_empty() { (0..lt.len()).filter(|i| lt.is_char_boundary(*i)).collect() } else { lt.match_indices(first_lit).map(|(i, _)| i).collect() };
            'outer: for a in starts {
                for b in (a + 1)..=lt.len() {
                    if !lt.is_char_boundary(b) {
                        continue;
                    }
                    if let Some(c) = match_pattern(pat, &lt[a..b]) {
                        found = Some((a, b, c));
                        break 'outer;
                    }
                }
            }
            let (a, b, caps) = found?;
            ((ls + a) as u32, (ls + b) as u32, fill(rule.get("replace").as_str()?, &caps))
        }
        "prefix_line_token" => {
            let tok = rule.get("token").as_str()?;
            let lt = &text[ls..le];
            let i = lt.find(tok)?;
            ((ls + i) as u32, (ls + i) as u32, rule.get("insert").as_str()?.to_string())
        }
        // 診断の場所を含む関数の宣言（`fn` / `critical fn` で始まる行。付いている属性の上）に、属性などを差し込む
        // insert something (an attribute, say) before the declaration of the function containing the diagnostic (the line
        // starting with `fn` / `critical fn`, above its attributes)
        "insert_before_enclosing_fn" => {
            let mut l = line;
            let is_fn = |t: &str| {
                let t = t.trim_start();
                t.starts_with("fn ") || t.starts_with("critical fn ") || t.starts_with("async fn ") || t.starts_with("pub fn ")
            };
            while l > 0 && !is_fn(src.line_text(l)) {
                l -= 1;
            }
            if !is_fn(src.line_text(l)) {
                return None;
            }
            while l > 0 && src.line_text(l - 1).trim_start().starts_with("#[") {
                l -= 1;
            }
            let at = src.line_start(l);
            let indent: String = src.line_text(l).chars().take_while(|c| *c == ' ').collect();
            (at, at, format!("{}{}\n", indent, rule.get("insert").as_str()?))
        }
        // 診断の最初の注記の場所（たとえば計算コアのルートの spawn_on）を書き換える / rewrite the place of the diagnostic's
        // first note (the spawn_on of a compute-core root, say)
        "replace_in_note" => {
            let n = d.notes.first()?;
            let (lo, hi) = (n.span.lo as usize, (n.span.hi as usize).min(text.len()));
            let caps = match_pattern(rule.get("pattern").as_str()?, &text[lo..hi])?;
            (lo as u32, hi as u32, fill(rule.get("replace").as_str()?, &caps))
        }
        _ => return None,
    };
    let source = format!("{}{}{}", &text[..lo as usize], new, &text[hi as usize..]);
    Some(RuleFix { title, source, lo, hi, new_text: new })
}

/// 診断に当てはまる規則をすべて当て、コンパイルし直して検証する（(修正, 検証済み) の並び）
/// apply every matching rule to a diagnostic and verify by recompiling ((fix, verified) pairs)
pub fn fixes_for(d: &Diag, src: &SourceFile, o: &crate::driver::Opts) -> Vec<(RuleFix, bool)> {
    let mut out = Vec::new();
    for r in crate::data::fixes() {
        if r.get("code").as_str() != Some(d.code) {
            continue;
        }
        let Some(f) = apply(r, d, src, o.lang) else { continue };
        let mut oo = *o;
        oo.gen_c = false;
        let again = crate::driver::compile_opts(&src.path, &f.source, &oo);
        // 同じ番号のエラーが、書き換えた範囲の近くに残っていなければ検証済み / verified when no error of the same code
        // remains near the rewritten range
        let shift = f.new_text.len() as i64 - (f.hi as i64 - f.lo as i64);
        let near = |x: &Diag| {
            let lo = f.lo as i64 - 2;
            let hi = f.hi as i64 + shift.max(0) + 2;
            (x.span.lo as i64) >= lo && (x.span.lo as i64) <= hi
        };
        // 警告（F3901・F2707）は、同じ番号の警告が近くから消え、エラーが増えなければ検証済み / a warning (F3901, F2707) is
        // verified when the warning of the same code is gone from nearby and no error was added
        let verified = if d.is_error() {
            !again.diags.iter().any(|x| x.is_error() && x.code == d.code && near(x)) && again.diags.iter().filter(|x| x.is_error()).count() < src_errors(src, o)
        } else {
            !again.diags.iter().any(|x| x.code == d.code && x.span.lo as i64 >= d.span.lo as i64 - 2 && (x.span.lo as i64) <= d.span.hi as i64 + shift.max(0) + 2) && again.diags.iter().filter(|x| x.is_error()).count() <= src_errors(src, o)
        };
        out.push((f, verified));
    }
    out
}

fn src_errors(src: &SourceFile, o: &crate::driver::Opts) -> usize {
    let mut oo = *o;
    oo.gen_c = false;
    crate::driver::compile_opts(&src.path, &src.text, &oo).diags.iter().filter(|x| x.is_error()).count()
}

/// `fascon fix`: 検証済みの修正を、エラーが無くなるか進まなくなるまで繰り返し当てる（当てた修正の見出しを返す）
/// `fascon fix`: apply verified fixes repeatedly until no error remains or nothing improves (returns the applied titles)
pub fn fix_all(path: &str, text: &str, o: &crate::driver::Opts) -> (String, Vec<String>) {
    let mut cur = text.to_string();
    let mut applied = Vec::new();
    for _ in 0..16 {
        let mut oo = *o;
        oo.gen_c = false;
        let cm = crate::driver::compile_opts(path, &cur, &oo);
        let src = SourceFile::new(path, &cur);
        let mut next = None;
        for d in cm.diags.iter().filter(|d| d.is_error()) {
            if let Some((f, _)) = fixes_for(d, &src, o).into_iter().find(|(_, v)| *v) {
                next = Some(f);
                break;
            }
        }
        match next {
            Some(f) => {
                applied.push(f.title.clone());
                cur = f.source;
            }
            None => break,
        }
    }
    (cur, applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pattern() {
        let c = match_pattern("{a} == {b}", "pin[0] == 7").unwrap();
        assert_eq!(fill("ct_eq(&{a}, &{b})", &c), "ct_eq(&pin[0], &7)");
        assert!(match_pattern("dma own {t},", "buf: dma own [u8; 2048], len").is_none());
        let c2 = match_pattern("dma own {t},", "dma own [u8; 2048],").unwrap();
        assert_eq!(c2[0].1, "[u8; 2048]");
    }
}
