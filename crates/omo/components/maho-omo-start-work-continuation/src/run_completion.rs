use boulder_state::{BoulderWorkStatus, get_work_for_session, read_boulder_state};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[derive(Default)]
pub struct RunCompletionTracker {
    last_seen: HashMap<String, BoulderWorkStatus>,
    reported: HashSet<String>,
    looked: HashSet<String>,
}
impl RunCompletionTracker {
    pub fn observe(&mut self, cwd: &Path, session_id: &str) -> Option<String> {
        if cwd.join(".omo/boulder.json").exists() && !matches!(read_boulder_state(cwd), Ok(Some(_)))
        {
            return None;
        }
        let work = get_work_for_session(cwd, &format!("senpi:{session_id}")).ok()?;
        let first = self
            .looked
            .insert(format!("{}\0{session_id}", cwd.display()));
        let work = work?;
        self.observe_work(
            work.work_id()?,
            work.status().unwrap_or(BoulderWorkStatus::Active),
            first,
        )
    }
    fn observe_work(&mut self, id: &str, status: BoulderWorkStatus, first: bool) -> Option<String> {
        let previous = self.last_seen.insert(id.to_owned(), status);
        if status != BoulderWorkStatus::Completed || self.reported.contains(id) {
            return None;
        }
        if !matches!(
            previous,
            Some(BoulderWorkStatus::Active | BoulderWorkStatus::Paused)
        ) && (previous.is_some() || first)
        {
            return None;
        }
        self.reported.insert(id.to_owned());
        Some(format!("ulw-execute:{id}"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn write_work(root: &Path, status: &str, owner: &str) {
        std::fs::create_dir_all(root.join(".omo")).unwrap();
        std::fs::write(root.join(".omo/boulder.json"), serde_json::json!({"schema_version":2,"active_work_id":"w1","works":{"w1":{"work_id":"w1","active_plan":".omo/plans/t.md","plan_name":"t","session_ids":[owner],"status":status,"started_at":"2026-10-24T00:00:00Z"}}}).to_string()).unwrap();
    }
    #[test]
    fn disk_reads_distinguish_absence_unknown_and_foreign_work() {
        let root = tempfile::tempdir().unwrap();
        let mut tracker = RunCompletionTracker::default();
        assert_eq!(tracker.observe(root.path(), "s1"), None);
        write_work(root.path(), "completed", "senpi:s1");
        assert_eq!(
            tracker.observe(root.path(), "s1").as_deref(),
            Some("ulw-execute:w1")
        );
        assert_eq!(tracker.observe(root.path(), "s1"), None);
        let mut unknown = RunCompletionTracker::default();
        std::fs::write(root.path().join(".omo/boulder.json"), "{broken").unwrap();
        assert_eq!(unknown.observe(root.path(), "s1"), None);
        write_work(root.path(), "completed", "senpi:s1");
        assert_eq!(unknown.observe(root.path(), "s1"), None);
        let mut foreign = RunCompletionTracker::default();
        write_work(root.path(), "active", "senpi:other");
        assert_eq!(foreign.observe(root.path(), "s1"), None);
        write_work(root.path(), "completed", "senpi:other");
        assert_eq!(foreign.observe(root.path(), "s1"), None);
    }

    #[test]
    fn observed_transition_and_new_work_complete_once() {
        for initial in [BoulderWorkStatus::Active, BoulderWorkStatus::Paused] {
            let mut tracker = RunCompletionTracker::default();
            assert_eq!(tracker.observe_work("w", initial, true), None);
            assert_eq!(
                tracker
                    .observe_work("w", BoulderWorkStatus::Completed, false)
                    .as_deref(),
                Some("ulw-execute:w")
            );
            assert_eq!(
                tracker.observe_work("w", BoulderWorkStatus::Completed, false),
                None
            );
        }
        let mut tracker = RunCompletionTracker::default();
        assert_eq!(
            tracker.observe_work("inherited", BoulderWorkStatus::Completed, true),
            None
        );
        assert_eq!(
            tracker
                .observe_work("new", BoulderWorkStatus::Completed, false)
                .as_deref(),
            Some("ulw-execute:new")
        );
        assert_eq!(
            tracker.observe_work("abandoned", BoulderWorkStatus::Abandoned, false),
            None
        );
    }
}
