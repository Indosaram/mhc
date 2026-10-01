//! Port of components/markdown-transform.ts.
use std::rc::Rc;
use std::sync::Arc;
use crate::theme::{Theme, ThemeColor};
use maho_tui::components::markdown::{MarkdownTheme, ThemeFn};

pub type MarkdownRenderTransform = Rc<dyn Fn(&str, usize) -> String>;

pub struct MarkdownComponent(pub maho_tui::components::markdown::Markdown);
impl maho_tui::tui::Component for MarkdownComponent {
    fn render(&mut self, width: usize) -> Vec<String> { self.0.render(width) }
    fn invalidate(&mut self) { self.0.invalidate(); }
}

/// A `Markdown` whose source is rewritten by transformers at render time.
///
/// senpi passes the transform into the Markdown component so it sees the width the renderer
/// resolved; this port's `Markdown` has no transform hook, so the wrapper applies the transform
/// with the same content width and hands the result to an inner `Markdown`.
pub struct TransformedMarkdown {
    markdown: maho_tui::components::markdown::Markdown,
    raw: String,
    transform: MarkdownRenderTransform,
    padding_x: usize,
    applied_width: Option<usize>,
}
impl TransformedMarkdown {
    pub fn new(
        raw: &str,
        padding_x: usize,
        padding_y: usize,
        theme: MarkdownTheme,
        style: Option<maho_tui::components::markdown::DefaultTextStyle>,
        options: maho_tui::components::markdown::MarkdownOptions,
        transform: MarkdownRenderTransform,
    ) -> Self {
        Self {
            markdown: maho_tui::components::markdown::Markdown::new(raw, padding_x, padding_y, theme, style, options),
            raw: raw.to_owned(),
            transform,
            padding_x,
            applied_width: None,
        }
    }
    pub fn set_raw(&mut self, raw: &str) {
        if self.raw == raw {
            return;
        }
        self.raw = raw.to_owned();
        self.applied_width = None;
    }
    pub fn markdown(&self) -> &maho_tui::components::markdown::Markdown {
        &self.markdown
    }
}
impl maho_tui::tui::Component for TransformedMarkdown {
    fn render(&mut self, width: usize) -> Vec<String> {
        let content_width = 1.max(width.saturating_sub(self.padding_x * 2));
        if self.applied_width != Some(content_width) {
            let transformed = (self.transform)(&self.raw, content_width);
            self.markdown.set_text(&transformed);
            self.applied_width = Some(content_width);
        }
        self.markdown.render(width)
    }
    fn invalidate(&mut self) {
        self.markdown.invalidate();
        self.applied_width = None;
    }
    fn dispose(&mut self) {
        self.markdown.invalidate();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType { User, Assistant, AssistantThinking }
#[derive(Clone, Copy, Debug)]
pub struct MarkdownTransformContext {
    pub message_type: MessageType,
    pub is_streaming: bool,
    pub available_width: usize,
}
pub type MarkdownTransformer = Rc<dyn Fn(&str, MarkdownTransformContext) -> Result<Option<String>, String>>;

pub fn get_markdown_theme(theme: &Theme) -> MarkdownTheme {
    static NEXT_THEME_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(32000);
    let fg = |color: ThemeColor| -> ThemeFn {
        let theme = theme.clone();
        Arc::new(move |text| theme.fg(color, text))
    };
    let bold = theme.clone();
    let italic = theme.clone();
    let underline = theme.clone();
    let strike = theme.clone();
    let highlight = theme.clone();
    MarkdownTheme {
        id: NEXT_THEME_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        heading: fg(ThemeColor::MdHeading), link: fg(ThemeColor::MdLink),
        link_url: fg(ThemeColor::MdLinkUrl), code: fg(ThemeColor::MdCode),
        code_block: fg(ThemeColor::MdCodeBlock), code_block_border: fg(ThemeColor::MdCodeBlockBorder),
        quote: fg(ThemeColor::MdQuote), quote_border: fg(ThemeColor::MdQuoteBorder),
        hr: fg(ThemeColor::MdHr), list_bullet: fg(ThemeColor::MdListBullet),
        bold: Arc::new(move |text| bold.bold(text)), italic: Arc::new(move |text| italic.italic(text)),
        underline: Arc::new(move |text| underline.underline(text)),
        strikethrough: Arc::new(move |text| strike.strikethrough(text)),
        highlight_code: Some(Arc::new(move |code, language| crate::theme::highlight_code(&highlight, code, language))),
        code_block_indent: None,
    }
}

pub fn create_markdown_transform(
    message_type: MessageType,
    is_streaming: bool,
    transformers: Vec<MarkdownTransformer>,
) -> impl Fn(&str, usize) -> String {
    move |markdown, available_width| {
        let context = MarkdownTransformContext { message_type, is_streaming, available_width };
        let mut transformed = markdown.to_owned();
        for transformer in &transformers {
            if let Ok(Some(next)) = transformer(&transformed, context) {
                transformed = next;
            }
        }
        transformed
    }
}
