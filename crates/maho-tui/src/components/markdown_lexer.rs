//! Port of marked v18 `src/Lexer.ts` + `src/Tokenizer.ts` (gfm, non-pedantic), plus the two LaTeX
//! tokenizer extensions and the strict strikethrough tokenizer declared by
//! `packages/tui/src/components/markdown.ts`.
//!
//! Index arithmetic follows marked: positions and lengths are UTF-16 code units, so
//! [`utf16_len`]/[`slice_utf16`] convert the byte offsets Rust regexes report.

use std::collections::HashMap;
use std::sync::LazyLock;

use super::markdown_helpers::{
    expand_tabs, indent_code_compensation, normalize_label, rtrim, split_cells,
    trim_trailing_blank_lines, Rule,
};
use super::markdown_rules as rules;
use super::markdown_token::{Align, Inline, InlineSlot, TableCell, Token};

pub fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn byte_at_utf16(value: &str, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    let mut units = 0usize;
    for (byte, character) in value.char_indices() {
        if units >= index {
            return byte;
        }
        units += character.len_utf16();
    }
    value.len()
}

pub fn slice_utf16(value: &str, from: usize, to: usize) -> &str {
    let start = byte_at_utf16(value, from);
    let end = byte_at_utf16(value, to);
    if start >= end {
        return "";
    }
    &value[start..end]
}

pub(crate) fn drop_utf16(value: &str, count: usize) -> &str {
    slice_utf16(value, count, utf16_len(value))
}

pub fn find_inline_math_pub(src: &str, open: &str, close: &str) -> Option<(String, String)> {
    find_inline_math(src, open, close)
}

pub fn find_malformed_inline_span_pub(src: &str, open: &str, close: &str) -> Option<String> {
    find_malformed_inline_span(src, open, close)
}

pub fn repeated_malformed_openers_pub(src: &str, opener: &str) -> Option<String> {
    repeated_malformed_openers(src, opener)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub href: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub in_link: bool,
    pub in_raw_block: bool,
    pub link_emitted: bool,
    pub top: bool,
}

#[derive(Debug, Clone)]
pub struct InlineEntry {
    pub src: String,
    pub slot: InlineSlot,
}

pub struct Lexer {
    pub links: HashMap<String, Link>,
    pub inline_slots: Vec<Vec<Token>>,
    pub state: State,
    pub inline_queue: Vec<InlineEntry>,
    pub inline_prefix: super::markdown_lexer_inline::InlinePrefixState,
}

impl Default for Lexer {
    fn default() -> Self {
        Self::new()
    }
}

fn rule(source: &str, flags: &str) -> Rule {
    Rule::new(source, flags)
}

static R_NEWLINE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_NEWLINE, ""));
static R_CODE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_CODE, ""));
static R_CODE_REMOVE_INDENT: LazyLock<Rule> =
    LazyLock::new(|| Rule::new(rules::OTHER_CODEREMOVEINDENT, "gm"));
static R_FENCES: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_FENCES, ""));
static R_HEADING: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_HEADING, ""));
static R_HR: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_HR, ""));
static R_BLOCKQUOTE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_BLOCKQUOTE, ""));
static R_LIST: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_LIST, ""));
static R_HTML: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_HTML, "i"));
static R_DEF: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_DEF, ""));
static R_TABLE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_TABLE, ""));
static R_LHEADING: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_LHEADING, ""));
static R_PARAGRAPH: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_PARAGRAPH, ""));
static R_TEXT: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::BLOCK_TEXT, ""));

static R_ENDING_HASH: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_ENDINGHASH, ""));
static R_ENDING_SPACE_TAB: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_ENDINGSPACETABCHAR, ""));
static R_TAB_CHAR_GLOBAL: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_TABCHARGLOBAL, "g"));
static R_BLANK_LINE: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_BLANKLINE, ""));
static R_DOUBLE_BLANK_LINE: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_DOUBLEBLANKLINE, ""));
static R_BLOCKQUOTE_START: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_BLOCKQUOTESTART, ""));
static R_BQ_SETEXT_REPLACE: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_BLOCKQUOTESETEXTREPLACE, "g"));
static R_BQ_SETEXT_REPLACE2: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_BLOCKQUOTESETEXTREPLACE2, "gm"));
static R_LIST_IS_TASK: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_LISTISTASK, ""));
static R_LIST_REPLACE_TASK: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_LISTREPLACETASK, ""));
static R_LIST_TASK_CHECKBOX: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_LISTTASKCHECKBOX, ""));
static R_ANY_LINE: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_ANYLINE, ""));
static R_HREF_BRACKETS: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_HREFBRACKETS, ""));
static R_TABLE_DELIMITER: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_TABLEDELIMITER, ""));
static R_TABLE_ALIGN_CHARS: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_TABLEALIGNCHARS, "g"));
static R_TABLE_ROW_BLANK_LINE: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_TABLEROWBLANKLINE, ""));
static R_TABLE_ALIGN_RIGHT: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_TABLEALIGNRIGHT, ""));
static R_TABLE_ALIGN_CENTER: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_TABLEALIGNCENTER, ""));
static R_TABLE_ALIGN_LEFT: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_TABLEALIGNLEFT, ""));
static R_MULTIPLE_SPACE: LazyLock<Rule> = LazyLock::new(|| rule(rules::OTHER_MULTIPLESPACEGLOBAL, "g"));

static R_ANY_PUNCTUATION: LazyLock<Rule> =
    LazyLock::new(|| Rule::new(rules::INLINE_ANYPUNCTUATION, "gu"));

