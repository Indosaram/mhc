//! Port of senpi `packages/coding-agent/src/modes/interactive/session-failure-info.ts`.
//!
//! senpi's `SessionFailureReport` (core/session-failure-report.ts) is computed in core; this
//! module takes its fields as [`SessionFailureReport`] and formats the `/session` block.

use crate::theme::theme::{Theme, ThemeColor};

/// senpi `SessionFailureReport`: what provider failures cost a session.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SessionFailureReport {
    pub requests: u64,
    pub errored_requests: u64,
    pub aborted_requests: u64,
    pub failure_share: f64,
    pub failed_duration_ms: u64,
    pub post_failure_requests: u64,
    pub post_failure_full_miss_requests: u64,
    pub post_failure_full_miss_input_tokens: u64,
}

/// senpi `utils/duration.ts` `formatDuration`.
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    if ms < 60_000 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    if ms < 3_600_000 {
        return format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000);
    }
    if ms < 86_400_000 {
        return format!("{}h {}m", ms / 3_600_000, (ms % 3_600_000) / 60_000);
    }
    format!("{}d {}h", ms / 86_400_000, (ms % 86_400_000) / 3_600_000)
}

/// JS `Number.prototype.toLocaleString()` for an integer in the en-US locale.
fn grouped(number: u64) -> String {
    let raw = number.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (index, ch) in raw.chars().enumerate() {
        if index > 0 && (raw.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn format_session_failure_info(report: Option<&SessionFailureReport>, theme: &Theme) -> String {
    let Some(report) = report else {
        return String::new();
    };
    let failed = report.errored_requests + report.aborted_requests;
    if failed == 0 {
        return String::new();
    }
    let label = |text: &str| theme.fg(ThemeColor::Dim, text);
    let share = format!("{:.1}% failed", report.failure_share * 100.0);
    let mut info = format!("\n{}\n", theme.bold("Failures"));
    info += &format!(
        "{} {} {}\n",
        label("Requests:"),
        report.requests,
        label(&format!(
            "({} errored, {} aborted, {share})",
            report.errored_requests, report.aborted_requests
        ))
    );
    info += &format!(
        "{} {}\n",
        label("Time in failed requests:"),
        format_duration(report.failed_duration_ms)
    );
    if report.post_failure_requests > 0 {
        info += &format!(
            "{} {} of {} {}\n",
            label("Retries after a failure with no cache hit:"),
            report.post_failure_full_miss_requests,
            report.post_failure_requests,
            label(&format!(
                "({} uncached input tokens)",
                grouped(report.post_failure_full_miss_input_tokens)
            ))
        );
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_tui::utils::strip_terminal_sequences;

    fn theme() -> Theme {
        Theme::builtin("dark", crate::theme::ColorMode::Truecolor).expect("dark theme")
    }

    #[test]
    fn is_empty_without_a_report_or_without_failures() {
        assert_eq!(format_session_failure_info(None, &theme()), "");
        let clean = SessionFailureReport {
            requests: 4,
            ..SessionFailureReport::default()
        };
        assert_eq!(format_session_failure_info(Some(&clean), &theme()), "");
    }

    #[test]
    fn formats_failures_duration_and_uncached_retries() {
        let report = SessionFailureReport {
            requests: 8,
            errored_requests: 1,
            aborted_requests: 1,
            failure_share: 0.25,
            failed_duration_ms: 75_000,
            post_failure_requests: 2,
            post_failure_full_miss_requests: 1,
            post_failure_full_miss_input_tokens: 12_345,
        };
        let info = strip_terminal_sequences(&format_session_failure_info(Some(&report), &theme()));
        assert_eq!(
            info,
            "\nFailures\nRequests: 8 (1 errored, 1 aborted, 25.0% failed)\n\
             Time in failed requests: 1m 15s\n\
             Retries after a failure with no cache hit: 1 of 2 (12,345 uncached input tokens)\n"
        );
    }

    #[test]
    fn omits_the_retry_line_without_post_failure_requests() {
        let report = SessionFailureReport {
            requests: 2,
            errored_requests: 1,
            failure_share: 0.5,
            failed_duration_ms: 900,
            ..SessionFailureReport::default()
        };
        let info = strip_terminal_sequences(&format_session_failure_info(Some(&report), &theme()));
        assert!(info.ends_with("Time in failed requests: 900ms\n"));
        assert!(!info.contains("Retries"));
    }

    #[test]
    fn formats_durations_like_senpi() {
        assert_eq!(format_duration(999), "999ms");
        assert_eq!(format_duration(1500), "1.5s");
        assert_eq!(format_duration(3_600_000 + 120_000), "1h 2m");
        assert_eq!(format_duration(86_400_000 * 2 + 3_600_000), "2d 1h");
    }
}
