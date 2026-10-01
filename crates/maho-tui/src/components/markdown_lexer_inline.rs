//! Inline half of the marked v18 lexer port: `inlineTokens` plus the inline tokenizers, the
//! strict strikethrough override, and the inline LaTeX extension declared by
//! `packages/tui/src/components/markdown.ts`.

use std::sync::LazyLock;

use super::markdown_helpers::{find_closing_bracket, normalize_label, rtrim, Captures, Rule};
use super::markdown_lexer::{
    drop_utf16, find_inline_math, find_malformed_inline_span, is_likely_literal_dollar_body,
    is_word_character, latex_token, repeated_malformed_openers, slice_utf16, utf16_len, Lexer,
};
use super::markdown_rules as rules;
use super::markdown_token::Token;

static R_ESCAPE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_ESCAPE, ""));
static R_INLINE_CODE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_CODE, ""));
static R_BR: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_BR, ""));
static R_INLINE_TEXT: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_TEXT, ""));
static R_TAG: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_TAG, ""));
static R_LINK: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_LINK, ""));
static R_REFLINK: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_REFLINK, ""));
static R_NOLINK: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_NOLINK, ""));
static R_REFLINK_SEARCH: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_REFLINKSEARCH, "g"));
static R_ANY_PUNCTUATION: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_ANYPUNCTUATION, "gu"));
static R_BLOCK_SKIP: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_BLOCKSKIP, "g"));
static R_EM_STRONG_LDELIM: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_EMSTRONGLDELIM, "u"));
static R_EM_STRONG_RDELIM_AST: LazyLock<Rule> =
    LazyLock::new(|| Rule::new(rules::INLINE_EMSTRONGRDELIMAST, "gu"));
static R_EM_STRONG_RDELIM_UND: LazyLock<Rule> =
    LazyLock::new(|| Rule::new(rules::INLINE_EMSTRONGRDELIMUND, "gu"));
static R_PUNCTUATION: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_PUNCTUATION, "u"));
static R_AUTOLINK: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_AUTOLINK, ""));
static R_URL: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_URL, ""));
static R_BACKPEDAL: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE__BACKPEDAL, ""));

static R_START_ATAG: LazyLock<Rule> = LazyLock::new(|| Rule::new(r"^<a ", "i"));
static R_END_ATAG: LazyLock<Rule> = LazyLock::new(|| Rule::new(r"^</a>", "i"));
static R_START_PRE_SCRIPT: LazyLock<Rule> =
    LazyLock::new(|| Rule::new(r"^<(pre|code|kbd|script)(\s|>)", "i"));
static R_END_PRE_SCRIPT: LazyLock<Rule> =
    LazyLock::new(|| Rule::new(r"^</(pre|code|kbd|script)(\s|>)", "i"));
static R_START_ANGLE: LazyLock<Rule> = LazyLock::new(|| Rule::new(r"^<", ""));
static R_END_ANGLE: LazyLock<Rule> = LazyLock::new(|| Rule::new(r">$", ""));
static R_MULTIPLE_SPACE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::OTHER_MULTIPLESPACEGLOBAL, "g"));
static R_STARTING_SPACE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::OTHER_STARTINGSPACECHAR, ""));
static R_ENDING_SPACE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::OTHER_ENDINGSPACECHAR, ""));
static R_NON_SPACE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::OTHER_NONSPACECHAR, ""));
static R_NEWLINE_CHAR: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::OTHER_NEWLINECHARGLOBAL, "g"));
static R_OUTPUT_LINK_REPLACE: LazyLock<Rule> =
    LazyLock::new(|| Rule::new(rules::OTHER_OUTPUTLINKREPLACE, "g"));

static R_STRICT_STRIKETHROUGH: LazyLock<Rule> = LazyLock::new(|| {
    Rule::new(
        r"^(~~)(?=[^\s~])((?:\\[\s\S]|[^\\])*?(?:\\[\s\S]|[^\s~\\]))\1(?=[^~]|$)",
        "",
    )
});

