pub const TEST_DECISION: &str = "Existing tests are the behavior of record: update those your change makes stale; one wrong before your change is a finding, not a test to edit green. The run proves the change: add a test only where the repository keeps tests for this behavior and a regression would otherwise pass unnoticed - sized like its neighbors, never restating the change.";

#[cfg(test)]
mod tests {
    use super::TEST_DECISION;

    #[test]
    fn decision_does_not_require_reading_tests_or_reproducing_bugs_first() {
        assert!(!TEST_DECISION.starts_with("Read existing tests first"));
        assert!(!TEST_DECISION.contains("Reproduce a bug before fixing it"));
        assert!(TEST_DECISION.starts_with("Existing tests are the behavior of record"));
    }
}
