//! Port of senpi `packages/tui/src/components/markdown.ts`.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use crate::components::latex::latex_to_unicode;
use crate::components::markdown_lexer::Lexer;
use crate::components::markdown_token::Token;
use crate::terminal_image::{get_capabilities, hyperlink, is_image_line};
use crate::utils::{apply_background_to_line, visible_width, wrap_text_with_ansi};

pub type ThemeFn = Arc<dyn Fn(&str) -> String + Send + Sync>;
pub type HighlightFn = Arc<dyn Fn(&str, Option<&str>) -> Vec<String> + Send + Sync>;

#[derive(Clone, Default)]
pub struct DefaultTextStyle {
    pub color: Option<ThemeFn>,
    pub bg_color: Option<ThemeFn>,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub underline: bool,
}

impl DefaultTextStyle {
    fn is_empty(&self) -> bool {
        self.color.is_none()
            && self.bg_color.is_none()
            && !self.bold
            && !self.italic
            && !self.strikethrough
            && !self.underline
    }
}

#[derive(Clone)]
pub struct MarkdownTheme {
    pub id: u64,
    pub heading: ThemeFn,
    pub link: ThemeFn,
    pub link_url: ThemeFn,
    pub code: ThemeFn,
    pub code_block: ThemeFn,
    pub code_block_border: ThemeFn,
    pub quote: ThemeFn,
    pub quote_border: ThemeFn,
    pub hr: ThemeFn,
    pub list_bullet: ThemeFn,
    pub bold: ThemeFn,
    pub italic: ThemeFn,
    pub strikethrough: ThemeFn,
    pub underline: ThemeFn,
    pub highlight_code: Option<HighlightFn>,
    pub code_block_indent: Option<String>,
}

#[derive(Clone, Default)]
pub struct MarkdownOptions {
    pub preserve_ordered_list_markers: bool,
    pub preserve_backslash_escapes: bool,
    pub render_latex: Option<bool>,
}

const RENDER_CACHE_MAX: usize = 256;
const PARSE_CACHE_MAX: usize = 128;
const CONTENT_KEY_CACHE_MAX: usize = 128;
const HIGHLIGHT_CACHE_MAX: usize = 512;
const MAX_HIGHLIGHT_BYTES: usize = 200_000;
const MAX_HIGHLIGHT_LINES: usize = 2000;

static OBJECT_IDS: LazyLock<Mutex<ObjectIds>> = LazyLock::new(|| Mutex::new(ObjectIds::default()));
static RENDER_CACHE: LazyLock<Mutex<HashMap<String, CacheEntry<Vec<String>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PARSE_CACHE: LazyLock<Mutex<HashMap<String, CacheEntry<Vec<Token>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static CONTENT_KEY_CACHE: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static HIGHLIGHT_CACHE: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static HIGHLIGHT_CALL_COUNT: LazyLock<Mutex<usize>> = LazyLock::new(|| Mutex::new(0));

#[derive(Default)]
struct ObjectIds {
    next: u64,
    ids: HashMap<u64, u64>,
}

#[derive(Clone)]
struct CacheEntry<T> {
    source: String,
    value: T,
}

fn object_id<T: ?Sized>(pointer: *const T) -> u64 {
    let key = pointer as *const () as u64;
    let mut state = OBJECT_IDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(id) = state.ids.get(&key) {
        return *id;
    }
    let id = state.next;
    state.next += 1;
    state.ids.insert(key, id);
    id
}

fn cache_set<T>(cache: &mut HashMap<String, T>, key: String, value: T, max_size: usize) {
    cache.remove(&key);
    cache.insert(key.clone(), value);
    if cache.len() > max_size
        && let Some(oldest) = cache.keys().next().cloned() {
            cache.remove(&oldest);
        }
}

fn content_key(text: &str) -> String {
    {
        let mut cache = CONTENT_KEY_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = cache.get(text) {
            let cached = cached.clone();
            cache_set(&mut cache, text.to_string(), cached.clone(), CONTENT_KEY_CACHE_MAX);
            return cached;
        }
    }

    let mut hash: u32 = 2_166_136_261;
    for unit in text.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(16_777_619);
    }
    let key = format!("{}:{}", text.encode_utf16().count(), radix36(hash));
    let mut cache = CONTENT_KEY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache_set(&mut cache, text.to_string(), key.clone(), CONTENT_KEY_CACHE_MAX);
    key
}

