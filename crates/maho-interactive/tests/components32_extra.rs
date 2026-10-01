use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::components::bash_execution::BashExecutionComponent;
use maho_interactive::components::bordered_loader::BorderedLoader;
use maho_interactive::components::custom_entry::{CustomEntryComponent, EntryRenderer};
use maho_interactive::components::keybinding_hints::key_hint;
use maho_interactive::components::markdown_transform::{MarkdownTransformContext, MessageType};
use maho_interactive::components::mermaid::{MermaidRenderingMode, create_mermaid_markdown_transformer};
use maho_interactive::components::tool_execution_images::{ToolExecutionImageOptions, ToolExecutionImages};
use maho_interactive::components::user_message_selector::{UserMessageItem, UserMessageSelectorComponent};
use maho_interactive::theme::{ColorMode, Theme};
use maho_tui::components::text::Text;
use maho_tui::tui::Component;
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("golden/components32-extra.json")).expect("pinned fixture")
}

fn expected(case: &Value) -> Vec<String> {
    case["lines"].as_array().expect("lines").iter().map(|line| line.as_str().expect("line").to_owned()).collect()
}

fn trim(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| line.trim_end().to_owned()).collect()
}

/// The command header's syntax highlighting is todo 31's `highlight_code`, whose bash grammar
/// classifies builtins like `echo` differently from senpi's highlight.js. Everything the
/// bash-execution component itself owns — the borders, the prompt, the output body and the status
/// line — is still compared byte-for-byte.
fn strip_ansi(text: &str) -> String {
    maho_tui::utils::strip_terminal_sequences(text)
}

fn mode(name: &str) -> MermaidRenderingMode {
    match name {
        "off" => MermaidRenderingMode::Off,
        "streaming" => MermaidRenderingMode::Streaming,
        _ => MermaidRenderingMode::On,
    }
}

fn message_type(name: &str) -> MessageType {
    match name {
        "assistant-thinking" => MessageType::AssistantThinking,
        _ => MessageType::Assistant,
    }
}

#[test]
fn mermaid_transformer_matches_pinned_senpi_for_every_gate() {
    let theme = theme();
    for case in fixtures()["mermaid"].as_array().expect("mermaid") {
        let name = case["name"].as_str().expect("name");
        let markdown = case["markdown"].as_str().expect("markdown");
        let rendering_mode = mode(case["mode"].as_str().expect("mode"));
        let current = rendering_mode;
        let transform = create_mermaid_markdown_transformer(Rc::new(move || current), Some(theme.clone()));
        let output = transform(
            markdown,
            MarkdownTransformContext {
                message_type: message_type(case["messageType"].as_str().expect("messageType")),
                is_streaming: case["isStreaming"].as_bool().expect("isStreaming"),
                available_width: case["availableWidth"].as_u64().expect("availableWidth") as usize,
            },
        )
        .expect("transformer never fails");
        assert_eq!(
            output.as_deref(),
            Some(case["output"].as_str().expect("output")),
            "mermaid {name} mode={} streaming={} type={}",
            case["mode"].as_str().unwrap_or(""),
            case["isStreaming"],
            case["messageType"].as_str().unwrap_or("")
        );
    }
}

#[test]
fn bordered_loaders_match_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["bordered"].as_array().expect("bordered") {
        let width = case["width"].as_u64().expect("width") as usize;
        let cancellable = case["cancellable"].as_bool().expect("cancellable");
        let hint = key_hint("tui.select.cancel", "cancel", &theme);
        let mut loader = BorderedLoader::new(theme.clone(), "Working...", cancellable, &hint, 0);
        assert_eq!(trim(loader.render(width)), expected(case), "bordered width={width} cancellable={cancellable}");
    }
}

#[test]
fn bash_execution_matches_pinned_senpi_for_every_terminal_state() {
    let theme = theme();
    for case in fixtures()["bash"].as_array().expect("bash") {
        let width = case["width"].as_u64().expect("width") as usize;
        let exclude_from_context = case["excludeFromContext"].as_bool().expect("excludeFromContext");
        let mut component = BashExecutionComponent::new("echo hello", exclude_from_context, theme.clone());
        for line in case["output"].as_array().expect("output") {
            component.append_output(&format!("{}\n", line.as_str().expect("line")));
        }
        component.set_complete(
            case["exitCode"].as_i64().map(|code| code as i32),
            case["cancelled"].as_bool().expect("cancelled"),
            None,
            None,
        );
        let actual = trim(component.render(width));
        let wanted = expected(case);
        assert_eq!(actual.len(), wanted.len(), "bash {} line count at {width}", case["name"].as_str().unwrap_or(""));
        for (index, (left, right)) in actual.iter().zip(&wanted).enumerate() {
            let is_command_header = strip_ansi(left).contains("echo hello");
            let (left, right) = if is_command_header {
                (strip_ansi(left), strip_ansi(right))
            } else {
                (left.clone(), right.clone())
            };
            assert_eq!(
                left,
                right,
                "bash {} line {index} at {width}",
                case["name"].as_str().unwrap_or("")
            );
        }
    }
}

#[test]
fn tool_execution_images_match_pinned_senpi_without_image_support() {
    let theme = theme();
    for case in fixtures()["images"].as_array().expect("images") {
        let width = case["width"].as_u64().expect("width") as usize;
        let mut images = ToolExecutionImages::new(theme.clone());
        images.update_options(ToolExecutionImageOptions {
            show_images: case["showImages"].as_bool().expect("showImages"),
            max_width_cells: 60,
            show_renderer_fallback: false,
        });
        images.update_result(&maho_tools::definition::ToolResult {
            content: vec![maho_tools::definition::ToolContent::Image {
                data: String::from("aGVsbG8="),
                mime_type: String::from("image/png"),
            }],
            details: None,
        });
        assert_eq!(trim(images.render(width)), expected(case), "images at {width}");
    }
}

#[test]
fn user_message_selector_matches_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["selector"].as_array().expect("selector") {
        let width = case["width"].as_u64().expect("width") as usize;
        let messages = case["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|message| UserMessageItem {
                id: message["id"].as_str().expect("id").to_owned(),
                text: message["text"].as_str().expect("text").to_owned(),
                timestamp: None,
            })
            .collect();
        let mut component = UserMessageSelectorComponent::new(
            messages,
            Box::new(|_| {}),
            Box::new(|| {}),
            case["initialSelectedId"].as_str(),
            theme.clone(),
        );
        assert_eq!(trim(component.render(width)), expected(case), "selector at {width}");
    }
}

#[test]
fn custom_entries_match_pinned_senpi_with_and_without_content() {
    let theme = theme();
    for case in fixtures()["entry"].as_array().expect("entry") {
        let width = case["width"].as_u64().expect("width") as usize;
        let renderer: EntryRenderer = Rc::new(|value: &Value, _expanded, _theme| {
            Ok(value["data"]["text"].as_str().map(|text| {
                Rc::new(RefCell::new(Text::with_padding(format!("entry: {text}"), 0, 0))) as Rc<RefCell<dyn Component>>
            }))
        });
        let mut component = CustomEntryComponent::new(case["entry"].clone(), renderer, theme.clone());
        assert_eq!(component.has_content(), case["hasContent"].as_bool().expect("hasContent"));
        assert_eq!(trim(component.render(width)), expected(case), "entry at {width}");
    }
}