static R_INLINE_LATEX_START: LazyLock<Rule> = LazyLock::new(|| Rule::new(r"\$|\\[()\[\]]", ""));
static R_CODE_SPAN_LITERAL: LazyLock<Rule> =
    LazyLock::new(|| Rule::new(r"^(?:\${1,2}|\\\(|\\\)|\\\[|\\\])", ""));
static R_LATEX_LITERAL: LazyLock<Rule> = LazyLock::new(|| Rule::new(r"^(?:\\\(|\\\)|\\\[|\\\])", ""));

#[derive(Debug, Clone, Default)]
pub struct InlinePrefixState {
    has_unclosed_code_span: bool,
    last_processed_raw: Option<String>,
    processed_token_count: usize,
}

fn previous_raw_character(tokens: &[Token]) -> Option<char> {
    tokens.last().and_then(|token| token.raw().chars().next_back())
}

fn first_character(value: &str) -> Option<char> {
    value.chars().next()
}

impl Lexer {
    fn follows_unclosed_code_span(&mut self, tokens: &[Token]) -> bool {
        let state = &mut self.inline_prefix;
        let previous_processed = state
            .processed_token_count
            .checked_sub(1)
            .and_then(|index| tokens.get(index));
        let stale = previous_processed.map(|token| token.raw().to_string())
            != state.last_processed_raw
            || state.processed_token_count > tokens.len();
        if state.processed_token_count == 0 && state.last_processed_raw.is_none() {
        } else if stale {
            state.has_unclosed_code_span = false;
            state.processed_token_count = 0;
            state.last_processed_raw = None;
        }

        for index in state.processed_token_count..tokens.len() {
            let token = &tokens[index];
            if matches!(token, Token::Text { .. }) && token.raw().contains('`') {
                state.has_unclosed_code_span = true;
            }
        }
        state.processed_token_count = tokens.len();
        state.last_processed_raw = tokens.last().map(|token| token.raw().to_string());
        state.has_unclosed_code_span
    }

    fn inline_latex_extension(&mut self, src: &str, tokens: &[Token]) -> Option<Token> {
        if let Some(literal) = R_CODE_SPAN_LITERAL.exec(src) {
            let literal = literal.whole().to_string();
            if self.follows_unclosed_code_span(tokens) {
                return Some(latex_token("latex_literal", &literal, &literal));
            }
        }
        if src.starts_with('$') {
            if src.starts_with("$$") {
                return Some(latex_token("latex_literal", "$$", "$$"));
            }
            let found = find_inline_math(src, "$", "$");
            if let Some((raw, text)) = &found {
                if is_likely_literal_dollar_body(text) {
                    return Some(latex_token("latex_literal", raw, raw));
                }
            }
            let previous = previous_raw_character(tokens);
            let next = found
                .as_ref()
                .and_then(|(raw, _)| first_character(drop_utf16(src, utf16_len(raw))));
            if let Some((raw, text)) = &found {
                let previous_ok = previous.is_none_or(|c| !is_word_character(c));
                let next_ok = next.is_none_or(|c| !is_word_character(c));
                if previous_ok && next_ok {
                    return Some(latex_token("latex_inline", raw, text));
                }
            }
            return Some(latex_token("latex_literal", "$", "$"));
        }
        if let Some(raw) = find_malformed_inline_span(src, "\\(", "\\)") {
            return Some(latex_token("latex_literal", &raw, &raw));
        }
        if let Some(raw) = find_malformed_inline_span(src, "\\[", "\\]") {
            return Some(latex_token("latex_literal", &raw, &raw));
        }
        if let Some(raw) = repeated_malformed_openers(src, "\\(") {
            return Some(latex_token("latex_literal", &raw, &raw));
        }
        if let Some(raw) = repeated_malformed_openers(src, "\\[") {
            return Some(latex_token("latex_literal", &raw, &raw));
        }
        if let Some((raw, text)) = find_inline_math(src, "\\(", "\\)") {
            let previous = previous_raw_character(tokens);
            let next = first_character(drop_utf16(src, utf16_len(&raw)));
            let previous_ok = previous.is_none_or(|c| !is_word_character(c));
            let next_ok = next.is_none_or(|c| !is_word_character(c));
            if previous_ok && next_ok {
                return Some(latex_token("latex_inline", &raw, &text));
            }
            return None;
        }
        if let Some((raw, text)) = find_inline_math(src, "\\[", "\\]") {
            let previous = previous_raw_character(tokens);
            let next = first_character(drop_utf16(src, utf16_len(&raw)));
            let previous_ok = previous.is_none_or(|c| !is_word_character(c));
            let next_ok = next.is_none_or(|c| !is_word_character(c));
            if previous_ok && next_ok {
                return Some(latex_token("latex_inline", &raw, &text));
            }
            return None;
        }
        let literal = R_LATEX_LITERAL.exec(src)?;
        let raw = literal.whole().to_string();
        let previous = previous_raw_character(tokens);
        if previous.is_some_and(is_word_character) {
            return None;
        }
        Some(latex_token("latex_literal", &raw, &raw))
    }

