//! Port of tool-progress.ts.
use crate::working_status::format_working_elapsed_seconds;
use serde_json::Value;
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub fn tool_spinner_glyph(frame: i64) -> &'static str {
    usize::try_from(frame % 10)
        .ok()
        .and_then(|i| FRAMES.get(i))
        .copied()
        .unwrap_or(FRAMES[0])
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolProgressDetails {
    pub activity: Option<String>,
    pub started_at: f64,
    pub max_wait_ms: Option<f64>,
}
pub fn read_tool_progress(details: &Value) -> Option<ToolProgressDetails> {
    let progress = details.get("progress")?;
    let started_at = progress
        .get("startedAt")?
        .as_f64()
        .filter(|v| v.is_finite())?;
    let activity = match progress.get("activity") {
        Some(v) => Some(v.as_str()?.to_owned()),
        None => None,
    };
    let max_wait_ms = match progress.get("maxWaitMs") {
        Some(v) => Some(v.as_f64().filter(|v| v.is_finite())?),
        None => None,
    };
    Some(ToolProgressDetails {
        activity,
        started_at,
        max_wait_ms,
    })
}
pub fn format_tool_progress_line(
    progress: &ToolProgressDetails,
    now: f64,
    frame: Option<i64>,
) -> String {
    let activity = progress
        .activity
        .as_deref()
        .filter(|v| !v.is_empty())
        .unwrap_or("working");
    let elapsed = format_working_elapsed_seconds((now - progress.started_at) / 1000.);
    let max = progress.max_wait_ms.map_or_else(String::new, |v| {
        format!(" / max {}", format_working_elapsed_seconds(v / 1000.))
    });
    format!(
        "{} {activity} · {elapsed}{max}",
        tool_spinner_glyph(frame.unwrap_or(0))
    )
}
