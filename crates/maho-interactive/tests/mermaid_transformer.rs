//! Port of senpi `test/mermaid.test.ts`: the Mermaid markdown transformer over the `grok_mermaid`
//! layout engine (rendering, oversize and unsupported fallbacks, theming, streaming, warnings, modes).
use std::rc::Rc;

use maho_interactive::components::markdown_transform::{MarkdownTransformContext, MessageType};
use maho_interactive::components::mermaid::{
    MermaidRenderingMode, create_mermaid_markdown_transformer,
};
use maho_interactive::theme::{ColorMode, Theme, ThemeColor};

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
            message_type: options.message_type,
            is_streaming: options.is_streaming,
            available_width: options.max_width,
        },
    )
    .expect("transformer never fails")
    .expect("transformer always answers")
}

fn plain(markdown: &str) -> String {
    transform_mermaid(markdown, TransformOptions::default())
}

#[test]
fn replaces_mermaid_code_blocks_with_unicode_diagrams() {
    let markdown = "Before\n\n```mermaid\nflowchart LR\n  A[Start] --> B[Done]\n```\nAfter";
    let rendered = plain(markdown);
    assert!(rendered.contains("Before"), "{rendered}");
    assert!(rendered.contains("┌───────┐"), "{rendered}");
    assert!(rendered.contains("│ Start ├───▶│ Done │"), "{rendered}");
    assert!(
        rendered.contains("└───────┘    └──────┘`\nAfter"),
        "{rendered}"
    );
    assert!(!rendered.contains("```mermaid"), "{rendered}");
    assert!(rendered.contains("After"), "{rendered}");
}

#[test]
fn leaves_unsupported_and_oversized_diagrams_unchanged() {
    let unsupported = "```mermaid\npie\n  title Pets\n  \"Dogs\" : 4\n```";
    let oversized = "```mermaid\nflowchart LR\n  A[Start] --> B[Done]\n```";
    assert_eq!(plain(unsupported), unsupported);
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
fn maps_semantic_spans_through_the_theme() {
    let theme = Theme::builtin("dark", ColorMode::Color256).expect("builtin theme");
    let border = theme.fg(ThemeColor::BorderMuted, "┌");
    let rendered = transform_mermaid(
        "```mermaid\nflowchart LR\n  A --> B\n```",
        TransformOptions {
            theme: Some(theme.clone()),
            ..Default::default()
        },
    );
    let border_prefix = &border[..border.find('┌').expect("glyph")];
    let edge = theme.fg(ThemeColor::Accent, "─");
    let accent_prefix = &edge[..edge.find('─').expect("glyph")];
    assert!(rendered.contains(border_prefix), "{rendered:?}");
    assert!(rendered.contains(accent_prefix), "{rendered:?}");
}

#[test]
fn renders_incomplete_mermaid_blocks_during_streaming() {
    let partial = "```mermaid\nflowchart LR\n  A --> B";
    let rendered = transform_mermaid(
        partial,
        TransformOptions {
            is_streaming: true,
            ..Default::default()
        },
    );
    assert!(rendered.contains("───▶"), "{rendered}");
}

#[test]
fn renders_diagrams_with_class_assignments() {
    let rendered = plain("```mermaid\nflowchart LR\n  A[Foo]:::highlight --> B[Bar]\n```");
    assert!(!rendered.contains("```mermaid"), "{rendered}");
    assert!(
        !rendered.contains("Mermaid diagram not rendered"),
        "{rendered}"
    );
    assert!(rendered.contains("│ Foo ├───▶│ Bar │"), "{rendered}");
}

#[test]
fn falls_back_to_the_code_block_with_a_warning_after_streaming() {
    let markdown = "```mermaid\nflowchart LR\n  A[Foo] invalid\n```";
    let final_render = plain(markdown);
    let followed_by_text = plain(&format!("{markdown}\nFollowing text"));
    let streaming = transform_mermaid(
        markdown,
        TransformOptions {
            is_streaming: true,
            ..Default::default()
        },
    );
    assert!(final_render.contains(markdown), "{final_render}");
    assert!(
        final_render.contains("```\n`Mermaid diagram not rendered"),
        "{final_render}"
    );
    assert!(
        final_render.contains("dropped, expected a link: \"invalid\""),
        "{final_render}"
    );
    assert!(!final_render.contains("more)"), "{final_render}");
    assert!(
        followed_by_text.contains("  \nFollowing text"),
        "{followed_by_text}"
    );
    assert!(
        !streaming.contains("Mermaid diagram not rendered"),
        "{streaming}"
    );
    assert!(!streaming.contains("```mermaid"), "{streaming}");
    assert!(streaming.contains("│ Foo │"), "{streaming}");
}

#[test]
fn summarizes_additional_partial_render_warnings() {
    let markdown = "```mermaid\nflowchart LR\n  A[Foo] invalid\n  B[Bar] also-invalid\n```";
    let rendered = plain(markdown);
    assert!(rendered.contains(markdown), "{rendered}");
    assert!(
        rendered.contains("dropped, expected a link: \"invalid\""),
        "{rendered}"
    );
    assert!(rendered.contains("(+1 more)"), "{rendered}");
    assert!(
        !rendered.contains("dropped, expected a link: \"also-invalid\""),
        "{rendered}"
    );
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
