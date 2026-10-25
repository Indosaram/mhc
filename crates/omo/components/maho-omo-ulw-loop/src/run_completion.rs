use std::collections::HashSet;
#[derive(Clone, Debug)]
pub struct PlanSnapshot {
    pub created_at: Option<String>,
    pub aggregate_complete: bool,
}
#[derive(Default)]
pub struct RunCompletionTracker {
    seen_running: HashSet<String>,
    seen_complete: HashSet<String>,
    read_before: bool,
}
impl RunCompletionTracker {
    pub fn observe(&mut self, plan: Option<PlanSnapshot>) -> Option<String> {
        let first = !self.read_before;
        self.read_before = true;
        let plan = plan?;
        let id = plan.created_at?;
        if !plan.aggregate_complete {
            self.seen_running.insert(id);
            return None;
        }
        if !self.seen_complete.insert(id.clone()) || (!self.seen_running.contains(&id) && first) {
            return None;
        }
        Some(format!("ulw-loop:{id}"))
    }
}
pub fn plan_snapshot(value: &serde_json::Value) -> Option<PlanSnapshot> {
    if value["ok"] != true || !value["plan"].is_object() {
        return None;
    }
    Some(PlanSnapshot {
        created_at: value["plan"]["createdAt"].as_str().map(str::to_owned),
        aggregate_complete: value["plan"]["aggregateCompletion"]["status"] == "complete",
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn plan(id: &str, complete: bool) -> Option<PlanSnapshot> {
        Some(PlanSnapshot {
            created_at: Some(id.into()),
            aggregate_complete: complete,
        })
    }
    #[test]
    fn transitions_inheritance_absence_and_reset() {
        let mut t = RunCompletionTracker::default();
        assert_eq!(t.observe(plan("old", true)), None);
        assert_eq!(t.observe(plan("old", true)), None);
        assert_eq!(t.observe(plan("active", false)), None);
        assert_eq!(
            t.observe(plan("active", true)).as_deref(),
            Some("ulw-loop:active")
        );
        assert_eq!(t.observe(plan("active", true)), None);
        assert_eq!(
            t.observe(plan("reset", true)).as_deref(),
            Some("ulw-loop:reset")
        );
        let mut absent = RunCompletionTracker::default();
        assert_eq!(absent.observe(None), None);
        assert_eq!(
            absent.observe(plan("new", true)).as_deref(),
            Some("ulw-loop:new")
        );
    }
}
