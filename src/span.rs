//! ソース位置 / source positions.
//!
//! Span はバイトオフセットの半開区間。行・列への変換は SourceFile が行う
//! （端末表示は文字単位の列、LSP は UTF-16 単位の列）。
//! A Span is a half-open byte range. SourceFile converts it to line/column
//! (character columns for the terminal, UTF-16 columns for LSP).

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(lo: u32, hi: u32) -> Span {
        Span { lo, hi }
    }
    pub fn to(self, other: Span) -> Span {
        Span { lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }
}

pub struct SourceFile {
    pub path: String,
    pub text: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn new(path: &str, text: &str) -> SourceFile {
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        SourceFile { path: path.to_string(), text: text.to_string(), line_starts }
    }

    /// 0 始まりの行番号 / zero-based line number
    pub fn line_of(&self, pos: u32) -> usize {
        match self.line_starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    pub fn line_start(&self, line: usize) -> u32 {
        self.line_starts.get(line).copied().unwrap_or(self.text.len() as u32)
    }

    /// (0 始まりの行, 0 始まりの文字列) / (zero-based line, zero-based char column)
    pub fn line_col(&self, pos: u32) -> (usize, usize) {
        let line = self.line_of(pos);
        let start = self.line_start(line) as usize;
        let end = (pos as usize).min(self.text.len());
        let col = self.text.get(start..end).map(|s| s.chars().count()).unwrap_or(0);
        (line, col)
    }

    /// LSP 用（UTF-16 単位の列） / for LSP (UTF-16 code-unit columns)
    pub fn line_col_utf16(&self, pos: u32) -> (usize, usize) {
        let line = self.line_of(pos);
        let start = self.line_start(line) as usize;
        let end = (pos as usize).min(self.text.len());
        let col = self.text.get(start..end).map(|s| s.encode_utf16().count()).unwrap_or(0);
        (line, col)
    }

    /// LSP の (行, UTF-16 列) からバイト位置へ / from an LSP (line, UTF-16 column) to a byte offset
    pub fn offset_of_utf16(&self, line: usize, col16: usize) -> u32 {
        let start = self.line_start(line) as usize;
        let mut units = 0usize;
        for (i, ch) in self.text[start.min(self.text.len())..].char_indices() {
            if ch == '\n' || units >= col16 {
                return (start + i) as u32;
            }
            units += ch.len_utf16();
        }
        self.text.len() as u32
    }

    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_start(line) as usize;
        let end = if line + 1 < self.line_starts.len() {
            (self.line_starts[line + 1] as usize).saturating_sub(1)
        } else {
            self.text.len()
        };
        let s = self.text.get(start.min(self.text.len())..end.min(self.text.len())).unwrap_or("");
        s.strip_suffix('\r').unwrap_or(s)
    }

    pub fn snippet(&self, sp: Span) -> &str {
        let mut lo = (sp.lo as usize).min(self.text.len());
        let mut hi = (sp.hi as usize).min(self.text.len()).max(lo);
        // 文字の途中を指す位置は文字の境界へ寄せる / move positions inside a character to a character boundary
        while lo > 0 && !self.text.is_char_boundary(lo) {
            lo -= 1;
        }
        while hi < self.text.len() && !self.text.is_char_boundary(hi) {
            hi += 1;
        }
        &self.text[lo..hi.max(lo)]
    }
}
