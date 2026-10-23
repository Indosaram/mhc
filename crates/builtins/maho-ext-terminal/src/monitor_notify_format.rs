//! Rendering of monitor wakes: one event body, a monitor's group header, and the size of a
//! monitor's share of a wake, so the notifier can fill a wake by whole monitors without clipping.
//! Lengths are UTF-16 code units, matching the upstream string lengths.
use crate::monitor_registry::MonitorEvent;

pub const SYSTEM_REMINDER_OPEN: &str = "<system-reminder>";
pub const SYSTEM_REMINDER_CLOSE: &str = "</system-reminder>";
pub const WAKE_BUDGET_PAUSE_NOTICE: &str = "Monitor paused after repeated updates. Completion still wakes this session; peek bash_output or re-arm only for intermediate events.";

pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn identity(event: &MonitorEvent) -> (&str, &str) {
    match event {
        MonitorEvent::Line {
            id, description, ..
        }
        | MonitorEvent::Summary {
            id, description, ..
        } => (id, description),
    }
}

pub fn event_body(event: &MonitorEvent) -> String {
    let text = match event {
        MonitorEvent::Line { line, .. } => line,
        MonitorEvent::Summary { summary, .. } => summary,
    };
    let sanitized = crate::output_format::sanitize_terminal_output(text);
    let mut result = String::new();
    let mut newline = false;
    for ch in sanitized.chars() {
        if matches!(ch, '\r' | '\n') {
            if !newline {
                result.push(' ');
            }
            newline = true;
        } else {
            result.push(ch);
            newline = false;
        }
    }
    result.trim_end().to_owned()
}

pub fn rendered_length(event: &MonitorEvent) -> usize {
    utf16_len(&format!(
        "{}{}",
        group_header(identity(event).1),
        event_body(event)
    ))
}

fn group_header(description: &str) -> String {
    format!("Monitor event({description}): ")
}

/// Characters one monitor adds to a wake: its group, its overflow notice, and their separators.
pub fn monitor_share_chars(events: &[MonitorEvent], notice: &str) -> usize {
    let group = events.first().map_or(0, |first| {
        utf16_len(&group_header(identity(first).1))
            + events
                .iter()
                .map(|event| utf16_len(&event_body(event)) + 1)
                .sum::<usize>()
    });
    group
        + if notice.is_empty() {
            0
        } else {
            utf16_len(notice) + 1
        }
}

fn clip(text: String, max_chars: usize) -> String {
    let units = text.encode_utf16().collect::<Vec<_>>();
    if units.len() <= max_chars {
        return text;
    }
    if max_chars <= 3 {
        return String::from_utf16_lossy(&units[..max_chars]);
    }
    format!("{}...", String::from_utf16_lossy(&units[..max_chars - 3]))
}

fn format_events(events: &[MonitorEvent]) -> String {
    let mut groups: Vec<(&str, &str, Vec<String>)> = vec![];
    for event in events {
        let (id, description) = identity(event);
        let body = event_body(event);
        if let Some((_, _, bodies)) = groups.iter_mut().find(|(own, _, _)| *own == id) {
            bodies.push(body);
        } else {
            groups.push((id, description, vec![body]));
        }
    }
    groups
        .into_iter()
        .map(|(_, description, bodies)| {
            format!("{}{}", group_header(description), bodies.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One wake message. The notifier fills a wake by whole monitors within `max_chars`, so the clip is a
/// last-resort guard that a correctly sized wake never reaches.
pub fn build_wake_message(
    events: &[MonitorEvent],
    notices: &[String],
    pause_notice: &str,
    max_chars: usize,
) -> String {
    let suffix = notices
        .iter()
        .map(String::as_str)
        .chain([pause_notice])
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let fixed = utf16_len(SYSTEM_REMINDER_OPEN)
        + utf16_len(SYSTEM_REMINDER_CLOSE)
        + if suffix.is_empty() {
            0
        } else {
            utf16_len(&suffix) + 1
        };
    let body = clip(format_events(events), max_chars.saturating_sub(fixed));
    format!(
        "{SYSTEM_REMINDER_OPEN}{body}{}{suffix}{SYSTEM_REMINDER_CLOSE}",
        if !body.is_empty() && !suffix.is_empty() {
            "\n"
        } else {
            ""
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(id: &str, description: &str, text: &str) -> MonitorEvent {
        MonitorEvent::Line {
            id: id.to_owned(),
            description: description.to_owned(),
            line: text.to_owned(),
        }
    }

    #[test]
    fn groups_by_monitor_and_sanitizes_newlines() {
        let events = vec![
            line("b1", "one", "first\r\nsecond\n"),
            line("b2", "two", "other"),
            MonitorEvent::Summary {
                id: "b1".to_owned(),
                description: "one".to_owned(),
                summary: "watcher completed".to_owned(),
            },
        ];
        assert_eq!(
            build_wake_message(&events, &[], "", 4096),
            "<system-reminder>Monitor event(one): first second\nwatcher completed\nMonitor event(two): other</system-reminder>"
        );
    }

    #[test]
    fn share_chars_match_the_rendered_group_and_notice() {
        let events = vec![line("b1", "one", "abc"), line("b1", "one", "de")];
        let notice = "[2 more]";
        let message = build_wake_message(&events, &[notice.to_owned()], "", 4096);
        let wrapper = utf16_len(SYSTEM_REMINDER_OPEN) + utf16_len(SYSTEM_REMINDER_CLOSE);
        assert_eq!(
            utf16_len(&message),
            wrapper + monitor_share_chars(&events, notice) - 1
        );
        assert_eq!(
            rendered_length(&events[0]),
            utf16_len("Monitor event(one): abc")
        );
    }

    #[test]
    fn notices_and_pause_notice_survive_last_resort_clip() {
        let events = vec![line("b1", "one", &"x".repeat(500))];
        let text = build_wake_message(&events, &["[3 more]".to_owned()], "paused", 150);
        assert_eq!(utf16_len(&text), 150);
        assert!(text.ends_with("...\n[3 more]\npaused</system-reminder>"));
    }
}