fn radix36(mut value: u32) -> String {
    if value == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

pub fn clear_render_cache() {
    RENDER_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    PARSE_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    CONTENT_KEY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    HIGHLIGHT_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

pub fn get_markdown_highlight_call_count() -> usize {
    *HIGHLIGHT_CALL_COUNT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn reset_markdown_highlight_call_count() {
    *HIGHLIGHT_CALL_COUNT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = 0;
}

#[derive(Clone)]
struct InlineStyleContext {
    apply_text: Arc<dyn Fn(&str) -> String>,
    style_prefix: String,
}

pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
    default_text_style: Option<DefaultTextStyle>,
    theme: MarkdownTheme,
    options: MarkdownOptions,
    default_style_prefix: Option<String>,
    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl Markdown {
    pub fn new(
        text: &str,
        padding_x: usize,
        padding_y: usize,
        theme: MarkdownTheme,
        default_text_style: Option<DefaultTextStyle>,
        options: MarkdownOptions,
    ) -> Self {
        Self {
            text: text.to_string(),
            padding_x,
            padding_y,
            default_text_style: default_text_style.filter(|style| !style.is_empty()),
            theme,
            options,
            default_style_prefix: None,
            cached_text: None,
            cached_width: None,
            cached_lines: None,
        }
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.invalidate();
    }

    pub fn invalidate(&mut self) {
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }

    fn theme_id(&self) -> u64 {
        self.theme.id
    }

    fn style_id(&self) -> u64 {
        match &self.default_text_style {
            Some(style) => object_id(style as *const DefaultTextStyle),
            None => u64::MAX,
        }
    }

    pub fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(lines), Some(text), Some(cached_width)) =
            (&self.cached_lines, &self.cached_text, self.cached_width)
            && *text == self.text && cached_width == width {
                return lines.clone();
            }

        let content_width = 1.max(width.saturating_sub(self.padding_x * 2));
        let text = self.text.clone();

        if text.trim().is_empty() {
            let result: Vec<String> = Vec::new();
            self.cached_text = Some(self.text.clone());
            self.cached_width = Some(width);
            self.cached_lines = Some(result.clone());
            return result;
        }

        let normalized_text = text.replace('\t', "   ");
        let normalized_content_key = content_key(&normalized_text);
        let capabilities = get_capabilities();
        let images_key = match capabilities.images {
            Some(crate::terminal_capabilities::ImageProtocol::Kitty) => "kitty",
            Some(crate::terminal_capabilities::ImageProtocol::Iterm2) => "iterm2",
            None => "",
        };
        let flags = u8::from(self.options.preserve_ordered_list_markers)
            | (u8::from(self.options.preserve_backslash_escapes) << 1)
            | (u8::from(self.options.render_latex == Some(false)) << 2);
        let render_key = [
            normalized_content_key.clone(),
            width.to_string(),
            content_width.to_string(),
            self.padding_x.to_string(),
            self.padding_y.to_string(),
            self.theme
                .code_block_indent
                .clone()
                .unwrap_or_else(|| "  ".to_string()),
            flags.to_string(),
            self.theme_id().to_string(),
            self.style_id().to_string(),
            images_key.to_string(),
            u8::from(capabilities.hyperlinks).to_string(),
        ]
        .join("\u{0}");

        {
            let cache = RENDER_CACHE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = cache.get(&render_key)
                && entry.source == normalized_text {
                    let lines = entry.value.clone();
                    drop(cache);
                    self.cached_text = Some(self.text.clone());
                    self.cached_width = Some(width);
                    self.cached_lines = Some(lines.clone());
                    return lines;
                }
        }

        let tokens = {
            let cached = {
                let cache = PARSE_CACHE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                cache
                    .get(&normalized_content_key)
                    .filter(|entry| entry.source == normalized_text)
                    .map(|entry| entry.value.clone())
            };
            match cached {
                Some(tokens) => tokens,
                None => {
                    let mut lexer = Lexer::new();
                    let mut tokens = lexer.lex(&normalized_text);
                    trim_partial_closing_fences(&mut tokens);
                    let mut cache = PARSE_CACHE
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    cache_set(
                        &mut cache,
                        normalized_content_key.clone(),
                        CacheEntry {
                            source: normalized_text.clone(),
                            value: tokens.clone(),
                        },
                        PARSE_CACHE_MAX,
                    );
                    tokens
                }
            }
        };

        let mut rendered_lines: Vec<String> = Vec::new();
        for index in 0..tokens.len() {
            let next_type = tokens.get(index + 1).map(Token::type_name);
            let token_lines = self.render_token(&tokens[index], content_width, next_type, None);
            rendered_lines.extend(token_lines);
        }

        let mut wrapped_lines: Vec<String> = Vec::new();
        for line in &rendered_lines {
            if is_image_line(line) {
                wrapped_lines.push(line.clone());
            } else {
                wrapped_lines.extend(wrap_text_with_ansi(line, content_width));
            }
        }

        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        let bg_fn = self
            .default_text_style
            .as_ref()
            .and_then(|style| style.bg_color.clone());
        let mut content_lines: Vec<String> = Vec::new();

        for line in &wrapped_lines {
            if is_image_line(line) {
                content_lines.push(line.clone());
                continue;
            }
            let line_with_margins = format!("{left_margin}{line}{right_margin}");
            match &bg_fn {
                Some(bg_fn) => content_lines.push(apply_background_to_line(
                    &line_with_margins,
                    width,
                    &|text: &str| bg_fn(text),
                )),
                None => {
                    let visible_len = visible_width(&line_with_margins);
                    let padding_needed = width.saturating_sub(visible_len);
                    content_lines.push(format!(
                        "{line_with_margins}{}",
                        " ".repeat(padding_needed)
                    ));
                }
            }
        }

        let empty_line = " ".repeat(width);
        let mut empty_lines: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            let line = match &bg_fn {
                Some(bg_fn) => {
                    apply_background_to_line(&empty_line, width, &|text: &str| bg_fn(text))
                }
                None => empty_line.clone(),
            };
            empty_lines.push(line);
        }

        let mut result = empty_lines.clone();
        result.extend(content_lines);
        result.extend(empty_lines);

        self.cached_text = Some(self.text.clone());
        self.cached_width = Some(width);
        self.cached_lines = Some(result.clone());
        {
            let mut cache = RENDER_CACHE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cache_set(
                &mut cache,
                render_key,
                CacheEntry {
                    source: normalized_text,
                    value: result.clone(),
                },
                RENDER_CACHE_MAX,
            );
        }

        if result.is_empty() {
            vec![String::new()]
        } else {
            result
        }
    }

    fn apply_default_style(&self, text: &str) -> String {
        let Some(style) = &self.default_text_style else {
            return text.to_string();
        };
        let mut styled = text.to_string();
        if let Some(color) = &style.color {
            styled = color(&styled);
        }
        if style.bold {
            styled = (self.theme.bold)(&styled);
        }
        if style.italic {
            styled = (self.theme.italic)(&styled);
        }
        if style.strikethrough {
            styled = (self.theme.strikethrough)(&styled);
        }
        if style.underline {
            styled = (self.theme.underline)(&styled);
        }
        styled
    }

    fn get_default_style_prefix(&mut self) -> String {
        if self.default_text_style.is_none() {
            return String::new();
        }
        if let Some(prefix) = &self.default_style_prefix {
            return prefix.clone();
        }
        let sentinel = "\u{0}";
        let mut styled = sentinel.to_string();
        if let Some(style) = self.default_text_style.clone() {
            if let Some(color) = &style.color {
                styled = color(&styled);
            }
            if style.bold {
                styled = (self.theme.bold)(&styled);
            }
            if style.italic {
                styled = (self.theme.italic)(&styled);
            }
            if style.strikethrough {
                styled = (self.theme.strikethrough)(&styled);
            }
            if style.underline {
                styled = (self.theme.underline)(&styled);
            }
        }
        let prefix = styled
            .find(sentinel)
            .map(|index| styled[..index].to_string())
            .unwrap_or_default();
        self.default_style_prefix = Some(prefix.clone());
        prefix
    }

    fn get_style_prefix(style_fn: &ThemeFn) -> String {
        let sentinel = "\u{0}";
        let styled = style_fn(sentinel);
        styled
            .find(sentinel)
            .map(|index| styled[..index].to_string())
            .unwrap_or_default()
    }

    fn default_inline_style_context(&mut self) -> InlineStyleContext {
        let prefix = self.get_default_style_prefix();
        let style = self.default_text_style.clone();
        let theme = self.theme.clone();
        InlineStyleContext {
            apply_text: Arc::new(move |text: &str| {
                let Some(style) = &style else {
                    return text.to_string();
                };
                let mut styled = text.to_string();
                if let Some(color) = &style.color {
                    styled = color(&styled);
                }
                if style.bold {
                    styled = (theme.bold)(&styled);
                }
                if style.italic {
                    styled = (theme.italic)(&styled);
                }
                if style.strikethrough {
                    styled = (theme.strikethrough)(&styled);
                }
                if style.underline {
                    styled = (theme.underline)(&styled);
                }
                styled
            }),
            style_prefix: prefix,
        }
    }

    fn exceeds_highlight_cap(&self, code: &str) -> bool {
        let mut newline_count = 0usize;
        for character in code.chars() {
            if character == '\n' {
                newline_count += 1;
            }
            if newline_count + 1 > MAX_HIGHLIGHT_LINES {
                return true;
            }
        }
        code.len() > MAX_HIGHLIGHT_BYTES
    }

    fn highlight_code_block(&self, code: &str, lang: Option<&str>) -> Option<Vec<String>> {
        let highlight = self.theme.highlight_code.clone()?;
        if self.exceeds_highlight_cap(code) {
            return None;
        }
        let key = format!("{}\u{0}{}\u{0}{code}", self.theme_id(), lang.unwrap_or(""));
        {
            let mut cache = HIGHLIGHT_CACHE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cached) = cache.get(&key) {
                let cached = cached.clone();
                cache_set(&mut cache, key, cached.clone(), HIGHLIGHT_CACHE_MAX);
                return Some(cached);
            }
        }
        *HIGHLIGHT_CALL_COUNT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        let highlighted = highlight(code, lang);
        let mut cache = HIGHLIGHT_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache_set(&mut cache, key, highlighted.clone(), HIGHLIGHT_CACHE_MAX);
        Some(highlighted)
    }

    fn render_code_block(
        &self,
        lines: &mut Vec<String>,
        code: &str,
        lang: Option<&str>,
        indent: &str,
    ) {
        lines.push((self.theme.code_block_border)(&format!("```{}", lang.unwrap_or(""))));
        match self.highlight_code_block(code, lang) {
            Some(highlighted) => {
                for line in highlighted {
                    lines.push(format!("{indent}{line}"));
                }
            }
            None => {
                if self.theme.highlight_code.is_some() && self.exceeds_highlight_cap(code) {
                    lines.push(format!(
                        "{indent}{}",
                        (self.theme.code_block)("[syntax highlighting skipped: code block too large]")
                    ));
                }
                for code_line in code.split('\n') {
                    lines.push(format!("{indent}{}", (self.theme.code_block)(code_line)));
                }
            }
        }
        lines.push((self.theme.code_block_border)("```"));
    }

    fn render_token(
        &mut self,
        token: &Token,
        width: usize,
        next_token_type: Option<&str>,
        style_context: Option<InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        match token {
            Token::Heading { depth, tokens, .. } => {
                let heading_prefix = format!("{} ", "#".repeat(*depth as usize));
                let theme = self.theme.clone();
                let heading_style_fn: ThemeFn = if *depth == 1 {
                    Arc::new(move |text: &str| {
                        (theme.heading)(&(theme.bold)(&(theme.underline)(text)))
                    })
                } else {
                    Arc::new(move |text: &str| (theme.heading)(&(theme.bold)(text)))
                };
                let heading_style_context = InlineStyleContext {
                    apply_text: {
                        let style_fn = heading_style_fn.clone();
                        Arc::new(move |text: &str| style_fn(text))
                    },
                    style_prefix: Self::get_style_prefix(&heading_style_fn),
                };
                let heading_text = self.render_inline_tokens(tokens.tokens(), Some(&heading_style_context));
                let styled_heading = if *depth >= 3 {
                    format!("{}{}", heading_style_fn(&heading_prefix), heading_text)
                } else {
                    heading_text
                };
                lines.push(styled_heading);
                if next_token_type.is_some_and(|value| value != "space") {
                    lines.push(String::new());
                }
            }

            Token::Paragraph { tokens, .. } => {
                let paragraph_text = self.render_inline_tokens(tokens.tokens(), style_context.as_ref());
                lines.push(paragraph_text);
                if next_token_type.is_some_and(|value| value != "list" && value != "space") {
                    lines.push(String::new());
                }
            }

            Token::LatexBlock { raw, text } => {
                let formula = if self.options.render_latex == Some(false) {
                    raw.trim().to_string()
                } else {
                    latex_to_unicode(text)
                };
                lines.push(match &style_context {
                    Some(context) => (context.apply_text)(&formula),
                    None => self.apply_default_style(&formula),
                });
                if next_token_type.is_some_and(|value| value != "space") {
                    lines.push(String::new());
                }
            }

            Token::Text { tokens, .. } => {
                let resolved = style_context
                    .clone()
                    .unwrap_or_else(|| self.default_inline_style_context());
                let inner: Vec<Token> = tokens
                    .as_ref()
                    .map(|inline| inline.tokens().to_vec())
                    .unwrap_or_else(|| vec![token.clone()]);
                lines.push(self.render_inline_tokens(&inner, Some(&resolved)));
            }

            Token::Code { text, lang, .. } => {
                let indent = self
                    .theme
                    .code_block_indent
                    .clone()
                    .unwrap_or_else(|| "  ".to_string());
                self.render_code_block(&mut lines, text, lang.as_deref(), &indent);
                if next_token_type.is_some_and(|value| value != "space") {
                    lines.push(String::new());
                }
            }

            Token::List { .. } => {
                let list_lines = self.render_list(token, 0, width, style_context.as_ref());
                lines.extend(list_lines);
            }

            Token::Table { .. } => {
                let table_lines = self.render_table(token, width, next_token_type, style_context.as_ref());
                lines.extend(table_lines);
            }

            Token::Blockquote { tokens, .. } => {
                let theme = self.theme.clone();
                let quote_style: ThemeFn =
                    Arc::new(move |text: &str| (theme.quote)(&(theme.italic)(text)));
                let quote_style_prefix = Self::get_style_prefix(&quote_style);
                let apply_quote_style = {
                    let prefix = quote_style_prefix.clone();
                    let style = quote_style.clone();
                    move |line: &str| {
                        if prefix.is_empty() {
                            return style(line);
                        }
                        let with_style = line.replace("\u{1b}[0m", &format!("\u{1b}[0m{prefix}"));
                        style(&with_style)
                    }
                };

                let quote_content_width = 1.max(width.saturating_sub(2));
                let quote_inline_style_context = InlineStyleContext {
                    apply_text: Arc::new(|text: &str| text.to_string()),
                    style_prefix: quote_style_prefix.clone(),
                };
                let mut rendered_quote_lines: Vec<String> = Vec::new();
                for index in 0..tokens.len() {
                    let next = tokens.get(index + 1).map(Token::type_name);
                    rendered_quote_lines.extend(self.render_token(
                        &tokens[index],
                        quote_content_width,
                        next,
                        Some(InlineStyleContext {
                            apply_text: quote_inline_style_context.apply_text.clone(),
                            style_prefix: quote_inline_style_context.style_prefix.clone(),
                        }),
                    ));
                }

                while rendered_quote_lines.last().is_some_and(|line| line.is_empty()) {
                    rendered_quote_lines.pop();
                }

                for quote_line in &rendered_quote_lines {
                    let styled_line = apply_quote_style(quote_line);
                    for wrapped_line in wrap_text_with_ansi(&styled_line, quote_content_width) {
                        lines.push(format!("{}{wrapped_line}", (self.theme.quote_border)("\u{2502} ")));
                    }
                }
                if next_token_type.is_some_and(|value| value != "space") {
                    lines.push(String::new());
                }
            }

            Token::Hr { .. } => {
                lines.push((self.theme.hr)(&"\u{2500}".repeat(width.min(80))));
                if next_token_type.is_some_and(|value| value != "space") {
                    lines.push(String::new());
                }
            }

            Token::Html { raw, .. } => {
                lines.push(self.apply_default_style(raw.trim()));
            }

            Token::Space { .. } => {
                lines.push(String::new());
            }

            _ => {
                lines.push(token.raw().to_string());
            }
        }

        lines
    }

    fn render_inline_tokens(
        &mut self,
        tokens: &[Token],
        style_context: Option<&InlineStyleContext>,
    ) -> String {
        let resolved = match style_context {
            Some(context) => context.clone(),
            None => self.default_inline_style_context(),
        };
        let apply_text = resolved.apply_text.clone();
        let style_prefix = resolved.style_prefix.clone();
        let apply_text_with_newlines = |text: &str| {
            text.split('\n')
                .map(|segment| apply_text(segment))
                .collect::<Vec<String>>()
                .join("\n")
        };

        let mut result = String::new();
        for token in tokens {
            match token {
                Token::Escape { raw, text } => {
                    let value = if self.options.preserve_backslash_escapes { raw } else { text };
                    result.push_str(&apply_text_with_newlines(value));
                }
                Token::Text { text, tokens, .. } => {
                    match tokens {
                        Some(inline) if !inline.tokens().is_empty() => {
                            let inner = inline.tokens().to_vec();
                            result.push_str(&self.render_inline_tokens(&inner, Some(&resolved)));
                        }
                        _ => result.push_str(&apply_text_with_newlines(text)),
                    }
                }
                Token::Paragraph { tokens, .. } => {
                    let inner = tokens.tokens().to_vec();
                    result.push_str(&self.render_inline_tokens(&inner, Some(&resolved)));
                }
                Token::Strong { tokens, .. } => {
                    let inner = tokens.tokens().to_vec();
                    let content = self.render_inline_tokens(&inner, Some(&resolved));
                    result.push_str(&(self.theme.bold)(&content));
                    result.push_str(&style_prefix);
                }
                Token::Em { tokens, .. } => {
                    let inner = tokens.tokens().to_vec();
                    let content = self.render_inline_tokens(&inner, Some(&resolved));
                    result.push_str(&(self.theme.italic)(&content));
                    result.push_str(&style_prefix);
                }
                Token::Codespan { text, .. } => {
                    result.push_str(&(self.theme.code)(text));
                    result.push_str(&style_prefix);
                }
                Token::LatexInline { raw, text } => {
                    let value = if self.options.render_latex == Some(false) {
                        raw.clone()
                    } else {
                        latex_to_unicode(text)
                    };
                    result.push_str(&apply_text_with_newlines(&value));
                }
                Token::LatexLiteral { text, .. } => {
                    result.push_str(&apply_text_with_newlines(text));
                }
                Token::Link { tokens, href, text, .. } => {
                    {
                        let inner = tokens.tokens().to_vec();
                        let link_text = self.render_inline_tokens(&inner, Some(&resolved));
                        let styled_link = (self.theme.link)(&(self.theme.underline)(&link_text));
                        if get_capabilities().hyperlinks {
                            result.push_str(&hyperlink(&styled_link, href));
                            result.push_str(&style_prefix);
                        } else {
                            let href_for_comparison = href.strip_prefix("mailto:").unwrap_or(href);
                            if text == href || text == href_for_comparison {
                                result.push_str(&styled_link);
                                result.push_str(&style_prefix);
                            } else {
                                result.push_str(&styled_link);
                                result.push_str(&(self.theme.link_url)(&format!(" ({href})")));
                                result.push_str(&style_prefix);
                            }
                        }
                    }
                }
                Token::Br { .. } => result.push('\n'),
                Token::Del { tokens, .. } => {
                    let inner = tokens.tokens().to_vec();
                    let content = self.render_inline_tokens(&inner, Some(&resolved));
                    result.push_str(&(self.theme.strikethrough)(&content));
                    result.push_str(&style_prefix);
                }
                Token::Html { raw, .. } => result.push_str(&apply_text_with_newlines(raw)),
                other => {
                    let raw = other.raw().to_string();
                    result.push_str(&apply_text_with_newlines(&raw));
                }
            }
        }

        while !style_prefix.is_empty() && result.ends_with(&style_prefix) {
            result.truncate(result.len() - style_prefix.len());
        }
        result
    }

    fn ordered_list_marker(&self, item: &Token) -> Option<String> {
        let raw = item.raw();
        ORDERED_LIST_MARKER
            .exec(raw)
            .map(|captures| format!("{} ", captures.group_or(1, "")))
    }

    fn unordered_list_marker(&self, item: &Token) -> Option<String> {
        let raw = item.raw();
        UNORDERED_LIST_MARKER
            .exec(raw)
            .map(|captures| format!("{} ", captures.group_or(1, "")))
    }

    fn render_list(
        &mut self,
        token: &Token,
        depth: usize,
        width: usize,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let Token::List { items, ordered, start, loose, .. } = token else {
            return Vec::new();
        };
        let mut lines: Vec<String> = Vec::new();
        let indent = "    ".repeat(depth);
        let start_number = *start;

        for (index, item) in items.iter().enumerate() {
            let is_last_item = index == items.len() - 1;
            let bullet = if *ordered {
                if self.options.preserve_ordered_list_markers {
                    self.ordered_list_marker(item)
                        .unwrap_or_else(|| format!("{}. ", start_number + index as u32))
                } else {
                    format!("{}. ", start_number + index as u32)
                }
            } else if self.options.preserve_ordered_list_markers {
                self.unordered_list_marker(item)
                    .unwrap_or_else(|| "- ".to_string())
            } else {
                "- ".to_string()
            };
            let (task, checked, item_tokens) = match item {
                Token::ListItem { task, checked, tokens, .. } => (*task, *checked, tokens.clone()),
                _ => (false, false, Vec::new()),
            };
            let task_marker = if task {
                format!("[{}] ", if checked { "x" } else { " " })
            } else {
                String::new()
            };
            let marker = format!("{bullet}{task_marker}");
            let first_prefix = format!("{indent}{}", (self.theme.list_bullet)(&marker));
            let continuation_prefix = format!("{indent}{}", " ".repeat(visible_width(&marker)));
            let item_width = 1.max(width.saturating_sub(visible_width(&first_prefix)));
            let mut rendered_any_line = false;

            for item_token in &item_tokens {
                if let Token::List { .. } = item_token {
                    lines.extend(self.render_list(item_token, depth + 1, width, style_context));
                    rendered_any_line = true;
                    continue;
                }
                let item_lines = self.render_token(item_token, item_width, None, style_context.cloned());
                for line in item_lines {
                    for wrapped_line in wrap_text_with_ansi(&line, item_width) {
                        let line_prefix = if rendered_any_line {
                            continuation_prefix.clone()
                        } else {
                            first_prefix.clone()
                        };
                        lines.push(format!("{line_prefix}{wrapped_line}"));
                        rendered_any_line = true;
                    }
                }
            }

            if !rendered_any_line {
                lines.push(first_prefix);
            }

            if *loose && !is_last_item {
                lines.push(String::new());
            }
        }

        lines
    }

    fn get_longest_word_width(&self, text: &str, max_width: Option<usize>) -> usize {
        let mut longest = 0usize;
        for word in text.split_whitespace() {
            if !word.is_empty() {
                longest = longest.max(visible_width(word));
            }
        }
        match max_width {
            Some(max_width) => longest.min(max_width),
            None => longest,
        }
    }

    fn wrap_cell_text(&self, text: &str, max_width: usize, style_prefix: &str) -> Vec<String> {
        let lines = wrap_text_with_ansi(text, 1.max(max_width));
        let count = lines.len();
        lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let style_reset = if index < count - 1 {
                    "\u{1b}[22;23;24;25;27;28;29;39m"
                } else {
                    ""
                };
                format!("{line}{style_reset}{style_prefix}")
            })
            .collect()
    }

    fn render_table(
        &mut self,
        token: &Token,
        available_width: usize,
        next_token_type: Option<&str>,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let Token::Table { header, rows, raw, .. } = token else {
            return Vec::new();
        };
        let mut lines: Vec<String> = Vec::new();
        let num_cols = header.len();
        if num_cols == 0 {
            return lines;
        }

        let border_overhead = 3 * num_cols + 1;
        let available_for_cells = available_width as i64 - border_overhead as i64;
        if available_for_cells < num_cols as i64 {
            let mut fallback_lines = if raw.is_empty() {
                Vec::new()
            } else {
                wrap_text_with_ansi(raw, available_width)
            };
            if next_token_type.is_some_and(|value| value != "space") {
                fallback_lines.push(String::new());
            }
            return fallback_lines;
        }
        let available_for_cells = available_for_cells as usize;

        let max_unbroken_word_width = 30usize;
        let mut natural_widths: Vec<usize> = vec![0; num_cols];
        let mut min_word_widths: Vec<usize> = vec![0; num_cols];
        for (index, cell) in header.iter().enumerate() {
            let header_text = self.render_cell_tokens(&cell.tokens, style_context);
            natural_widths[index] = visible_width(&header_text);
            min_word_widths[index] =
                1.max(self.get_longest_word_width(&header_text, Some(max_unbroken_word_width)));
        }
        for row in rows {
            for (index, cell) in row.iter().enumerate() {
                let cell_text = self.render_cell_tokens(&cell.tokens, style_context);
                natural_widths[index] = natural_widths[index].max(visible_width(&cell_text));
                min_word_widths[index] = min_word_widths[index]
                    .max(self.get_longest_word_width(&cell_text, Some(max_unbroken_word_width)));
            }
        }

        let mut min_column_widths = min_word_widths.clone();
        let mut min_cells_width: usize = min_column_widths.iter().sum();

        if min_cells_width > available_for_cells {
            min_column_widths = vec![1; num_cols];
            let remaining = available_for_cells.saturating_sub(num_cols);
            if remaining > 0 {
                let total_weight: usize = min_word_widths
                    .iter()
                    .map(|width| width.saturating_sub(1))
                    .sum();
                let growth: Vec<usize> = min_word_widths
                    .iter()
                    .map(|width| {
                        let weight = width.saturating_sub(1);
                        (weight * remaining).checked_div(total_weight).unwrap_or(0)
                    })
                    .collect();
                for index in 0..num_cols {
                    min_column_widths[index] += growth[index];
                }
                let allocated: usize = growth.iter().sum();
                let mut leftover = remaining - allocated;
                let mut index = 0;
                while leftover > 0 && index < num_cols {
                    min_column_widths[index] += 1;
                    leftover -= 1;
                    index += 1;
                }
            }
            min_cells_width = min_column_widths.iter().sum();
        }

        let total_natural_width: usize = natural_widths.iter().sum::<usize>() + border_overhead;
        let column_widths: Vec<usize>;
        if total_natural_width <= available_width {
            column_widths = natural_widths
                .iter()
                .enumerate()
                .map(|(index, width)| (*width).max(min_column_widths[index]))
                .collect();
        } else {
            let total_grow_potential: usize = natural_widths
                .iter()
                .enumerate()
                .map(|(index, width)| width.saturating_sub(min_column_widths[index]))
                .sum();
            let extra_width = available_for_cells.saturating_sub(min_cells_width);
            let mut widths: Vec<usize> = min_column_widths
                .iter()
                .enumerate()
                .map(|(index, min_width)| {
                    let natural_width = natural_widths[index];
                    let min_width_delta = natural_width.saturating_sub(*min_width);
                    let grow = (min_width_delta * extra_width)
                        .checked_div(total_grow_potential)
                        .unwrap_or(0);
                    min_width + grow
                })
                .collect();
            let allocated: usize = widths.iter().sum();
            let mut remaining = available_for_cells.saturating_sub(allocated);
            while remaining > 0 {
                let mut grew = false;
                for index in 0..num_cols {
                    if remaining == 0 {
                        break;
                    }
                    if widths[index] < natural_widths[index] {
                        widths[index] += 1;
                        remaining -= 1;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            column_widths = widths;
        }

        let top_border_cells: Vec<String> = column_widths
            .iter()
            .map(|width| "\u{2500}".repeat(*width))
            .collect();
        lines.push(format!("\u{250c}\u{2500}{}\u{2500}\u{2510}", top_border_cells.join("\u{2500}\u{252c}\u{2500}")));

        let style_prefix = style_context.map(|context| context.style_prefix.clone()).unwrap_or_default();
        let header_cell_lines: Vec<Vec<String>> = header
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let text = self.render_cell_tokens(&cell.tokens, style_context);
                self.wrap_cell_text(&text, column_widths[index], &style_prefix)
            })
            .collect();
        let header_line_count = header_cell_lines.iter().map(Vec::len).max().unwrap_or(0);

        for line_index in 0..header_line_count {
            let row_parts: Vec<String> = header_cell_lines
                .iter()
                .enumerate()
                .map(|(column_index, cell_lines)| {
                    let text = cell_lines.get(line_index).cloned().unwrap_or_default();
                    let padded = format!(
                        "{text}{}",
                        " ".repeat(column_widths[column_index].saturating_sub(visible_width(&text)))
                    );
                    (self.theme.bold)(&padded)
                })
                .collect();
            lines.push(format!("\u{2502} {} \u{2502}", row_parts.join(" \u{2502} ")));
        }

        let separator_cells: Vec<String> = column_widths
            .iter()
            .map(|width| "\u{2500}".repeat(*width))
            .collect();
        let separator_line = format!(
            "\u{251c}\u{2500}{}\u{2500}\u{2524}",
            separator_cells.join("\u{2500}\u{253c}\u{2500}")
        );
        lines.push(separator_line.clone());

        for (row_index, row) in rows.iter().enumerate() {
            let row_cell_lines: Vec<Vec<String>> = row
                .iter()
                .enumerate()
                .map(|(index, cell)| {
                    let text = self.render_cell_tokens(&cell.tokens, style_context);
                    self.wrap_cell_text(&text, column_widths[index], &style_prefix)
                })
                .collect();
            let row_line_count = row_cell_lines.iter().map(Vec::len).max().unwrap_or(0);
            for line_index in 0..row_line_count {
                let row_parts: Vec<String> = row_cell_lines
                    .iter()
                    .enumerate()
                    .map(|(column_index, cell_lines)| {
                        let text = cell_lines.get(line_index).cloned().unwrap_or_default();
                        format!(
                            "{text}{}",
                            " ".repeat(
                                column_widths[column_index].saturating_sub(visible_width(&text))
                            )
                        )
                    })
                    .collect();
                lines.push(format!("\u{2502} {} \u{2502}", row_parts.join(" \u{2502} ")));
            }
            if row_index < rows.len() - 1 {
                lines.push(separator_line.clone());
            }
        }

        let bottom_border_cells: Vec<String> = column_widths
            .iter()
            .map(|width| "\u{2500}".repeat(*width))
            .collect();
        lines.push(format!(
            "\u{2514}\u{2500}{}\u{2500}\u{2518}",
            bottom_border_cells.join("\u{2500}\u{2534}\u{2500}")
        ));

        if next_token_type.is_some_and(|value| value != "space") {
            lines.push(String::new());
        }
        lines
    }

    fn render_cell_tokens(
        &mut self,
        inline: &crate::components::markdown_token::Inline,
        style_context: Option<&InlineStyleContext>,
    ) -> String {
        let tokens = inline.tokens().to_vec();
        self.render_inline_tokens(&tokens, style_context)
    }
}

