//! 1.0 の仕様辞書（日英） / the 1.0 reference dictionary (Japanese and English)

use crate::diag::Lang;
use crate::json::J;

pub struct Entry {
    pub name: &'static str,
    pub cat: (&'static str, &'static str),
    pub summary: (&'static str, &'static str),
    pub detail: (&'static str, &'static str),
    pub constraints: (&'static str, &'static str),
    pub remedy: (&'static str, &'static str),
    pub aliases: &'static [&'static str],
}

// 項目は data/dictionary.json（コンパイラに埋め込み、上書きのファイルを重ねる: src/data.rs）。ここにはもう文を書かない
// the entries live in data/dictionary.json (embedded in the compiler, with override files layered on top: src/data.rs);
// no text is written here any more

pub fn entries_json(lang: Lang) -> Vec<J> {
    entries()
        .iter()
        .map(|e| {
            J::obj(vec![
                ("name", J::s(e.name)),
                ("category", J::s(lang.pick(e.cat.0, e.cat.1))),
                ("summary", J::s(lang.pick(e.summary.0, e.summary.1))),
                ("detail", J::s(lang.pick(e.detail.0, e.detail.1))),
                ("constraints", J::s(lang.pick(e.constraints.0, e.constraints.1))),
                ("remedy", J::s(lang.pick(e.remedy.0, e.remedy.1))),
                ("aliases", J::Arr(e.aliases.iter().map(|a| J::s(*a)).collect())),
            ])
        })
        .collect()
}

/// 名前か別名が検索語と一致する項目（大文字と小文字は区別しない） / entries whose name or alias equals the term
/// (case-insensitive)
pub fn find_exact(term: &str) -> Vec<&'static Entry> {
    let t = term.to_lowercase();
    entries().iter().filter(|e| e.name.to_lowercase() == t || e.aliases.iter().any(|a| a.to_lowercase() == t)).collect()
}

/// JSON の項目のうち、名前か別名が検索語と一致するもの / the JSON entries whose name or alias equals the term
pub fn filter_json_exact(v: &[J], term: &str) -> Vec<J> {
    let t = term.to_lowercase();
    v.iter().filter(|e| json_name(e) == t || json_aliases(e).iter().any(|a| *a == t)).cloned().collect()
}

fn json_name(e: &J) -> String {
    e.get("name").as_str().unwrap_or("").to_lowercase()
}

fn json_aliases(e: &J) -> Vec<String> {
    match e.get("aliases") {
        J::Arr(a) => a.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect(),
        _ => Vec::new(),
    }
}

pub fn find(term: &str) -> Vec<&'static Entry> {
    let t = term.to_lowercase();
    let mut v: Vec<&Entry> = entries().iter().filter(|e| e.name.to_lowercase() == t || e.aliases.iter().any(|a| a.to_lowercase() == t)).collect();
    if v.is_empty() {
        v = entries()
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&t) || e.aliases.iter().any(|a| a.to_lowercase().contains(&t)) || e.summary.0.contains(term) || e.summary.1.to_lowercase().contains(&t))
            .collect();
    }
    v
}

/// JSON の項目（0.1.0 の辞書など）を `find` と同じ規則で絞る: 名前か別名が一致する項目、無ければ名前・別名・要約に
/// 検索語を含む項目。検索語が空なら全件
/// filter JSON entries (such as those of the 0.1.0 dictionary) with the rules of `find`: entries whose name or alias
/// equals the term, otherwise entries whose name, aliases or summary contain it; an empty term keeps everything
pub fn filter_json(v: Vec<J>, term: &str) -> Vec<J> {
    if term.is_empty() {
        return v;
    }
    let t = term.to_lowercase();
    let exact = filter_json_exact(&v, term);
    if !exact.is_empty() {
        return exact;
    }
    v.into_iter()
        .filter(|e| json_name(e).contains(&t) || json_aliases(e).iter().any(|a| a.contains(&t)) || e.get("summary").as_str().map_or(false, |s| s.to_lowercase().contains(&t)))
        .collect()
}

pub fn render(e: &Entry, lang: Lang) -> String {
    render_parts(
        e.name,
        lang.pick(e.cat.0, e.cat.1),
        lang.pick(e.summary.0, e.summary.1),
        lang.pick(e.detail.0, e.detail.1),
        lang.pick(e.constraints.0, e.constraints.1),
        lang.pick(e.remedy.0, e.remedy.1),
        lang == Lang::Ja,
    )
}

/// 1 項目の文（1.0 の項目と、JSON で受け取った 0.1.0 の項目で共通） / the text of one entry (shared by 1.0 entries and
/// 0.1.0 entries received as JSON)
pub fn render_parts(name: &str, cat: &str, summary: &str, detail: &str, constraints: &str, remedy: &str, ja: bool) -> String {
    let pick = |j: &'static str, e: &'static str| if ja { j } else { e };
    let mut s = format!("■ {}  [{}]\n  {}\n", name, cat, summary);
    if !detail.is_empty() {
        s.push_str(&format!("  {}: {}\n", pick("意味", "Meaning"), detail));
    }
    if !constraints.is_empty() {
        s.push_str(&format!("  {}: {}\n", pick("制約", "Constraints"), constraints));
    }
    if !remedy.is_empty() {
        s.push_str(&format!("  {}: {}\n", pick("対策", "Remedy"), remedy));
    }
    s
}

/// 両言語の辞書（data/dictionary.json の形） / the bilingual dictionary (the shape of data/dictionary.json)
pub fn dump_bilingual() -> J {
    let pair = |p: (&str, &str)| J::obj(vec![("ja", J::s(p.0)), ("en", J::s(p.1))]);
    J::Arr(
        entries()
            .iter()
            .map(|e| {
                J::obj(vec![
                    ("name", J::s(e.name)),
                    ("category", pair(e.cat)),
                    ("summary", pair(e.summary)),
                    ("detail", pair(e.detail)),
                    ("constraints", pair(e.constraints)),
                    ("remedy", pair(e.remedy)),
                    ("aliases", J::Arr(e.aliases.iter().map(|a| J::s(*a)).collect())),
                ])
            })
            .collect(),
    )
}

pub fn entries() -> &'static [Entry] {
    crate::data::dictionary()
}
