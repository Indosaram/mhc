//! Port of `tool/tool-widgets.ts`: bounded nested tool-call widgets for eval cards. Core tools
//! (bash/read/write/grep/find/ls) draw through the native built-in call renderers; every other
//! call falls back to a compact `tool.name(args)` row.
use std::{cell::RefCell, collections::{BTreeMap, HashMap}, path::{Path, PathBuf}, rc::Rc, sync::LazyLock};
use maho_ext_api::Theme;
use maho_tui::{components::text::Text, terminal_text::sanitize_terminal_label, tui::Component};
use regex::Regex;
use serde_json::Value;

pub struct WidgetOptions<'a> {
    pub cwd: &'a Path,
    pub theme: Option<&'a Theme>,
    pub expanded: bool,
    pub width: usize,
}

const MIN_RENDERER_WIDTH: usize = 20;
const MAX_RENDERER_LINES: usize = 5;
const MAX_FALLBACK_ARGUMENT_CODE_POINTS: usize = 120;
const MAX_COLLAPSED_ERROR_CODE_POINTS: usize = 512;
const MAX_COLLAPSED_ERROR_LINES: usize = 4;
const MAX_PREVIEW_LINES: usize = 2;
const MAX_COLLAPSED_WIDGET_LINES: usize = 8;
const TOOL_ERROR_OMISSION_MARKER: &str = "[tool error omitted]";
const WIDGET_TRUNCATION_MARKER: &str = "… (widget truncated)";
static TERMINAL_ESCAPE_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:\x1B\][\s\S]*?(?:\x07|\x1B\\|\x{9C}))|[\x1B\x{9B}][\[\]()#;?]*(?:[0-9]{1,4}(?:[;:][0-9]{0,4})*)?[0-9A-PR-TZcf-nq-uy=><~]").expect("terminal escape pattern"));
static TERMINAL_CONTROL_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\x00-\x09\x0B-\x1F\x7F-\x{9F}]+").expect("terminal control pattern"));
thread_local! {
    static CORE_DEFINITIONS_BY_CWD: RefCell<HashMap<PathBuf, Rc<BTreeMap<String, maho_tools::definition::ToolDefinition>>>> = RefCell::new(HashMap::new());
}

pub fn code_point_prefix(text: &str, max_code_points: usize) -> String {
    text.chars().take(max_code_points).collect()
}

pub fn format_duration(milliseconds: f64) -> String {
    let total_seconds=(milliseconds.max(0.0)/1000.0).floor() as u64;
    if total_seconds<1 {return "<1s".into();}
    let seconds=total_seconds%60;
    let total_minutes=total_seconds/60;
    if total_minutes<1 {return format!("{seconds}s");}
    let minutes=total_minutes%60;
    let hours=total_minutes/60;
    if hours<1 {return if seconds==0 {format!("{minutes}m")} else {format!("{minutes}m {seconds}s")};}
    if minutes==0 {format!("{hours}h")} else {format!("{hours}h {minutes}m")}
}

/// senpi `style(theme, color, text)`: `theme.fg(color, text)`, or the plain text without a theme.
pub fn style(theme: Option<&Theme>, color: &str, text: &str) -> String {
    theme.map_or_else(|| text.to_owned(), |theme| theme.fg(color, text))
}

/// The guest theme exports ANSI prefixes; the native built-in renderers and the code highlighter
/// take the interactive `Theme`, so the exported prefixes are decoded back into theme colors.
/// Prefixes that are not plain 256-color or truecolor SGR codes are skipped.
pub fn native_theme(source: &Theme) -> Option<maho_interactive::theme::Theme> {
    use maho_interactive::theme::{ColorMode, theme_json::{ColorValue, ThemeJson}};
    let colors = source.colors.iter().chain(&source.backgrounds).filter_map(|(key, prefix)| {
        let fields = prefix.strip_prefix("\x1b[")?.strip_suffix('m')?.split(';').collect::<Vec<_>>();
        let value = match fields.as_slice() {
            ["39" | "49"] => ColorValue::Text(String::new()),
            ["38" | "48", "5", index] => ColorValue::Index(index.parse().ok()?),
            ["38" | "48", "2", red, green, blue] => ColorValue::Text(format!("#{:02x}{:02x}{:02x}", red.parse::<u8>().ok()?, green.parse::<u8>().ok()?, blue.parse::<u8>().ok()?)),
            _ => return None,
        };
        Some((key.clone(), value))
    }).collect();
    let mode = if source.colors.values().chain(source.backgrounds.values()).any(|prefix| prefix.starts_with("\x1b[38;2;") || prefix.starts_with("\x1b[48;2;")) { ColorMode::Truecolor } else { ColorMode::Color256 };
    maho_interactive::theme::Theme::from_json(ThemeJson {appearance:None, name: source.name.clone().unwrap_or_default(), vars: BTreeMap::new(), colors, export_colors: BTreeMap::new() }, mode).ok()
}

