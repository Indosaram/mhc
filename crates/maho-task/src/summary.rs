//! The model-facing `<task-result>` envelope of a settled run (ported from oh-my-pi
//! `task/result-summary.ts` and `prompts/tools/task-summary.md`): status, duration, error, the
//! output inline or a head preview with a pointer to the full output.
use crate::record::{TaskRecord, TaskStatus};

/// Largest output inlined whole.
pub const INLINE_OUTPUT_CHARS: usize = 12_000;
/// Head shown for a larger output.
pub const PREVIEW_CHARS: usize = 5_000;

/// Escapes the envelope's own tags inside child text.
pub fn escape_tags(text: &str) -> String {
    text.replace("<task-result", "&lt;task-result")
        .replace("</task-result", "&lt;/task-result")
}

/// Preview head: prefers a line boundary when one is in the second half of the window.
fn preview_head(output: &str) -> &str {
    let mut end = PREVIEW_CHARS.min(output.len());
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    let slice = &output[..end];
    match slice.rfind('\n') {
        Some(newline) if newline >= end / 2 => &slice[..newline],
        _ => slice,
    }
}

/// Human duration (`850ms`, `12.3s`, `4m05s`).
pub fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1_000)
    }
}

fn status_label(record: &TaskRecord) -> String {
    match record.status {
        TaskStatus::Failed => match record.exit_code {
            Some(code) => format!("failed (exit {code})"),
            None => "failed".to_owned(),
        },
        other => other.as_str().to_owned(),
    }
}

/// Renders the envelope for `record` with the run's `output` (and `stderr_tail` used when the
/// output is empty). `output_path` is where the full output can be read.
pub fn format_result(
    record: &TaskRecord,
    output: &str,
    stderr_tail: &str,
    output_path: &str,
) -> String {
    let duration = record
        .finished_at_ms
        .map(|end| end.saturating_sub(record.started_at_ms))
        .unwrap_or(0);
    let mut text = format!(
        "<task-result id=\"{}\" agent=\"{}\" status=\"{}\" duration=\"{}\"{}>\n",
        record.id,
        record.agent,
        status_label(record),
        format_duration(duration),
        record
            .model
            .as_deref()
            .map(|model| format!(" model=\"{model}\""))
            .unwrap_or_default()
    );
    let body = if output.trim().is_empty() {
        stderr_tail.trim()
    } else {
        output.trim()
    };
    if let Some(error) = record.error.as_deref().filter(|e| !e.is_empty()) {
        text.push_str(&format!("<error>{}</error>\n", escape_tags(error)));
    }
    if body.is_empty() {
        text.push_str("<output>\n(no output)\n</output>\n");
    } else if body.len() > INLINE_OUTPUT_CHARS {
        text.push_str(&format!(
            "<preview full-output=\"{output_path}\" size=\"{}\">\n{}\n</preview>\n",
            body.len(),
            escape_tags(preview_head(body))
        ));
    } else {
        text.push_str(&format!("<output>\n{}\n</output>\n", escape_tags(body)));
    }
    if !record.hook_notes.is_empty() {
        text.push_str(&format!(
            "<post-run>\n{}\n</post-run>\n",
            escape_tags(&record.hook_notes.join("\n"))
        ));
    }
    match record.status {
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted => {
            text.push_str(&format!(
                "<resume>task_resume id=\"{}\" continues this agent with its full context.</resume>\n",
                record.id
            ));
        }
        TaskStatus::Queued | TaskStatus::Running => {}
    }
    text.push_str("</task-result>");
    text
}

/// One-line status of a task (for `task_list` and progress text).
pub fn status_line(record: &TaskRecord) -> String {
    format!(
        "{}  {}  agent={}  model={}  runs={}{}  {}",
        record.id,
        status_label(record),
        record.agent,
        record.model.as_deref().unwrap_or("(default)"),
        record.runs,
        if record.background { "  background" } else { "" },
        record.description
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::sample;

    #[test]
    fn inline_and_preview() {
        let mut record = sample("Fix");
        record.status = TaskStatus::Completed;
        record.finished_at_ms = Some(1_501);
        let text = format_result(&record, "done <task-result x>", "", "/s/Fix.md");
        assert!(text.starts_with("<task-result id=\"Fix\" agent=\"task\" status=\"completed\" duration=\"1.5s\">"), "{text}");
        assert!(text.contains("<output>\ndone &lt;task-result x>\n</output>"), "{text}");
        assert!(text.contains("task_resume id=\"Fix\""));
        let long = format!("{}\n{}", "a".repeat(4_000), "b".repeat(20_000));
        let text = format_result(&record, &long, "", "/s/Fix.md");
        assert!(text.contains("<preview full-output=\"/s/Fix.md\""), "{text}");
        assert!(!text.contains('b'.to_string().repeat(10).as_str()));
    }

    #[test]
    fn failure_falls_back_to_stderr() {
        let mut record = sample("X");
        record.status = TaskStatus::Failed;
        record.exit_code = Some(2);
        record.error = Some("boom".into());
        let text = format_result(&record, "", "stderr text", "/x");
        assert!(text.contains("status=\"failed (exit 2)\""));
        assert!(text.contains("<error>boom</error>"));
        assert!(text.contains("stderr text"));
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(850), "850ms");
        assert_eq!(format_duration(245_000), "4m05s");
    }
}
