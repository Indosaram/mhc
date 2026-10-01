//! Port of the `grok-mermaid` package (v0.2.3) as used by `components/mermaid.ts`.
//!
//! Ported here: the public types, the grapheme-aware width measurement (with the pinned
//! per-code-point table), and `source_box` — the documented fallback every consumer uses when
//! `render` yields no art. The diagram layout engine (`parse.ts`/`layout.ts`/`layout-seq.ts`,
//! ~2200 lines covering flowchart, state, class, ER and sequence grammars) is not ported, so
//! `render` reports no art and every mermaid block takes the source-box path. That is recorded as
//! `partial` in this crate's parity ledger.
pub mod labels;
pub mod source_box;
pub mod width;
pub mod width_data;

pub use source_box::source_box;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cls {
    Border,
    Text,
    Edge,
    EdgeLabel,
    Title,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub cls: Cls,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MermaidArt {
    pub plain: Vec<String>,
    pub styled: Vec<Vec<Span>>,
    pub width: usize,
    pub warnings: Vec<String>,
}

pub fn render(_src: &str) -> Option<MermaidArt> {
    None
}
