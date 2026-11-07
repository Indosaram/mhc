//! Port of `components/mermaid.ts`.
//!
//! The transformer replaces top-level `mermaid` code blocks with terminal diagrams drawn by the
//! `crate::grok_mermaid` layout engine (port of `grok-mermaid` 0.2.3). A block keeps its original
//! source whenever the engine has no art to show (unsupported type, blank or oversized diagram), and
//! a final render with parse warnings keeps the source plus a one-line warning.
use std::rc::Rc;

use maho_tui::components::markdown_lexer::Lexer;
use maho_tui::components::markdown_token::Token;
use serde_json::Value;

use super::markdown_transform::MarkdownTransformer;
use crate::grok_mermaid::Cls;
use crate::theme::{Theme, ThemeColor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MermaidRenderingMode {
    Off,
    Streaming,
    On,
}

impl MermaidRenderingMode {
    /// senpi `getMermaidRenderingMode`: `off` and `final` as written, anything else `streaming`.
    pub fn from_setting(setting: Option<&Value>) -> Self {
        match setting.and_then(Value::as_str) {
            Some("off") => Self::Off,
            Some("final") => Self::On,
            _ => Self::Streaming,
        }
    }
}

impl From<maho_core::settings_shapes::MermaidRenderingMode> for MermaidRenderingMode {
    fn from(mode: maho_core::settings_shapes::MermaidRenderingMode) -> Self {
        match mode {
            maho_core::settings_shapes::MermaidRenderingMode::Off => Self::Off,
            maho_core::settings_shapes::MermaidRenderingMode::Final => Self::On,
            maho_core::settings_shapes::MermaidRenderingMode::Streaming => Self::Streaming,
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
    let padding = if content.starts_with('`') || content.ends_with('`') {
        " "
    } else {
        ""
    };
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
        .map(|row| {
            row.iter()
                .map(|span| style_span(&span.text, span.cls, theme))
                .collect::<String>()
        })
        .collect()
}

fn is_mermaid(token: &Token) -> bool {
    match token {
        Token::Code {
            lang: Some(lang), ..
        } => lang
            .split_whitespace()
            .next()
            .is_some_and(|first| first.eq_ignore_ascii_case("mermaid")),
        _ => false,
    }
}

pub fn create_mermaid_markdown_transformer(
    get_mode: Rc<dyn Fn() -> MermaidRenderingMode>,
    theme: Option<Theme>,
) -> MarkdownTransformer {
    create_live_mermaid_markdown_transformer(get_mode, Rc::new(move || theme.clone()))
}

/// senpi styles diagrams through the module-level `theme` proxy, which always resolves to the active
/// theme; `get_theme` is read on every transform so a theme switch restyles the next render.
pub fn create_live_mermaid_markdown_transformer(
    get_mode: Rc<dyn Fn() -> MermaidRenderingMode>,
    get_theme: Rc<dyn Fn() -> Option<Theme>>,
) -> MarkdownTransformer {
    Rc::new(move |markdown, context| {
        let mode = get_mode();
        if mode == MermaidRenderingMode::Off
            || context.message_type == super::markdown_transform::MessageType::AssistantThinking
            || (context.is_streaming && mode != MermaidRenderingMode::Streaming)
        {
            return Ok(Some(markdown.to_owned()));
        }
        let theme = get_theme();

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
                out.push_str(token.raw());
                continue;
            };
            if art.width > context.available_width {
                out.push_str(token.raw());
                continue;
            }
            if !context.is_streaming && !art.warnings.is_empty() {
                let suffix = if art.warnings.len() > 1 {
                    format!(" (+{} more)", art.warnings.len() - 1)
                } else {
                    String::new()
                };
                let warning = format!("Mermaid diagram not rendered: {}{suffix}", art.warnings[0]);
                let styled_warning = theme.as_ref().map_or_else(
                    || warning.clone(),
                    |theme| theme.fg(ThemeColor::Warning, &warning),
                );
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
            out.push_str(
                &lines
                    .iter()
                    .map(|line| code_span(line))
                    .collect::<Vec<_>>()
                    .join("  \n"),
            );
            out.push('\n');
        }
        Ok(Some(out))
    })
}

#[cfg(test)]
mod tests {
    //! Port of senpi `test/mermaid.test.ts`.
    use super::*;
    use crate::components::markdown_transform::{MarkdownTransformContext, MessageType};
    use crate::theme::ColorMode;

    struct TransformOptions {
        max_width: usize,
        is_streaming: bool,
        message_type: MessageType,
        mode: MermaidRenderingMode,
        theme: Option<Theme>,
    }

    impl Default for TransformOptions {
        fn default() -> Self {
            Self {
                max_width: 100,
                is_streaming: false,
                message_type: MessageType::Assistant,
                mode: MermaidRenderingMode::Streaming,
                theme: None,
            }
        }
    }

    fn transform_mermaid(markdown: &str, options: TransformOptions) -> String {
        let mode = options.mode;
        let transformer = create_mermaid_markdown_transformer(Rc::new(move || mode), options.theme);
        transformer(
            markdown,
            MarkdownTransformContext {
                available_width: options.max_width,
                is_streaming: options.is_streaming,
                message_type: options.message_type,
            },
        )
        .expect("transformer never fails")
        .expect("transformer always answers")
    }

    #[test]
    fn replaces_mermaid_code_blocks_with_unicode_diagrams() {
        let markdown = "Before\n\n```mermaid\nflowchart LR\n  A[Start] --> B[Done]\n```\nAfter";
        let rendered = transform_mermaid(markdown, TransformOptions::default());

        assert!(rendered.contains("Before"));
        assert!(rendered.contains("┌───────┐"));
        assert!(rendered.contains("│ Start ├───▶│ Done │"));
        assert!(
            rendered.contains("└───────┘    └──────┘`\nAfter"),
            "{rendered:?}"
        );
        assert!(!rendered.contains("```mermaid"));
        assert!(rendered.contains("After"));
    }

    #[test]
    fn leaves_unsupported_and_oversized_diagrams_unchanged() {
        let unsupported = "```mermaid\npie\n  title Pets\n  \"Dogs\" : 4\n```";
        let oversized = "```mermaid\nflowchart LR\n  A[Start] --> B[Done]\n```";

        assert_eq!(
            transform_mermaid(unsupported, TransformOptions::default()),
            unsupported
        );
        assert_eq!(
            transform_mermaid(
                oversized,
                TransformOptions {
                    max_width: 10,
                    ..Default::default()
                }
            ),
            oversized
        );
    }

    #[test]
    fn maps_semantic_spans_through_the_pi_theme() {
        let theme = Theme::builtin("dark", ColorMode::Truecolor).expect("builtin dark theme");
        let border = theme.get_fg_ansi(ThemeColor::BorderMuted);
        let accent = theme.get_fg_ansi(ThemeColor::Accent);
        let rendered = transform_mermaid(
            "```mermaid\nflowchart LR\n  A --> B\n```",
            TransformOptions {
                theme: Some(theme),
                ..Default::default()
            },
        );

        assert!(rendered.contains(&border), "{rendered:?}");
        assert!(rendered.contains(&accent), "{rendered:?}");
    }

    #[test]
    fn renders_incomplete_mermaid_blocks_during_streaming() {
        let partial_markdown = "```mermaid\nflowchart LR\n  A --> B";

        assert!(
            transform_mermaid(
                partial_markdown,
                TransformOptions {
                    is_streaming: true,
                    ..Default::default()
                }
            )
            .contains("───▶")
        );
    }

    #[test]
    fn renders_diagrams_with_class_assignments() {
        let markdown = "```mermaid\nflowchart LR\n  A[Foo]:::highlight --> B[Bar]\n```";
        let rendered = transform_mermaid(markdown, TransformOptions::default());

        assert!(!rendered.contains("```mermaid"));
        assert!(!rendered.contains("Mermaid diagram not rendered"));
        assert!(rendered.contains("│ Foo ├───▶│ Bar │"), "{rendered:?}");
    }

    #[test]
    fn falls_back_to_the_code_block_with_a_warning_after_streaming() {
        let markdown = "```mermaid\nflowchart LR\n  A[Foo] invalid\n```";
        let final_render = transform_mermaid(markdown, TransformOptions::default());
        let followed_by_text = transform_mermaid(
            &format!("{markdown}\nFollowing text"),
            TransformOptions::default(),
        );
        let streaming = transform_mermaid(
            markdown,
            TransformOptions {
                is_streaming: true,
                ..Default::default()
            },
        );

        assert!(final_render.contains(markdown));
        assert!(
            final_render.contains("```\n`Mermaid diagram not rendered"),
            "{final_render:?}"
        );
        assert!(final_render.contains("dropped, expected a link: \"invalid\""));
        assert!(!final_render.contains("more)"));
        assert!(
            followed_by_text.contains("  \nFollowing text"),
            "{followed_by_text:?}"
        );
        assert!(!streaming.contains("Mermaid diagram not rendered"));
        assert!(!streaming.contains("```mermaid"));
        assert!(streaming.contains("│ Foo │"));
    }

    #[test]
    fn summarizes_additional_partial_render_warnings() {
        let markdown = "```mermaid\nflowchart LR\n  A[Foo] invalid\n  B[Bar] also-invalid\n```";
        let rendered = transform_mermaid(markdown, TransformOptions::default());

        assert!(rendered.contains(markdown));
        assert!(rendered.contains("dropped, expected a link: \"invalid\""));
        assert!(rendered.contains("(+1 more)"));
        assert!(!rendered.contains("dropped, expected a link: \"also-invalid\""));
    }

    #[test]
    fn respects_rendering_modes_and_skips_thinking_blocks() {
        let markdown = "```mermaid\nflowchart LR\n  A --> B\n```";

        assert_eq!(
            transform_mermaid(
                markdown,
                TransformOptions {
                    mode: MermaidRenderingMode::Off,
                    ..Default::default()
                }
            ),
            markdown
        );
        assert_eq!(
            transform_mermaid(
                markdown,
                TransformOptions {
                    mode: MermaidRenderingMode::On,
                    is_streaming: true,
                    ..Default::default()
                }
            ),
            markdown
        );
        assert!(
            !transform_mermaid(
                markdown,
                TransformOptions {
                    mode: MermaidRenderingMode::On,
                    ..Default::default()
                }
            )
            .contains("```mermaid")
        );
        assert_eq!(
            transform_mermaid(
                markdown,
                TransformOptions {
                    message_type: MessageType::AssistantThinking,
                    ..Default::default()
                }
            ),
            markdown
        );
    }
}