fn list_item_regex(bull: &str) -> Rule {
    Rule::new(&format!(r"^( {{0,3}}{bull})((?:[\t ][^\n]*)?(?:\n|$))"), "")
}

/// marked's `cachedIndentRegex` builds each indentation-sensitive rule from `indent - 1` clamped to
/// `0..=3`, not from `indent` itself.
fn cached_indent(indent: usize) -> usize {
    indent.saturating_sub(1).min(3)
}

fn next_bullet_regex(indent: usize) -> Rule {
    Rule::new(
        &format!(
            r"^ {{0,{}}}(?:[*+-]|\d{{1,9}}[.)])((?:[ \t][^\n]*)?(?:\n|$))",
            cached_indent(indent)
        ),
        "",
    )
}

fn hr_regex(indent: usize) -> Rule {
    Rule::new(
        &format!(
            r"^ {{0,{}}}((?:-[\t ]*){{3,}}|(?:_[ \t]*){{3,}}|(?:\*[ \t]*){{3,}})(?:\n+|$)",
            cached_indent(indent)
        ),
        "",
    )
}

fn fences_begin_regex(indent: usize) -> Rule {
    Rule::new(
        &format!(r"^ {{0,{}}}(?:```|~~~)", cached_indent(indent)),
        "",
    )
}

fn heading_begin_regex(indent: usize) -> Rule {
    Rule::new(&format!(r"^ {{0,{}}}#", cached_indent(indent)), "")
}

fn blockquote_begin_regex(indent: usize) -> Rule {
    Rule::new(&format!(r"^ {{0,{}}}>", cached_indent(indent)), "")
}

const TAG: &str = "address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul";

fn html_begin_regex(indent: usize) -> Rule {
    Rule::new(
        &format!(
            r"^ {{0,{}}}(?:</?(?:{TAG})(?: +|$|/?>)|<(?:script|pre|style|textarea|!--))",
            cached_indent(indent)
        ),
        "i",
    )
}

const MAX_LATEX_FORMULA_LENGTH: usize = 4096;

pub(crate) fn is_word_character(character: char) -> bool {
    static WORD: LazyLock<Rule> = LazyLock::new(|| rule(r"^[\p{L}\p{M}\p{N}_]$", "u"));
    WORD.matches(&character.to_string())
}

pub(crate) fn starts_with_chars(chars: &[char], index: usize, pattern: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    chars.len() >= index + pattern.len() && chars[index..index + pattern.len()] == pattern[..]
}

pub(crate) fn find_inline_math(src: &str, open: &str, close: &str) -> Option<(String, String)> {
    if !src.starts_with(open) {
        return None;
    }
    let chars: Vec<char> = src.chars().collect();
    let body_start = open.chars().count();
    let scan_end = chars.len().min(body_start + MAX_LATEX_FORMULA_LENGTH + 1);
    let mut index = body_start;
    while index < scan_end {
        if chars[index] == '\n' || chars[index] == '`' {
            return None;
        }
        if starts_with_chars(&chars, index, close) {
            let text: String = chars[body_start..index].iter().collect();
            let normalized = if open == close {
                text.clone()
            } else {
                text.trim().to_string()
            };
            let has_edge_space =
                text.starts_with(|c: char| c.is_whitespace()) || text.ends_with(char::is_whitespace);
            if normalized.is_empty() || (open == close && has_edge_space) {
                return None;
            }
            let raw: String = chars[..index + close.chars().count()].iter().collect();
            return Some((raw, normalized));
        }
        if open != close && starts_with_chars(&chars, index, open) {
            return None;
        }
        if chars[index] == '\\' {
            if chars.get(index + 1).is_some_and(|c| *c == '\n' || *c == '\r') {
                return None;
            }
            index += 1;
        }
        index += 1;
    }
    None
}

pub(crate) fn find_malformed_inline_span(src: &str, open: &str, close: &str) -> Option<String> {
    if !src.starts_with(open) {
        return None;
    }
    let chars: Vec<char> = src.chars().collect();
    let scan_end = chars.len().min(open.chars().count() + MAX_LATEX_FORMULA_LENGTH + 1);
    let mut competing_opener = false;
    let mut closers: Vec<&str> = vec![close];
    let mut index = open.chars().count();
    while index < scan_end {
        if chars[index] == '\n' || chars[index] == '`' {
            return competing_opener.then(|| chars[..index].iter().collect());
        }
        let nested_open = if starts_with_chars(&chars, index, "\\(") {
            Some(("\\(", "\\)"))
        } else if starts_with_chars(&chars, index, "\\[") {
            Some(("\\[", "\\]"))
        } else {
            None
        };
        if let Some((nested, nested_close)) = nested_open {
            competing_opener = true;
            closers.push(nested_close);
            index += nested.chars().count();
            continue;
        }
        if let Some(expected) = closers.last().copied()
            && starts_with_chars(&chars, index, expected) {
                closers.pop();
                if closers.is_empty() {
                    return competing_opener
                        .then(|| chars[..index + expected.chars().count()].iter().collect());
                }
                index += expected.chars().count();
                continue;
            }
        if chars[index] == '\\' {
            if chars.get(index + 1).is_some_and(|c| *c == '\n' || *c == '\r') {
                return competing_opener.then(|| chars[..index].iter().collect());
            }
            index += 1;
        }
        index += 1;
    }
    competing_opener.then(|| chars[..scan_end.min(chars.len())].iter().collect())
}

