//! 診断（日英両対応） / diagnostics (Japanese and English)

use crate::span::{SourceFile, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Ja,
    En,
}

impl Lang {
    pub fn pick<'a>(self, ja: &'a str, en: &'a str) -> &'a str {
        match self {
            Lang::Ja => ja,
            Lang::En => en,
        }
    }
    pub fn parse(s: &str) -> Option<Lang> {
        let s = s.trim().to_ascii_lowercase();
        if s.starts_with("ja") {
            Some(Lang::Ja)
        } else if s.starts_with("en") {
            Some(Lang::En)
        } else {
            None
        }
    }
    /// 既定の表示言語: FASCON_LANG → OS の表示言語 → 英語
    /// default display language: FASCON_LANG → the OS display language → English
    pub fn detect() -> Lang {
        if let Ok(v) = std::env::var("FASCON_LANG") {
            if let Some(l) = Lang::parse(&v) {
                return l;
            }
        }
        #[cfg(windows)]
        {
            #[link(name = "kernel32")]
            extern "system" {
                fn GetUserDefaultUILanguage() -> u16;
            }
            // 0x11 = LANG_JAPANESE（下位 10 ビットが主言語） / primary language in the low 10 bits
            let id = unsafe { GetUserDefaultUILanguage() };
            return if id & 0x3ff == 0x11 { Lang::Ja } else { Lang::En };
        }
        #[allow(unreachable_code)]
        {
            for k in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
                if let Ok(v) = std::env::var(k) {
                    if v.is_empty() {
                        continue;
                    }
                    return if v.to_ascii_lowercase().starts_with("ja") { Lang::Ja } else { Lang::En };
                }
            }
            Lang::En
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Note {
    pub span: Span,
    pub ja: String,
    pub en: String,
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub code: &'static str,
    pub severity: Severity,
    pub span: Span,
    pub ja: String,
    pub en: String,
    pub notes: Vec<Note>,
    pub hint_ja: Option<String>,
    pub hint_en: Option<String>,
}

impl Diag {
    /// メッセージの頭に文字列を付ける（モジュールの中の診断の場所など） / prefix the message (e.g. where in a module)
    pub fn prefix_message(&mut self, tag: &str) {
        self.ja = format!("{}{}", tag, self.ja);
        self.en = format!("{}{}", tag, self.en);
    }
    pub fn error(code: &'static str, span: Span, ja: impl Into<String>, en: impl Into<String>) -> Diag {
        Diag {
            code,
            severity: Severity::Error,
            span,
            ja: ja.into(),
            en: en.into(),
            notes: Vec::new(),
            hint_ja: None,
            hint_en: None,
        }
    }
    pub fn warning(code: &'static str, span: Span, ja: impl Into<String>, en: impl Into<String>) -> Diag {
        let mut d = Diag::error(code, span, ja, en);
        d.severity = Severity::Warning;
        d
    }
    pub fn note(mut self, span: Span, ja: impl Into<String>, en: impl Into<String>) -> Diag {
        self.notes.push(Note { span, ja: ja.into(), en: en.into() });
        self
    }
    pub fn hint(mut self, ja: impl Into<String>, en: impl Into<String>) -> Diag {
        self.hint_ja = Some(ja.into());
        self.hint_en = Some(en.into());
        self
    }
    pub fn msg(&self, lang: Lang) -> &str {
        lang.pick(&self.ja, &self.en)
    }
    pub fn hint_text(&self, lang: Lang) -> Option<&str> {
        match lang {
            Lang::Ja => self.hint_ja.as_deref(),
            Lang::En => self.hint_en.as_deref(),
        }
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// 端末向けの整形（0.1.0 と同じ書式） / terminal rendering (same layout as 0.1.0)
pub fn render(d: &Diag, src: &SourceFile, lang: Lang) -> String {
    let mut out = String::new();
    let (line, col) = src.line_col(d.span.lo);
    let kind = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    out.push_str(&format!("{}[{}]: {}\n", kind, d.code, d.msg(lang)));
    out.push_str(&format!("  --> {}:{}:{}\n", src.path, line + 1, col + 1));
    let lno = format!("{}", line + 1);
    let pad = " ".repeat(lno.len().max(2) + 1);
    out.push_str(&format!("{}|\n", pad));
    let text = src.line_text(line);
    out.push_str(&format!("{:>w$} | {}\n", lno, text, w = lno.len().max(2) + 0));
    let (eline, ecol) = src.line_col(d.span.hi.max(d.span.lo));
    let width = if eline == line { ecol.saturating_sub(col).max(1) } else { text.chars().count().saturating_sub(col).max(1) };
    out.push_str(&format!("{}| {}{}\n", pad, " ".repeat(col), "^".repeat(width)));
    for n in &d.notes {
        let (nl, nc) = src.line_col(n.span.lo);
        out.push_str(&format!("{}= {}:{}:{}: {}\n", pad, src.path, nl + 1, nc + 1, lang.pick(&n.ja, &n.en)));
    }
    if let Some(h) = d.hint_text(lang) {
        out.push_str(&format!("{}= help: {}\n", pad, h));
    }
    out.push_str(&format!(
        "{}= {}: fascon explain {} --line {}\n",
        pad,
        lang.pick("詳細", "details"),
        src.path,
        line + 1
    ));
    out
}

pub fn sort_dedup(diags: &mut Vec<Diag>) {
    diags.sort_by(|a, b| (a.span.lo, a.code).cmp(&(b.span.lo, b.code)));
    diags.dedup_by(|a, b| a.span == b.span && a.code == b.code);
}