pub fn render_all_visual_lines(text: &str, width: usize) -> Vec<String> {
    Text::with_padding(text, 0, 0).render(width.max(1)).into_iter().map(|line| line.trim_end().to_owned()).collect()
}

fn render_first_visual_lines(text: &str, max_lines: usize, width: usize) -> Vec<String> {
    render_all_visual_lines(text, width).into_iter().take(max_lines).collect()
}

fn indent(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| format!("  {line}")).collect()
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str { value[key].as_str().unwrap_or("") }

/// senpi `prepareArguments` + TypeBox `Check` against the core definition's parameters.
fn prepared_core_arguments(name: &str, args: &Value, cwd: &Path) -> Option<Value> {
    let definitions = CORE_DEFINITIONS_BY_CWD.with(|cache| cache.borrow_mut().entry(cwd.to_path_buf())
        .or_insert_with(|| Rc::new(maho_tools::index::create_all_tool_definitions(cwd, maho_tools::index::ToolsOptions::default()))).clone());
    let definition = definitions.get(name)?;
    let prepared = match &definition.prepare_arguments { Some(prepare) => prepare(args.clone()).ok()?, None => args.clone() };
    let tool = maho_ai::types::Tool { name: name.into(), description: String::new(), parameters: definition.parameters.clone(), freeform: None, constrained_sampling: None };
    let call = maho_ai::types::ToolCall { name: name.into(), arguments: prepared.as_object()?.clone(), ..Default::default() };
    maho_ai::utils::validation::validate_tool_arguments(&tool, &call).ok()?;
    Some(prepared)
}

fn render_with(name: &str, summary: &Value, options: &WidgetOptions<'_>) -> Option<Vec<String>> {
    let args = prepared_core_arguments(name, summary.get("args")?, options.cwd)?;
    let theme = options.theme?;
    let inner_width = options.width.checked_sub(2).filter(|width| *width >= MIN_RENDERER_WIDTH)?;
    let native = native_theme(theme)?;
    let renderer = maho_interactive::tools::renderers::create_all_tool_renderers().get(name).cloned()?;
    let cwd = options.cwd.to_string_lossy();
    let tool_call_id = summary["callId"].as_str().unwrap_or(name);
    let context = maho_interactive::tools::renderers::ToolRenderContext {
        args: &args, tool_call_id, cwd: &cwd, execution_started: true, args_complete: true, is_partial: false,
        expanded: options.expanded, show_images: false, is_error: summary["ok"] != true, has_result: false,
        spinner_frame: None, now_ms: 0.0, invalidate: Rc::new(|| {}),
    };
    let component = renderer.borrow_mut().render_call(&native, &context)?;
    let rendered: Vec<String> = component.borrow_mut().render(inner_width).into_iter().map(|line| line.trim_end().to_owned()).collect();
    let mut retained: Vec<String> = rendered.iter().take(MAX_RENDERER_LINES).cloned().collect();
    if rendered.len() > retained.len() { retained.push(style(Some(theme), "muted", WIDGET_TRUNCATION_MARKER)); }
    Some(indent(retained))
}

fn grep_details_summary(summary: &Value) -> Option<String> {
    let details = summary.get("details").filter(|details| details.is_object())?;
    let file_count = details["fileCount"].as_f64()?;
    let engine = details["engine"].as_str()?;
    let match_count = match details.get("matchCount") {
        None | Some(Value::Null) => "n/a".to_owned(),
        Some(value) => format!("{}", value.as_f64()?),
    };
    Some(format!("matches={match_count} files={file_count} ({engine})"))
}