pub(crate) fn is_likely_literal_dollar_body(text: &str) -> bool {
    static BRACE: LazyLock<Rule> = LazyLock::new(|| rule(r"^\{", ""));
    static PAREN: LazyLock<Rule> = LazyLock::new(|| rule(r"^\([^)\r\n]*\)[/\\]$", ""));
    static SYMBOL: LazyLock<Rule> = LazyLock::new(|| rule(r"^[!#$?@*-][/\\]$", ""));
    static WORD: LazyLock<Rule> = LazyLock::new(|| rule(r"^[A-Za-z_][A-Za-z0-9_]*[./\\:-]$", ""));
    static NUMBER: LazyLock<Rule> = LazyLock::new(|| rule(r"^[\d][\d,.]*(?:[-\u{2013}\u{2014}]|[/\\])$", ""));
    BRACE.matches(text)
        || PAREN.matches(text)
        || SYMBOL.matches(text)
        || WORD.matches(text)
        || NUMBER.matches(text)
}

fn find_block_math(src: &str, open: &str, close: &str) -> Option<(String, String)> {
    if !src.starts_with(open) {
        return None;
    }
    let chars: Vec<char> = src.chars().collect();
    let body_start = open.chars().count();
    let scan_end = chars.len().min(body_start + MAX_LATEX_FORMULA_LENGTH + 1);
    let line_validation_end = chars.len().min(scan_end + close.chars().count() + 256);
    let mut index = body_start;
    while index < scan_end {
        if chars[index] == '`' {
            return None;
        }
        if open != close && starts_with_chars(&chars, index, open) {
            return None;
        }
        if !starts_with_chars(&chars, index, close) {
            index += 1;
            continue;
        }
        let close_end = index + close.chars().count();
        let mut raw_line_end = close_end;
        while raw_line_end < line_validation_end
            && chars[raw_line_end] != '\n'
            && (chars[raw_line_end] == ' ' || chars[raw_line_end] == '\t')
        {
            raw_line_end += 1;
        }
        if raw_line_end < chars.len() && chars[raw_line_end] != '\n' {
            return None;
        }
        let text = chars[body_start..index]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
        if text.is_empty() {
            return None;
        }
        let mut raw_end = raw_line_end;
        while raw_end < line_validation_end && chars[raw_end] == '\n' {
            raw_end += 1;
        }
        return Some((chars[..raw_end].iter().collect(), text));
    }
    None
}

pub(crate) fn repeated_malformed_openers(src: &str, opener: &str) -> Option<String> {
    let chars: Vec<char> = src.chars().collect();
    let opener_len = opener.chars().count();
    let mut end = opener_len;
    while starts_with_chars(&chars, end, opener) {
        end += opener_len;
    }
    (end > opener_len).then(|| chars[..end].iter().collect())
}

pub(crate) fn latex_token(kind: &str, raw: &str, text: &str) -> Token {
    match kind {
        "latex_block" => Token::LatexBlock {
            raw: raw.to_string(),
            text: text.to_string(),
        },
        "latex_inline" => Token::LatexInline {
            raw: raw.to_string(),
            text: text.to_string(),
        },
        _ => Token::LatexLiteral {
            raw: raw.to_string(),
            text: text.to_string(),
        },
    }
}

fn block_math_tokenizer(src: &str) -> Option<Token> {
    static LEADING_SPACES: LazyLock<Rule> = LazyLock::new(|| rule(r"^ {0,3}", ""));
    let leading = LEADING_SPACES
        .exec(src)
        .map(|captures| captures.whole().to_string())
        .unwrap_or_default();
    let candidate = drop_utf16(src, utf16_len(&leading));
    let found = find_block_math(candidate, "$$", "$$")
        .or_else(|| find_block_math(candidate, "\\[", "\\]"));
    found.map(|(raw, text)| latex_token("latex_block", &format!("{leading}{raw}"), &text))
}

