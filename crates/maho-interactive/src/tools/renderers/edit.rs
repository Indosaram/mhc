//! Port of `core/tools/renderers/edit.ts`.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tools::definition::ToolResult;
use maho_tools::edit_diff::{
    Edit, apply_edits_to_normalized_content, generate_diff_string, normalize_to_lf,
};
use maho_tools::path_utils::resolve_to_cwd;
use maho_tui::components::box_::Box as TuiBox;
use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::tui::{Component, Container};
use serde_json::Value;

use super::{RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use crate::theme::{Theme, ThemeBg, ThemeColor};
use crate::tools::diff_render::render_tool_diff;
use crate::tools::render_utils::{render_tool_path, str_value};

#[derive(Clone, Debug)]
pub enum EditPreview {
    Diff { diff: String, first_changed_line: Option<usize> },
    Error { error: String },
}

struct RenderablePreviewInput {
    path: String,
    edits: Vec<Edit>,
}

fn get_renderable_preview_input(args: Option<&Value>) -> Option<RenderablePreviewInput> {
    let args = args?;
    let path = match args.get("path") {
        Some(Value::String(path)) => path.clone(),
        _ => match args.get("file_path") {
            Some(Value::String(path)) => path.clone(),
            _ => return None,
        },
    };
    if path.is_empty() {
        return None;
    }
    if let Some(edits) = args.get("edits").and_then(Value::as_array)
        && !edits.is_empty()
        && edits.iter().all(|edit| edit.get("oldText").is_some_and(Value::is_string) && edit.get("newText").is_some_and(Value::is_string))
    {
        return Some(RenderablePreviewInput {
            path,
            edits: edits
                .iter()
                .map(|edit| Edit {
                    old_text: edit.get("oldText").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    new_text: edit.get("newText").and_then(Value::as_str).unwrap_or_default().to_owned(),
                })
                .collect(),
        });
    }
    if args.get("oldText").is_some_and(Value::is_string) && args.get("newText").is_some_and(Value::is_string) {
        return Some(RenderablePreviewInput {
            path,
            edits: vec![Edit {
                old_text: args.get("oldText").and_then(Value::as_str).unwrap_or_default().to_owned(),
                new_text: args.get("newText").and_then(Value::as_str).unwrap_or_default().to_owned(),
            }],
        });
    }
    None
}

fn compute_preview(input: &RenderablePreviewInput, cwd: &str) -> EditPreview {
    let absolute_path = resolve_to_cwd(&input.path, std::path::Path::new(cwd));
    let Ok(raw) = std::fs::read_to_string(&absolute_path) else {
        return EditPreview::Error { error: format!("Could not edit file: {}.", input.path) };
    };
    let content = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let normalized = normalize_to_lf(content);
    match apply_edits_to_normalized_content(&normalized, &input.edits, &input.path) {
        Ok(applied) => {
            let diff = generate_diff_string(&applied.base_content, &applied.new_content, 4);
            EditPreview::Diff { diff: diff.diff, first_changed_line: diff.first_changed_line }
        }
        Err(error) => EditPreview::Error { error: error.to_string() },
    }
}

fn preview_args_key(input: Option<&RenderablePreviewInput>) -> Option<String> {
    let input = input?;
    let edits: Vec<Value> = input
        .edits
        .iter()
        .map(|edit| serde_json::json!({ "oldText": edit.old_text, "newText": edit.new_text }))
        .collect();
    Some(serde_json::json!({ "path": input.path, "edits": edits }).to_string())
}

fn format_edit_call(args: Option<&Value>, theme: &Theme, cwd: &str) -> String {
    let raw_path = args.and_then(|args| str_value(args.get("file_path").or_else(|| args.get("path")))).flatten();
    format!("{} {}", theme.fg(ThemeColor::ToolTitle, &theme.bold("edit")), render_tool_path(raw_path, theme, cwd, None))
}

fn get_edit_header_bg(preview: Option<&EditPreview>, settled_error: bool, theme: &Theme) -> Rc<dyn Fn(&str) -> String> {
    match preview {
        Some(EditPreview::Error { .. }) => {
            let theme = theme.clone();
            Rc::new(move |text| theme.bg(ThemeBg::ToolErrorBg, text))
        }
        Some(EditPreview::Diff { .. }) => {
            let theme = theme.clone();
            Rc::new(move |text| theme.bg(ThemeBg::ToolSuccessBg, text))
        }
        None if settled_error => {
            let theme = theme.clone();
            Rc::new(move |text| theme.bg(ThemeBg::ToolErrorBg, text))
        }
        None => {
            let theme = theme.clone();
            Rc::new(move |text| theme.bg(ThemeBg::ToolPendingBg, text))
        }
    }
}

fn build_edit_call_component(
    args: Option<&Value>,
    preview: Option<&EditPreview>,
    settled_error: bool,
    theme: &Theme,
    cwd: &str,
) -> Rc<RefCell<dyn Component>> {
    let mut component = TuiBox::with_padding(1, 1);
    component.set_bg_fn(Some(get_edit_header_bg(preview, settled_error, theme)));
    component.add_child(Rc::new(RefCell::new(Text::with_padding(format_edit_call(args, theme, cwd), 0, 0))));
    if let Some(preview) = preview {
        let body = match preview {
            EditPreview::Error { error } => theme.fg(ThemeColor::Error, error),
            EditPreview::Diff { diff, .. } => render_tool_diff(
                diff,
                args.and_then(|args| str_value(args.get("file_path").or_else(|| args.get("path")))).flatten().as_deref(),
                theme,
            ),
        };
        component.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        component.add_child(Rc::new(RefCell::new(Text::with_padding(body, 0, 0))));
    }
    Rc::new(RefCell::new(component))
}

fn format_edit_result(
    args: Option<&Value>,
    preview: Option<&EditPreview>,
    result: &ToolResult,
    theme: &Theme,
    is_error: bool,
) -> Option<String> {
    let raw_path = args.and_then(|args| str_value(args.get("file_path").or_else(|| args.get("path")))).flatten();
    let preview_diff = match preview {
        Some(EditPreview::Diff { diff, .. }) => Some(diff.clone()),
        _ => None,
    };
    let preview_error = match preview {
        Some(EditPreview::Error { error }) => Some(error.clone()),
        _ => None,
    };
    if is_error {
        let error_text = result
            .content
            .iter()
            .filter_map(|content| match content {
                maho_tools::definition::ToolContent::Text { text, .. } => Some(text.clone()),
                maho_tools::definition::ToolContent::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if error_text.is_empty() || Some(error_text.clone()) == preview_error {
            return None;
        }
        return Some(theme.fg(ThemeColor::Error, &error_text));
    }
    let result_diff = result.details.as_ref().and_then(|details| details.get("diff")).and_then(Value::as_str);
    if let Some(result_diff) = result_diff
        && Some(result_diff.to_owned()) != preview_diff
    {
        return Some(render_tool_diff(result_diff, raw_path.as_deref(), theme));
    }
    None
}

#[derive(Default)]
pub struct EditRenderers {
    preview: Option<EditPreview>,
    preview_args_key: Option<String>,
    settled_error: bool,
}

impl ToolRenderers for EditRenderers {
    fn is_built_in(&self) -> bool {
        true
    }
    fn render_call(&mut self, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        let preview_input = get_renderable_preview_input(Some(context.args));
        let args_key = preview_args_key(preview_input.as_ref());
        if self.preview_args_key != args_key {
            self.preview = None;
            self.preview_args_key = args_key;
            self.settled_error = false;
        }
        if context.args_complete && preview_input.is_some() && self.preview.is_none() {
            self.preview = preview_input.as_ref().map(|input| compute_preview(input, context.cwd));
        }
        Some(build_edit_call_component(Some(context.args), self.preview.as_ref(), self.settled_error, theme, context.cwd))
    }

    fn render_result(
        &mut self,
        result: &ToolResult,
        _options: ToolRenderResultOptions,
        theme: &Theme,
        context: &ToolRenderContext<'_>,
    ) -> Option<RenderedComponent> {
        let result_diff = if !context.is_error {
            result.details.as_ref().and_then(|details| details.get("diff")).and_then(Value::as_str)
        } else {
            None
        };
        if let Some(result_diff) = result_diff {
            self.preview = Some(EditPreview::Diff {
                diff: result_diff.to_owned(),
                first_changed_line: result
                    .details
                    .as_ref()
                    .and_then(|details| details.get("firstChangedLine"))
                    .and_then(Value::as_u64)
                    .map(|value| value as usize),
            });
        }
        self.settled_error = context.is_error;

        let mut component = Container::new();
        if let Some(output) = format_edit_result(Some(context.args), self.preview.as_ref(), result, theme, context.is_error) {
            component.add_child(Rc::new(RefCell::new(Spacer::new(1))));
            component.add_child(Rc::new(RefCell::new(Text::with_padding(output, 1, 0))));
        }
        Some(Rc::new(RefCell::new(component)))
    }
}
