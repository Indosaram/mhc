//! Port of `core/tools/renderers/read.ts`.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::LazyLock;

use maho_tools::definition::ToolResult;
use maho_tools::path_utils::resolve_to_cwd;
use maho_tools::read_classifiers::{CompactReadKind, classify_read};
use maho_tui::components::text::Text;
use serde_json::Value;

use super::{RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use crate::components::keybinding_hints::{key_hint, key_text};
use crate::theme::{Theme, ThemeColor};
use crate::tools::render_utils::{get_text_output, render_tool_path, replace_tabs, str_value};

static COMPACT_RESOURCE_FILE_NAMES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"]));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactKind {
    Docs,
    Resource,
    Skill,
    Memory,
}

#[derive(Clone, Debug)]
pub struct CompactReadClassification {
    pub kind: CompactKind,
    pub label: String,
    pub headline: Option<String>,
}

fn format_read_line_range(args: Option<&Value>, theme: &Theme) -> String {
    let offset = args.and_then(|args| args.get("offset")).and_then(Value::as_u64);
    let limit = args.and_then(|args| args.get("limit")).and_then(Value::as_u64);
    if offset.is_none() && limit.is_none() {
        return String::new();
    }
    let start_line = offset.unwrap_or(1);
    match limit {
        Some(limit) => theme.fg(ThemeColor::Warning, &format!(":{start_line}-{}", start_line + limit - 1)),
        None => theme.fg(ThemeColor::Warning, &format!(":{start_line}")),
    }
}

fn read_path(args: Option<&Value>) -> Option<String> {
    let args = args?;
    str_value(args.get("file_path").or_else(|| args.get("path"))).flatten()
}

fn format_read_call(args: Option<&Value>, theme: &Theme, cwd: &str) -> String {
    format!(
        "{} {}{}",
        theme.fg(ThemeColor::ToolTitle, &theme.bold("read")),
        render_tool_path(read_path(args), theme, cwd, None),
        format_read_line_range(args, theme)
    )
}

fn trim_trailing_empty_lines(lines: &mut Vec<String>) {
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
}

fn get_pi_docs_classification(absolute_path: &str) -> Option<CompactReadClassification> {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let relative_path = Path::new(absolute_path).strip_prefix(&package_root).ok()?;
    let label = relative_path.to_string_lossy().replace('\\', "/");
    if label.is_empty() || label == ".." || label.starts_with("../") || Path::new(&label).is_absolute() {
        return None;
    }
    if label == "README.md" || label.starts_with("docs/") || label.starts_with("examples/") {
        return Some(CompactReadClassification { kind: CompactKind::Docs, label, headline: None });
    }
    None
}

pub fn get_compact_read_classification(args: Option<&Value>, cwd: &str) -> Option<CompactReadClassification> {
    let raw_path = read_path(args)?;
    if raw_path.is_empty() {
        return None;
    }
    let absolute_path = resolve_to_cwd(&raw_path, Path::new(cwd));
    let file_name = absolute_path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    if file_name == "SKILL.md" {
        let label = absolute_path
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or(file_name);
        return Some(CompactReadClassification { kind: CompactKind::Skill, label, headline: None });
    }

    if let Some(classification) = classify_read(&absolute_path, Path::new(cwd)) {
        return Some(CompactReadClassification {
            kind: match classification.kind {
                CompactReadKind::Docs => CompactKind::Docs,
                CompactReadKind::Resource => CompactKind::Resource,
                CompactReadKind::Skill => CompactKind::Skill,
                CompactReadKind::Memory => CompactKind::Memory,
            },
            label: classification.label,
            headline: classification.headline,
        });
    }

    if let Some(docs) = get_pi_docs_classification(&absolute_path.to_string_lossy()) {
        return Some(docs);
    }

    if COMPACT_RESOURCE_FILE_NAMES.contains(file_name.as_str()) {
        return Some(CompactReadClassification {
            kind: CompactKind::Resource,
            label: maho_core::paths::format_path_relative_to_cwd_or_absolute(&absolute_path.to_string_lossy(), cwd),
            headline: None,
        });
    }

    None
}

