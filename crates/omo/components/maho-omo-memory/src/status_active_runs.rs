//! Insertion-ordered in-flight reflection runs, partitioned by identity.
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveReflectionRunDetails {
    pub trigger: String,
    pub category: String,
    pub model: Option<String>,
    pub started_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveReflectionRun {
    pub run_id: String,
    pub details: ActiveReflectionRunDetails,
}

#[derive(Debug, Default)]
pub struct ActiveReflectionRuns {
    by_identity: BTreeMap<String, Vec<ActiveReflectionRun>>,
}

impl ActiveReflectionRuns {
    pub fn start(&mut self, identity: &str, run_id: &str, details: ActiveReflectionRunDetails) {
        let runs = self.by_identity.entry(identity.to_owned()).or_default();
        if let Some(run) = runs.iter_mut().find(|run| run.run_id == run_id) {
            run.details = details;
        } else {
            runs.push(ActiveReflectionRun { run_id: run_id.to_owned(), details });
        }
    }

    pub fn settle(&mut self, identity: &str, run_id: &str) {
        if let Some(runs) = self.by_identity.get_mut(identity) {
            runs.retain(|run| run.run_id != run_id);
            if runs.is_empty() {
                self.by_identity.remove(identity);
            }
        }
    }

    pub fn clear(&mut self, identity: &str) {
        self.by_identity.remove(identity);
    }

    pub fn is_active(&self, identity: &str) -> bool {
        self.current(identity).is_some()
    }

    pub fn current(&self, identity: &str) -> Option<&ActiveReflectionRun> {
        self.by_identity.get(identity)?.first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn details() -> ActiveReflectionRunDetails {
        ActiveReflectionRunDetails { trigger: "step_count".into(), category: "quick".into(), model: None, started_at: "2026-08-12T00:00:00.000Z".into() }
    }
    fn started() -> ActiveReflectionRuns {
        let mut runs = ActiveReflectionRuns::default();
        runs.start("agent-test", "run-1", details());
        runs
    }
    #[test]
    fn initially_idle() { assert!(!ActiveReflectionRuns::default().is_active("agent-test")); }
    #[test]
    fn launch_is_active() { assert!(started().is_active("agent-test")); }
    #[test]
    fn settle_returns_to_idle() { let mut runs = started(); runs.settle("agent-test", "run-1"); assert!(!runs.is_active("agent-test")); }
    #[test]
    fn overlapping_runs_remain_active() { let mut runs = started(); runs.start("agent-test", "run-2", details()); runs.settle("agent-test", "run-1"); assert!(runs.is_active("agent-test")); runs.settle("agent-test", "run-2"); assert!(!runs.is_active("agent-test")); }
    #[test]
    fn identities_are_independent() { let mut runs = started(); runs.start("agent-b", "run-2", details()); runs.settle("agent-test", "run-1"); assert!(!runs.is_active("agent-test")); assert!(runs.is_active("agent-b")); }
    #[test]
    fn duplicate_launch_replaces_details() { let mut runs = started(); runs.start("agent-test", "run-1", details()); runs.settle("agent-test", "run-1"); assert!(!runs.is_active("agent-test")); }
    #[test]
    fn unknown_settle_stays_idle() { let mut runs = ActiveReflectionRuns::default(); runs.settle("agent-test", "ghost-run"); assert!(!runs.is_active("agent-test")); }
    #[test]
    fn current_returns_launch_facts() { assert_eq!(started().current("agent-test"), Some(&ActiveReflectionRun { run_id: "run-1".into(), details: details() })); }
    #[test]
    fn settled_current_is_absent() { let mut runs = started(); runs.settle("agent-test", "run-1"); assert!(runs.current("agent-test").is_none()); }
    #[test]
    fn clear_for_shutdown() { let mut runs = started(); runs.start("agent-test", "run-2", details()); runs.clear("agent-test"); assert!(!runs.is_active("agent-test")); }
    #[test]
    fn current_preserves_insertion_order() { let mut runs = started(); runs.start("agent-test", "run-0", details()); runs.start("agent-test", "run-1", details()); assert_eq!(runs.current("agent-test").unwrap().run_id, "run-1"); }
}
