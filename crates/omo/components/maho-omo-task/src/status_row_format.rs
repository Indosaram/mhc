use std::collections::BTreeMap;
use senpi_task::{renderer_text::{excerpt_renderer_text, normalize_renderer_text, optional_renderer_text, renderer_visible_width}, state::{ResidencyState, TaskRecord, TaskRunStats, TaskStatus}, status_line::{StatusTargetInput, TaskIdentityInput, format_live_spend, format_status_target, task_identity_label, tool_count_suffix}};

pub const LIVE_STATUS_REFRESH_MS: u64 = 250;
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn is_terminal(status: TaskStatus) -> bool { status.is_terminal() }
fn is_suspended(record: &TaskRecord) -> bool { matches!(record.residency_state, ResidencyState::PersistedOnly | ResidencyState::RpcDetached) }
fn status_label(record: &TaskRecord) -> &str { if is_suspended(record) { "suspended" } else { record.status.as_str() } }
pub fn task_identity(record: &TaskRecord) -> String {
    task_identity_label(&TaskIdentityInput { task_id: &record.task_id, name: record.name.as_deref(), description: record.description.as_deref(), task_summary: record.task_summary.as_deref() })
}
fn record_status_target(record: &TaskRecord) -> String {
    let target = format_status_target(&StatusTargetInput { category: record.category.as_deref(), agent_type: record.agent_type.as_deref(), resolved_model: record.resolved_model.as_ref(), model: Some(&record.model), fallback_count: record.fallback_attempts.as_ref().map(|items| u64::try_from(items.len()).unwrap_or(u64::MAX)) }).unwrap_or_else(|| "task".into());
    senpi_task::status_line::format_parent_model_fallback(record).map_or_else(|| target.clone(), |fallback| format!("{target} · {fallback}"))
}
pub fn format_task_row(record: &TaskRecord, stats: Option<&TaskRunStats>) -> String {
    let identity = task_identity(record);
    let mut parts = vec![identity.clone()];
    if identity != normalize_renderer_text(&record.task_id) { parts.push(format!("({})", normalize_renderer_text(&record.task_id))); }
    parts.extend([record_status_target(record), format!("mode:{}", normalize_renderer_text(&record.execution_mode)), format!("status:{}", status_label(record))]);
    if let Some(pid) = record.pid { parts.push(format!("pid:{pid}")); }
    if let Some(progress) = optional_renderer_text(record.final_response.as_deref()) { parts.push(format!("progress:{}", excerpt_renderer_text(&progress, Some(60)))); }
    if let Some(stats) = stats {
        parts.push(format!("turn {}{}", stats.turns, tool_count_suffix(stats.tool_calls)));
        if let Some(spend) = format_live_spend(stats.cost_usd.filter(|cost| *cost > 0.0)) { parts.push(spend); }
        if let Some(tps) = stats.tokens_per_second { parts.push(format!("{tps:.0} tok/s")); }
        let seconds = stats.runtime_ms / 1000;
        let elapsed = if seconds < 60 { format!("{seconds}s") } else if seconds < 3600 { format!("{}m {}s", seconds / 60, seconds % 60) } else { format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60) };
        parts.push(format!("elapsed:{elapsed}"));
    }
    parts.join(" ")
}
/// Footer status text: live subagents, plus the failed ones still in view. Details live in /tasks.
pub fn task_footer_status(records: &[TaskRecord]) -> Option<String> {
    let live = records.iter().filter(|record| !is_terminal(record.status)).count();
    let failed = records.iter().filter(|record| matches!(record.status, TaskStatus::Error | TaskStatus::Lost)).count();
    let mut parts = Vec::new();
    if live > 0 { parts.push(format!("{live} subagent{}", if live == 1 { "" } else { "s" })); }
    // Without a live count, name the subject so the failure does not read as part of a neighbour.
    if failed > 0 { parts.push(if live > 0 { format!("{failed} failed") } else { format!("subagents {failed} failed") }); }
    (!parts.is_empty()).then(|| parts.join(" "))
}
pub fn build_widget_rows(records: &[TaskRecord]) -> Vec<String> {
    let active: Vec<_> = records.iter().filter(|record| !is_terminal(record.status)).collect();
    let mut rows: Vec<_> = active.iter().take(5).map(|record| {
        let context = [excerpt_renderer_text(&record_status_target(record), Some(46)), excerpt_renderer_text(&record.execution_mode, Some(10)), excerpt_renderer_text(status_label(record), Some(9))].join(" ");
        let identity_width = 70usize.saturating_sub(renderer_visible_width(&context) + 1);
        if identity_width == 0 { excerpt_renderer_text(&context, Some(70)) } else { excerpt_renderer_text(&format!("{}|{context}", excerpt_renderer_text(&task_identity(record), Some(identity_width))), Some(70)) }
    }).collect();
    if active.len() > 5 { rows.push(format!("+{} more", active.len() - 5)); }
    rows
}
pub fn task_status_description(record: &TaskRecord) -> String {
    optional_renderer_text(record.task_summary.as_deref()).or_else(|| optional_renderer_text(record.description.as_deref())).or_else(|| optional_renderer_text(record.name.as_deref())).unwrap_or_else(|| normalize_renderer_text(&record.task_id))
}
pub fn background_widget_rows(records: &[TaskRecord], activity: &BTreeMap<String, String>, now: i64, live_stats: &dyn Fn(&str) -> Option<TaskRunStats>, max_width: Option<usize>) -> Vec<String> {
    let active: Vec<_> = records.iter().filter(|record| !is_terminal(record.status)).collect();
    let width = max_width.filter(|width| *width > 0).unwrap_or(220).min(220);
    let mut rows: Vec<_> = active.iter().take(5).map(|record| live_row(record, activity.get(&record.task_id).map_or("running", String::as_str), now, width, live_stats(&record.task_id))).collect();
    if active.len() > 5 { rows.push(format!("+{} more", active.len() - 5)); }
    rows
}
fn live_row(record: &TaskRecord, activity: &str, now: i64, width: usize, stats: Option<TaskRunStats>) -> String {
    let started = chrono::DateTime::parse_from_rfc3339(&record.created_at).map_or(now, |date| date.timestamp_millis());
    let seconds = now.saturating_sub(started).max(0) / 1000;
    let elapsed = if seconds < 60 { format!("{seconds}s") } else { format!("{}m {}s", seconds / 60, seconds % 60) };
    let frame_index = usize::try_from(now.max(0) / 250).unwrap_or(0) % 10;
    let frame = SPINNER_FRAMES[frame_index];
    let identity = task_status_description(record);
    let target = record_status_target(record);
    let activity = if is_suspended(record) { "suspended".into() } else { normalize_renderer_text(activity) };
    let minimum = format!("{frame} {} · {} · {} · {elapsed}", excerpt_renderer_text(&identity, Some(12)), excerpt_renderer_text(&target, Some(20)), excerpt_renderer_text(&activity, Some(8)));
    let mut remaining = width.saturating_sub(renderer_visible_width(&minimum));
    let mut tokens = Vec::new();
    if let Some(stats) = stats {
        tokens.push(format!("turn {}{}", stats.turns, tool_count_suffix(stats.tool_calls)));
        if let Some(spend) = format_live_spend(stats.cost_usd) { tokens.push(spend); }
        if let Some(tps) = stats.tokens_per_second { tokens.push(format!("{tps} tok/s")); }
    }
    tokens.retain(|token| { let size = renderer_visible_width(token) + 3; if size > remaining { false } else { remaining -= size; true } });
    let activity_width = renderer_visible_width(&activity).min(8 + remaining);
    remaining = remaining.saturating_sub(activity_width.saturating_sub(8));
    let target_width = renderer_visible_width(&target).min(20 + remaining);
    remaining = remaining.saturating_sub(target_width.saturating_sub(20));
    let identity_width = 80.min(12 + remaining);
    let mut context = vec![excerpt_renderer_text(&target, Some(target_width))];
    context.extend(tokens);
    context.extend([excerpt_renderer_text(&activity, Some(activity_width)), elapsed]);
    excerpt_renderer_text(&format!("{frame} {} · {}", excerpt_renderer_text(&identity, Some(identity_width)), context.join(" · ")), Some(width))
}