    fn link_in_text(&self, text: &str) -> bool {
        if !text.contains('[') {
            return false;
        }
        for captures in R_BLOCK_SKIP.exec_all(text) {
            let matched = captures.whole();
            if R_LINK.exec(matched).is_some() {
                let start = captures.start();
                if start > 0 && text.as_bytes().get(start - 1) == Some(&b'!') {
                    continue;
                }
                return true;
            }
        }
        for captures in R_REFLINK_SEARCH.exec_all(text) {
            let matched = captures.whole();
            let ref_start = matched.rfind('[').map(|index| index).unwrap_or(0);
            if matched.starts_with('!') {
                continue;
            }
            let label = slice_utf16(matched, utf16_len(&matched[..ref_start]) + 1, utf16_len(matched).saturating_sub(1));
            if !self.links.contains_key(&normalize_label(label)) {
                continue;
            }
            if ref_start > 1 && !self.link_in_text(slice_utf16(matched, 1, utf16_len(&matched[..ref_start]) - 1)) {
                continue;
            }
            return true;
        }
        false
    }

    fn mask_reflinks(&self, src: &str) -> String {
        let mut result = String::new();
        let mut last = 0usize;
        let mut cursor = 0usize;
        loop {
            let Some(captures) = R_REFLINK_SEARCH.exec_from(src, cursor) else {
                break;
            };
            let matched = captures.whole();
            let start = captures.start();
            let end = captures.end();
            result.push_str(&src[last..start]);
            result.push_str(&self.mask_reflink(matched));
            last = end;
            cursor = if end == start { end + 1 } else { end };
            if cursor > src.len() {
                break;
            }
        }
        result.push_str(&src[last.min(src.len())..]);
        result
    }

    fn mask_reflink(&self, matched: &str) -> String {
        let ref_start = matched.rfind('[').unwrap_or(0);
        let total = utf16_len(matched);
        let ref_start_u16 = utf16_len(&matched[..ref_start]);
        let label = slice_utf16(matched, ref_start_u16 + 1, total.saturating_sub(1));
        if !self.links.contains_key(&normalize_label(label)) {
            return matched.to_string();
        }
        if ref_start_u16 > 1 && !matched.starts_with('!') {
            let text = slice_utf16(matched, 1, ref_start_u16.saturating_sub(1));
            if self.link_in_text(text) {
                return format!(
                    "[{}][{}]",
                    self.mask_reflinks(text),
                    "a".repeat((total - ref_start_u16).saturating_sub(2))
                );
            }
        }
        format!("[{}]", "a".repeat(total.saturating_sub(2)))
    }