fn render_core_widget(name: &str, summary: &Value, options: &WidgetOptions<'_>) -> Option<Vec<String>> {
    match name {
        "bash" | "read" | "write" | "find" | "ls" => render_with(name, summary, options),
        "grep" => {
            let mut lines = render_with(name, summary, options)?;
            if let Some(extra) = grep_details_summary(summary) { lines.push(format!("  {}", style(options.theme, "muted", &extra))); }
            Some(lines)
        }
        _ => None,
    }
}

fn compact_arguments(args: Option<&Value>) -> String {
    let Some(args) = args else { return String::new() };
    match serde_json::to_string(args) {
        Ok(serialized) => code_point_prefix(&sanitize_terminal_label(&serialized), MAX_FALLBACK_ARGUMENT_CODE_POINTS),
        Err(_) => "[unserializable args]".into(),
    }
}

fn render_fallback(summary: &Value, width: usize) -> Vec<String> {
    let name = sanitize_terminal_label(text(summary, "name"));
    indent(render_all_visual_lines(&format!("tool.{name}({})", compact_arguments(summary.get("args"))), width))
}

fn render_status(summary: &Value, options: &WidgetOptions<'_>, width: usize) -> Vec<String> {
    let ok = summary["ok"] == true;
    let mut parts = vec![style(options.theme, if ok { "success" } else { "error" }, if ok { "✓" } else { "✗" })];
    if let Some(duration) = summary["durationMs"].as_f64() { parts.push(format_duration(duration)); }
    if summary["argsTruncated"] == true { parts.push("(args truncated)".into()); }
    indent(render_first_visual_lines(&parts.join(" "), 1, width))
}

fn sanitize_terminal_control(text: &str) -> String {
    TERMINAL_CONTROL_PATTERN.replace_all(&TERMINAL_ESCAPE_PATTERN.replace_all(text, ""), " ").into_owned()
}

fn render_preview(preview: &str, options: &WidgetOptions<'_>, width: usize) -> Vec<String> {
    let sanitized = sanitize_terminal_control(preview);
    if sanitized.is_empty() { return vec![]; }
    indent(render_first_visual_lines(&style(options.theme, "muted", &sanitized), MAX_PREVIEW_LINES, width))
}

fn render_collapsed_error(error: &str, options: &WidgetOptions<'_>, width: usize) -> Vec<String> {
    let guarded_error = code_point_prefix(error, MAX_COLLAPSED_ERROR_CODE_POINTS);
    let guarded_lines = render_all_visual_lines(&style(options.theme, "error", &guarded_error), width);
    if guarded_error.len() == error.len() && guarded_lines.len() <= MAX_COLLAPSED_ERROR_LINES { return indent(guarded_lines); }
    let marker_lines = render_all_visual_lines(&style(options.theme, "muted", TOOL_ERROR_OMISSION_MARKER), width);
    let error_budget = MAX_COLLAPSED_ERROR_LINES.saturating_sub(marker_lines.len());
    let mut lines: Vec<String> = guarded_lines.into_iter().take(error_budget).collect();
    let marker_budget = MAX_COLLAPSED_ERROR_LINES.saturating_sub(lines.len());
    lines.extend(marker_lines.into_iter().take(marker_budget));
    indent(lines)
}

fn render_error(error: &str, options: &WidgetOptions<'_>, width: usize) -> Vec<String> {
    let sanitized = sanitize_terminal_control(error);
    if options.expanded { return indent(render_all_visual_lines(&style(options.theme, "error", &sanitized), width)); }
    render_collapsed_error(&sanitized, options, width)
}

/// senpi `renderToolCallWidget`. The theme-less fallback is QA-only: `render.rs` routes theme-less
/// renders to the legacy rows before this is called.
pub fn render_tool_call_widget(summary: &Value, options: &WidgetOptions<'_>) -> Vec<String> {
    let inner_width = options.width.saturating_sub(2).max(1);
    let mut lines = render_core_widget(text(summary, "name"), summary, options).unwrap_or_else(|| render_fallback(summary, inner_width));
    lines.extend(render_status(summary, options, inner_width));
    let ok = summary["ok"] == true;
    match (ok, summary["resultPreview"].as_str(), summary["error"].as_str()) {
        (true, Some(preview), _) => lines.extend(render_preview(preview, options, inner_width)),
        (false, _, Some(error)) => lines.extend(render_error(error, options, inner_width)),
        _ => {}
    }
    if !options.expanded { lines.truncate(MAX_COLLAPSED_WIDGET_LINES); }
    lines
}
