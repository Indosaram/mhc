//! Filling one monitor wake within `max_chars`, measured from the real wrapper (reminder tags, the
//! pause notice, each monitor's actual overflow notice), never an assumed reserve. Monitors with a
//! summary go first, then queue order; a monitor that does not fit whole waits for the next wake.
//! Only when the first monitor alone is too big does it send a prefix of its lines; the rest stay
//! queued. The line cap already holds per monitor at enqueue, so a wake is never clipped.
use crate::monitor_notify_format::{
    SYSTEM_REMINDER_CLOSE, SYSTEM_REMINDER_OPEN, WAKE_BUDGET_PAUSE_NOTICE, monitor_share_chars,
    utf16_len,
};
use crate::monitor_overflow::OverflowBook;
use crate::monitor_registry::MonitorEvent;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeShare {
    pub events: Vec<MonitorEvent>,
    /// Every queued line went; when false the monitor keeps its rest and overflow for the next wake.
    pub whole: bool,
}

fn event_id(event: &MonitorEvent) -> &str {
    match event {
        MonitorEvent::Line { id, .. } | MonitorEvent::Summary { id, .. } => id,
    }
}

/// Returns the wake's monitors in fill order (`(id, share)`), summary monitors first.
pub fn fit_wake(
    queue: &[MonitorEvent],
    overflow: &mut OverflowBook,
    candidates: &[String],
    summary_ids: &BTreeSet<String>,
    max_chars: usize,
) -> Vec<(String, WakeShare)> {
    let mut queued: Vec<String> = vec![];
    for id in queue
        .iter()
        .map(|event| event_id(event).to_owned())
        .chain(overflow.ids())
    {
        if candidates.contains(&id) && !queued.contains(&id) {
            queued.push(id);
        }
    }
    let ordered = queued
        .iter()
        .filter(|id| summary_ids.contains(*id))
        .chain(queued.iter().filter(|id| !summary_ids.contains(*id)))
        .cloned()
        .collect::<Vec<_>>();
    let budget = max_chars as i64
        - (utf16_len(SYSTEM_REMINDER_OPEN)
            + utf16_len(SYSTEM_REMINDER_CLOSE)
            + utf16_len(WAKE_BUDGET_PAUSE_NOTICE)
            + 1) as i64;
    let mut wake: Vec<(String, WakeShare)> = vec![];
    let mut chars = 0i64;
    for id in ordered {
        overflow.flush(&id);
        let own = queue
            .iter()
            .filter(|event| event_id(event) == id)
            .cloned()
            .collect::<Vec<_>>();
        let share = monitor_share_chars(&own, &overflow.notice(&id)) as i64;
        if chars + share <= budget || (wake.is_empty() && own.is_empty()) {
            wake.push((
                id,
                WakeShare {
                    events: own,
                    whole: true,
                },
            ));
            chars += share;
        } else if wake.is_empty() {
            let mut count = 1;
            while count < own.len() && monitor_share_chars(&own[..count + 1], "") as i64 <= budget {
                count += 1;
            }
            let prefix = own[..count].to_vec();
            chars += monitor_share_chars(&prefix, "") as i64;
            let whole = prefix.len() == own.len() && overflow.count(&id) == 0;
            wake.push((
                id,
                WakeShare {
                    events: prefix,
                    whole,
                },
            ));
        }
    }
    wake
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(id: &str, text: &str) -> MonitorEvent {
        MonitorEvent::Line {
            id: id.to_owned(),
            description: id.to_owned(),
            line: text.to_owned(),
        }
    }
    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }
    // The pause-notice reserve plus wrapper: a wake needs at least this much before any line fits.
    fn reserve() -> usize {
        utf16_len(SYSTEM_REMINDER_OPEN)
            + utf16_len(SYSTEM_REMINDER_CLOSE)
            + utf16_len(WAKE_BUDGET_PAUSE_NOTICE)
            + 1
    }

    #[test]
    fn whole_monitors_fill_in_order_and_a_late_monitor_waits() {
        let queue = vec![line("a", &"x".repeat(40)), line("b", &"y".repeat(40))];
        let mut book = OverflowBook::new(None);
        let one = monitor_share_chars(&queue[..1], "");
        let wake = fit_wake(
            &queue,
            &mut book,
            &ids(&["a", "b"]),
            &BTreeSet::new(),
            reserve() + one + 5,
        );
        assert_eq!(
            wake.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["a"]
        );
        assert!(wake[0].1.whole);
    }

    #[test]
    fn summary_monitors_go_first() {
        let queue = vec![
            line("a", "x"),
            MonitorEvent::Summary {
                id: "b".to_owned(),
                description: "b".to_owned(),
                summary: "done".to_owned(),
            },
        ];
        let mut book = OverflowBook::new(None);
        let summaries = BTreeSet::from(["b".to_owned()]);
        let wake = fit_wake(&queue, &mut book, &ids(&["a", "b"]), &summaries, 4096);
        assert_eq!(
            wake.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    #[test]
    fn first_oversized_monitor_sends_a_prefix_and_keeps_the_rest() {
        let queue = (0..10)
            .map(|_| line("a", &"x".repeat(30)))
            .collect::<Vec<_>>();
        let mut book = OverflowBook::new(None);
        let wake = fit_wake(
            &queue,
            &mut book,
            &ids(&["a"]),
            &BTreeSet::new(),
            reserve() + 100,
        );
        assert_eq!(wake.len(), 1);
        assert!(!wake[0].1.whole);
        assert!(!wake[0].1.events.is_empty() && wake[0].1.events.len() < 10);
    }

    #[test]
    fn overflow_only_monitor_is_not_whole_when_a_prefix_is_sent() {
        let queue = vec![line("a", "x")];
        let mut book = OverflowBook::new(None);
        book.record(&line("a", "dropped"), "dropped", false);
        let wake = fit_wake(
            &queue,
            &mut book,
            &ids(&["a"]),
            &BTreeSet::new(),
            reserve() + 1,
        );
        assert_eq!(wake.len(), 1);
        assert!(!wake[0].1.whole);
    }
}