    fn masked_source(&self, src: &str) -> String {
        let mut masked = src.to_string();
        if !self.links.is_empty() && src.contains('[') {
            masked = self.mask_reflinks(&masked);
        }
        masked = replace_with_length(&masked, &R_ANY_PUNCTUATION, |_| None);
        masked = replace_with_length(&masked, &R_BLOCK_SKIP, |captures| {
            let matched = captures.whole();
            let offset = captures
                .group(2)
                .map(utf16_len)
                .unwrap_or(0);
            Some(format!(
                "{}{}{}",
                slice_utf16(matched, 0, offset),
                "[",
                "a".repeat(utf16_len(matched).saturating_sub(offset + 2))
            ))
        });
        masked
    }

    pub fn inline_tokens(&mut self, source: &str, tokens: &mut Vec<Token>, prev_char_in: &str) {
        let saved_prefix = std::mem::take(&mut self.inline_prefix);
        let saved_state = self.state.clone();
        let mut src = source.to_string();
        let masked_src = self.masked_source(&src);

        let mut keep_prev_char = false;
        let mut prev_char = prev_char_in.to_string();
        let mut src_length = usize::MAX;

        while !src.is_empty() {
            let length = utf16_len(&src);
            if length < src_length {
                src_length = length;
            } else {
                break;
            }

            if !keep_prev_char {
                prev_char = String::new();
            }
            keep_prev_char = false;

            if let Some(token) = self.inline_latex_extension(&src, tokens) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                push_inline(tokens, token);
                continue;
            }

            if let Some(token) = self.tokenizer_escape(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                push_inline(tokens, token);
                continue;
            }

            if let Some(token) = self.tokenizer_tag(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                push_inline(tokens, token);
                continue;
            }

            if let Some(token) = self.tokenizer_link(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                push_inline(tokens, token);
                continue;
            }

            if let Some(token) = self.tokenizer_reflink(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                let mergeable = matches!(token, Token::Text { .. })
                    && matches!(tokens.last(), Some(Token::Text { .. }));
                if mergeable {
                    if let (Some(Token::Text { raw: lraw, text: ltext, .. }), Token::Text { raw, text, .. }) =
                        (tokens.last_mut(), &token)
                    {
                        lraw.push_str(raw);
                        ltext.push_str(text);
                    }
                } else {
                    push_inline(tokens, token);
                }
                continue;
            }

            if let Some(token) = self.tokenizer_em_strong(&src, &masked_src, &prev_char) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                push_inline(tokens, token);
                continue;
            }

            if let Some(token) = self.tokenizer_codespan(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                push_inline(tokens, token);
                continue;
            }

            if let Some(token) = self.tokenizer_br(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                push_inline(tokens, token);
                continue;
            }

            if let Some(token) = self.tokenizer_del(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                push_inline(tokens, token);
                continue;
            }

            if let Some(token) = self.tokenizer_autolink(&src) {
                src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                push_inline(tokens, token);
                continue;
            }

            if !self.state.in_link {
                if let Some(token) = self.tokenizer_url(&src) {
                    src = drop_utf16(&src, utf16_len(token.raw())).to_string();
                    push_inline(tokens, token);
                    continue;
                }
            }

            let cut_src = clip_to_inline_extension(&src);
            if let Some(token) = self.tokenizer_inline_text(&cut_src) {
                let raw = token.raw().to_string();
                src = drop_utf16(&src, utf16_len(&raw)).to_string();
                if !raw.ends_with('_') {
                    prev_char = raw.chars().next_back().map(String::from).unwrap_or_default();
                }
                keep_prev_char = true;
                let mergeable = matches!(tokens.last(), Some(Token::Text { .. }));
                if mergeable {
                    if let (Some(Token::Text { raw: lraw, text: ltext, .. }), Token::Text { raw, text, .. }) =
                        (tokens.last_mut(), &token)
                    {
                        lraw.push_str(raw);
                        ltext.push_str(text);
                    }
                } else {
                    push_inline(tokens, token);
                }
                continue;
            }