static ORDERED_LIST_MARKER: LazyLock<crate::components::markdown_helpers::Rule> =
    LazyLock::new(|| crate::components::markdown_helpers::Rule::new(r"^(?: {0,3})(\d{1,9}[.)])[ \t]+", ""));
static UNORDERED_LIST_MARKER: LazyLock<crate::components::markdown_helpers::Rule> =
    LazyLock::new(|| {
        crate::components::markdown_helpers::Rule::new(r"^(?: {0,3})([-+*])(?:[ \t]+|(?=\r?\n|$))", "")
    });

fn trim_partial_closing_fences(tokens: &mut [Token]) {
    let Some(last) = tokens.last_mut() else {
        return;
    };
    match last {
        Token::List { items, .. } => {
            if let Some(Token::ListItem { tokens, .. }) = items.last_mut() {
                trim_partial_closing_fences(tokens);
            }
            return;
        }
        Token::Blockquote { tokens, .. } => {
            trim_partial_closing_fences(tokens);
            return;
        }
        Token::Code { .. } => {}
        _ => return,
    }
    let raw = last.raw().to_string();
    let marker = FENCE_MARKER
        .exec(&raw)
        .map(|captures| captures.group_or(1, "").to_string());
    let Some(marker) = marker else {
        return;
    };
    let last_line = raw.split('\n').next_back().unwrap_or("").to_string();
    if last_line.is_empty() || last_line.len() >= marker.len() {
        return;
    }
    let first = marker.chars().next().unwrap_or('`');
    if last_line != first.to_string().repeat(last_line.chars().count()) {
        return;
    }
    if let Token::Code { text, .. } = last {
        let trimmed = text
            .get(..text.len().saturating_sub(last_line.len()))
            .unwrap_or("")
            .to_string();
        let trimmed = trimmed.strip_suffix('\n').unwrap_or(&trimmed).to_string();
        *text = trimmed;
    }
}

static FENCE_MARKER: LazyLock<crate::components::markdown_helpers::Rule> =
    LazyLock::new(|| crate::components::markdown_helpers::Rule::new(r"^(`{3,}|~{3,})", ""));

