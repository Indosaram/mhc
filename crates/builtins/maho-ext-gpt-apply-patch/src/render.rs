use std::{cell::RefCell, rc::Rc, sync::Arc};

use maho_ext_api::{Theme as ExtensionTheme, ToolRenderers};
use maho_interactive::{theme::{Theme, ThemeBg, ThemeColor, theme::ColorMode, theme_json::{ColorValue, ThemeJson}}, tools::diff_render::render_tool_diff};
use maho_tui::{components::{box_::Box as TuiBox, text::Text}, tui::Component};
use serde_json::Value;

use crate::{preview_format::{ApplyPatchRenderState, display_path, truncate_preview}, types::{ApplyPatchPreview, ApplyPatchToolDetails}};

fn theme(source: &ExtensionTheme) -> Theme {
    let colors = source.colors.iter().chain(&source.backgrounds)
        .map(|(key, value)| (key.clone(), ColorValue::Text(value.clone()))).collect();
    Theme::from_json(ThemeJson {
        name: source.name.clone().unwrap_or_default(), colors,
        vars: source.vars.iter().map(|(key, value)| (key.clone(), ColorValue::Text(value.clone()))).collect(),
        export_colors: Default::default(),
    }, ColorMode::Truecolor).unwrap_or_else(|error| std::panic::panic_any(error))
}

fn line(text: &str, theme: &Theme) -> String {
    match text.trim_start().chars().next() {
        Some('+') => theme.fg(ThemeColor::ToolDiffAdded, text),
        Some('-') => theme.fg(ThemeColor::ToolDiffRemoved, text),
        Some('•') => theme.fg(ThemeColor::ToolTitle, &theme.bold(text)),
        Some('└') => theme.fg(ThemeColor::Accent, text),
        _ => theme.fg(ThemeColor::ToolDiffContext, text),
    }
}

fn preview(preview: &ApplyPatchPreview, cwd: &str, theme: &Theme) -> String {
    let mut lines = Vec::new();
    let multiple = preview.files.len() != 1;
    if multiple { lines.push(format!("• Edited {} files (+{} -{})", preview.files.len(), preview.added, preview.removed)); }
    for file in &preview.files {
        let mut path = display_path(&file.file_path, cwd);
        if let Some(destination) = file.move_path.as_deref().filter(|path| !path.is_empty()) { path.push_str(&format!(" → {}", display_path(destination, cwd))); }
        let summary = if file.binary == Some(true) { "(binary)".into() } else { format!("(+{} -{})", file.added, file.removed) };
        let header = if multiple { format!("  └ {path} {summary}") } else {
            let operation = match file.operation { crate::types::ApplyPatchOperation::Add => "Added", crate::types::ApplyPatchOperation::Delete => "Deleted", crate::types::ApplyPatchOperation::Update => "Edited" };
            format!("• {operation} {path} {summary}")
        };
        lines.push(header);
        if !file.diff.is_empty() {
            let rendered = render_tool_diff(&truncate_preview(&file.diff), Some(file.move_path.as_deref().unwrap_or(&file.file_path)), theme);
            lines.extend(rendered.split('\n').map(|line| if multiple { format!("    {line}") } else { line.into() }));
        }
    }
    lines.join("\n")
}

fn boxed(title: &str, body: &str, background: ThemeBg, theme: &Theme) -> Box<dyn Component> {
    let mut component = TuiBox::with_padding(1, 1);
    let background_theme = theme.clone();
    component.set_bg_fn(Some(Rc::new(move |text| background_theme.bg(background, text))));
    component.add_child(Rc::new(RefCell::new(Text::with_padding(format!("{}\n\n{body}", theme.fg(ThemeColor::ToolTitle, &theme.bold(title))), 0, 0))));
    Box::new(component)
}

