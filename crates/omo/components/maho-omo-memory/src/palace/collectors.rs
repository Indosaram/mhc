pub use super::entry_collector::{
    PalaceCoreEntry, PalaceEntryState, PalaceExternalEntry, UNCOMMITTED_LABEL, collect_core,
    collect_external,
};
pub use super::history_collector::{
    HISTORY_MAX_COMMITS, HISTORY_PER_DIFF_CAP, HISTORY_RECENT_DIFFS, HISTORY_TOTAL_PAYLOAD_CAP,
    PalaceCommit, PalaceHistory, PalaceHistoryCaps, collect_history, is_reflection_commit_subject,
};
pub use super::reflection_collector::{
    PalaceReflection, PalaceReflectionOutcome, collect_reflection,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palace::test_support::create_palace_fixture;

    #[test]
    fn collectors_barrel_exposes_the_established_collector_modules() {
        let fixture = create_palace_fixture(false);
        let head = fixture.head.clone();

        let core = collect_core(&fixture.repo, Some(head.as_str())).unwrap();
        assert!(core.iter().any(|entry| entry.path == "system/persona.md"));
        assert_eq!(UNCOMMITTED_LABEL, crate::palace::entry_collector::UNCOMMITTED_LABEL);
        assert_eq!(
            HISTORY_MAX_COMMITS,
            crate::palace::history_collector::HISTORY_MAX_COMMITS
        );
        assert_eq!(
            HISTORY_PER_DIFF_CAP,
            crate::palace::history_collector::HISTORY_PER_DIFF_CAP
        );
        assert!(is_reflection_commit_subject("feat(reflection): captured"));
    }
}
