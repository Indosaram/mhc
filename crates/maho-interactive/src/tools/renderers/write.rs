//! Port of `core/tools/renderers/write.ts`.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tools::definition::ToolResult;
use maho_tui::components::text::Text;
use maho_tui::tui::Container;
use serde_json::Value;

use super::{RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use crate::components::keybinding_hints::key_hint;
use crate::components::write_result::format_write_result;
use crate::theme::{Theme, ThemeColor};
use crate::tools::diff_render::render_tool_diff;
use crate::tools::render_utils::{normalize_display_text, render_tool_path, replace_tabs, str_value};

const WRITE_PARTIAL_FULL_HIGHLIGHT_LINES: usize = 50;

#[derive(Clone, Debug)]
pub struct WriteHighlightCache {
    raw_path: Option<String>,
    lang: &'static str,
    raw_content: String,
    normalized_lines: Vec<String>,
    highlighted_lines: Vec<String>,
}

fn highlight_single_line(line: &str, lang: &str, theme: &Theme) -> String {
    crate::theme::highlight_code(theme, line, Some(lang)).first().cloned().unwrap_or_default()
}

fn refresh_write_highlight_prefix(cache: &mut WriteHighlightCache, theme: &Theme) {
    let prefix_count = WRITE_PARTIAL_FULL_HIGHLIGHT_LINES.min(cache.normalized_lines.len());
    if prefix_count == 0 {
        return;
    }
    let prefix_source = cache.normalized_lines[..prefix_count].join("\n");
    let prefix_highlighted = crate::theme::highlight_code(theme, &prefix_source, Some(cache.lang));
    for index in 0..prefix_count {
        cache.highlighted_lines[index] = prefix_highlighted
            .get(index)
            .cloned()
            .unwrap_or_else(|| highlight_single_line(cache.normalized_lines.get(index).map_or("", String::as_str), cache.lang, theme));
    }
}

fn rebuild_write_highlight_cache_full(
    raw_path: Option<&str>,
    file_content: &str,
    theme: &Theme,
) -> Option<WriteHighlightCache> {
    let lang = raw_path.and_then(crate::theme::get_language_from_path)?;
    let display_content = normalize_display_text(file_content);
    let normalized = replace_tabs(&display_content);
    Some(WriteHighlightCache {
        raw_path: raw_path.map(str::to_owned),
        lang,
        raw_content: file_content.to_owned(),
        normalized_lines: normalized.split('\n').map(str::to_owned).collect(),
        highlighted_lines: crate::theme::highlight_code(theme, &normalized, Some(lang)),
    })
}

fn update_write_highlight_cache_incremental(
    cache: Option<WriteHighlightCache>,
    raw_path: Option<&str>,
    file_content: &str,
    theme: &Theme,
) -> Option<WriteHighlightCache> {
    let lang = raw_path.and_then(crate::theme::get_language_from_path)?;
    let Some(mut cache) = cache else {
        return rebuild_write_highlight_cache_full(raw_path, file_content, theme);
    };
    if cache.lang != lang || cache.raw_path.as_deref() != raw_path {
        return rebuild_write_highlight_cache_full(raw_path, file_content, theme);
    }
    if !file_content.starts_with(&cache.raw_content) {
        return rebuild_write_highlight_cache_full(raw_path, file_content, theme);
    }
    if file_content.len() == cache.raw_content.len() {
        return Some(cache);
    }

    let delta_raw = &file_content[cache.raw_content.len()..];
    let delta_display = normalize_display_text(delta_raw);
    let delta_normalized = replace_tabs(&delta_display);
    cache.raw_content = file_content.to_owned();
    if cache.normalized_lines.is_empty() {
        cache.normalized_lines.push(String::new());
        cache.highlighted_lines.push(String::new());
    }

    let segments: Vec<&str> = delta_normalized.split('\n').collect();
    let last_index = cache.normalized_lines.len() - 1;
    cache.normalized_lines[last_index].push_str(segments[0]);
    cache.highlighted_lines[last_index] = highlight_single_line(&cache.normalized_lines[last_index], cache.lang, theme);
    for segment in segments.iter().skip(1) {
        cache.normalized_lines.push((*segment).to_owned());
        cache.highlighted_lines.push(highlight_single_line(segment, cache.lang, theme));
    }
    refresh_write_highlight_prefix(&mut cache, theme);
    Some(cache)
}

fn trim_trailing_empty_lines(lines: &mut Vec<String>) {
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
}

fn generate_added_content_diff(content: &str, visible_line_count: usize) -> String {
    let mut visible_lines: Vec<String> = content.split('\n').map(str::to_owned).collect();
    trim_trailing_empty_lines(&mut visible_lines);
    let line_num_width = visible_lines.len().max(1).to_string().len();
    let mut output: Vec<String> = Vec::new();
    for (index, line) in visible_lines.iter().take(visible_line_count).enumerate() {
        output.push(format!("+{:>width$} {}", index + 1, line, width = line_num_width));
    }
    if visible_lines.len() > visible_line_count {
        output.push(format!(" {:>width$} ...", "", width = line_num_width));
    }
    output.join("\n")
}

fn format_write_call(
    args: Option<&Value>,
    options: ToolRenderResultOptions,
    args_complete: bool,
    theme: &Theme,
    cache: Option<&WriteHighlightCache>,
    cwd: &str,
) -> String {
    let raw_path = args.and_then(|args| str_value(args.get("file_path").or_else(|| args.get("path")))).flatten();
    let file_content = args.and_then(|args| str_value(args.get("content"))).flatten();
    let path_display = render_tool_path(raw_path.clone(), theme, cwd, None);
    let mut text = format!("{} {path_display}", theme.fg(ThemeColor::ToolTitle, &theme.bold("write")));

    match &file_content {
        None => text += &format!("\n\n{}", theme.fg(ThemeColor::Error, "[invalid content arg - expected string]")),
        Some(content) if !content.is_empty() => {
            let lang = raw_path.as_deref().and_then(crate::theme::get_language_from_path);
            let normalized_content = replace_tabs(&normalize_display_text(content));
            let mut lines: Vec<String> = normalized_content.split('\n').map(str::to_owned).collect();
            trim_trailing_empty_lines(&mut lines);
            let total_lines = lines.len();
            let max_lines = if options.expanded { lines.len() } else { 10 };
            let remaining = lines.len().saturating_sub(max_lines);
            if args_complete {
                text += &format!(
                    "\n\n{}",
                    render_tool_diff(&generate_added_content_diff(&normalized_content, max_lines), raw_path.as_deref(), theme)
                );
            } else {
                let rendered_lines: Vec<String> = match lang {
                    Some(_) => cache.map(|cache| cache.highlighted_lines.clone()).unwrap_or_else(|| {
                        crate::theme::highlight_code(theme, &normalized_content, lang)
                    }),
                    None => lines.clone(),
                };
                text += &format!(
                    "\n\n{}",
                    rendered_lines
                        .iter()
                        .take(max_lines)
                        .map(|line| if lang.is_some() { line.clone() } else { theme.fg(ThemeColor::ToolOutput, line) })
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
            if remaining > 0 {
                text += &format!(
                    "{} {}{}",
                    theme.fg(ThemeColor::Muted, &format!("\n... ({remaining} more lines, {total_lines} total,")),
                    key_hint("app.tools.expand", "to expand", theme),
                    theme.fg(ThemeColor::Muted, ")")
                );
            }
        }
        Some(_) => {}
    }
    text
}

#[derive(Default)]
pub struct WriteRenderers {
    cache: Option<WriteHighlightCache>,
}

impl ToolRenderers for WriteRenderers {
    fn is_built_in(&self) -> bool {
        true
    }
    fn render_call(&mut self, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        let raw_path = context
            .args
            .get("file_path")
            .or_else(|| context.args.get("path"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let file_content = str_value(context.args.get("content")).flatten();
        match &file_content {
            Some(content) => {
                self.cache = if context.args_complete {
                    rebuild_write_highlight_cache_full(raw_path.as_deref(), content, theme)
                } else {
                    update_write_highlight_cache_incremental(self.cache.take(), raw_path.as_deref(), content, theme)
                };
            }
            None => self.cache = None,
        }
        let text = format_write_call(
            Some(context.args),
            ToolRenderResultOptions { expanded: context.expanded, is_partial: context.is_partial },
            context.args_complete,
            theme,
            self.cache.as_ref(),
            context.cwd,
        );
        Some(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))))
    }

    fn render_result(
        &mut self,
        result: &ToolResult,
        _options: ToolRenderResultOptions,
        theme: &Theme,
        context: &ToolRenderContext<'_>,
    ) -> Option<RenderedComponent> {
        let output = format_write_result(result, context.is_error, theme);
        let mut component = Container::new();
        if let Some(output) = output {
            component.add_child(Rc::new(RefCell::new(Text::with_padding(output, 0, 0))));
        }
        Some(Rc::new(RefCell::new(component)))
    }
}
