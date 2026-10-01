//! Port of components/continuity-notice.ts.
use serde_json::Value;
use crate::theme::{Theme, ThemeColor};

pub const CONTINUITY_DIAGNOSTIC_TYPE: &str = "claude_sdk_oauth_session_continuity";
pub const RESUME_FALLBACK_DIAGNOSTIC_TYPE: &str = "claude_sdk_oauth_resume_fallback";

#[derive(Default)]
pub struct ContinuityNoticeTracker { rendered_disabled: bool }
impl ContinuityNoticeTracker {
    pub fn reset(&mut self) { self.rendered_disabled = false; }
    pub fn notice_for(&mut self, message: &Value, theme: &Theme) -> Option<String> {
        for diagnostic in message.get("diagnostics").and_then(Value::as_array).into_iter().flatten() {
            let diagnostic_type = diagnostic.get("type").and_then(Value::as_str);
            if diagnostic_type == Some(RESUME_FALLBACK_DIAGNOSTIC_TYPE) {
                return Some(theme.fg(ThemeColor::Muted, "Session continuity lost - resume failed, resent the full conversation"));
            }
            if diagnostic_type != Some(CONTINUITY_DIAGNOSTIC_TYPE) { continue; }
            let details = &diagnostic["details"];
            let label = match details["kind"].as_str() {
                Some("flatten") => "Session continuity lost - resent the full conversation",
                Some("disabled") if !self.rendered_disabled => {
                    self.rendered_disabled = true;
                    "Session continuity disabled (resumeMode: off) - resending the conversation each turn"
                }
                _ => continue,
            };
            let mut text = label.to_owned();
            if let Some(reason) = details["reason"].as_str().filter(|reason| !reason.is_empty()) {
                text.push_str(&format!(" ({reason})"));
            }
            if let Some(bytes) = details["payloadBytes"].as_f64() {
                let size = if bytes < 1024. { format!("{bytes}B") }
                    else if bytes < 1024. * 1024. { format!("{:.1}KB", bytes / 1024.) }
                    else { format!("{:.1}MB", bytes / (1024. * 1024.)) };
                text.push_str(&format!(" - sent {size}"));
                if let Some(collapsed) = details["collapsedDirectives"].as_f64().filter(|count| *count > 0.) {
                    text.push_str(&format!(", {collapsed} duplicate ultrawork blocks collapsed"));
                }
            }
            return Some(theme.fg(ThemeColor::Muted, &text));
        }
        None
    }
}