            break;
        }

        self.inline_prefix = saved_prefix;
        self.state = saved_state;
    }

    fn tokenizer_escape(&self, src: &str) -> Option<Token> {
        let captures = R_ESCAPE.exec(src)?;
        Some(Token::Escape {
            raw: captures.whole().to_string(),
            text: captures.group_or(1, "").to_string(),
        })
    }

    fn tokenizer_tag(&mut self, src: &str) -> Option<Token> {
        let captures = R_TAG.exec(src)?;
        let whole = captures.whole();
        if !self.state.in_link && R_START_ATAG.matches(whole) {
            self.state.in_link = true;
        } else if self.state.in_link && R_END_ATAG.matches(whole) {
            self.state.in_link = false;
        }
        if !self.state.in_raw_block && R_START_PRE_SCRIPT.matches(whole) {
            self.state.in_raw_block = true;
        } else if self.state.in_raw_block && R_END_PRE_SCRIPT.matches(whole) {
            self.state.in_raw_block = false;
        }
        Some(Token::Html {
            raw: whole.to_string(),
            in_link: self.state.in_link,
            in_raw_block: self.state.in_raw_block,
            block: false,
            pre: false,
            text: whole.to_string(),
        })
    }

    fn tokenizer_link(&mut self, src: &str) -> Option<Token> {
        let captures = R_LINK.exec(src)?;
        let label_start = if captures.whole().starts_with('!') { 2 } else { 1 };
        if is_label_end_inside_token(src, captures.group_or(1, ""), label_start) {
            return None;
        }
        let mut cap2 = captures.group_or(2, "").to_string();
        let cap3 = captures.group_or(3, "");
        let mut raw = captures.whole().to_string();
        let label = captures.group_or(1, "").to_string();

        let trimmed_url = cap2.trim();
        if R_START_ANGLE.matches(trimmed_url) {
            if !R_END_ANGLE.matches(trimmed_url) {
                return None;
            }
            let inner = slice_utf16(trimmed_url, 0, utf16_len(trimmed_url).saturating_sub(1));
            let rtrimmed = rtrim(inner, '\\', false);
            if (utf16_len(trimmed_url) - utf16_len(&rtrimmed)) % 2 == 0 {
                return None;
            }
        } else {
            let last_paren = find_closing_bracket(&cap2, "()");
            if last_paren == -2 {
                return None;
            }
            if last_paren > -1 {
                let start = if captures.whole().starts_with('!') { 5 } else { 4 };
                let link_len = start + utf16_len(&label) + last_paren as usize;
                cap2 = slice_utf16(&cap2, 0, last_paren as usize).to_string();
                raw = slice_utf16(&raw, 0, link_len).trim().to_string();
            }
        }

        let mut href = cap2.clone();
        let mut title = if cap3.is_empty() {
            String::new()
        } else {
            slice_utf16(cap3, 1, utf16_len(cap3).saturating_sub(1)).to_string()
        };
        href = href.trim().to_string();
        if R_START_ANGLE.matches(&href) {
            href = slice_utf16(&href, 1, utf16_len(&href).saturating_sub(1)).to_string();
        }
        let href = if href.is_empty() {
            href
        } else {
            R_ANY_PUNCTUATION.replace_all(&href, "$1")
        };
        title = if title.is_empty() {
            title
        } else {
            R_ANY_PUNCTUATION.replace_all(&title, "$1")
        };

        self.output_link(&label, &href, &title, &raw, captures.whole().starts_with('!'))
    }

    fn output_link(
        &mut self,
        label: &str,
        href: &str,
        title: &str,
        raw: &str,
        is_image: bool,
    ) -> Option<Token> {
        let text = R_OUTPUT_LINK_REPLACE.replace_all(label, "$1");
        self.state.in_link = true;
        let outer_link_emitted = self.state.link_emitted;
        let outer_in_raw_block = self.state.in_raw_block;
        self.state.link_emitted = false;
        let mut inner = Vec::new();
        self.inline_tokens(&text, &mut inner, "");
        let text_has_link = self.state.link_emitted;
        self.state.link_emitted = outer_link_emitted;
        self.state.in_link = false;

        let slot = self.inline_slots.len();
        self.inline_slots.push(inner);

        if !is_image {
            if text_has_link {
                self.state.in_raw_block = outer_in_raw_block;
                return None;
            }
            self.state.link_emitted = true;
        }

        let title = if title.is_empty() { None } else { Some(title.to_string()) };
        if is_image {
            Some(Token::Image {
                raw: raw.to_string(),
                href: href.to_string(),
                title,
                text,
                tokens: slot,
            })
        } else {
            Some(Token::Link {
                raw: raw.to_string(),
                href: href.to_string(),
                title,
                text,
                tokens: slot,
                autolink: false,
            })
        }
    }

    fn tokenizer_reflink(&mut self, src: &str) -> Option<Token> {
        let (captures, from_nolink) = match R_REFLINK.exec(src) {
            Some(captures) => (captures, false),
            None => (R_NOLINK.exec(src)?, true),
        };
        let label_start = if captures.whole().starts_with('!') { 2 } else { 1 };
        if is_label_end_inside_token(src, captures.group_or(1, ""), label_start) {
            return None;
        }
        let link_string = if from_nolink {
            captures.group_or(1, "")
        } else {
            captures.group(2).unwrap_or_else(|| captures.group_or(1, ""))
        };
        let link_string = R_MULTIPLE_SPACE.replace_all(link_string, " ");
        let Some(link) = self.links.get(&normalize_label(&link_string)).cloned() else {
            let text = captures.whole().chars().next().map(String::from).unwrap_or_default();
            return Some(Token::Text {
                raw: text.clone(),
                text,
                tokens: None,
                escaped: false,
            });
        };
        let raw = captures.whole().to_string();
        let label = captures.group_or(1, "").to_string();
        self.output_link(
            &label,
            &link.href,
            link.title.as_deref().unwrap_or(""),
            &raw,
            captures.whole().starts_with('!'),
        )
    }

    fn tokenizer_em_strong(&mut self, src: &str, masked_src: &str, prev_char: &str) -> Option<Token> {
        let captures = R_EM_STRONG_LDELIM.exec(src)?;
        let g1 = captures.group(1).is_some();
        let g2 = captures.group(2).is_some();
        let g3 = captures.group(3).is_some();
        let g4 = captures.group(4).is_some();
        if !g1 && !g2 && !g3 && !g4 {
            return None;
        }
        if g4 && !prev_char.is_empty() && is_unicode_alphanumeric(prev_char) {
            return None;
        }
        let next_char = captures.group(1).or_else(|| captures.group(3)).unwrap_or("");
        if next_char.is_empty() || prev_char.is_empty() || R_PUNCTUATION.matches(prev_char) {
            let l_length = utf16_len(captures.whole()) - 1;
            let mut delim_total = l_length as i64;
            let mut mid_delim_total = 0i64;
            let delim_char = captures.whole().chars().next().unwrap_or('*');
            let mid_run = prev_char == delim_char.to_string();
            let end_rule = if delim_char == '*' {
                &R_EM_STRONG_RDELIM_AST
            } else {
                &R_EM_STRONG_RDELIM_UND
            };
            let masked = drop_utf16(masked_src, utf16_len(masked_src).saturating_sub(utf16_len(src)) + l_length);

            let mut cursor = 0usize;
            while let Some(match_captures) = end_rule.exec_from(masked, cursor) {
                let whole = match_captures.whole();
                let start = match_captures.start();
                let end = match_captures.end();
                let r_delim = (1..=6)
                    .find_map(|index| match_captures.group(index))
                    .unwrap_or("");
                if r_delim.is_empty() {
                    cursor = if end == start { end + 1 } else { end };
                    if cursor > masked.len() {
                        break;
                    }
                    continue;
                }
                let r_length = utf16_len(r_delim) as i64;
                let left_delim = match_captures.group(3).is_some() || match_captures.group(4).is_some();
                let either = match_captures.group(5).is_some() || match_captures.group(6).is_some();
                if left_delim {
                    delim_total += r_length;
                    cursor = if end == start { end + 1 } else { end };
                    if cursor > masked.len() {
                        break;
                    }
                    continue;
                }
                if either {
                    if l_length as i64 % 3 != 0 && (l_length as i64 + r_length) % 3 == 0 {
                        mid_delim_total += r_length;
                        cursor = if end == start { end + 1 } else { end };
                        if cursor > masked.len() {
                            break;
                        }
                        continue;
                    }
                    if mid_run {
                        break;
                    }
                }
                delim_total -= r_length;
                if delim_total > 0 {
                    cursor = if end == start { end + 1 } else { end };
                    if cursor > masked.len() {
                        break;
                    }
                    continue;
                }
                let r_length = r_length.min(r_length + delim_total + mid_delim_total);
                let last_char_length = utf16_len(
                    whole
                        .chars()
                        .next()
                        .map(String::from)
                        .unwrap_or_default()
                        .as_str(),
                );
                let raw_len = l_length + start + last_char_length + r_length as usize;
                let raw = slice_utf16(src, 0, raw_len).to_string();
                if (l_length.min(r_length as usize)) % 2 == 1 {
                    let text = slice_utf16(&raw, 1, utf16_len(&raw).saturating_sub(1)).to_string();
                    let mut inner = Vec::new();
                    self.inline_tokens(&text, &mut inner, "");
                    let slot = self.inline_slots.len();
                    self.inline_slots.push(inner);
                    return Some(Token::Em { raw, text, tokens: slot });
                }
                let text = slice_utf16(&raw, 2, utf16_len(&raw).saturating_sub(2)).to_string();
                let mut inner = Vec::new();
                self.inline_tokens(&text, &mut inner, "");
                let slot = self.inline_slots.len();
                self.inline_slots.push(inner);
                return Some(Token::Strong { raw, text, tokens: slot });
            }
        }
        None
    }

    fn tokenizer_codespan(&self, src: &str) -> Option<Token> {
        let captures = R_INLINE_CODE.exec(src)?;
        let mut text = R_NEWLINE_CHAR.replace_all(captures.group_or(2, ""), " ");
        let has_non_space = R_NON_SPACE.matches(&text);
        let both_ends = R_STARTING_SPACE.matches(&text) && R_ENDING_SPACE.matches(&text);
        if has_non_space && both_ends {
            text = slice_utf16(&text, 1, utf16_len(&text).saturating_sub(1)).to_string();
        }
        Some(Token::Codespan {
            raw: captures.whole().to_string(),
            text,
        })
    }

    fn tokenizer_br(&self, src: &str) -> Option<Token> {
        let captures = R_BR.exec(src)?;
        Some(Token::Br {
            raw: captures.whole().to_string(),
        })
    }

    fn tokenizer_del(&mut self, src: &str) -> Option<Token> {
        let captures = R_STRICT_STRIKETHROUGH.exec(src)?;
        let text = captures.group_or(2, "").to_string();
        let mut inner = Vec::new();
        self.inline_tokens(&text, &mut inner, "");
        let slot = self.inline_slots.len();
        self.inline_slots.push(inner);
        Some(Token::Del {
            raw: captures.whole().to_string(),
            text,
            tokens: slot,
        })
    }

    fn tokenizer_autolink(&mut self, src: &str) -> Option<Token> {
        let captures = R_AUTOLINK.exec(src)?;
        let text = captures.group_or(1, "").to_string();
        let href = if captures.group(2) == Some("@") {
            format!("mailto:{text}")
        } else {
            text.clone()
        };
        let slot = self.inline_slots.len();
        self.inline_slots.push(vec![Token::Text {
            raw: text.clone(),
            text: text.clone(),
            tokens: None,
            escaped: false,
        }]);
        Some(Token::Link {
            raw: captures.whole().to_string(),
            href,
            title: None,
            text,
            tokens: slot,
            autolink: true,
        })
    }

    fn tokenizer_url(&mut self, src: &str) -> Option<Token> {
        let captures = R_URL.exec(src)?;
        let mut whole = captures.whole().to_string();
        let (text, href) = if captures.group(2) == Some("@") {
            (whole.clone(), format!("mailto:{whole}"))
        } else {
            loop {
                let previous = whole.clone();
                let backpedal = R_BACKPEDAL
                    .exec(&whole)
                    .map(|captures| captures.whole().to_string())
                    .unwrap_or_default();
                whole = backpedal;
                if previous == whole {
                    break;
                }
            }
            let href = if captures.group(1) == Some("www.") {
                format!("http://{whole}")
            } else {
                whole.clone()
            };
            (whole.clone(), href)
        };
        let slot = self.inline_slots.len();
        self.inline_slots.push(vec![Token::Text {
            raw: text.clone(),
            text: text.clone(),
            tokens: None,
            escaped: false,
        }]);
        Some(Token::Link {
            raw: whole,
            href,
            title: None,
            text,
            tokens: slot,
            autolink: true,
        })
    }

    fn tokenizer_inline_text(&self, src: &str) -> Option<Token> {
        let captures = R_INLINE_TEXT.exec(src)?;
        let escaped = self.state.in_raw_block;
        Some(Token::Text {
            raw: captures.whole().to_string(),
            text: captures.whole().to_string(),
            tokens: None,
            escaped,
        })
    }
}

