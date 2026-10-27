//! Port of provider-error-presentation.ts.
use crate::theme::theme::{Theme, ThemeColor};
use maho_tui::{components::text::Text, tui::Component};
use serde_json::Value;
use std::sync::LazyLock;
static NON_NETWORK: LazyLock<Option<regex::Regex>> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)auth|api.?key|permission|quota|credit|billing|rate.?limit|too many requests|429",
    )
    .ok()
});
static NETWORK: LazyLock<Option<regex::Regex>> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)network error|service unavailable|connection (?:error|lost|reset)|fetch failed|ECONNRESET|ECONNREFUSED|ENOTFOUND|EAI_AGAIN|socket hang up|overloaded",
    )
    .ok()
});
/// senpi `isRetryableProviderError`: any transient provider failure (network drop, 429, 5xx), and
/// never a hard auth/quota/billing failure.
pub fn is_retryable_provider_error(raw: Option<&str>) -> bool {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return false;
    };
    is_network_provider_error(Some(raw), false)
        || maho_ai::utils::retry::is_retryable_error_message(raw)
}
pub fn is_network_provider_error(raw: Option<&str>, envelope_only: bool) -> bool {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return false;
    };
    if NON_NETWORK.as_ref().is_some_and(|r| r.is_match(raw)) {
        return false;
    }
    let network = NETWORK.as_ref();
    if !envelope_only {
        return network.is_some_and(|r| r.is_match(raw));
    }
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if value
        .get("type")
        .is_some_and(|v| v.as_str() != Some("error"))
        || value
            .get("error")
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .is_none()
    {
        return false;
    }
    value
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str)
        .is_some_and(|m| network.is_some_and(|r| r.is_match(m)))
}
pub fn is_network_provider_message(message: &Value) -> bool {
    matches!(
        message.get("stopReason").and_then(Value::as_str),
        Some("error" | "aborted")
    ) && is_network_provider_error(message.get("errorMessage").and_then(Value::as_str), false)
}
#[derive(Debug, Clone)]
pub struct ProviderFailureNotice {
    pub details: Vec<String>,
    pub expanded: bool,
    pub summary: Option<String>,
}
fn sanitize_error(raw: &str) -> String {
    let text = maho_tui::utils::strip_terminal_sequences(raw)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    text.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect::<String>()
        .split('\n')
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}
impl ProviderFailureNotice {
    pub fn render(&self, width: usize, theme: &Theme, expand_key: &str) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(summary) = &self.summary {
            lines.extend(
                Text::with_padding(
                    theme.fg(
                        ThemeColor::Warning,
                        &format!("{summary} ({expand_key} for details)"),
                    ),
                    1,
                    0,
                )
                .render(width),
            );
        }
        if self.expanded {
            for raw in &self.details {
                lines.extend(
                    Text::with_padding(theme.fg(ThemeColor::Dim, &sanitize_error(raw)), 1, 0)
                        .render(width),
                );
            }
        }
        lines
    }
}
#[derive(Default)]
pub struct ProviderErrorPresentation {
    pub notices: Vec<ProviderFailureNotice>,
    current: Option<usize>,
    pending: bool,
    retry_awaiting_finish: bool,
}
impl ProviderErrorPresentation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn record(&mut self, raw: &str, expanded: bool) {
        self.pending = true;
        self.retry_awaiting_finish = true;
        let index = match self.current {
            Some(i) => i,
            None => {
                self.notices.push(ProviderFailureNotice {
                    details: Vec::new(),
                    expanded,
                    summary: Some("The model provider may be having trouble.".into()),
                });
                let i = self.notices.len() - 1;
                self.current = Some(i);
                i
            }
        };
        if !self.notices[index].details.iter().any(|d| d == raw) {
            self.notices[index].details.push(raw.into());
        }
    }
    pub fn retrying(&mut self, raw: &str, expanded: bool) {
        self.record(raw, expanded);
        self.hide_summary();
    }
    fn hide_summary(&mut self) {
        if let Some(i) = self.current {
            self.notices[i].summary = None;
        }
    }
    pub fn clear(&mut self) {
        self.pending = false;
        self.retry_awaiting_finish = false;
        self.hide_summary();
    }
    pub fn clear_transient(&mut self) {
        self.pending = false;
        self.hide_summary();
    }
    pub fn has_active_retry(&self) -> bool { self.pending }
    pub fn awaiting_retry_finish(&self) -> bool { self.retry_awaiting_finish }
    pub fn finish(&mut self, raw: Option<&str>, attempts: Option<usize>) {
        if let Some(raw) = raw.filter(|r| !r.is_empty()) {
            self.record(raw, false);
        }
        if !self.pending && !self.retry_awaiting_finish {
            return;
        }
        self.pending = false;
        self.retry_awaiting_finish = false;
        let count = attempts.map_or_else(String::new, |n| format!(" after {n} retries"));
        if let Some(i) = self.current {
            self.notices[i].summary = Some(format!(
                "The model provider could not complete the request{count}. Try again or choose another model with /model."
            ));
        }
    }
    pub fn new_turn(&mut self) {
        self.finish(None, None);
        self.current = None;
    }
}

#[cfg(test)]
mod retry_episode_tests {
    use super::*;
    #[test]
    fn retry_lifecycle_distinguishes_transient_clear_from_episode_end() {
        let mut state = ProviderErrorPresentation::new();
        state.record("network error", false);
        assert!(state.has_active_retry());
        assert!(state.awaiting_retry_finish());
        state.clear_transient();
        assert!(!state.has_active_retry());
        assert!(state.awaiting_retry_finish());
        state.finish(None, Some(2));
        assert!(!state.awaiting_retry_finish());
        assert!(state.notices[0].summary.as_deref().is_some_and(|s| s.contains("after 2 retries")));
    }
}