pub fn renderers() -> ToolRenderers<ApplyPatchRenderState, Value> {
    ToolRenderers {
        render_call: Some(Arc::new(|args, source_theme, context| {
            let theme = theme(source_theme);
            let input = crate::params::normalize_apply_patch_arguments(args).input;
            if !context.execution_started && !input.is_empty() {
                let mut streaming = context.state.streaming.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                streaming.update(&input);
                let (title, body) = if let Some(error) = &streaming.error { ("Invalid patch stream", error.clone()) }
                else if !streaming.hunks.is_empty() { ("Applying patch", crate::streaming_render::format_streaming_hunks(&streaming.hunks)) }
                else { ("Applying patch", crate::text::extract_patched_paths(&input).iter().map(|path| format!("• {path}")).collect::<Vec<_>>().join("\n")) };
                if !body.is_empty() { return boxed(title, &body.split('\n').map(|text| line(text, &theme)).collect::<Vec<_>>().join("\n"), ThemeBg::ToolPendingBg, &theme); }
            }
            let text = if context.args_complete {
                let state = crate::preview_format::get_apply_patch_render_state(&context.tool_call_id, &context.cwd.to_string_lossy(), &input);
                let state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                format!("apply_patch: {}", state.call_text)
            } else { "apply_patch: Patching".into() };
            Box::new(Text::with_padding(theme.fg(ThemeColor::ToolTitle, &theme.bold(&text)), 0, 0))
        })),
        render_result: Some(Arc::new(|result, options, source_theme, context| {
            let theme = theme(source_theme);
            let text = result.content.iter().filter_map(|block| match block { maho_ext_api::ContentBlock::Text(text) if !text.text.is_empty() => Some(text.text.as_str()), _ => None }).collect::<Vec<_>>().join("\n");
            let details = serde_json::from_value::<ApplyPatchToolDetails>(result.details.clone()).ok();
            if let Some(details) = details {
                let failure = context.is_error && details.result.as_ref().is_some_and(|result| !result.failures.is_empty());
                let cwd = context.cwd.to_string_lossy();
                let mut body = details.preview.as_ref().map(|value| preview(value, &cwd, &theme)).unwrap_or_default();
                if failure {
                    if !body.is_empty() && !text.is_empty() { body.push_str("\n\n"); }
                    body.push_str(&theme.fg(ThemeColor::ToolOutput, &text));
                    let title = if details.result.as_ref().is_some_and(|result| !result.applied_files.is_empty()) { "Patch partially failed" } else { "Patch failed" };
                    return boxed(title, &body, ThemeBg::ToolErrorBg, &theme);
                }
                if details.preview.is_some() || details.result.is_some() {
                    let title = if let Some(progress) = details.progress { format!("Applying patch ({}/{})", progress.applied + progress.failed, progress.total) }
                    else if options.is_partial || details.preview.is_none() { "Applying patch".into() } else { "Applied patch".into() };
                    return boxed(&title, &body, if options.is_partial { ThemeBg::ToolPendingBg } else { ThemeBg::ToolSuccessBg }, &theme);
                }
            }
            Box::new(Text::with_padding(theme.fg(ThemeColor::ToolOutput, &text), 0, 0))
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expanded_preview_headers_remain_unstyled_for_single_and_multiple_files() {
        let theme=theme(&ExtensionTheme {colors:[("toolTitle".into(),"#ff0000".into()),("accent".into(),"#00ff00".into())].into(),..Default::default()});
        let file=crate::types::ApplyPatchPreviewFile {file_path:"/work/a.rs".into(),move_path:None,operation:crate::types::ApplyPatchOperation::Update,binary:None,diff:String::new(),patch:None,added:1,removed:1};
        for files in [vec![file.clone()],vec![file.clone(),file]] {
            let rendered=preview(&ApplyPatchPreview {files,added:2,removed:2},"/work",&theme);
            assert!(!rendered.contains('\u{1b}'));
            assert!(rendered.contains("a.rs"));
        }
    }
    #[test]
    fn expanded_preview_reuses_shared_inline_diff_and_destination_language() {
        let theme = theme(&ExtensionTheme {
            colors: [("toolDiffAdded".into(), "#00ff00".into()), ("toolDiffRemoved".into(), "#ff0000".into())].into(),
            ..Default::default()
        });
        let file = crate::types::ApplyPatchPreviewFile {
            file_path: "/work/old.rs".into(), move_path: Some("/work/new.rs".into()),
            operation: crate::types::ApplyPatchOperation::Update, binary: None,
            diff: "-1 let value = 1;\n+1 let value = 2;".into(), patch: None, added: 1, removed: 1,
        };
        let rendered = preview(&ApplyPatchPreview { files: vec![file.clone()], added: 1, removed: 1 }, "/work", &theme);
        let shared = render_tool_diff(&file.diff, file.move_path.as_deref(), &theme);
        assert!(rendered.ends_with(&shared));
        assert!(rendered.contains("\u{1b}[7m"));
        assert!(rendered.contains("old.rs → new.rs"));
    }
}
