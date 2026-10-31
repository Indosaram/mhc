//! Port of `tool/render.ts`: the themed eval call/result pipeline. Without a theme (print/json
//! modes, or a host theme that exports no colors) every `style` call returns the plain text.
use super::{
    display_code::display_code,
    eval_request::normalize_eval_summary,
    tool_widgets::{
        WidgetOptions, code_point_prefix, format_duration, native_theme,
        render_all_visual_lines as visual, render_tool_call_widget, style,
    },
    types::{EvalLanguage, EvalToolRequest},
};
use maho_ext_api::{AgentToolResult, ContentBlock, Theme};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Default)]
pub struct EvalRenderContext {
    pub expanded: bool,
    pub has_result: bool,
    pub is_partial: bool,
    pub is_error: bool,
    pub show_images: bool,
    pub image_protocol: Option<String>,
    pub spinner_frame: Option<usize>,
    pub now: f64,
    pub theme: Option<Theme>,
    pub cwd: PathBuf,
}

const TOOL_CALL_PREVIEW_COUNT: usize = 5;
const TOOL_CALL_COLLAPSED_VISUAL_LINES: usize = 4;
const TOOL_CALL_COLLAPSED_ERROR_CODE_POINTS: usize = 512;

fn preview_text(text: &str, max_lines: usize, width: usize) -> (Vec<String>, usize) {
    let mut lines = visual(text, width);
    if lines.len() <= max_lines {
        return (lines, 0);
    }
    let skipped = lines.len() - max_lines;
    (lines.split_off(skipped), skipped)
}
fn preview(
    text: &str,
    width: usize,
    limit: usize,
    kind: &str,
    theme: Option<&Theme>,
) -> Vec<String> {
    let (lines, skipped) = preview_text(text, limit, width);
    if skipped == 0 {
        return lines;
    }
    let mut result = visual(
        &style(theme, "muted", &format!("{skipped} earlier {kind} lines")),
        width,
    );
    result.extend(lines);
    result
}
fn summary(text: &str, width: usize, expanded: bool) -> Vec<String> {
    let lines = visual(text, width);
    if expanded || lines.len() <= 3 {
        return lines;
    }
    let mut lines: Vec<_> = visual(text, width.saturating_sub(1).max(1))
        .into_iter()
        .take(3)
        .collect();
    if let Some(last) = lines.last_mut() {
        last.push('…');
    }
    lines
}
fn styled_summary(text: &str, width: usize, expanded: bool, theme: Option<&Theme>) -> Vec<String> {
    summary(text, width, expanded)
        .into_iter()
        .map(|line| style(theme, "muted", &line))
        .collect()
}
fn bold(theme: Option<&Theme>, text: &str) -> String {
    if theme.is_none() {
        return text.to_owned();
    }
    format!("\x1b[1m{}\x1b[22m", text.replace("\x1b[22m", "\x1b[1m"))
}
fn language(language: EvalLanguage) -> &'static str {
    match language {
        EvalLanguage::Js => "js",
    }
}
fn language_for_highlighter(language: EvalLanguage) -> &'static str {
    match language {
        EvalLanguage::Js => "javascript",
    }
}
fn parse_language(_: &Value) -> EvalLanguage {
    EvalLanguage::Js
}
fn highlighted_code(code: &str, language: EvalLanguage, theme: Option<&Theme>) -> String {
    let display = if code.trim().is_empty() {
        "...".to_owned()
    } else {
        display_code(code, language)
    };
    match theme.and_then(native_theme) {
        Some(native) => maho_interactive::theme::highlight_code(
            &native,
            &display,
            Some(language_for_highlighter(language)),
        )
        .join("\n"),
        None => display,
    }
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn prefixed(
    body: &str,
    width: usize,
    prefix: &str,
    continuation: &str,
    color: &str,
    theme: Option<&Theme>,
) -> Vec<String> {
    let lines = visual(body, width.saturating_sub(prefix.chars().count()).max(1));
    if lines.is_empty() {
        return vec![style(theme, color, prefix.trim_end())];
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            format!(
                "{}{line}",
                style(theme, color, if i == 0 { prefix } else { continuation })
            )
        })
        .collect()
}
fn presentation(status: &str, frame: Option<usize>) -> (&str, &str, &'static str) {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    match status {
        "pending" => ("pending", "○", "muted"),
        "queued" => ("queued", "○", "muted"),
        "running" => ("running", FRAMES[frame.unwrap_or(0) % 10], "warning"),
        "detached" => ("detached", "↗", "warning"),
        "complete" | "completed" => ("done", "✓", "success"),
        "error" => ("error", "✗", "error"),
        "failed" => ("failed", "✗", "error"),
        "aborted" => ("aborted", "×", "error"),
        "cancelled" => ("cancelled", "×", "error"),
        _ => ("running", FRAMES[frame.unwrap_or(0) % 10], "warning"),
    }
}
fn throughput(details: &Value) -> Option<String> {
    let calls = details["toolCallCount"].as_f64()?;
    if calls <= 0.0 {
        return None;
    }
    let seconds = details["wallDurationMs"].as_f64().unwrap_or(0.0) / 1000.0;
    let rate = if seconds > 0.0 {
        format!("{:.2} calls/s", calls / seconds)
    } else {
        "n/a calls/s".into()
    };
    Some(format!(
        "{calls} {} · {rate}",
        if calls == 1.0 { "call" } else { "calls" }
    ))
}
fn runtime_badge(details: &Value) -> String {
    let runtime = &details["runtime"];
    if !runtime.is_object() {
        return String::new();
    }
    let badge = super::runtime_label::format_runtime_badge(
        parse_language(&details["language"]),
        text(runtime, "name"),
        text(runtime, "version"),
        runtime["path"].as_str(),
        &std::env::var("HOME").unwrap_or_default(),
    );
    format!(" ({badge})")
}
fn event_number(event: &Value, key: &str) -> f64 {
    event[key].as_f64().filter(|n| n.is_finite()).unwrap_or(0.0)
}
fn plural(count: f64, singular: &str, multiple: &str) -> String {
    format!("{count} {}", if count == 1.0 { singular } else { multiple })
}
fn format_status(event: &Value, theme: Option<&Theme>) -> String {
    let op = text(event, "op");
    let icon = style(
        theme,
        "muted",
        if op.starts_with("git_") {
            "⌁"
        } else {
            match op {
                "read" | "write" | "cat" | "touch" => "▣",
                "ls" | "cd" | "pwd" | "mkdir" => "▤",
                "run" | "sh" => "▶",
                "completion" => "◇",
                "phase" => "◆",
                _ => "•",
            }
        },
    );
    if !text(event, "error").is_empty() {
        return format!(
            "{icon} {}: {}",
            style(theme, "warning", op),
            style(theme, "dim", text(event, "error"))
        );
    }
    let mut parts = vec![];
    match op {
        "read" | "write" => {
            parts.push(format!(
                "{} chars",
                event["chars"]
                    .as_f64()
                    .unwrap_or_else(|| event_number(event, "bytes"))
            ));
            if !text(event, "path").is_empty() {
                parts.push(format!(
                    "{} {}",
                    if op == "read" { "from" } else { "to" },
                    text(event, "path")
                ));
            }
        }
        "cat" => {
            parts.push(plural(event_number(event, "files"), "file", "files"));
            parts.push(format!("{} chars", event_number(event, "chars")));
        }
        "ls" => parts.push(plural(event_number(event, "count"), "entry", "entries")),
        "env" => {
            let key = text(event, "key");
            let value: String =
                std::char::decode_utf16(text(event, "value").encode_utf16().take(30))
                    .map(|r| r.unwrap_or('�'))
                    .collect();
            if matches!(text(event, "action"), "get" | "set") && !key.is_empty() {
                parts.push(format!(
                    "{}{key}={value}",
                    if text(event, "action") == "set" {
                        "set "
                    } else {
                        ""
                    }
                ));
            } else {
                parts.push(plural(
                    event_number(event, "count"),
                    "variable",
                    "variables",
                ));
            }
        }
        "git_status" => {
            if event["clean"] == true {
                parts.push("clean".into());
            } else {
                let changes: Vec<_> = ["staged", "modified", "untracked"]
                    .into_iter()
                    .filter(|key| event_number(event, key) > 0.0)
                    .map(|key| format!("{} {key}", event_number(event, key)))
                    .collect();
                parts.push(if changes.is_empty() {
                    "unknown".into()
                } else {
                    changes.join(", ")
                });
            }
            if !text(event, "branch").is_empty() {
                parts.push(format!("on {}", text(event, "branch")));
            }
        }
        "git_diff" => {
            parts.push(plural(event_number(event, "lines"), "line", "lines"));
            if event["staged"] == true {
                parts.push("staged".into());
            }
        }
        "git_log" => parts.push(plural(event_number(event, "commits"), "commit", "commits")),
        "run" | "sh" => {
            let command = event["command"]
                .as_str()
                .unwrap_or_else(|| text(event, "cmd"));
            if !command.is_empty() {
                parts.push(command.into());
            }
            if let Some(code) = event["exitCode"].as_f64() {
                parts.push(format!("exit {code}"));
            }
        }
        "completion" => {
            let model = text(event, "model");
            let tier = text(event, "tier");
            if !model.is_empty() {
                parts.push(model.into());
            }
            if !tier.is_empty() && tier != model {
                parts.push(tier.into());
            }
            parts.push(format!("{} chars", event_number(event, "chars")));
        }
        "log" => parts.push(text(event, "message").into()),
        "phase" => parts.push(text(event, "title").into()),
        "status-events-omitted" => parts.push(format!(
            "{} earlier events omitted",
            event_number(event, "count")
        )),
        _ => {
            if let Some(count) = event.get("count") {
                parts.push(
                    count
                        .as_str()
                        .map_or_else(|| count.to_string(), str::to_owned),
                );
            }
            if !text(event, "path").is_empty() {
                parts.push(text(event, "path").into());
            }
        }
    }
    let description = parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    format!(
        "{icon} {}{}",
        style(theme, "muted", op),
        if description.is_empty() {
            String::new()
        } else {
            format!(" {}", style(theme, "dim", &description))
        }
    )
}
fn status_lines(events: &[Value], expanded: bool, theme: Option<&Theme>) -> Vec<String> {
    let events: Vec<_> = events
        .iter()
        .filter(|event| text(event, "op") != "agent")
        .collect();
    let omitted = events
        .first()
        .filter(|event| text(event, "op") == "status-events-omitted")
        .map_or(0.0, |event| event_number(event, "count"));
    let visible = if omitted > 0.0 {
        &events[1..]
    } else {
        &events[..]
    };
    let skipped = if expanded {
        0
    } else {
        visible.len().saturating_sub(3)
    };
    let mut lines = vec![];
    let count = skipped as f64 + omitted;
    if count > 0.0 {
        lines.push(style(
            theme,
            "dim",
            &format!("├ … {count} earlier status events"),
        ));
    }
    for (index, event) in visible[skipped..].iter().enumerate() {
        lines.push(format!(
            "{} {}",
            style(
                theme,
                "dim",
                if index + skipped + 1 == visible.len() {
                    "└"
                } else {
                    "├"
                }
            ),
            format_status(event, theme)
        ));
    }
    lines
}
fn agent_lines(
    events: &[Value],
    theme: Option<&Theme>,
    spinner_frame: Option<usize>,
    width: usize,
) -> Vec<String> {
    let mut rows: Vec<&Value> = vec![];
    for event in events.iter().filter(|event| text(event, "op") == "agent") {
        let id = text(event, "id");
        if !id.is_empty()
            && let Some(index) = rows.iter().position(|row| text(row, "id") == id)
        {
            rows[index] = event;
        } else {
            rows.push(event);
        }
    }
    let mut lines = vec![];
    for (index, event) in rows.iter().enumerate() {
        let status = match text(event, "status") {
            "pending" => "pending",
            "completed" => "completed",
            "failed" => "failed",
            "aborted" => "aborted",
            _ => "running",
        };
        let (label, icon, color) = presentation(status, spinner_frame);
        let id = if text(event, "id").is_empty() {
            "agent"
        } else {
            text(event, "id")
        };
        let mut body = format!("{} {} {label}", style(theme, color, icon), bold(theme, id));
        if matches!(status, "completed" | "failed" | "aborted")
            && event_number(event, "durationMs") > 0.0
        {
            body.push_str(&format!(
                " · {}",
                style(
                    theme,
                    "dim",
                    &format_duration(event_number(event, "durationMs"))
                )
            ));
        }
        let last = index + 1 == rows.len();
        let continuation = if last { "  " } else { "│ " };
        lines.extend(prefixed(
            &body,
            width,
            if last { "└ " } else { "├ " },
            continuation,
            "dim",
            theme,
        ));
        if status == "running" {
            let tool = text(event, "currentTool");
            let intent = text(event, "lastIntent");
            if !tool.is_empty() || !intent.is_empty() {
                let detail = if tool.is_empty() {
                    intent.into()
                } else if intent.is_empty() {
                    tool.into()
                } else {
                    format!("{tool}: {intent}")
                };
                lines.extend(prefixed(
                    &detail,
                    width,
                    &format!("{continuation}└ "),
                    &format!("{continuation}  "),
                    "dim",
                    theme,
                ));
            }
        }
    }
    lines
}