fn merge_continuation(last: &mut Token, raw: &str, text: &str, kind: MergeKind) {
    let separator = if last.raw().ends_with('\n') { "" } else { "\n" };
    let mut new_raw = last.raw().to_string();
    new_raw.push_str(separator);
    new_raw.push_str(raw);
    last.set_raw(new_raw);
    match (last, kind) {
        (Token::Paragraph { text: ptext, .. }, MergeKind::Paragraph) => {
            ptext.push('\n');
            ptext.push_str(text);
        }
        (Token::Text { text: ptext, .. }, _) => {
            ptext.push('\n');
            ptext.push_str(text);
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeKind {
    Paragraph,
    Other,
}

impl Lexer {
    pub fn new() -> Self {
        Self {
            links: HashMap::new(),
            inline_slots: Vec::new(),
            state: State {
                in_link: false,
                in_raw_block: false,
                link_emitted: false,
                top: true,
            },
            inline_queue: Vec::new(),
            inline_prefix: super::markdown_lexer_inline::InlinePrefixState::default(),
        }
    }

    pub fn lex(&mut self, source: &str) -> Vec<Token> {
        let mut src = String::new();
        let mut chars = source.chars().peekable();
        while let Some(character) = chars.next() {
            if character == '\r' {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                src.push('\n');
            } else {
                src.push(character);
            }
        }

        let mut tokens = Vec::new();
        self.block_tokens(&src, &mut tokens, false);

        let mut index = 0;
        while index < self.inline_queue.len() {
            let entry = self.inline_queue[index].clone();
            let mut out = Vec::new();
            self.inline_tokens(&entry.src, &mut out, "");
            self.inline_slots[entry.slot] = out;
            index += 1;
        }
        let slots = self.inline_slots.clone();
        resolve_inline_tokens(&mut tokens, &slots);
        tokens
    }

    pub fn inline(&mut self, src: &str) -> InlineSlot {
        self.inline_slots.push(Vec::new());
        let slot = self.inline_slots.len() - 1;
        self.inline_queue.push(InlineEntry {
            src: src.to_string(),
            slot,
        });
        slot
    }

    fn last_paragraph_source(&mut self, text: &str) {
        if let Some(entry) = self.inline_queue.last_mut() {
            entry.src = text.to_string();
        }
    }

    fn block_tokens(&mut self, source: &str, tokens: &mut Vec<Token>, last_paragraph_clipped_in: bool) {
        let mut src = source.to_string();
        let mut last_paragraph_clipped = last_paragraph_clipped_in;
        let mut src_length = usize::MAX;

        while !src.is_empty() {
            let length = utf16_len(&src);
            if length < src_length {
                src_length = length;
            } else {
                break;
            }

            if let Some(token) = block_math_tokenizer(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.tokenizer_space(&src) {
                let raw_len = utf16_len(token.raw());
                src = drop_utf16(&src, raw_len).to_string();
                if raw_len == 1 && !tokens.is_empty() {
                    if let Some(last) = tokens.last_mut() {
                        let mut raw = last.raw().to_string();
                        raw.push('\n');
                        last.set_raw(raw);
                    }
                } else {
                    tokens.push(token);
                }
                continue;
            }

            if let Some(token) = self.tokenizer_code(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                let text = match &token {
                    Token::Code { text, .. } => text.clone(),
                    _ => String::new(),
                };
                if matches!(tokens.last(), Some(Token::Paragraph { .. } | Token::Text { .. })) {
                    if let Some(last) = tokens.last_mut() {
                        merge_continuation(last, token.raw(), &text, MergeKind::Other);
                    }
                    let merged = tokens.last().map(|t| t.raw().to_string()).unwrap_or_default();
                    self.last_paragraph_source(&merged);
                } else {
                    tokens.push(token);
                }
                continue;
            }

            if let Some(token) = self.tokenizer_fences(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.tokenizer_heading(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.tokenizer_hr(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.tokenizer_blockquote(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.tokenizer_list(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.tokenizer_html(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.tokenizer_def(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                if matches!(tokens.last(), Some(Token::Paragraph { .. } | Token::Text { .. })) {
                    let raw = token.raw().to_string();
                    if let Some(last) = tokens.last_mut() {
                        merge_continuation(last, &raw, &raw, MergeKind::Other);
                    }
                    let merged = tokens.last().map(|t| t.raw().to_string()).unwrap_or_default();
                    self.last_paragraph_source(&merged);
                } else if let Token::Def { tag, href, title, .. } = &token
                    && !self.links.contains_key(tag) {
                        self.links.insert(
                            tag.clone(),
                            Link {
                                href: href.clone(),
                                title: title.clone(),
                            },
                        );
                        tokens.push(token);
                    }
                continue;
            }

            if let Some(token) = self.tokenizer_table(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                tokens.push(token);
                continue;
            }

            if let Some(token) = self.tokenizer_lheading(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                tokens.push(token);
                continue;
            }

            if self.state.top
                && let Some(token) = self.tokenizer_paragraph(&src) {
                    let raw = token.raw().to_string();
                    let text = match &token {
                        Token::Paragraph { text, .. } => text.clone(),
                        _ => String::new(),
                    };
                    let mergeable =
                        last_paragraph_clipped && matches!(tokens.last(), Some(Token::Paragraph { .. }));
                    if mergeable {
                        self.inline_queue.pop();
                        if let Some(last) = tokens.last_mut() {
                            merge_continuation(last, &raw, &text, MergeKind::Paragraph);
                        }
                        let merged = tokens.last().map(|t| t.raw().to_string()).unwrap_or_default();
                        self.last_paragraph_source(&merged);
                    } else {
                        tokens.push(token);
                    }
                    last_paragraph_clipped = false;
                    src = drop_utf16(&src, utf16_len(&raw)).to_string();
                    continue;
                }

            if let Some(token) = self.tokenizer_text(&src) {
                let raw = token.raw().to_string();
                let text = match &token {
                    Token::Text { text, .. } => text.clone(),
                    _ => String::new(),
                };
                let mergeable = matches!(tokens.last(), Some(Token::Text { .. }));
                if mergeable {
                    self.inline_queue.pop();
                    if let Some(last) = tokens.last_mut() {
                        merge_continuation(last, &raw, &text, MergeKind::Other);
                    }
                    let merged = tokens.last().map(|t| t.raw().to_string()).unwrap_or_default();
                    self.last_paragraph_source(&merged);
                } else {
                    tokens.push(token);
                }
                src = drop_utf16(&src, utf16_len(&raw)).to_string();
                continue;
            }

            break;
        }

        self.state.top = true;
    }

    fn tokenizer_space(&self, src: &str) -> Option<Token> {
        let captures = R_NEWLINE.exec(src)?;
        Some(Token::Space {
            raw: captures.whole().to_string(),
        })
    }

    fn tokenizer_code(&self, src: &str) -> Option<Token> {
        let captures = R_CODE.exec(src)?;
        let raw = trim_trailing_blank_lines(captures.whole());
        let text = R_CODE_REMOVE_INDENT.replace_all(&raw, "");
        Some(Token::Code {
            raw,
            lang: None,
            text,
            code_block_style: Some("indented"),
        })
    }

    fn tokenizer_fences(&self, src: &str) -> Option<Token> {
        let captures = R_FENCES.exec(src)?;
        let raw = captures.whole().to_string();
        let body = captures.group_or(3, "");
        let text = indent_code_compensation(&raw, body);
        let lang = match captures.group(2) {
            Some(value) if !value.is_empty() => {
                Some(R_ANY_PUNCTUATION.replace_all(value.trim(), "$1"))
            }
            _ => None,
        };
        Some(Token::Code {
            raw,
            lang,
            text,
            code_block_style: None,
        })
    }

    fn tokenizer_heading(&mut self, src: &str) -> Option<Token> {
        let captures = R_HEADING.exec(src)?;
        let depth = captures.group_or(1, "").chars().count() as u8;
        let mut text = captures.group_or(2, "").trim().to_string();
        if R_ENDING_HASH.matches(&text) {
            let trimmed = rtrim(&text, '#', false);
            if trimmed.is_empty() || R_ENDING_SPACE_TAB.matches(&trimmed) {
                text = trimmed.trim().to_string();
            }
        }
        let raw = rtrim(captures.whole(), '\n', false);
        let tokens = self.inline(&text);
        Some(Token::Heading {
            raw,
            depth,
            text,
            tokens: Inline::Slot(tokens),
        })
    }

    fn tokenizer_hr(&self, src: &str) -> Option<Token> {
        let captures = R_HR.exec(src)?;
        Some(Token::Hr {
            raw: rtrim(captures.whole(), '\n', false),
        })
    }

    fn tokenizer_blockquote(&mut self, src: &str) -> Option<Token> {
        let captures = R_BLOCKQUOTE.exec(src)?;
        let mut lines: Vec<String> = rtrim(captures.whole(), '\n', false)
            .split('\n')
            .map(str::to_string)
            .collect();
        let mut raw = String::new();
        let mut text = String::new();
        let mut tokens: Vec<Token> = Vec::new();

        while !lines.is_empty() {
            let mut in_blockquote = false;
            let mut current_lines: Vec<String> = Vec::new();
            let mut index = 0usize;
            while index < lines.len() {
                if R_BLOCKQUOTE_START.matches(&lines[index]) {
                    current_lines.push(lines[index].clone());
                    in_blockquote = true;
                } else if !in_blockquote {
                    current_lines.push(lines[index].clone());
                } else {
                    break;
                }
                index += 1;
            }
            lines = lines[index..].to_vec();

            let current_raw = current_lines.join("\n");
            let current_text = R_BQ_SETEXT_REPLACE2.replace_all(
                &R_BQ_SETEXT_REPLACE.replace_all(&current_raw, "\n    $1"),
                "",
            );
            raw = if raw.is_empty() {
                current_raw.clone()
            } else {
                format!("{raw}\n{current_raw}")
            };
            text = if text.is_empty() {
                current_text.clone()
            } else {
                format!("{text}\n{current_text}")
            };

            let top = self.state.top;
            self.state.top = true;
            self.block_tokens(&current_text, &mut tokens, true);
            self.state.top = top;

            if lines.is_empty() {
                break;
            }

            let last = tokens.last().cloned();
            match last {
                Some(Token::Code { .. }) => break,
                Some(Token::Blockquote { raw: old_raw, text: old_text, .. }) => {
                    let continuation = lines.join("\n");
                    let new_text = format!(
                        "{old_raw}\n{}",
                        R_BQ_SETEXT_REPLACE2.replace_all(&continuation, "")
                    );
                    if let Some(new_token) = self.tokenizer_blockquote(&new_text) {
                        let _new_raw = new_token.raw().to_string();
                        let new_text_value = match &new_token {
                            Token::Blockquote { text, .. } => text.clone(),
                            _ => String::new(),
                        };
                        raw = format!("{raw}\n{continuation}");
                        let keep = utf16_len(&text).saturating_sub(utf16_len(&old_text));
                        text = format!("{}{}", slice_utf16(&text, 0, keep), new_text_value);
                        if let Some(slot) = tokens.last_mut() {
                            *slot = new_token;
                        }
                    }
                    break;
                }
                Some(Token::List { raw: old_raw, .. }) => {
                    let new_text = format!("{old_raw}\n{}", lines.join("\n"));
                    if let Some(new_token) = self.tokenizer_list(&new_text) {
                        let new_raw = new_token.raw().to_string();
                        let keep = utf16_len(&raw).saturating_sub(utf16_len(&old_raw));
                        raw = format!("{}{}", slice_utf16(&raw, 0, keep), new_raw);
                        let keep_text = utf16_len(&text).saturating_sub(utf16_len(&old_raw));
                        text = format!("{}{}", slice_utf16(&text, 0, keep_text), new_raw);
                        if let Some(slot) = tokens.last_mut() {
                            *slot = new_token;
                        }
                        let consumed = tokens.last().map(|t| utf16_len(t.raw())).unwrap_or(0);
                        lines = drop_utf16(&new_text, consumed)
                            .split('\n')
                            .map(str::to_string)
                            .collect();
                        continue;
                    }
                    break;
                }
                _ => {}
            }
        }

        Some(Token::Blockquote { raw, text, tokens })
    }

    fn tokenizer_list(&mut self, src: &str) -> Option<Token> {
        let captures = R_LIST.exec(src)?;
        let bullet = captures.group_or(1, "").trim().to_string();
        let is_ordered = bullet.chars().count() > 1;
        let start = if is_ordered {
            bullet[..bullet.len() - 1].parse::<u32>().unwrap_or(0)
        } else {
            1
        };
        let bull = if is_ordered {
            format!("\\d{{1,9}}\\{}", &bullet[bullet.len() - 1..])
        } else {
            format!("\\{bullet}")
        };
        let item_regex = list_item_regex(&bull);

        let mut list_raw = String::new();
        let mut items: Vec<Token> = Vec::new();
        let mut loose = false;
        let mut ends_with_blank_line = false;
        let mut src = src.to_string();

        while !src.is_empty() {
            let mut end_early = false;
            let (mut raw, item_prefix, first_line) = {
                let Some(item) = item_regex.exec(&src) else {
                    break;
                };
                if R_HR.matches(&src) {
                    break;
                }
                (
                    item.whole().to_string(),
                    item.group_or(1, "").to_string(),
                    item.group_or(2, "").split('\n').next().unwrap_or("").to_string(),
                )
            };
            src = drop_utf16(&src, utf16_len(&raw)).to_string();

            let mut line = expand_tabs(&first_line, utf16_len(&item_prefix));
            let mut next_line = src.split('\n').next().unwrap_or("").to_string();
            let mut blank_line = line.trim().is_empty();

            let mut item_contents = String::new();
            let indent = if blank_line {
                utf16_len(&item_prefix) + 1
            } else {
                let found = line.chars().position(|c| c != ' ').unwrap_or(0);
                let found = if found > 4 { 1 } else { found };
                item_contents = line.chars().skip(found).collect();
                found + utf16_len(&item_prefix)
            };

            if blank_line && R_BLANK_LINE.matches(&next_line) {
                raw.push_str(&next_line);
                raw.push('\n');
                src = drop_utf16(&src, utf16_len(&next_line) + 1).to_string();
                end_early = true;
            }

            if !end_early {
                let next_bullet = next_bullet_regex(indent);
                let hr_rule = hr_regex(indent);
                let fences_rule = fences_begin_regex(indent);
                let heading_rule = heading_begin_regex(indent);
                let html_rule = html_begin_regex(indent);
                let blockquote_rule = blockquote_begin_regex(indent);

                while !src.is_empty() {
                    let raw_line = src.split('\n').next().unwrap_or("").to_string();
                    next_line = raw_line.clone();
                    let next_line_without_tabs = R_TAB_CHAR_GLOBAL.replace_all(&next_line, "    ");

                    if fences_rule.matches(&next_line)
                        || heading_rule.matches(&next_line)
                        || html_rule.matches(&next_line)
                        || blockquote_rule.matches(&next_line)
                        || next_bullet.matches(&next_line)
                        || hr_rule.matches(&next_line)
                    {
                        break;
                    }

                    let leading_spaces = next_line_without_tabs
                        .chars()
                        .position(|c| c != ' ')
                        .unwrap_or(next_line_without_tabs.chars().count());
                    if leading_spaces >= indent || next_line.trim().is_empty() {
                        item_contents.push('\n');
                        item_contents.push_str(&next_line_without_tabs.chars().skip(indent).collect::<String>());
                    } else {
                        if blank_line {
                            break;
                        }
                        let line_leading = R_TAB_CHAR_GLOBAL
                            .replace_all(&line, "    ")
                            .chars()
                            .position(|c| c != ' ')
                            .unwrap_or(0);
                        if line_leading >= 4
                            || fences_rule.matches(&line)
                            || heading_rule.matches(&line)
                            || hr_rule.matches(&line)
                        {
                            break;
                        }
                        item_contents.push('\n');
                        item_contents.push_str(&next_line);
                    }

                    blank_line = next_line.trim().is_empty();
                    raw.push_str(&raw_line);
                    raw.push('\n');
                    src = drop_utf16(&src, utf16_len(&raw_line) + 1).to_string();
                    line = next_line_without_tabs.chars().skip(indent).collect();
                }
            }

            if !loose {
                if ends_with_blank_line {
                    loose = true;
                } else if R_DOUBLE_BLANK_LINE.matches(&raw) {
                    ends_with_blank_line = true;
                }
            }

            items.push(Token::ListItem {
                task: R_LIST_IS_TASK.matches(&item_contents),
                raw,
                checked: false,
                loose: false,
                text: item_contents,
                tokens: Vec::new(),
            });
            list_raw.push_str(items.last().map(|t| t.raw()).unwrap_or(""));
        }

        let Some(last) = items.last_mut() else {
            return None;
        };
        let trimmed_raw = last.raw().trim_end().to_string();
        let trimmed_text = match last {
            Token::ListItem { text, .. } => text.trim_end().to_string(),
            _ => String::new(),
        };
        last.set_raw(trimmed_raw);
        if let Token::ListItem { text, .. } = last {
            *text = trimmed_text;
        }
        list_raw = list_raw.trim_end().to_string();

        for item in items.iter_mut() {
            self.state.top = false;
            let item_text = match item {
                Token::ListItem { text, .. } => text.clone(),
                _ => String::new(),
            };
            let mut child_tokens = Vec::new();
            self.block_tokens(&item_text, &mut child_tokens, false);
            if !loose {
                let spacers: Vec<&Token> = child_tokens
                    .iter()
                    .filter(|t| matches!(t, Token::Space { .. }))
                    .collect();
                loose = !spacers.is_empty()
                    && spacers
                        .iter()
                        .any(|t| R_ANY_LINE.matches(t.raw()));
            }
            if let Token::ListItem { tokens, .. } = item {
                *tokens = child_tokens;
            }
        }

        for item in items.iter_mut() {
            let is_task = match item {
                Token::ListItem { task, .. } => *task,
                _ => false,
            };
            if !is_task {
                continue;
            }
            let first_is_textual = matches!(
                item,
                Token::ListItem { tokens, .. }
                    if matches!(tokens.first(), Some(Token::Text { .. } | Token::Paragraph { .. }))
            );
            if !first_is_textual {
                if let Token::ListItem { task, .. } = item {
                    *task = false;
                }
                continue;
            }
            let (new_text, new_first_raw, new_first_text) = {
                let Token::ListItem { text, tokens, .. } = item else {
                    continue;
                };
                let new_text = R_LIST_REPLACE_TASK.replace_all(text, "");
                let (raw_value, text_value) = match tokens.first() {
                    Some(Token::Text { raw, text, .. }) | Some(Token::Paragraph { raw, text, .. }) => (
                        R_LIST_REPLACE_TASK.replace_all(raw, ""),
                        R_LIST_REPLACE_TASK.replace_all(text, ""),
                    ),
                    _ => (String::new(), String::new()),
                };
                (new_text, raw_value, text_value)
            };
            {
                let Token::ListItem { text, tokens, .. } = item else {
                    continue;
                };
                *text = new_text;
                if let Some(first) = tokens.first_mut() {
                    match first {
                        Token::Text { raw, text, .. } | Token::Paragraph { raw, text, .. } => {
                            *raw = new_first_raw;
                            *text = new_first_text;
                        }
                        _ => {}
                    }
                }
            }

            for entry in self.inline_queue.iter_mut().rev() {
                if R_LIST_IS_TASK.matches(&entry.src) {
                    entry.src = R_LIST_REPLACE_TASK.replace_all(&entry.src, "");
                    break;
                }
            }

            let task_raw = match item {
                Token::ListItem { raw, .. } => R_LIST_TASK_CHECKBOX
                    .exec(raw)
                    .map(|captures| captures.whole().to_string()),
                _ => None,
            };
            if let Some(task_raw) = task_raw {
                let checkbox_raw = format!("{task_raw} ");
                let checked = task_raw != "[ ]";
                let checkbox = Token::Checkbox {
                    raw: checkbox_raw.clone(),
                    checked,
                };
                if let Token::ListItem {
                    checked: item_checked,
                    tokens,
                    ..
                } = item
                {
                    *item_checked = checked;
                    if loose {
                        let first_is_paragraph = matches!(
                            tokens.first(),
                            Some(Token::Paragraph { .. })
                        );
                        if first_is_paragraph {
                            if let Some(Token::Paragraph { raw, text, .. }) = tokens.first_mut() {
                                *raw = format!("{checkbox_raw}{raw}");
                                *text = format!("{checkbox_raw}{text}");
                            }
                            tokens.insert(0, checkbox);
                        } else {
                            tokens.insert(
                                0,
                                Token::Paragraph {
                                    raw: checkbox_raw.clone(),
                                    text: checkbox_raw,
                                    tokens: Inline::Slot(0),
                                },
                            );
                            if let Some(Token::Paragraph { tokens: slot, .. }) = tokens.first_mut() {
                                let slot_index = self.inline_slots.len();
                                self.inline_slots.push(vec![checkbox]);
                                *slot = Inline::Slot(slot_index);
                            }
                        }
                    } else {
                        tokens.insert(0, checkbox);
                    }
                }
            }
        }

        if loose {
            for item in items.iter_mut() {
                if let Token::ListItem { loose: item_loose, tokens, .. } = item {
                    *item_loose = true;
                    for token in tokens.iter_mut() {
                        if matches!(token, Token::Text { .. }) {
                            let converted = match token {
                                Token::Text { raw, text, tokens, .. } => Token::Paragraph {
                                    raw: raw.clone(),
                                    text: text.clone(),
                                    tokens: tokens.clone().unwrap_or_default(),
                                },
                                _ => continue,
                            };
                            *token = converted;
                        }
                    }
                }
            }
        }

        Some(Token::List {
            raw: list_raw,
            ordered: is_ordered,
            start,
            loose,
            items,
        })
    }

    fn tokenizer_html(&self, src: &str) -> Option<Token> {
        let captures = R_HTML.exec(src)?;
        let raw = trim_trailing_blank_lines(captures.whole());
        let pre = matches!(captures.group(1), Some("pre") | Some("script") | Some("style"));
        Some(Token::Html {
            raw: raw.clone(),
            block: true,
            pre,
            text: raw,
            in_link: false,
            in_raw_block: false,
        })
    }

    fn tokenizer_def(&self, src: &str) -> Option<Token> {
        let captures = R_DEF.exec(src)?;
        let tag = R_MULTIPLE_SPACE.replace_all(&normalize_label(captures.group_or(1, "")), " ");
        let href = captures.group(2).map_or(String::new(), |value| {
            let stripped = R_HREF_BRACKETS.replace_all(value, "$1");
            R_ANY_PUNCTUATION.replace_all(&stripped, "$1")
        });
        let title = captures.group(3).map(|value| {
            let inner = slice_utf16(value, 1, utf16_len(value).saturating_sub(1));
            R_ANY_PUNCTUATION.replace_all(inner, "$1")
        });
        Some(Token::Def {
            tag,
            raw: rtrim(captures.whole(), '\n', false),
            href,
            title,
        })
    }

    fn tokenizer_table(&mut self, src: &str) -> Option<Token> {
        let captures = R_TABLE.exec(src)?;
        let align_row = captures.group_or(2, "");
        if !R_TABLE_DELIMITER.matches(align_row) {
            return None;
        }
        let headers = split_cells(captures.group_or(1, ""), None);
        let aligns: Vec<String> = R_TABLE_ALIGN_CHARS
            .replace_all(align_row, "")
            .split('|')
            .map(str::to_string)
            .collect();
        let rows_section = captures.group_or(3, "").trim().to_string();
        let rows: Vec<String> = if rows_section.is_empty() {
            Vec::new()
        } else {
            R_TABLE_ROW_BLANK_LINE
                .replace_all(&rows_section, "")
                .split('\n')
                .map(str::to_string)
                .collect()
        };
        if headers.len() != aligns.len() {
            return None;
        }

        let align: Vec<Align> = aligns
            .iter()
            .map(|value| {
                if R_TABLE_ALIGN_RIGHT.matches(value) {
                    Align::Right
                } else if R_TABLE_ALIGN_CENTER.matches(value) {
                    Align::Center
                } else if R_TABLE_ALIGN_LEFT.matches(value) {
                    Align::Left
                } else {
                    Align::None
                }
            })
            .collect();

        let header: Vec<TableCell> = headers
            .iter()
            .enumerate()
            .map(|(index, text)| TableCell {
                text: text.clone(),
                tokens: Inline::Slot(self.inline(text)),
                header: true,
                align: align[index].clone(),
            })
            .collect();

        let column_count = header.len();
        let mut table_rows: Vec<Vec<TableCell>> = Vec::new();
        for row in rows {
            let cells = split_cells(&row, Some(column_count));
            table_rows.push(
                cells
                    .iter()
                    .enumerate()
                    .map(|(index, text)| TableCell {
                        text: text.clone(),
                        tokens: Inline::Slot(self.inline(text)),
                        header: false,
                        align: align[index].clone(),
                    })
                    .collect(),
            );
        }

        Some(Token::Table {
            raw: rtrim(captures.whole(), '\n', false),
            header,
            align,
            rows: table_rows,
        })
    }

    fn tokenizer_lheading(&mut self, src: &str) -> Option<Token> {
        let captures = R_LHEADING.exec(src)?;
        let text = captures.group_or(1, "").trim().to_string();
        let depth = if captures.group_or(2, "").starts_with('=') { 1 } else { 2 };
        let tokens = self.inline(&text);
        Some(Token::Heading {
            raw: rtrim(captures.whole(), '\n', false),
            depth,
            text,
            tokens: Inline::Slot(tokens),
        })
    }

    fn tokenizer_paragraph(&mut self, src: &str) -> Option<Token> {
        let captures = R_PARAGRAPH.exec(src)?;
        let group = captures.group_or(1, "");
        let text = if group.ends_with('\n') {
            drop_utf16(group, utf16_len(group) - 1).to_string()
        } else {
            group.to_string()
        };
        let tokens = self.inline(&text);
        Some(Token::Paragraph {
            raw: captures.whole().to_string(),
            text,
            tokens: Inline::Slot(tokens),
        })
    }

    fn tokenizer_text(&mut self, src: &str) -> Option<Token> {
        let captures = R_TEXT.exec(src)?;
        let raw = captures.whole().to_string();
        let tokens = self.inline(&raw);
        Some(Token::Text {
            raw: raw.clone(),
            text: raw,
            tokens: Some(Inline::Slot(tokens)),
            escaped: false,
        })
    }
}

/// Replaces every [`Inline::Slot`] with the tokens the queue produced, depth-first.
pub fn resolve_inline_tokens(tokens: &mut [Token], slots: &[Vec<Token>]) {
    for token in tokens.iter_mut() {
        resolve_inline_token(token, slots);
    }
}

fn resolve_inline_token(token: &mut Token, slots: &[Vec<Token>]) {
    match token {
        Token::Heading { tokens, .. }
        | Token::Paragraph { tokens, .. }
        | Token::Link { tokens, .. }
        | Token::Image { tokens, .. }
        | Token::Strong { tokens, .. }
        | Token::Em { tokens, .. }
        | Token::Del { tokens, .. } => resolve_inline(tokens, slots),
        Token::Text { tokens, .. } => {
            if let Some(Inline::Slot(slot)) = tokens {
                let mut resolved = slots.get(*slot).cloned().unwrap_or_default();
                resolve_inline_tokens(&mut resolved, slots);
                *tokens = Some(Inline::Resolved(resolved));
            }
        }
        Token::List { items, .. } => {
            for item in items.iter_mut() {
                resolve_inline_token(item, slots);
            }
        }
        Token::ListItem { tokens, .. } => {
            for inner in tokens.iter_mut() {
                resolve_inline_token(inner, slots);
            }
        }
        Token::Blockquote { tokens, .. } => {
            for inner in tokens.iter_mut() {
                resolve_inline_token(inner, slots);
            }
        }
        Token::Table { header, rows, .. } => {
            for cell in header.iter_mut() {
                resolve_inline(&mut cell.tokens, slots);
            }
            for row in rows.iter_mut() {
                for cell in row.iter_mut() {
                    resolve_inline(&mut cell.tokens, slots);
                }
            }
        }
        _ => {}
    }
}

fn resolve_inline(inline: &mut Inline, slots: &[Vec<Token>]) {
    let Inline::Slot(slot) = *inline else {
        return;
    };
    let mut resolved = slots.get(slot).cloned().unwrap_or_default();
    resolve_inline_tokens(&mut resolved, slots);
    *inline = Inline::Resolved(resolved);
}

#[cfg(test)]
#[path = "markdown_lexer_tests.rs"]
mod tests;
