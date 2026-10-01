//! Token model for the marked v18 lexer port (`components/markdown.ts`).
//!
//! `tokens` fields that marked fills through `lexer.inline()` hold a slot index into the lexer's
//! inline queue ([`InlineSlot`]) rather than a nested vector, because Rust cannot hand out a
//! mutable reference into a vector it is still building.

pub type InlineSlot = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCell {
    pub text: String,
    pub tokens: InlineSlot,
    pub header: bool,
    pub align: Align,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Space {
        raw: String,
    },
    Code {
        raw: String,
        lang: Option<String>,
        text: String,
        code_block_style: Option<&'static str>,
    },
    Heading {
        raw: String,
        depth: u8,
        text: String,
        tokens: InlineSlot,
    },
    Hr {
        raw: String,
    },
    Blockquote {
        raw: String,
        text: String,
        tokens: Vec<Token>,
    },
    List {
        raw: String,
        ordered: bool,
        start: u32,
        loose: bool,
        items: Vec<Token>,
    },
    ListItem {
        raw: String,
        task: bool,
        checked: bool,
        loose: bool,
        text: String,
        tokens: Vec<Token>,
    },
    Checkbox {
        raw: String,
        checked: bool,
    },
    Html {
        raw: String,
        block: bool,
        pre: bool,
        text: String,
        in_link: bool,
        in_raw_block: bool,
    },
    Def {
        tag: String,
        raw: String,
        href: String,
        title: Option<String>,
    },
    Table {
        raw: String,
        header: Vec<TableCell>,
        align: Vec<Align>,
        rows: Vec<Vec<TableCell>>,
    },
    Paragraph {
        raw: String,
        text: String,
        tokens: InlineSlot,
    },
    Text {
        raw: String,
        text: String,
        tokens: Option<InlineSlot>,
        escaped: bool,
    },
    Escape {
        raw: String,
        text: String,
    },
    Link {
        raw: String,
        href: String,
        title: Option<String>,
        text: String,
        tokens: InlineSlot,
        autolink: bool,
    },
    Image {
        raw: String,
        href: String,
        title: Option<String>,
        text: String,
        tokens: InlineSlot,
    },
    Strong {
        raw: String,
        text: String,
        tokens: InlineSlot,
    },
    Em {
        raw: String,
        text: String,
        tokens: InlineSlot,
    },
    Codespan {
        raw: String,
        text: String,
    },
    Br {
        raw: String,
    },
    Del {
        raw: String,
        text: String,
        tokens: InlineSlot,
    },
    LatexBlock {
        raw: String,
        text: String,
    },
    LatexInline {
        raw: String,
        text: String,
    },
    LatexLiteral {
        raw: String,
        text: String,
    },
}

impl Token {
    pub fn type_name(&self) -> &'static str {
        match self {
            Token::Space { .. } => "space",
            Token::Code { .. } => "code",
            Token::Heading { .. } => "heading",
            Token::Hr { .. } => "hr",
            Token::Blockquote { .. } => "blockquote",
            Token::List { .. } => "list",
            Token::ListItem { .. } => "list_item",
            Token::Checkbox { .. } => "checkbox",
            Token::Html { .. } => "html",
            Token::Def { .. } => "def",
            Token::Table { .. } => "table",
            Token::Paragraph { .. } => "paragraph",
            Token::Text { .. } => "text",
            Token::Escape { .. } => "escape",
            Token::Link { .. } => "link",
            Token::Image { .. } => "image",
            Token::Strong { .. } => "strong",
            Token::Em { .. } => "em",
            Token::Codespan { .. } => "codespan",
            Token::Br { .. } => "br",
            Token::Del { .. } => "del",
            Token::LatexBlock { .. } => "latex_block",
            Token::LatexInline { .. } => "latex_inline",
            Token::LatexLiteral { .. } => "latex_literal",
        }
    }

    pub fn raw(&self) -> &str {
        match self {
            Token::Space { raw }
            | Token::Hr { raw }
            | Token::Code { raw, .. }
            | Token::Heading { raw, .. }
            | Token::Blockquote { raw, .. }
            | Token::List { raw, .. }
            | Token::ListItem { raw, .. }
            | Token::Checkbox { raw, .. }
            | Token::Html { raw, .. }
            | Token::Def { raw, .. }
            | Token::Table { raw, .. }
            | Token::Paragraph { raw, .. }
            | Token::Text { raw, .. }
            | Token::Escape { raw, .. }
            | Token::Link { raw, .. }
            | Token::Image { raw, .. }
            | Token::Strong { raw, .. }
            | Token::Em { raw, .. }
            | Token::Codespan { raw, .. }
            | Token::Br { raw }
            | Token::Del { raw, .. }
            | Token::LatexBlock { raw, .. }
            | Token::LatexInline { raw, .. }
            | Token::LatexLiteral { raw, .. } => raw,
        }
    }

    pub fn set_raw(&mut self, value: String) {
        match self {
            Token::Space { raw }
            | Token::Hr { raw }
            | Token::Code { raw, .. }
            | Token::Heading { raw, .. }
            | Token::Blockquote { raw, .. }
            | Token::List { raw, .. }
            | Token::ListItem { raw, .. }
            | Token::Checkbox { raw, .. }
            | Token::Html { raw, .. }
            | Token::Def { raw, .. }
            | Token::Table { raw, .. }
            | Token::Paragraph { raw, .. }
            | Token::Text { raw, .. }
            | Token::Escape { raw, .. }
            | Token::Link { raw, .. }
            | Token::Image { raw, .. }
            | Token::Strong { raw, .. }
            | Token::Em { raw, .. }
            | Token::Codespan { raw, .. }
            | Token::Br { raw }
            | Token::Del { raw, .. }
            | Token::LatexBlock { raw, .. }
            | Token::LatexInline { raw, .. }
            | Token::LatexLiteral { raw, .. } => *raw = value,
        }
    }
}