fn push_inline(tokens: &mut Vec<Token>, token: Token) {
    tokens.push(token);
}

fn replace_with_length<F>(value: &str, rule: &Rule, build: F) -> String
where
    F: Fn(&Captures<'_>) -> Option<String>,
{
    let mut result = String::new();
    let mut last = 0usize;
    let mut cursor = 0usize;
    loop {
        let Some(captures) = rule.exec_from(value, cursor) else {
            break;
        };
        let start = captures.start();
        let end = captures.end();
        result.push_str(&value[last..start]);
        match build(&captures) {
            Some(replacement) => result.push_str(&replacement),
            None => result.push_str(&"+".repeat(end - start)),
        }
        last = end;
        cursor = if end == start { end + 1 } else { end };
        if cursor > value.len() {
            break;
        }
    }
    result.push_str(&value[last.min(value.len())..]);
    result
}

fn is_unicode_alphanumeric(value: &str) -> bool {
    static RULE: LazyLock<Rule> = LazyLock::new(|| Rule::new(r"[\p{L}\p{N}]", "u"));
    RULE.matches(value)
}

fn is_label_end_inside_token(src: &str, label: &str, label_start: usize) -> bool {
    if !label.contains('<') {
        return false;
    }
    static R_INLINE_CODE_RULE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_CODE, ""));
    static R_TAG_RULE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_TAG, ""));
    static R_AUTOLINK_RULE: LazyLock<Rule> = LazyLock::new(|| Rule::new(rules::INLINE_AUTOLINK, ""));
    let chars: Vec<char> = label.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] == '\\' {
            index += 2;
            continue;
        }
        if chars[index] == '`' {
            let rest: String = chars[index..].iter().collect();
            if let Some(captures) = R_INLINE_CODE_RULE.exec(&rest) {
                index += utf16_len(captures.whole());
                continue;
            }
        }
        if chars[index] != '<' {
            index += 1;
            continue;
        }
        let token_src = drop_utf16(src, label_start + index);
        let token = R_TAG_RULE
            .exec(token_src)
            .or_else(|| R_AUTOLINK_RULE.exec(token_src));
        let Some(captures) = token else {
            index += 1;
            continue;
        };
        if utf16_len(captures.whole()) > chars.len() - index {
            return true;
        }
        index += utf16_len(captures.whole());
    }
    false
}

fn clip_to_inline_extension(src: &str) -> String {
    let Some(captures) = R_INLINE_LATEX_START.exec(src) else {
        return src.to_string();
    };
    let start = captures.start();
    if start == 0 {
        return src.to_string();
    }
    let temp_src = drop_utf16(src, 1);
    let Some(captures) = R_INLINE_LATEX_START.exec(temp_src) else {
        return src.to_string();
    };
    let start_index = captures.start();
    slice_utf16(src, 0, start_index + 1).to_string()
}
