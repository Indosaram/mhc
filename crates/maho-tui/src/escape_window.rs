//! Port of senpi `packages/tui/src/escape-window.ts`.

/// A pending lone ESC whose next input is delivered after the escape window is
/// released as a standalone Escape press before the new input is processed. The
/// decision uses the arrival-clock gap observed at delivery time, so an
/// over-window gap is always treated as a lone Escape regardless of whether the
/// flush timer callback ran first (#3628). Within the window the pair still
/// merges into one Alt-key sequence.
pub fn is_stale_escape(pending: &str, gap_ms: Option<f64>, escape_window_ms: f64) -> bool {
    pending == "\x1b" && gap_ms.is_some_and(|gap| gap >= escape_window_ms)
}

#[cfg(test)]
mod tests {
    use super::is_stale_escape;

    #[test]
    fn only_a_lone_escape_past_the_window_is_stale() {
        assert!(is_stale_escape("\x1b", Some(50.0), 50.0));
        assert!(!is_stale_escape("\x1b", Some(49.0), 50.0));
        assert!(!is_stale_escape("\x1b", None, 50.0));
        assert!(!is_stale_escape("\x1b[", Some(500.0), 50.0));
    }
}
