//! Section data models and the composed panel rows (spec 4-5).

use crate::git::{ChangedFile, FileClass};
use crate::goal::Goal;
use crate::links::{Link, hyperlink};
use crate::settings::Sections;
use crate::tasks::{Status, TaskBoard};
use crate::text::{bar, compact_count, duration, fit_rows, path_tail, sanitize};
use crate::tools::ToolLog;

/// Color class applied to a row segment (mapped to theme colors by the caller).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Heading,
    Normal,
    Dim,
    Error,
    Success,
    Accent,
}

/// Applies a theme color to already sanitized text.
pub type Painter<'a> = &'a dyn Fn(Style, &str) -> String;

/// A painter that leaves text unchanged.
pub fn plain(_: Style, text: &str) -> String {
    text.to_owned()
}

/// Maps a style to a theme color name.
pub fn theme_color(style: Style) -> &'static str {
    match style {
        Style::Heading => "accent",
        Style::Normal => "text",
        Style::Dim => "dim",
        Style::Error => "error",
        Style::Success => "success",
        Style::Accent => "accent",
    }
}

/// Host facts shown in the session and context sections.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionFacts {
    pub model_id: Option<String>,
    pub session_cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_hit_rate: Option<f64>,
    pub elapsed_ms: u64,
    pub context_window: Option<u64>,
    pub context_tokens: Option<u64>,
}

/// One usage window of a provider account (spec 5 Usage).
#[derive(Clone, Debug, PartialEq)]
pub struct UsageWindow {
    pub label: String,
    pub percent: f64,
    pub resets_in: Option<String>,
    pub updated_ago: Option<String>,
    pub error: Option<String>,
}

/// Memory store status (spec 5 Memory).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryInfo {
    pub identity: String,
    pub rows: Vec<String>,
}

/// Everything the panel renders.
#[derive(Clone, Debug, Default)]
pub struct PanelView {
    pub sections: Sections,
    pub clickable: bool,
    pub session: SessionFacts,
    pub goal: Option<Goal>,
    pub usage: Vec<UsageWindow>,
    pub tasks: TaskBoard,
    pub now_ms: u64,
    pub tools: ToolLog,
    /// `None` = not a git repository (no section).
    pub files: Option<Vec<ChangedFile>>,
    pub memory: Option<MemoryInfo>,
    pub location: String,
}

/// Bar width used in rows.
const BAR: usize = 10;

fn text(painter: Painter<'_>, style: Style, raw: &str) -> String {
    painter(style, &sanitize(raw))
}

fn link_row(view: &PanelView, link: Link, row: String) -> String {
    if view.clickable {
        hyperlink(&link, &row)
    } else {
        row
    }
}