/// One eval card half. The call/result pipelines above are width-aware, so the lines are built at
/// render time from the card state the host hands the renderer.
struct EvalCard {
    args: Value,
    result: Option<AgentToolResult>,
    expanded: bool,
    has_result: bool,
    is_partial: bool,
    is_error: bool,
    show_images: bool,
    image_protocol: Option<String>,
    spinner_frame: Option<usize>,
    theme: Option<Theme>,
    cwd: PathBuf,
    ticker: std::rc::Rc<LiveTicker>,
}

const LIVE_ELAPSED_TICK_MS: u64 = 1_000;
// A live row repaints on every tick, so this many ticks without a render means the row is gone.
const LIVE_TICKER_MAX_IDLE_TICKS: u32 = 60;

/// senpi `PlainTextComponent.syncLiveTicker`: the host animates no eval row, so while a cell is
/// non-terminal the card repaints itself once a second to advance the header's elapsed time.
struct LiveTicker {
    ticker: std::cell::Cell<Option<maho_tui::ui_intervals::UiIntervalId>>,
    live: std::cell::Cell<bool>,
    invalidate: std::rc::Rc<dyn Fn()>,
    idle_ticks: std::cell::Cell<u32>,
}

impl LiveTicker {
    fn new(is_live: bool, invalidate: std::rc::Rc<dyn Fn()>) -> std::rc::Rc<Self> {
        let ticker = std::rc::Rc::new(Self {
            ticker: std::cell::Cell::new(None),
            live: std::cell::Cell::new(false),
            invalidate,
            idle_ticks: std::cell::Cell::new(0),
        });
        ticker.sync_live_ticker(is_live);
        ticker
    }
    fn sync_live_ticker(self: &std::rc::Rc<Self>, is_live: bool) {
        self.live.set(is_live);
        if !is_live {
            self.stop_live_ticker();
            return;
        }
        self.arm_ticker();
    }
    fn stop_live_ticker(&self) {
        if let Some(id) = self.ticker.take() {
            maho_tui::ui_intervals::clear_ui_interval(id);
        }
    }
    fn arm_ticker(self: &std::rc::Rc<Self>) {
        if self.ticker.get().is_some() {
            return;
        }
        let weak = std::rc::Rc::downgrade(self);
        self.ticker
            .set(Some(maho_tui::ui_intervals::set_ui_interval(
                std::time::Duration::from_millis(LIVE_ELAPSED_TICK_MS),
                std::rc::Rc::new(move || {
                    if let Some(ticker) = weak.upgrade() {
                        ticker.tick();
                    }
                }),
            )));
    }
    fn tick(&self) {
        self.idle_ticks.set(self.idle_ticks.get() + 1);
        if self.idle_ticks.get() >= LIVE_TICKER_MAX_IDLE_TICKS {
            self.stop_live_ticker();
            return;
        }
        (self.invalidate)();
    }
    fn rendered(self: &std::rc::Rc<Self>) {
        self.idle_ticks.set(0);
        if self.live.get() {
            self.arm_ticker();
        }
    }
}

