//! Port of `components/mermaid.ts`.
//!
//! The transformer replaces top-level `mermaid` code blocks with terminal diagrams. The layout
//! engine that draws them is not ported (see `crate::grok_mermaid`), so every block falls through
//! to the source-box rendering path, which is the documented answer whenever there is no art to
//! show.
use std::rc::Rc;

use maho_tui::components::markdown_lexer::Lexer;
use maho_tui::components::markdown_token::Token;
use serde_json::Value;

use super::markdown_transform::MarkdownTransformer;
use crate::grok_mermaid::{Cls, source_box};
use crate::theme::{Theme, ThemeColor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MermaidRenderingMode {
    Off,
    Streaming,
    On,
}

impl MermaidRenderingMode {
    pub fn from_setting(setting: Option<&Value>) -> Self {
        match setting.and_then(Value::as_str) {
            Some("off") => Self::Off,
            Some("streaming") => Self::Streaming,
            _ => Self::On,
        }
    }
}

fn code_span(line: &str) -> String {
    let content = if line.is_empty() { "\u{a0}" } else { line };
    let longest_backtick_run = content
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest_backtick_run + 1);
    let padding = if content.starts_with('`') || content.ends_with('`') { " " } else { "" };
    format!("{fence}{padding}{content}{padding}{fence}")
}

fn style_span(text: &str, cls: Cls, theme: &Theme) -> String {
    match cls {
        Cls::Border => theme.fg(ThemeColor::BorderMuted, text),
        Cls::Text => theme.fg(ThemeColor::Text, text),
        Cls::Edge => theme.fg(ThemeColor::Accent, text),
        Cls::EdgeLabel => theme.fg(ThemeColor::Muted, text),
        Cls::Title => theme.fg(ThemeColor::Accent, &theme.bold(text)),
        Cls::None => text.to_owned(),
    }
}

fn themed_lines(art: &crate::grok_mermaid::MermaidArt, theme: &Theme) -> Vec<String> {
    art.styled
        .iter()
        .map(|row| row.iter().map(|span| style_span(&span.text, span.cls, theme)).collect::<String>())
        .collect()
}

fn is_mermaid(token: &Token) -> bool {
    match token {
        Token::Code { lang: Some(lang), .. } => {
            lang.split_whitespace().next().is_some_and(|first| first.eq_ignore_ascii_case("mermaid"))
        }
        _ => false,
    }
}

pub fn create_mermaid_markdown_transformer(
    get_mode: Rc<dyn Fn() -> MermaidRenderingMode>,
    theme: Option<Theme>,
) -> MarkdownTransformer {
    Rc::new(move |markdown, context| {
        let mode = get_mode();
        if mode == MermaidRenderingMode::Off
            || context.message_type == super::markdown_transform::MessageType::AssistantThinking
            || (context.is_streaming && mode != MermaidRenderingMode::Streaming)
        {
            return Ok(Some(markdown.to_owned()));
        }

        let tokens = Lexer::new().lex(markdown);
        let mut out = String::new();
        for token in tokens {
            if !is_mermaid(&token) {
                out.push_str(token.raw());
                continue;
            }
            let Token::Code { text, .. } = &token else {
                out.push_str(token.raw());
                continue;
            };
            let Some(art) = crate::grok_mermaid::render(text) else {
                out.push_str(&source_box_markdown(text, theme.as_ref()));
                continue;
            };
            if art.width > context.available_width {
                out.push_str(&source_box_markdown(text, theme.as_ref()));
                continue;
            }
            if !context.is_streaming && !art.warnings.is_empty() {
                let suffix = if art.warnings.len() > 1 { format!(" (+{} more)", art.warnings.len() - 1) } else { String::new() };
                let warning = format!("Mermaid diagram not rendered: {}{suffix}", art.warnings[0]);
                let styled_warning = theme.as_ref().map_or_else(|| warning.clone(), |theme| theme.fg(ThemeColor::Warning, &warning));
                out.push_str(token.raw());
                out.push('\n');
                out.push_str(&code_span(&styled_warning));
                out.push_str("  \n");
                continue;
            }
            let lines = match &theme {
                Some(theme) => themed_lines(&art, theme),
                None => art.plain.clone(),
            };
            out.push_str(&lines.iter().map(|line| code_span(line)).collect::<Vec<_>>().join("  \n"));
            out.push('\n');
        }
        Ok(Some(out))
    })
}

fn source_box_markdown(text: &str, theme: Option<&Theme>) -> String {
    let art = source_box(text, None);
    let lines = match theme {
        Some(theme) => themed_lines(&art, theme),
        None => art.plain.clone(),
    };
    let mut out = lines.iter().map(|line| code_span(line)).collect::<Vec<_>>().join("  \n");
    out.push('\n');
    out
}
