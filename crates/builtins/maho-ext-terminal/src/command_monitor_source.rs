//! Port of senpi `terminal/command-monitor-source.ts`: a command watch's record - a PTY
//! session's output, line-buffered and filtered. A durable (restartable) watch counts its matched
//! lines in a persisted rolling fire window and carries the runaway guard, which pauses it with a
//! visible notice only after a sustained flood; a busy watch is never muted, its wakes are paced by
//! the notifier instead. The registry (`monitor_registry.rs`) owns the records and their output
//! subscriptions (upstream `CommandMonitorSource.register`/`#settle`).
use crate::monitor_registry::{MonitorEvent, MonitorFireWindow, MonitorSnapshotEntry};

/// One-minute bucket of matched lines plus the run of consecutive flood minutes before it.
#[derive(Clone, Debug, PartialEq)]
pub struct RunawayWindow {
    pub minute_start_ms: f64,
    pub count: usize,
    pub flood_minutes: usize,
}

pub struct CommandMonitor {
    pub snapshot: MonitorSnapshotEntry,
    pub muted_dropped: usize,
    pub runaway: Option<RunawayWindow>,
    /// Per-monitor wake-rate override of a persistent watch; `None` uses terminal.monitorWakesPerHour.
    pub wakes_per_hour: Option<f64>,
    /// Output lines are a person's words; every delivered line is tagged `human_input`.
    pub human_input: bool,
    filter: Option<fancy_regex::Regex>,
    lines: crate::monitor_line_buffer::MonitorLineBuffer,
    settled: bool,
}
impl CommandMonitor {
    pub fn new(snapshot: MonitorSnapshotEntry, filter: Option<fancy_regex::Regex>) -> Self {
        let mut snapshot = snapshot;
        snapshot.fire_count = Some(0);
        Self {
            snapshot,
            muted_dropped: 0,
            runaway: None,
            wakes_per_hour: None,
            human_input: false,
            filter,
            lines: Default::default(),
            settled: false,
        }
    }
    pub fn with_wakes_per_hour(mut self, wakes_per_hour: Option<f64>) -> Self {
        self.wakes_per_hour = wakes_per_hour;
        self
    }
    pub fn with_human_input(mut self, human_input: bool) -> Self {
        self.human_input = human_input;
        self
    }
    /// Count one matched line into the minute bucket. A minute with more lines than the guard allows is a
    /// flood minute; the watch trips once the current minute floods and the minutes right before it flooded
    /// too, `minutes` in a row. A quiet or skipped minute resets the run.
    fn floods_runaway(&mut self, now: f64, guard: &crate::settings::MonitorRunawayGuard) -> bool {
        let previous = self.runaway.clone();
        if previous
            .as_ref()
            .is_none_or(|current| now - current.minute_start_ms >= 60_000.0)
        {
            let elapsed = previous.as_ref().map_or(0.0, |previous| {
                ((now - previous.minute_start_ms) / 60_000.0).floor()
            });
            let previous_flooded = previous.as_ref().is_some_and(|previous| {
                elapsed == 1.0 && previous.count as f64 > guard.lines_per_minute
            });
            self.runaway = Some(RunawayWindow {
                minute_start_ms: previous.as_ref().map_or(now, |previous| {
                    previous.minute_start_ms + elapsed * 60_000.0
                }),
                count: 0,
                flood_minutes: match &previous {
                    Some(previous) if previous_flooded => previous.flood_minutes + 1,
                    _ => 0,
                },
            });
        }
        let Some(current) = &mut self.runaway else {
            return false;
        };
        current.count += 1;
        current.count as f64 > guard.lines_per_minute
            && (current.flood_minutes + 1) as f64 >= guard.minutes
    }
    fn record_fire(&mut self, now: f64) {
        self.snapshot.fire_count = Some(self.snapshot.fire_count.unwrap_or(0) + 1);
        self.snapshot.last_fired_at_ms = Some(now);
    }
    pub fn consume(
        &mut self,
        chunk: &str,
        now: f64,
        guard: &crate::settings::MonitorRunawayGuard,
    ) -> Vec<MonitorEvent> {
        if self.settled || chunk.is_empty() {
            return vec![];
        }
        let mut events = vec![];
        for line in self.lines.append(chunk) {
            if self
                .filter
                .as_ref()
                .is_some_and(|filter| !filter.is_match(&line).unwrap_or(false))
            {
                continue;
            }
            if self.snapshot.paused {
                self.muted_dropped += 1;
                continue;
            }
            self.record_fire(now);
            events.push(MonitorEvent::Line {
                id: self.snapshot.id.clone(),
                description: self.snapshot.description.clone(),
                line,
                human_input: self.human_input,
            });
            if let Some(window) = &mut self.snapshot.fire_window {
                if now - window.start_ms >= crate::shared::FIRE_BUDGET_WINDOW_MS as f64 {
                    *window = MonitorFireWindow {
                        start_ms: now,
                        count: 0,
                    };
                }
                window.count += 1;
            }
            if self.snapshot.fire_window.is_some() && self.floods_runaway(now, guard) {
                self.snapshot.paused = true;
                self.record_fire(now);
                events.push(MonitorEvent::Summary {
                    id: self.snapshot.id.clone(),
                    description: self.snapshot.description.clone(),
                    summary: crate::shared::runaway_pause_summary(
                        &self.snapshot.id,
                        guard.lines_per_minute as usize,
                        guard.minutes as usize,
                    ),
                });
            }
        }
        events
    }
    pub fn pause(&mut self) -> bool {
        if self.snapshot.paused {
            return false;
        }
        self.snapshot.paused = true;
        true
    }
    pub fn resume(&mut self) -> Option<usize> {
        if !self.snapshot.paused {
            return None;
        }
        self.snapshot.paused = false;
        if let Some(window) = &mut self.snapshot.fire_window {
            window.count = 0;
        }
        self.runaway = None;
        Some(std::mem::take(&mut self.muted_dropped))
    }
    pub fn settle(&mut self, summary: String, now: f64) -> Option<MonitorEvent> {
        if self.settled {
            return None;
        }
        self.settled = true;
        self.record_fire(now);
        Some(MonitorEvent::Summary {
            id: self.snapshot.id.clone(),
            description: self.snapshot.description.clone(),
            summary,
        })
    }
    pub fn dispose(&mut self) {
        self.settled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// command-monitor-source.ts: a watch registered with humanInput tags every delivered line.
    #[test]
    fn a_human_input_watch_tags_its_lines() {
        let guard = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor_runaway;
        let snapshot = MonitorSnapshotEntry {
            id: "bash_1".to_owned(),
            description: "inbox".to_owned(),
            ..Default::default()
        };
        let mut human = CommandMonitor::new(snapshot.clone(), None).with_human_input(true);
        assert!(matches!(
            human.consume("hi\n", 0.0, &guard).as_slice(),
            [MonitorEvent::Line {
                human_input: true,
                ..
            }]
        ));
        let mut machine = CommandMonitor::new(snapshot, None);
        assert!(matches!(
            machine.consume("ok\n", 0.0, &guard).as_slice(),
            [MonitorEvent::Line {
                human_input: false,
                ..
            }]
        ));
    }
    fn monitor() -> CommandMonitor {
        CommandMonitor::new(
            MonitorSnapshotEntry {
                id: "bash_1".to_owned(),
                description: "ready".to_owned(),
                fire_window: Some(MonitorFireWindow {
                    start_ms: 10.0,
                    count: 0,
                }),
                ..Default::default()
            },
            Some(fancy_regex::Regex::new("^ready").unwrap()),
        )
    }
    #[test]
    fn filtering_buffers_partial_lines_and_counts_only_muted_matches() {
        let mut monitor = monitor();
        assert!(monitor.consume("ignored\nrea", 10.0, &guard()).is_empty());
        assert_eq!(monitor.consume("dy\n", 11.0, &guard()).len(), 1);
        assert!(monitor.pause());
        assert!(
            monitor
                .consume("ignored\nready again\n", 12.0, &guard())
                .is_empty()
        );
        assert_eq!(monitor.resume(), Some(1));
        assert_eq!(monitor.resume(), None);
        assert_eq!(monitor.snapshot.fire_window.as_ref().unwrap().count, 0);
        assert!(
            monitor
                .settle("watcher exited (exit code 0)".to_owned(), 13.0)
                .is_some()
        );
        assert!(monitor.settle("duplicate".to_owned(), 14.0).is_none());
        assert!(monitor.consume("ready\n", 15.0, &guard()).is_empty());
    }
    fn guard() -> crate::settings::MonitorRunawayGuard {
        crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor_runaway
    }
    fn flood(monitor: &mut CommandMonitor, now: f64, lines: usize) -> Vec<MonitorEvent> {
        monitor.consume(&"ready\n".repeat(lines), now, &guard())
    }
    #[test]
    fn busy_watch_is_not_muted_by_a_line_count() {
        let mut monitor = monitor();
        let events = flood(&mut monitor, 20.0, 500);
        assert_eq!(events.len(), 500);
        assert!(
            events
                .iter()
                .all(|event| matches!(event, MonitorEvent::Line { .. }))
        );
        assert!(!monitor.snapshot.paused);
        assert_eq!(monitor.snapshot.fire_window.as_ref().unwrap().count, 500);
    }
    #[test]
    fn runaway_trips_only_after_consecutive_flood_minutes_and_rearm_clears_history() {
        let mut monitor = monitor();
        let guard = guard();
        let minutes = guard.minutes as usize;
        let per_minute = guard.lines_per_minute as usize + 1;
        for minute in 0..minutes - 1 {
            let events = flood(&mut monitor, 10.0 + minute as f64 * 60_000.0, per_minute);
            assert!(!monitor.snapshot.paused);
            assert!(
                events
                    .iter()
                    .all(|event| matches!(event, MonitorEvent::Line { .. }))
            );
        }
        let events = flood(
            &mut monitor,
            10.0 + (minutes - 1) as f64 * 60_000.0,
            per_minute,
        );
        assert!(monitor.snapshot.paused);
        assert!(
            matches!(events.last(),Some(MonitorEvent::Summary {summary,..}) if *summary==crate::shared::runaway_pause_summary("bash_1",guard.lines_per_minute as usize,guard.minutes as usize)&&summary.starts_with(crate::shared::RUNAWAY_PAUSE_SUMMARY_PREFIX))
        );
        assert!(monitor.resume().is_some());
        assert_eq!(monitor.runaway, None);
        assert_eq!(
            monitor.snapshot.fire_window.as_ref().unwrap().start_ms,
            10.0
        );
    }
    #[test]
    fn a_quiet_or_skipped_minute_resets_the_flood_run() {
        let mut monitor = monitor();
        let per_minute = guard().lines_per_minute as usize + 1;
        for minute in 0..4 {
            flood(&mut monitor, 10.0 + minute as f64 * 60_000.0, per_minute);
        }
        flood(&mut monitor, 10.0 + 6.0 * 60_000.0, per_minute);
        assert!(!monitor.snapshot.paused);
        assert_eq!(monitor.runaway.as_ref().unwrap().flood_minutes, 0);
    }
}
