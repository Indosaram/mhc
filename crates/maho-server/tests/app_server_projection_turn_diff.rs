use maho_server::app_server::projection_turn_diff::TurnDiffTracker;
#[test]
fn cumulative_diff_uses_source_order_replaces_prior_diff_and_suppresses_duplicates() {
    let mut tracker = TurnDiffTracker::default();
    assert_eq!(tracker.update("b", "B", ["a", "b"]), Some("B".into()));
    assert_eq!(tracker.update("a", "A", ["a", "b"]), Some("AB".into()));
    assert_eq!(tracker.update("a", "A", ["a", "b"]), None);
    assert_eq!(tracker.update("a", "", ["a", "b"]), None);
    assert_eq!(tracker.update("a", "C", ["a", "b"]), Some("CB".into()));
    assert_eq!(tracker.update("b", "B", ["b", "a"]), Some("BC".into()));
}