fn format_compact_read_call(classification: &CompactReadClassification, args: Option<&Value>, theme: &Theme) -> String {
    let expand_hint = theme.fg(ThemeColor::Dim, &format!(" ({} to expand)", key_text("app.tools.expand")));
    if classification.kind == CompactKind::Skill {
        return theme.fg(ThemeColor::CustomMessageLabel, "\x1b[1m[skill]\x1b[22m ")
            + &theme.fg(ThemeColor::CustomMessageText, &classification.label)
            + &format_read_line_range(args, theme)
            + &expand_hint;
    }
    if classification.kind == CompactKind::Memory {
        let headline = classification.headline.clone().unwrap_or_else(|| String::from("Recalled"));
        return theme.fg(ThemeColor::Accent, &format!("\x1b[1m✦ {headline}\x1b[22m"))
            + " "
            + &theme.fg(ThemeColor::CustomMessageText, &classification.label)
            + &format_read_line_range(args, theme)
            + &expand_hint;
    }
    let kind = match classification.kind {
        CompactKind::Docs => "docs",
        CompactKind::Resource => "resource",
        CompactKind::Skill => "skill",
        CompactKind::Memory => "memory",
    };
    theme.fg(ThemeColor::ToolTitle, &theme.bold(&format!("read {kind}")))
        + " "
        + &theme.fg(ThemeColor::Accent, &classification.label)
        + &format_read_line_range(args, theme)
        + &expand_hint
}

fn format_read_result(
    args: Option<&Value>,
    result: &ToolResult,
    options: ToolRenderResultOptions,
    theme: &Theme,
    show_images: bool,
    is_error: bool,
) -> String {
    if !options.expanded && !is_error {
        return String::new();
    }
    let raw_path = read_path(args);
    let output = get_text_output(Some(result), show_images);
    let language = if !is_error { raw_path.as_deref().and_then(crate::theme::get_language_from_path) } else { None };
    let mut lines = match language {
        Some(language) => crate::theme::highlight_code(theme, &replace_tabs(&output), Some(language)),
        None => output.split('\n').map(str::to_owned).collect(),
    };
    trim_trailing_empty_lines(&mut lines);
    let max_lines = if options.expanded { lines.len() } else { 10 };
    let display_lines: Vec<String> = lines.iter().take(max_lines).cloned().collect();
    let remaining = lines.len().saturating_sub(max_lines);
    let mut text = format!(
        "\n{}",
        display_lines
            .iter()
            .map(|line| if language.is_some() {
                replace_tabs(line)
            } else {
                theme.fg(ThemeColor::ToolOutput, &replace_tabs(line))
            })
            .collect::<Vec<_>>()
            .join("\n")
    );
    if remaining > 0 {
        text += &format!(
            "{}{}{}",
            theme.fg(ThemeColor::Muted, &format!("\n... ({remaining} more lines,")),
            key_hint("app.tools.expand", "to expand", theme),
            theme.fg(ThemeColor::Muted, ")")
        );
    }
    text
}

#[derive(Default)]
pub struct ReadRenderers {
    classifications: HashMap<Option<String>, Option<CompactReadClassification>>,
}

impl ToolRenderers for ReadRenderers {
    fn render_call(&mut self, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        let args = Some(context.args);
        let classification = if context.expanded {
            None
        } else {
            let raw_path = read_path(args);
            match self.classifications.get(&raw_path) {
                Some(cached) => cached.clone(),
                None => {
                    let value = get_compact_read_classification(args, context.cwd);
                    self.classifications.insert(raw_path, value.clone());
                    value
                }
            }
        };
        let text_value = match classification {
            Some(classification) => format_compact_read_call(&classification, args, theme),
            None => format_read_call(args, theme, context.cwd),
        };
        Some(Rc::new(RefCell::new(Text::with_padding(text_value, 0, 0))))
    }

    fn render_result(
        &mut self,
        result: &ToolResult,
        options: ToolRenderResultOptions,
        theme: &Theme,
        context: &ToolRenderContext<'_>,
    ) -> Option<RenderedComponent> {
        let text = format_read_result(Some(context.args), result, options, theme, context.show_images, context.is_error);
        Some(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))))
    }
}
