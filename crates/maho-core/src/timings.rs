//! Port of senpi packages/coding-agent/src/core/timings.ts.
//!
//! Central timing instrumentation for startup profiling. Enabled with the brand TIMING env
//! variable set to 1 (senpi reads PI_TIMING through envValue).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::brand::env_value;
use crate::config::current_env;

#[derive(Debug, Clone, PartialEq)]
pub struct TimingEntry {
    pub label: String,
    pub ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimingLabel {
    Main,
    Extensions,
    Reload,
    Tui,
}

impl TimingLabel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Extensions => "extensions",
            Self::Reload => "reload",
            Self::Tui => "tui",
        }
    }
}

struct TimingNamespace {
    timings: Vec<TimingEntry>,
    last_time: Instant,
}

fn namespaces() -> &'static Mutex<HashMap<TimingLabel, TimingNamespace>> {
    static NAMESPACES: OnceLock<Mutex<HashMap<TimingLabel, TimingNamespace>>> = OnceLock::new();
    NAMESPACES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<TimingLabel, TimingNamespace>> {
    namespaces().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

static ENABLED_OVERRIDE: Mutex<Option<bool>> = Mutex::new(None);

/// senpi computes ENABLED once at import time; the Rust port reads it lazily and lets tests pin it.
fn enabled() -> bool {
    if let Some(value) = *ENABLED_OVERRIDE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
        return value;
    }
    env_value("TIMING", &current_env()).as_deref() == Some("1")
}

fn now() -> Instant {
    Instant::now()
}

fn millis_between(earlier: Instant, later: Instant) -> f64 {
    match later.checked_duration_since(earlier) {
        Some(delta) => delta.as_secs_f64() * 1000.0,
        None => -(earlier.duration_since(later).as_secs_f64() * 1000.0),
    }
}

pub fn reset_timings(namespace: TimingLabel) {
    if !enabled() {
        return;
    }
    lock().insert(namespace, TimingNamespace { timings: Vec::new(), last_time: now() });
}

pub fn time(label: &str, namespace: TimingLabel) {
    if !enabled() {
        return;
    }
    let current = now();
    let mut map = lock();
    let entry = map.entry(namespace).or_insert_with(|| TimingNamespace { timings: Vec::new(), last_time: now() });
    entry.timings.push(TimingEntry { label: label.to_owned(), ms: millis_between(entry.last_time, current) });
    entry.last_time = current;
}

/// Records a phase this module could not measure itself; the namespace cursor is left untouched.
pub fn record_timing(label: &str, ms: f64, namespace: TimingLabel) {
    if !enabled() {
        return;
    }
    let mut map = lock();
    let entry = map.entry(namespace).or_insert_with(|| TimingNamespace { timings: Vec::new(), last_time: now() });
    entry.timings.push(TimingEntry { label: label.to_owned(), ms });
}

pub fn get_timings(namespace: TimingLabel) -> Vec<TimingEntry> {
    lock().get(&namespace).map(|entry| entry.timings.clone()).unwrap_or_default()
}

pub fn format_timings(namespace: TimingLabel) -> Option<String> {
    let entries: Vec<TimingEntry> = get_timings(namespace).into_iter().filter(|entry| entry.ms >= 0.0).collect();
    if entries.is_empty() {
        return None;
    }
    let total: f64 = entries.iter().map(|entry| entry.ms).sum();
    let parts = entries
        .iter()
        .map(|entry| format!("{} {}ms", entry.label, entry.ms.round() as i64))
        .collect::<Vec<_>>()
        .join("  ");
    Some(format!("{parts}  |  total {}ms", total.round() as i64))
}

fn print_timing_group(title: &str, timings: &[TimingEntry]) {
    let printable: Vec<&TimingEntry> = timings.iter().filter(|timing| timing.ms >= 0.0).collect();
    if printable.is_empty() {
        return;
    }
    eprintln!("\n--- {title} ---");
    for timing in &printable {
        eprintln!("  {}: {}ms", timing.label, timing.ms.round() as i64);
    }
    let total: f64 = printable.iter().map(|timing| timing.ms).sum();
    eprintln!("  TOTAL: {}ms", total.round() as i64);
    eprintln!("{}\n", "-".repeat(title.len() + 8));
}

pub fn print_timings() {
    if !enabled() {
        return;
    }
    let snapshot: Vec<(TimingLabel, Vec<TimingEntry>)> =
        lock().iter().map(|(namespace, entry)| (*namespace, entry.timings.clone())).collect();
    for (namespace, timings) in snapshot {
        print_timing_group(&format!("Startup Timings: {}", namespace.as_str()), &timings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn enable() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *ENABLED_OVERRIDE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(true);
        guard
    }

    #[test]
    fn resetting_clears_a_namespace() {
        let _guard = enable();
        time("a", TimingLabel::Tui);
        reset_timings(TimingLabel::Tui);
        assert!(get_timings(TimingLabel::Tui).is_empty());
    }

    #[test]
    fn recorded_phases_are_reported_with_a_total() {
        let _guard = enable();
        reset_timings(TimingLabel::Extensions);
        record_timing("parse", 12.0, TimingLabel::Extensions);
        record_timing("load", 8.0, TimingLabel::Extensions);
        let formatted = format_timings(TimingLabel::Extensions).expect("timings");
        assert!(formatted.contains("parse 12ms"));
        assert!(formatted.contains("load 8ms"));
        assert!(formatted.contains("total 20ms"));
    }

    #[test]
    fn negative_phases_are_filtered_out() {
        let _guard = enable();
        reset_timings(TimingLabel::Reload);
        record_timing("skew", -3.0, TimingLabel::Reload);
        assert!(format_timings(TimingLabel::Reload).is_none());
    }

    #[test]
    fn an_unrecorded_namespace_has_no_entries() {
        let _guard = enable();
        assert!(format_timings(TimingLabel::Main).is_none());
    }
}
