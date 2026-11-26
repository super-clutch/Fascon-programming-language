//! 1 周目のパス: ファイル全体のトップレベル宣言（構造体・関数・static・型の別名・アリーナ・バンプアリーナ・MMIO・ポート）の
//! 名前だけを先に登録する。2 周目（構文解析・名前解決・型検査）は、この表を見るので宣言の順序に縛られない
//! （関数の後ろに `arena` や `bump_arena` を書いても「見つかりません（F2104）」にならない）。
//! Pass 1: register only the names of every top-level declaration of the file (structs, functions, statics, type
//! aliases, arenas, bump arenas, MMIO regions, ports) before anything else. Pass 2 (parsing, name resolution, type
//! checking) consults this table, so declaration order no longer matters (an `arena` or `bump_arena` written after
//! the functions that use it is never "not found" (F2104)).

use crate::lexer::{Tok, Token};
use crate::span::Span;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymKind {
    Struct,
    Fn,
    Static,
    Alias,
    Arena,
    Bump,
    Mmio,
    Port,
}

impl SymKind {
    /// リージョン（式の中で名前だけで使えるもの） / a region (usable by its bare name inside expressions)
    pub fn is_region(self) -> bool {
        matches!(self, SymKind::Arena | SymKind::Bump | SymKind::Mmio | SymKind::Port)
    }
    /// Id を持つリージョン（`Id<名前>` に書ける） / a region with Ids (can appear in `Id<name>`)
    pub fn has_ids(self) -> bool {
        matches!(self, SymKind::Arena | SymKind::Bump)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SymTab {
    pub map: HashMap<String, (SymKind, Span)>,
    /// 宣言の順（報告用） / declaration order (for reports)
    pub order: Vec<String>,
}

impl SymTab {
    pub fn kind(&self, n: &str) -> Option<SymKind> {
        self.map.get(n).map(|x| x.0)
    }
    pub fn is_region(&self, n: &str) -> bool {
        self.kind(n).map_or(false, |k| k.is_region())
    }
    pub fn has_ids(&self, n: &str) -> bool {
        self.kind(n).map_or(false, |k| k.has_ids())
    }
}

/// トークン列を 1 回なめて、深さ 0 の宣言の名前を集める / one sweep over the tokens collecting depth-0 declarations
pub fn prescan(toks: &[Token]) -> SymTab {
    let mut t = SymTab::default();
    let mut depth = 0i32;
    let mut i = 0usize;
    let name_at = |k: usize| -> Option<(String, Span)> {
        match toks.get(k).map(|x| &x.tok) {
            Some(Tok::Ident(s)) | Some(Tok::ArenaName(s)) => Some((s.clone(), toks[k].span)),
            _ => None,
        }
    };
    while i < toks.len() {
        let tk = &toks[i].tok;
        match tk {
            Tok::LBrace => depth += 1,
            Tok::RBrace => depth -= 1,
            _ if depth != 0 => {}
            Tok::Fn | Tok::Struct | Tok::Type | Tok::ArenaKw | Tok::BumpArena | Tok::Mmio | Tok::PortIo | Tok::Static | Tok::Const => {
                let kind = match tk {
                    Tok::Fn => SymKind::Fn,
                    Tok::Struct => SymKind::Struct,
                    Tok::Type => SymKind::Alias,
                    Tok::ArenaKw => SymKind::Arena,
                    Tok::BumpArena => SymKind::Bump,
                    Tok::Mmio => SymKind::Mmio,
                    Tok::PortIo => SymKind::Port,
                    _ => SymKind::Static,
                };
                let mut k = i + 1;
                if kind == SymKind::Static && matches!(toks.get(k).map(|x| &x.tok), Some(Tok::Mut)) {
                    k += 1;
                }
                if let Some((n, sp)) = name_at(k) {
                    if !t.map.contains_key(&n) {
                        t.order.push(n.clone());
                        t.map.insert(n, (kind, sp));
                    }
                }
            }
            // 文脈依存キーワードの項目 / items introduced by contextual keywords
            Tok::Ident(w) if w == "task_arena" || w == "gc_arena" || w == "gpu_arena" || w == "irq" || w == "dma_arena" || w == "dma" || w == "syscall" || w == "trap" || w == "user_mem" || w == "chan" => {
                let kind = if w == "irq" || w == "syscall" || w == "trap" { SymKind::Fn } else { SymKind::Arena };
                let ok = match w.as_str() {
                    "irq" | "syscall" => matches!(toks.get(i + 2).map(|x| &x.tok), Some(Tok::Ident(a)) if a == "at"),
                    "trap" => matches!(toks.get(i + 2).map(|x| &x.tok), Some(Tok::LParen)),
                    "user_mem" => matches!(toks.get(i + 2).map(|x| &x.tok), Some(Tok::For)),
                    "chan" => matches!(toks.get(i + 2).map(|x| &x.tok), Some(Tok::Colon)),
                    "dma" => matches!(toks.get(i + 2).map(|x| &x.tok), Some(Tok::Colon)),
                    _ => true,
                };
                if ok {
                    if let Some((n, sp)) = name_at(i + 1) {
                        if !t.map.contains_key(&n) {
                            t.order.push(n.clone());
                            t.map.insert(n, (kind, sp));
                        }
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    t
}