fn last_segment(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

fn money(value: f64) -> String {
    format!("${value:.2}")
}

fn session_section(view: &PanelView, p: Painter<'_>) -> Vec<String> {
    let facts = &view.session;
    let spend = view.tasks.spend;
    let cost = facts.session_cost + spend;
    let tokens = facts.input_tokens + facts.output_tokens;
    let mut heading = "SESSION".to_owned();
    if cost != 0.0 || tokens != 0 {
        heading.push_str(&format!(" {} · {}", money(cost), compact_count(tokens)));
    }
    let mut rows = vec![text(p, Style::Heading, &heading)];
    if let Some(model) = &facts.model_id {
        rows.push(text(p, Style::Normal, last_segment(model)));
    }
    rows.push(text(
        p,
        Style::Dim,
        &format!("elapsed {}", duration(facts.elapsed_ms)),
    ));
    if tokens != 0 {
        let mut line = format!(
            "in {} · out {}",
            compact_count(facts.input_tokens),
            compact_count(facts.output_tokens)
        );
        if let Some(rate) = facts.cache_hit_rate {
            line.push_str(&format!(" · cache {:.0}%", rate * 100.0));
        }
        rows.push(text(p, Style::Dim, &line));
    }
    if spend != 0.0 {
        rows.push(text(p, Style::Dim, &format!("tasks {}", money(spend))));
    }
    rows
}

fn goal_section(view: &PanelView, p: Painter<'_>) -> Vec<String> {
    let Some(goal) = &view.goal else {
        return Vec::new();
    };
    let mut heading = format!("GOAL {}", goal.status);
    if let Some(secs) = goal.time_used_seconds.filter(|s| *s > 0.0) {
        heading.push_str(&format!(" · {}", duration((secs * 1000.0) as u64)));
    }
    let mut rows = vec![text(p, Style::Heading, &heading)];
    rows.push(link_row(
        view,
        Link::Goal,
        text(p, Style::Normal, &goal.objective),
    ));
    rows.push(text(
        p,
        Style::Dim,
        &format!("tokens {}", compact_count(goal.tokens_used)),
    ));
    if let Some(pct) = goal.budget_percent() {
        rows.push(text(
            p,
            Style::Normal,
            &format!("{} {pct:.0}%", bar(pct, BAR)),
        ));
    }
    if goal.continuations != 0 {
        rows.push(text(
            p,
            Style::Dim,
            &format!("continuations {}", goal.continuations),
        ));
    }
    rows
}

fn context_section(view: &PanelView, p: Painter<'_>) -> Vec<String> {
    let Some(window) = view.session.context_window.filter(|w| *w > 0) else {
        return Vec::new();
    };
    match view.session.context_tokens {
        Some(used) => {
            let pct = used as f64 * 100.0 / window as f64;
            vec![
                text(
                    p,
                    Style::Heading,
                    &format!("CONTEXT {}/{}", compact_count(used), compact_count(window)),
                ),
                text(p, Style::Normal, &format!("{} {pct:.0}%", bar(pct, BAR))),
            ]
        }
        None => vec![
            text(
                p,
                Style::Heading,
                &format!("CONTEXT ?/{}", compact_count(window)),
            ),
            text(p, Style::Dim, "measuring…"),
        ],
    }
}

fn usage_section(view: &PanelView, p: Painter<'_>) -> Vec<String> {
    if view.usage.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![text(p, Style::Heading, "USAGE")];
    for window in &view.usage {
        rows.push(text(
            p,
            Style::Normal,
            &format!(
                "{} {} {:.0}%",
                window.label,
                bar(window.percent, BAR),
                window.percent
            ),
        ));
        let mut detail = Vec::new();
        if let Some(reset) = &window.resets_in {
            detail.push(format!("resets {reset}"));
        }
        if let Some(ago) = &window.updated_ago {
            detail.push(format!("updated {ago}"));
        }
        if !detail.is_empty() {
            rows.push(text(p, Style::Dim, &detail.join(" · ")));
        }
        if let Some(error) = &window.error {
            rows.push(text(p, Style::Error, error));
        }
    }
    rows
}

fn agents_section(view: &PanelView, p: Painter<'_>) -> Vec<String> {
    let tasks = view.tasks.tasks();
    if tasks.is_empty() {
        return Vec::new();
    }
    let counts = view.tasks.counts();
    let mut rows = vec![text(
        p,
        Style::Heading,
        &format!(
            "AGENTS {} running · {} parked · {} done",
            counts.running, counts.parked, counts.done
        ),
    )];
    for task in tasks {
        let mut line = format!("{} {}", task.status.glyph(), task.name);
        let style = match task.status {
            Status::Parked => {
                if let Some(reason) = task.reason.as_deref().or(Some(task.raw_status.as_str()))
                    && !reason.is_empty()
                {
                    line.push_str(&format!(" · {reason}"));
                }
                Style::Dim
            }
            status => {
                let elapsed = match (task.started_at_ms, task.finished_at_ms) {
                    (Some(start), Some(end)) if !status.is_active() => {
                        Some(end.saturating_sub(start))
                    }
                    (Some(start), _) => Some(view.now_ms.saturating_sub(start)),
                    _ => None,
                };
                if let Some(ms) = elapsed {
                    line.push_str(&format!(" {}", duration(ms)));
                }
                if status == Status::Running
                    && let Some(activity) = &task.activity
                {
                    line.push_str(&format!(" · {activity}"));
                }
                if status == Status::Finished {
                    Style::Success
                } else {
                    Style::Normal
                }
            }
        };
        rows.push(link_row(
            view,
            Link::Agent(task.id.clone()),
            text(p, style, &line),
        ));
    }
    rows
}

fn tools_section(view: &PanelView, p: Painter<'_>) -> Vec<String> {
    if view.tools.is_empty() {
        return Vec::new();
    }
    let mut heading = "TOOLS".to_owned();
    if view.tools.earlier() > 0 {
        heading.push_str(&format!(" +{} earlier", view.tools.earlier()));
    }
    let mut rows = vec![text(p, Style::Heading, &heading)];
    for call in view.tools.shown() {
        let line = match &call.summary {
            Some(arg) => format!("{} {}", call.name, sanitize(arg)),
            None => call.name.clone(),
        };
        rows.push(text(p, Style::Normal, &line));
    }
    rows
}

fn files_section(view: &PanelView, p: Painter<'_>) -> Vec<String> {
    let Some(files) = &view.files else {
        return Vec::new();
    };
    if files.is_empty() {
        return Vec::new();
    }
    let mut heading = format!("FILES {} changed", files.len());
    if files.len() > crate::git::SHOWN {
        heading.push_str(&format!(" · +{} more", files.len() - crate::git::SHOWN));
    }
    let mut rows = vec![text(p, Style::Heading, &heading)];
    for file in files.iter().take(crate::git::SHOWN) {
        let style = match file.class() {
            FileClass::Untracked => Style::Dim,
            FileClass::Conflict => Style::Error,
            FileClass::StagedOnly => Style::Success,
            FileClass::Normal => Style::Normal,
        };
        let line = format!("{} {}{}", file.xy, file.basename(), file.counts_label());
        rows.push(link_row(
            view,
            Link::File(file.path.clone()),
            text(p, style, &line),
        ));
    }
    rows
}

fn memory_section(view: &PanelView, p: Painter<'_>) -> Vec<String> {
    let Some(memory) = &view.memory else {
        return Vec::new();
    };
    let mut rows = vec![link_row(
        view,
        Link::Memory,
        text(p, Style::Heading, &format!("MEMORY {}", memory.identity)),
    )];
    for row in &memory.rows {
        rows.push(text(p, Style::Dim, row));
    }
    rows
}

/// Builds the unfitted rows: enabled non-empty sections in order, one blank row between blocks,
/// then the location row.
pub fn compose(view: &PanelView, width: usize, p: Painter<'_>) -> Vec<String> {
    let s = view.sections;
    let blocks: [(bool, fn(&PanelView, Painter<'_>) -> Vec<String>); 8] = [
        (s.session, session_section),
        (s.goal, goal_section),
        (s.context, context_section),
        (s.usage, usage_section),
        (s.agents, agents_section),
        (s.tools, tools_section),
        (s.files, files_section),
        (s.memory, memory_section),
    ];
    let mut rows = Vec::new();
    for (enabled, build) in blocks {
        if !enabled {
            continue;
        }
        let block = build(view, p);
        if block.is_empty() {
            continue;
        }
        rows.extend(block);
        rows.push(String::new());
    }
    rows.push(text(
        p,
        Style::Dim,
        &path_tail(&sanitize(&view.location), width),
    ));
    rows
}

/// The panel rows fitted to exactly `width` cells.
pub fn render(view: &PanelView, width: usize, p: Painter<'_>) -> Vec<String> {
    fit_rows(&compose(view, width, p), width)
}

/// The location row text: cwd with `$HOME` as `~`, plus ` · <branch>`.
pub fn location(
    cwd: &std::path::Path,
    home: Option<&std::path::Path>,
    branch: Option<&str>,
) -> String {
    let mut out = match home.and_then(|h| cwd.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => cwd.display().to_string(),
    };
    if let Some(branch) = branch {
        out.push_str(&format!(" · {branch}"));
    }
    out
}