impl Drop for LiveTicker {
    fn drop(&mut self) {
        self.stop_live_ticker();
    }
}

fn has_live_cell(details: &Value) -> bool {
    details["cells"].as_array().is_some_and(|cells| {
        cells.iter().any(|cell| {
            matches!(cell["status"].as_str(), Some("pending" | "running"))
                && !cell["startedAt"].is_null()
        })
    })
}

impl maho_tui::tui::Component for EvalCard {
    fn dispose(&mut self) {
        self.ticker.stop_live_ticker();
    }
    fn render(&mut self, width: usize) -> Vec<String> {
        self.ticker.rendered();
        let context = EvalRenderContext {
            expanded: self.expanded,
            has_result: self.has_result,
            is_partial: self.is_partial,
            is_error: self.is_error,
            show_images: self.show_images,
            image_protocol: self.image_protocol.clone(),
            spinner_frame: self.spinner_frame,
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0),
            theme: self.theme.clone(),
            cwd: self.cwd.clone(),
        };
        let request = super::eval_request::parse_eval_request(&self.args);
        match (&self.result, request) {
            (Some(result), Ok(request)) => render_eval_result(result, &request, &context, width),
            (Some(result), Err(_)) => {
                render_eval_result(result, &EvalToolRequest::List, &context, width)
            }
            (None, Ok(request)) => render_eval_call(&request, &context, width),
            // Streaming arguments are incomplete until the model finishes the call.
            (None, Err(_)) => {
                if context.has_result {
                    vec![]
                } else {
                    visual(&style(context.theme.as_ref(), "toolTitle", "eval"), width)
                }
            }
        }
    }
}

fn eval_card(
    args: &Value,
    result: Option<&AgentToolResult>,
    theme: &Theme,
    context: &maho_ext_api::ToolRenderContext<(), Value>,
) -> Box<dyn maho_tui::tui::Component> {
    // A host theme that exports no colors (print/json modes) renders like senpi's `theme === undefined`.
    let theme = (!theme.colors.is_empty()).then(|| theme.clone());
    let live = result.is_some_and(|result| {
        result
            .details
            .get("action")
            .is_none_or(|action| action.is_null())
            && has_live_cell(&result.details)
    });
    let ticker = LiveTicker::new(live, context.invalidate.clone());
    Box::new(EvalCard {
        ticker,
        args: args.clone(),
        result: result.cloned(),
        expanded: context.expanded,
        has_result: context.has_result.unwrap_or(result.is_some()),
        is_partial: context.is_partial,
        is_error: context.is_error,
        show_images: context.show_images,
        image_protocol: context
            .image_protocol
            .map(|protocol| format!("{protocol:?}")),
        spinner_frame: context.spinner_frame,
        theme,
        cwd: context.cwd.clone(),
    })
}

/// senpi registers the eval tool with its renderCall/renderResult pair; without it the host draws
/// the generic JSON-argument fallback card.
pub fn eval_tool_renderers() -> maho_ext_api::ToolRenderers<(), Value> {
    maho_ext_api::ToolRenderers {
        render_call: Some(std::sync::Arc::new(|args, theme, context| {
            eval_card(args, None, theme, context)
        })),
        render_result: Some(std::sync::Arc::new(|result, options, theme, context| {
            context.expanded = options.expanded;
            context.is_partial = options.is_partial;
            let args = context.args.clone();
            eval_card(&args, Some(result), theme, context)
        })),
    }
}
// Port of the pinned render split (render-cell.ts, render-live.ts, render-tail.ts and
// live-headline.ts): a settled cell collapses to one line, a live cell keeps a fixed-height
// frame, and a detached cell keeps a collapsed frame while it has output or status history.
const CODE_PREVIEW_LINES: usize = 4;
const OUTPUT_PREVIEW_LINES: usize = 8;
const LIVE_BODY_ROWS: usize = 6;
const LIVE_TAIL_ROWS: usize = 3;
const LIVE_RENDER_TICK_MS: f64 = 100.0;
const HEADLINE_FLOOR_CELLS: usize = 12;
const LIVE_LINE_PREFIX: (&str, &str, &str) = ("╶─ ", "   ", "borderAccent");
const FRAME_HEADER_PREFIX: (&str, &str, &str) = ("╭─ ", "│  ", "borderAccent");
const FRAME_INNER_PREFIX: (&str, &str, &str) = ("│ ", "│ ", "borderMuted");
const FRAME_SECTION_PREFIX: (&str, &str, &str) = ("├─ ", "│  ", "dim");
const MUTED_INNER_PREFIX: (&str, &str, &str) = ("│ ", "│ ", "muted");

struct RenderEnv<'a> {
    expanded: bool,
    theme: Option<&'a Theme>,
    spinner_frame: Option<usize>,
    width: usize,
    now: f64,
}

#[derive(Default)]
struct CellBadges {
    reset: bool,
    timeout: Option<f64>,
    /// `(calls, wallDurationMs)`.
    throughput: Option<(f64, Option<f64>)>,
    /// The call lane still streaming its arguments: the header names that state instead of "running".
    streaming: bool,
}

fn spinner(frame: usize) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[frame % FRAMES.len()]
}
fn render_prefixed(text: &str, env: &RenderEnv<'_>, prefix: (&str, &str, &str)) -> Vec<String> {
    prefixed(text, env.width, prefix.0, prefix.1, prefix.2, env.theme)
}
fn width_of(text: &str) -> usize {
    maho_tui::utils::visible_width(text)
}
/// The longest grapheme prefix of `text` that fits `cells` screen cells.
fn cell_prefix_text(text: &str, cells: usize) -> String {
    let mut kept = String::new();
    let mut used = 0;
    for segment in maho_tui::utils::graphemes(text) {
        let width = width_of(segment);
        if used + width > cells {
            break;
        }
        kept.push_str(segment);
        used += width;
    }
    kept
}
fn leads_with_headline(status: &str) -> bool {
    matches!(status, "pending" | "queued" | "running" | "detached")
}
fn is_live_cell_status(status: &str) -> bool {
    matches!(status, "pending" | "running")
}
/// live-headline.ts `liveHeadline(summary, code, undefined)`: the sanitized summary, else the first
/// code line with content once sanitized, else an ellipsis.
fn live_headline(summary: Option<&str>, code: &str) -> String {
    let summary = summary
        .map(crate::host_sdk::sanitize_terminal_label)
        .filter(|summary| !summary.is_empty());
    summary
        .or_else(|| {
            code.split('\n')
                .map(crate::host_sdk::sanitize_terminal_label)
                .find(|line| !line.is_empty())
        })
        .unwrap_or_else(|| "…".into())
}
fn headlined(
    icon: &str,
    summary: Option<&str>,
    code: &str,
    rest: &str,
    env: &RenderEnv<'_>,
    badgeless_rest: Option<&str>,
    protected_tail: Option<&str>,
) -> String {
    let headline = live_headline(summary, code);
    if env.expanded {
        return format!(
            "{icon} {headline} · {rest}{}",
            protected_tail.map_or_else(String::new, |tail| format!(" · {tail}"))
        );
    }
    let width = env.width.saturating_sub(3).max(1);
    let full_tail =
        protected_tail.map_or_else(|| rest.to_owned(), |tail| format!("{rest} · {tail}"));
    if let Some(badgeless) = badgeless_rest
        && badgeless != rest
        && width_of(&format!("{icon} {headline} · {full_tail}")) > width
    {
        return fit_one_line(icon, &headline, badgeless, width, protected_tail);
    }
    fit_one_line(icon, &headline, rest, width, protected_tail)
}
fn fit_one_line(
    icon: &str,
    headline: &str,
    rest: &str,
    width: usize,
    protected_tail: Option<&str>,
) -> String {
    let lead = format!("{icon} ");
    let middle = " · ";
    let tail = protected_tail.map_or_else(String::new, |tail| format!("{middle}{tail}"));
    let full = format!("{lead}{headline}{middle}{rest}{tail}");
    if width_of(&full) <= width {
        return full;
    }
    let mut kept_rest = rest;
    while let Some(cut) = kept_rest.rfind(middle).filter(|cut| *cut > 0) {
        let candidate = &kept_rest[..cut];
        let line = format!("{lead}{headline}{middle}{candidate}{tail}");
        if width_of(&line) <= width {
            return line;
        }
        kept_rest = candidate;
    }
    let floor_budget =
        (width as i64 - width_of(&format!("{lead}{middle}{kept_rest}{tail}")) as i64 - 1).max(4)
            as usize;
    let floor = HEADLINE_FLOOR_CELLS.max(floor_budget);
    let mut cut_headline = headline.to_owned();
    if width_of(headline) > floor {
        let candidate = format!("{}…", cell_prefix_text(headline, floor - 1));
        if width_of(&format!("{lead}{candidate}{middle}{kept_rest}{tail}")) <= width {
            cut_headline = candidate;
        }
    }
    let line = format!("{lead}{cut_headline}{middle}{kept_rest}{tail}");
    if width_of(&line) <= width {
        return line;
    }
    let line = format!("{lead}{cut_headline}{middle}{kept_rest}");
    if width_of(&line) <= width {
        return line;
    }
    let budget =
        (width as i64 - width_of(&format!("{lead}{middle}{kept_rest}")) as i64 - 1).max(4) as usize;
    let shortened = if width_of(&cut_headline) <= budget {
        cut_headline
    } else {
        format!("{}…", cell_prefix_text(&cut_headline, budget - 1))
    };
    format!("{lead}{shortened}{middle}{kept_rest}")
}
fn cell_elapsed_ms(cell: &Value, env: &RenderEnv<'_>) -> Option<f64> {
    match cell["startedAt"].as_f64() {
        Some(started) if is_live_cell_status(text(cell, "status")) => {
            Some((env.now - started).max(0.0))
        }
        _ => cell["durationMs"].as_f64(),
    }
}
fn throughput_badge((calls, wall): (f64, Option<f64>)) -> Option<String> {
    throughput(&serde_json::json!({"toolCallCount": calls, "wallDurationMs": wall}))
}
fn timeout_badge(timeout: f64) -> String {
    format!("timeout {timeout}s")
}
fn cell_header(cell: &Value, env: &RenderEnv<'_>, badges: &CellBadges) -> String {
    let status = text(cell, "status");
    let elapsed = cell_elapsed_ms(cell, env);
    let frame = env
        .spinner_frame
        .unwrap_or_else(|| (elapsed.unwrap_or(0.0) / LIVE_RENDER_TICK_MS).floor() as usize);
    let (label, icon, color) = if badges.streaming {
        ("streaming", spinner(frame), "warning")
    } else {
        presentation(status, Some(frame))
    };
    let language = text(cell, "language");
    let leads = leads_with_headline(status);
    let base = if leads {
        format!("eval {language} {label}")
    } else {
        format!("eval {language} {label} {icon}")
    };
    let mut segments = vec![];
    match cell["queuedBehind"].as_array() {
        Some(queued) if !queued.is_empty() => segments.push(format!(
            "queued behind {}",
            queued
                .iter()
                .filter_map(Value::as_str)
                .map(crate::host_sdk::sanitize_terminal_label)
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Some(_) if status == "queued" => {
            segments.push(format!("waiting for the {language} kernel to be ready"))
        }
        _ => {}
    }
    if let Some(badge) = badges.throughput.and_then(throughput_badge) {
        segments.push(badge);
    }
    let runtime = runtime_badge(cell);
    if !runtime.is_empty() {
        segments.push(runtime.trim_start().to_owned());
    }
    if badges.reset {
        segments.push("reset".into());
    }
    if let Some(timeout) = badges.timeout {
        segments.push(timeout_badge(timeout));
    }
    let elapsed = badges.throughput.and_then(|(_, wall)| wall).or(elapsed);
    let protected_tail = elapsed.map(format_duration);
    let header = if leads {
        headlined(
            icon,
            cell["summary"].as_str(),
            text(cell, "code"),
            &join_segments(&base, &segments),
            env,
            None,
            protected_tail.as_deref(),
        )
    } else {
        segments.extend(protected_tail);
        join_segments(&base, &segments)
    };
    style(env.theme, color, &header)
}
fn join_segments(base: &str, segments: &[String]) -> String {
    if segments.is_empty() {
        base.to_owned()
    } else {
        format!("{base} · {}", segments.join(" · "))
    }
}
fn render_cell(cell: &Value, env: &RenderEnv<'_>, badges: &CellBadges) -> Vec<String> {
    let status = text(cell, "status");
    let mut lines = match status {
        "complete" | "error" | "cancelled" if !env.expanded => {
            render_terminal_cell_line(cell, env, badges)
        }
        "detached" if !env.expanded => render_detached_cell_frame(cell, env, badges),
        "complete" | "error" | "cancelled" | "detached" => {
            render_expanded_cell_frame(cell, env, badges, false)
        }
        _ if env.expanded => render_expanded_cell_frame(cell, env, badges, false),
        _ => render_live_cell_frame(cell, env, badges),
    };
    if let Some(events) = cell["statusEvents"].as_array()
        && events.iter().any(|event| text(event, "op") == "agent")
    {
        lines.extend(agent_lines(events, env.theme, env.spinner_frame, env.width));
    }
    lines
}
fn render_terminal_cell_line(
    cell: &Value,
    env: &RenderEnv<'_>,
    badges: &CellBadges,
) -> Vec<String> {
    let (label, icon, color) = presentation(text(cell, "status"), env.spinner_frame);
    let runtime = runtime_badge(cell);
    let mut segments = vec![];
    if let Some(badge) = badges.throughput.and_then(throughput_badge) {
        segments.push(badge);
    }
    if let Some(elapsed) = badges
        .throughput
        .and_then(|(_, wall)| wall)
        .or_else(|| cell_elapsed_ms(cell, env))
    {
        segments.push(format_duration(elapsed));
    }
    if badges.reset {
        segments.push("reset".into());
    }
    if let Some(timeout) = badges.timeout {
        segments.push(timeout_badge(timeout));
    }
    let tail = if segments.is_empty() {
        String::new()
    } else {
        format!(" · {}", segments.join(" · "))
    };
    let language = text(cell, "language");
    let rest = format!("eval {language}{runtime} {label}{tail}");
    let badgeless = format!("eval {language} {label}{tail}");
    let line = headlined(
        icon,
        cell["summary"].as_str(),
        text(cell, "code"),
        &rest,
        env,
        Some(if runtime.is_empty() {
            rest.as_str()
        } else {
            badgeless.as_str()
        }),
        None,
    );
    render_prefixed(&style(env.theme, color, &line), env, LIVE_LINE_PREFIX)
}
fn non_agent_events(cell: &Value) -> Vec<Value> {
    cell["statusEvents"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter(|event| text(event, "op") != "agent")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}
fn render_detached_cell_frame(
    cell: &Value,
    env: &RenderEnv<'_>,
    badges: &CellBadges,
) -> Vec<String> {
    if text(cell, "output").trim().is_empty() && non_agent_events(cell).is_empty() {
        return render_prefixed(&cell_header(cell, env, badges), env, LIVE_LINE_PREFIX);
    }
    render_expanded_cell_frame(cell, env, badges, true)
}
fn render_expanded_cell_frame(
    cell: &Value,
    env: &RenderEnv<'_>,
    badges: &CellBadges,
    collapsed_previews: bool,
) -> Vec<String> {
    let collapsed = collapsed_previews && !env.expanded;
    let mut lines = render_prefixed(&cell_header(cell, env, badges), env, FRAME_HEADER_PREFIX);
    if let Some(value) = cell["summary"].as_str()
        && !leads_with_headline(text(cell, "status"))
    {
        lines.extend(
            summary(value, env.width.saturating_sub(2).max(1), env.expanded)
                .into_iter()
                .map(|line| {
                    format!(
                        "{}{}",
                        style(env.theme, "muted", "│ "),
                        style(env.theme, "muted", &line)
                    )
                }),
        );
    }
    let inner_width = env.width.saturating_sub(2).max(1);
    let code = highlighted_code(
        text(cell, "code"),
        parse_language(&cell["language"]),
        env.theme,
    );
    let (code_lines, skipped) = preview_text(
        &code,
        if collapsed {
            CODE_PREVIEW_LINES
        } else {
            usize::MAX
        },
        inner_width,
    );
    if skipped > 0 {
        lines.extend(render_prefixed(
            &format!("{skipped} earlier code lines"),
            env,
            MUTED_INNER_PREFIX,
        ));
    }
    for line in code_lines {
        lines.extend(render_prefixed(&line, env, FRAME_INNER_PREFIX));
    }
    lines.extend(cell_output_section(
        cell,
        env,
        if collapsed {
            OUTPUT_PREVIEW_LINES
        } else {
            usize::MAX
        },
        None,
    ));
    lines.extend(cell_status_section(cell, env, None));
    lines.push(style(env.theme, "borderMuted", "╰─"));
    lines
}
fn render_live_cell_frame(cell: &Value, env: &RenderEnv<'_>, badges: &CellBadges) -> Vec<String> {
    let mut lines = render_prefixed(&cell_header(cell, env, badges), env, FRAME_HEADER_PREFIX);
    let mut tail = vec![];
    if !text(cell, "output").trim_end().is_empty() {
        tail = cell_output_section(cell, env, LIVE_TAIL_ROWS, Some(LIVE_TAIL_ROWS));
    } else if !non_agent_events(cell).is_empty() {
        tail = cell_status_section(cell, env, Some(LIVE_TAIL_ROWS));
    }
    let code_rows = if tail.is_empty() {
        LIVE_BODY_ROWS
    } else {
        LIVE_BODY_ROWS - LIVE_TAIL_ROWS
    };
    lines.extend(live_code_window(cell, env, code_rows));
    lines.extend(tail);
    lines.push(style(env.theme, "borderMuted", "╰─"));
    lines
}
static TERMINAL_ESCAPE_SEQUENCE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(
    || {
        regex::Regex::new(r"(?:\x1B\][\s\S]*?(?:\x07|\x1B\\|\x{9C}))|[\x1B\x{9B}][\[\]()#;?]*(?:\d{1,4}(?:[;:]\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~]")
        .expect("static terminal escape pattern")
    },
);
static TERMINAL_CONTROL_RUN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"[\x00-\x08\x0B-\x1F\x7F-\x{9F}]+").expect("static control pattern")
});
/// Streamed code is not trusted input: escape sequences and control characters are stripped, tabs
/// become two spaces, and other whitespace is kept.
fn sanitize_cell_code(code: &str) -> String {
    code.split('\n')
        .map(|line| {
            let line = TERMINAL_ESCAPE_SEQUENCE.replace_all(line, "");
            TERMINAL_CONTROL_RUN
                .replace_all(&line, "")
                .replace('\t', "  ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn live_code_window(cell: &Value, env: &RenderEnv<'_>, window_rows: usize) -> Vec<String> {
    let inner_width = env.width.saturating_sub(2).max(1);
    let code = highlighted_code(
        &sanitize_cell_code(text(cell, "code")),
        parse_language(&cell["language"]),
        env.theme,
    );
    let rows = visual(&code, inner_width);
    let fits = rows.len() <= window_rows;
    let budget = if fits { window_rows } else { window_rows - 1 };
    let kept = &rows[rows.len().saturating_sub(budget)..];
    let skipped_visual = rows.len() - kept.len();
    let mut window = vec![];
    if !fits {
        let hidden = hidden_source_lines(&code, inner_width, skipped_visual);
        window.extend(render_prefixed(
            &format!("{hidden} earlier code lines"),
            env,
            MUTED_INNER_PREFIX,
        ));
    }
    for line in kept {
        window.extend(render_prefixed(line, env, FRAME_INNER_PREFIX));
    }
    while window.len() < window_rows {
        window.push(style(env.theme, "borderMuted", "│ "));
    }
    window
}
/// Source lines whose first visual row lies above the first shown row.
fn hidden_source_lines(code: &str, inner_width: usize, skipped_visual: usize) -> usize {
    let mut consumed = 0;
    let mut hidden = 0;
    for line in code.split('\n') {
        if consumed >= skipped_visual {
            break;
        }
        hidden += 1;
        consumed += visual(line, inner_width).len().max(1);
    }
    hidden
}
fn cell_output_section(
    cell: &Value,
    env: &RenderEnv<'_>,
    max_lines: usize,
    row_budget: Option<usize>,
) -> Vec<String> {
    let output = text(cell, "output").trim_end();
    if output.is_empty() {
        return vec![];
    }
    let mut lines = render_prefixed("output", env, FRAME_SECTION_PREFIX);
    let color = if text(cell, "status") == "error" {
        "error"
    } else {
        "toolOutput"
    };
    let styled = output
        .split('\n')
        .map(|line| style(env.theme, color, line))
        .collect::<Vec<_>>()
        .join("\n");
    let inner_width = env.width.saturating_sub(2).max(1);
    if let Some(row_budget) = row_budget {
        let body_rows = row_budget - 1;
        let (preview, skipped) = preview_text(&styled, body_rows, inner_width);
        let kept = if skipped > 0 {
            &preview[preview.len().saturating_sub(body_rows - 1)..]
        } else {
            &preview[..]
        };
        let omitted = skipped + preview.len() - kept.len();
        if omitted > 0 {
            lines.extend(render_prefixed(
                &format!("{omitted} earlier output lines"),
                env,
                MUTED_INNER_PREFIX,
            ));
        }
        for line in kept {
            lines.extend(render_prefixed(line, env, FRAME_INNER_PREFIX));
        }
        while lines.len() < row_budget {
            lines.push(style(env.theme, "borderMuted", "│ "));
        }
        lines.truncate(row_budget);
        return lines;
    }
    let (preview, skipped) = preview_text(&styled, max_lines, inner_width);
    if skipped > 0 {
        lines.extend(render_prefixed(
            &format!("{skipped} earlier output lines"),
            env,
            MUTED_INNER_PREFIX,
        ));
    }
    for line in preview {
        lines.extend(render_prefixed(&line, env, FRAME_INNER_PREFIX));
    }
    lines
}
fn cell_status_section(
    cell: &Value,
    env: &RenderEnv<'_>,
    row_budget: Option<usize>,
) -> Vec<String> {
    let events = non_agent_events(cell);
    if events.is_empty() {
        return vec![];
    }
    let mut lines = render_prefixed("status", env, FRAME_SECTION_PREFIX);
    let Some(row_budget) = row_budget else {
        for line in status_lines(&events, env.expanded, env.theme) {
            lines.extend(render_prefixed(&line, env, FRAME_INNER_PREFIX));
        }
        return lines;
    };
    let omitted_by_bound = events
        .first()
        .filter(|event| text(event, "op") == "status-events-omitted")
        .and_then(|event| event["count"].as_f64())
        .unwrap_or(0.0);
    let visible = if omitted_by_bound > 0.0 {
        &events[1..]
    } else {
        &events[..]
    };
    let body_rows = row_budget - 1;
    let per_event: Vec<Vec<String>> = visible
        .iter()
        .map(|event| {
            status_lines(std::slice::from_ref(event), true, env.theme)
                .iter()
                .flat_map(|line| render_prefixed(line, env, FRAME_INNER_PREFIX))
                .collect()
        })
        .collect();
    let mut kept_events: Vec<&Vec<String>> = vec![];
    let mut used = 0;
    let mut first_shown = per_event.len();
    for (index, rows) in per_event.iter().enumerate().rev() {
        let marker = usize::from(index > 0 || omitted_by_bound > 0.0);
        if used + rows.len() + marker <= body_rows || kept_events.is_empty() {
            kept_events.insert(0, rows);
            used += rows.len();
            first_shown = index;
        } else {
            break;
        }
    }
    let skipped = first_shown as f64 + omitted_by_bound;
    let event_rows: Vec<String> = kept_events.into_iter().flatten().cloned().collect();
    let needs_marker = skipped > 0.0 || event_rows.len() > body_rows;
    let shown_budget = body_rows.saturating_sub(usize::from(needs_marker));
    let cut_rows = event_rows.len().saturating_sub(shown_budget);
    let mut body = vec![];
    if needs_marker {
        body.push(status_marker(skipped, cut_rows, env));
    }
    body.extend(event_rows.into_iter().skip(cut_rows));
    lines.extend(body.into_iter().take(body_rows));
    while lines.len() < row_budget {
        lines.push(style(env.theme, "borderMuted", "│ "));
    }
    lines
}
/// The one-row fold marker: the longest wording that fits the width wins.
fn status_marker(skipped: f64, cut_rows: usize, env: &RenderEnv<'_>) -> String {
    let candidates = if cut_rows == 0 {
        vec![
            format!("├ … {skipped} earlier status events"),
            format!("├ … {skipped} earlier"),
            format!("├ … {skipped}"),
        ]
    } else if skipped == 0.0 {
        vec![
            format!("├ … {cut_rows} earlier rows of this event"),
            format!("├ … {cut_rows} earlier rows"),
            format!("├ … {cut_rows} rows"),
        ]
    } else {
        vec![
            format!("├ … {skipped} earlier status events, {cut_rows} rows"),
            format!("├ … {skipped} events, {cut_rows} rows"),
            format!("├ … {skipped} ev, {cut_rows} rows"),
            format!("├ … {skipped}ev {cut_rows}r"),
        ]
    };
    let inner_width = env
        .width
        .saturating_sub(width_of(FRAME_INNER_PREFIX.0))
        .max(1);
    let text = candidates
        .iter()
        .find(|candidate| width_of(candidate) <= inner_width)
        .or(candidates.last())
        .cloned()
        .unwrap_or_default();
    prefixed(&text, env.width, "│ ", "│ ", "dim", env.theme)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn render_env(context: &EvalRenderContext, width: usize) -> RenderEnv<'_> {
    RenderEnv {
        expanded: context.expanded,
        theme: context.theme.as_ref(),
        spinner_frame: context.spinner_frame,
        width,
        now: context.now,
    }
}

pub fn render_eval_call(
    args: &EvalToolRequest,
    context: &EvalRenderContext,
    width: usize,
) -> Vec<String> {
    let theme = context.theme.as_ref();
    if context.has_result {
        return vec![];
    }
    let input = match args {
        EvalToolRequest::List => return visual(&style(theme, "toolTitle", "eval list"), width),
        EvalToolRequest::Peek { cell_id } => {
            return visual(
                &style(theme, "toolTitle", &format!("eval peek {cell_id}")),
                width,
            );
        }
        EvalToolRequest::Stop { cell_id } => {
            return visual(
                &style(theme, "toolTitle", &format!("eval stop {cell_id}")),
                width,
            );
        }
        EvalToolRequest::Run(input) => input,
    };
    let normalized = normalize_eval_summary(&Value::String(input.summary.clone()));
    if theme.is_some() || context.spinner_frame.is_some() {
        // The host never supplies a spinner frame for an eval call, so the call lane itself is the
        // state: run args are present and no result exists yet.
        let cell = serde_json::json!({"language":language(input.language),"code":input.code,"summary":normalized,"status":"running","output":""});
        let badges = CellBadges {
            reset: input.reset == Some(true),
            timeout: input.timeout,
            throughput: None,
            streaming: true,
        };
        return render_cell(&cell, &render_env(context, width), &badges);
    }
    let mut title = format!("eval {}", language(input.language));
    if input.reset == Some(true) {
        title.push_str(" reset");
    }
    if let Some(timeout) = input.timeout {
        title.push_str(&format!(" timeout {timeout}s"));
    }
    let mut lines = visual(&title, width);
    if let Some(normalized) = normalized {
        lines.extend(summary(&normalized, width, context.expanded));
    }
    let code = if input.code.trim().is_empty() {
        "...".into()
    } else {
        display_code(&input.code, input.language)
    };
    lines.extend(preview(
        &code,
        width,
        if context.expanded { usize::MAX } else { 4 },
        "code",
        theme,
    ));
    lines
}

fn render_json_outputs(values: &[Value], context: &EvalRenderContext, width: usize) -> Vec<String> {
    let theme = context.theme.as_ref();
    let mut lines = vec![];
    for (index, value) in values.iter().enumerate() {
        lines.extend(visual(
            &style(theme, "dim", &format!("display[{}]", index + 1)),
            width,
        ));
        lines.extend(render_json_entry_lines(value, context, width));
    }
    lines
}

/// render-detail.ts `renderJsonEntryLines`: legacy stored entries (Buffer dumps, oversized values)
/// collapse to a bounded placeholder or summary instead of walking the whole value.
fn render_json_entry_lines(
    value: &Value,
    context: &EvalRenderContext,
    width: usize,
) -> Vec<String> {
    use super::display_json_size::{
        MAX_DISPLAY_JSON_BYTES, buffer_placeholder, display_json_size_exceeds, format_byte_length,
        serialized_buffer_byte_length,
    };
    use super::json_tree::format_scalar;
    let theme = context.theme.as_ref();
    let (depth, line_cap, scalar_len) = if context.expanded {
        (6, 200, 2000)
    } else {
        (2, 6, 60)
    };
    let fallback = |text: &str| visual(&style(theme, "dim", text), width);
    if let Some(bytes) = serialized_buffer_byte_length(value) {
        return fallback(&buffer_placeholder(bytes));
    }
    if let Some(record) = value.as_object()
        && let Some((key, bytes)) = record
            .iter()
            .find_map(|(key, item)| serialized_buffer_byte_length(item).map(|bytes| (key, bytes)))
    {
        let mut lines = fallback(&format!("{key}: {}", buffer_placeholder(bytes)));
        let others: Vec<_> = record.iter().filter(|(other, _)| *other != key).collect();
        let shown = line_cap.saturating_sub(1).min(others.len());
        for (other, item) in &others[..shown] {
            lines.extend(fallback(&format!(
                "  {other}: {}",
                format_scalar(item, scalar_len)
            )));
        }
        if others.len() > shown {
            lines.extend(fallback(&format!("  … {} more keys", others.len() - shown)));
        }
        return lines;
    }
    if display_json_size_exceeds(value, MAX_DISPLAY_JSON_BYTES) {
        return fallback(&format!(
            "… oversized display value elided ({}, ~{}+ stored)",
            format_scalar(value, scalar_len),
            format_byte_length(MAX_DISPLAY_JSON_BYTES)
        ));
    }
    let tree =
        super::json_tree::render_json_tree_lines_themed(value, theme, depth, line_cap, scalar_len);
    let mut lines = vec![];
    for line in tree.lines {
        lines.extend(visual(&line, width));
    }
    if tree.truncated {
        lines.extend(visual(&style(theme, "dim", "…"), width));
    }
    lines
}

fn legacy_tool_call_row(call: &Value, context: &EvalRenderContext, width: usize) -> Vec<String> {
    let theme = context.theme.as_ref();
    let ok = call["ok"] == true;
    let color = if ok { "success" } else { "error" };
    let title = format!(
        "- tool.{}: {}",
        text(call, "name"),
        if ok { "ok" } else { "error" }
    );
    let Some(error) = call["error"].as_str() else {
        return visual(&style(theme, color, &title), width);
    };
    if context.expanded {
        return visual(&style(theme, color, &format!("{title} ({error})")), width);
    }
    let guarded = code_point_prefix(error, TOOL_CALL_COLLAPSED_ERROR_CODE_POINTS);
    let full = visual(&style(theme, color, &format!("{title} ({guarded})")), width);
    if guarded == error && full.len() <= TOOL_CALL_COLLAPSED_VISUAL_LINES {
        return full;
    }
    let marker = visual(&style(theme, "muted", "[tool error omitted]"), width);
    let mut row: Vec<_> = visual(&style(theme, color, &title), width)
        .into_iter()
        .take(
            TOOL_CALL_COLLAPSED_VISUAL_LINES
                .saturating_sub(marker.len())
                .max(1),
        )
        .collect();
    row.extend(
        visual(&style(theme, color, &format!("  ({guarded})")), width)
            .into_iter()
            .take(TOOL_CALL_COLLAPSED_VISUAL_LINES.saturating_sub(row.len() + marker.len())),
    );
    row.extend(
        marker
            .into_iter()
            .take(TOOL_CALL_COLLAPSED_VISUAL_LINES.saturating_sub(row.len())),
    );
    row
}

/// senpi `nestedToolCallBlock` over the legacy `toolCallRows`: with a theme and at least one call
/// carrying captured args, calls with args draw the nested widget; the rest keep the legacy row.
fn tool_call_lines(calls: &[Value], context: &EvalRenderContext, width: usize) -> Vec<String> {
    let theme = context.theme.as_ref();
    let skipped = if context.expanded {
        0
    } else {
        calls.len().saturating_sub(TOOL_CALL_PREVIEW_COUNT)
    };
    let mut lines = if skipped > 0 {
        visual(
            &style(
                theme,
                "muted",
                &format!(
                    "{skipped} earlier tool {}",
                    if skipped == 1 { "call" } else { "calls" }
                ),
            ),
            width,
        )
    } else {
        vec![]
    };
    let nested = theme.is_some() && calls.iter().any(|call| call.get("args").is_some());
    for call in &calls[skipped..] {
        if nested && call.get("args").is_some() {
            lines.extend(render_tool_call_widget(
                call,
                &WidgetOptions {
                    cwd: &context.cwd,
                    theme,
                    expanded: context.expanded,
                    width,
                },
            ));
        } else {
            lines.extend(legacy_tool_call_row(call, context, width));
        }
    }
    lines
}

pub fn render_eval_result(
    result: &AgentToolResult,
    args: &EvalToolRequest,
    context: &EvalRenderContext,
    width: usize,
) -> Vec<String> {
    let theme = context.theme.as_ref();
    let details = &result.details;
    let fallback = context.show_images && context.image_protocol.is_none();
    let output = result
        .content
        .iter()
        .filter_map(|part| match part {
            ContentBlock::Text(part)
                if part.audience != Some(maho_ai::types::TextAudience::Model) =>
            {
                Some(part.text.clone())
            }
            ContentBlock::Image(part) if fallback => Some(format!(
                "[image: {}]",
                crate::host_sdk::sanitize_terminal_label(&part.mime_type)
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if details.get("action").is_some() {
        let mut lines = visual(&style(theme, "toolTitle", "eval list"), width);
        lines.extend(visual(&style(theme, "toolOutput", &output), width));
        return lines;
    }
    let mut lines = vec![];
    let json_outputs = details["jsonOutputs"].as_array().filter(|v| !v.is_empty());
    if let Some(cells) = details["cells"]
        .as_array()
        .filter(|cells| !cells.is_empty())
    {
        let env = render_env(context, width);
        let run = match args {
            EvalToolRequest::Run(input) => Some(input),
            _ => None,
        };
        for (index, cell) in cells.iter().enumerate() {
            let throughput =
                (!context.is_partial && cells.len() == 1 && text(cell, "status") == "complete")
                    .then(|| details["toolCallCount"].as_f64())
                    .flatten()
                    .map(|calls| (calls, details["wallDurationMs"].as_f64()));
            let badges = CellBadges {
                reset: index == 0 && run.is_some_and(|run| run.reset == Some(true)),
                timeout: if index == 0 {
                    run.and_then(|run| run.timeout)
                } else {
                    None
                },
                throughput,
                streaming: false,
            };
            lines.extend(render_cell(cell, &env, &badges));
            if index + 1 < cells.len() {
                lines.push(String::new());
            }
        }
        if let Some(values) = json_outputs {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.extend(render_json_outputs(values, context, width));
        }
        if fallback {
            for part in &result.content {
                if let ContentBlock::Image(part) = part {
                    if !lines.is_empty() {
                        lines.push(String::new());
                    }
                    lines.extend(visual(
                        &format!(
                            "[image: {}]",
                            crate::host_sdk::sanitize_terminal_label(&part.mime_type)
                        ),
                        width,
                    ));
                }
            }
        }
        if let Some(phase) = details["phase"].as_str() {
            lines.extend(visual(
                &style(theme, "muted", &format!("phase {phase}")),
                width,
            ));
        }
    } else {
        let status = if context.is_error || details["isError"] == true {
            "error"
        } else if context.is_partial {
            "running"
        } else {
            "done"
        };
        let color = match status {
            "running" => "warning",
            "done" => "success",
            _ => "error",
        };
        lines.extend(visual(
            &style(
                theme,
                color,
                &format!(
                    "eval {}{} {status}",
                    details["language"].as_str().unwrap_or("?"),
                    runtime_badge(details)
                ),
            ),
            width,
        ));
        if let Some(value) = details["summary"].as_str() {
            lines.extend(styled_summary(value, width, context.expanded, theme));
        }
        let mut metadata = vec![];
        if let Some(phase) = details["phase"].as_str().filter(|s| !s.is_empty()) {
            metadata.push(format!("phase {phase}"));
        }
        if !context.is_partial
            && let Some(duration) = details["wallDurationMs"]
                .as_f64()
                .or_else(|| details["durationMs"].as_f64())
        {
            metadata.push(format!("took {}", format_duration(duration)));
        }
        if status == "done"
            && let Some(badge) = throughput(details)
        {
            metadata.push(badge);
        }
        if !metadata.is_empty() {
            lines.extend(visual(&style(theme, "muted", &metadata.join(" | ")), width));
        }
        lines.push(String::new());
        if !output.trim_end().is_empty() {
            lines.extend(preview(
                &style(theme, "toolOutput", output.trim_end()),
                width,
                if context.expanded { usize::MAX } else { 8 },
                "output",
                theme,
            ));
        } else if !(context.show_images
            && context.image_protocol.is_some()
            && result
                .content
                .iter()
                .any(|part| matches!(part, ContentBlock::Image(_))))
        {
            lines.extend(visual(&style(theme, "muted", "(no output)"), width));
        }
        if let Some(events) = details["statusEvents"].as_array() {
            let mut status = status_lines(events, context.expanded, theme);
            status.extend(agent_lines(events, theme, context.spinner_frame, width));
            if !status.is_empty() {
                lines.push(String::new());
                for line in status {
                    lines.extend(visual(&line, width));
                }
            }
        }
        if let Some(values) = json_outputs {
            lines.push(String::new());
            lines.extend(render_json_outputs(values, context, width));
        }
    }
    if let Some(calls) = details["toolCalls"]
        .as_array()
        .filter(|calls| !calls.is_empty())
    {
        lines.push(String::new());
        lines.extend(tool_call_lines(calls, context, width));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn theme() -> Theme {
        let colors = [
            "toolTitle",
            "muted",
            "dim",
            "error",
            "success",
            "warning",
            "borderAccent",
            "borderMuted",
            "toolOutput",
            "mdCodeBlock",
            "accent",
        ]
        .iter()
        .enumerate()
        .map(|(index, key)| ((*key).to_owned(), format!("\x1b[38;5;{}m", index + 1)))
        .collect();
        Theme {
            colors,
            ..Default::default()
        }
    }

    #[test]
    fn themed_result_colors_header_and_legacy_rows() {
        let mut result = AgentToolResult::text("hi");
        result.details = json!({"language":"js","toolCalls":[{"name":"x","ok":true}]});
        let theme = theme();
        let context = EvalRenderContext {
            theme: Some(theme.clone()),
            ..Default::default()
        };
        let lines = render_eval_result(&result, &EvalToolRequest::List, &context, 80);
        assert!(lines[0].contains(&theme.fg("success", "eval js done")));
        assert!(
            lines
                .last()
                .is_some_and(|line| line.contains(&theme.fg("success", "- tool.x: ok")))
        );
    }

    #[test]
    fn themed_calls_with_args_render_nested_widgets() {
        let mut result = AgentToolResult::text("");
        result.details = json!({"language":"js","toolCalls":[{"name":"custom","ok":true,"args":{"a":1},"durationMs":1500,"resultPreview":"done"}]});
        let theme = theme();
        let context = EvalRenderContext {
            theme: Some(theme.clone()),
            ..Default::default()
        };
        let lines = render_eval_result(&result, &EvalToolRequest::List, &context, 80);
        let tail = &lines[lines.len() - 3..];
        assert_eq!(tail[0], "  tool.custom({\"a\":1})");
        assert!(
            tail[1].starts_with("  ")
                && tail[1].contains(&theme.fg("success", "✓"))
                && tail[1].ends_with(" 1s")
        );
        assert!(tail[2].starts_with("  ") && tail[2].contains(&theme.fg("muted", "done")));
    }

    #[test]
    fn unthemed_calls_with_args_keep_legacy_rows() {
        let mut result = AgentToolResult::text("");
        result.details =
            json!({"language":"js","toolCalls":[{"name":"custom","ok":true,"args":{"a":1}}]});
        let lines = render_eval_result(
            &result,
            &EvalToolRequest::List,
            &EvalRenderContext::default(),
            80,
        );
        assert_eq!(lines.last().map(String::as_str), Some("- tool.custom: ok"));
    }

    #[test]
    fn themed_agent_ids_are_bold() {
        let mut result = AgentToolResult::text("");
        result.details = json!({"language":"js","statusEvents":[{"op":"agent","id":"scout","status":"pending"}]});
        let context = EvalRenderContext {
            theme: Some(theme()),
            ..Default::default()
        };
        let lines = render_eval_result(&result, &EvalToolRequest::List, &context, 80);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("\x1b[1mscout\x1b[22m"))
        );
    }
}
