//! Turn-relative queued-input age tracking (queued-input-age.ts).
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlivedQueuedInput {
    pub queued_id: String,
    pub age_ms: i64,
}

#[derive(Debug, Default)]
pub struct QueuedInputAges {
    runs_started: u64,
    queued_at: HashMap<String, (u64, i64)>,
    reported: HashSet<String>,
}

impl QueuedInputAges {
    pub fn note_run_started(&mut self) {
        self.runs_started += 1;
    }
    pub fn note_queued(&mut self, queued_id: &str, now_ms: i64) {
        self.queued_at
            .insert(queued_id.into(), (self.runs_started, now_ms));
    }
    /// Call only after a normally completed turn, not after aborts or failures.
    pub fn take_outlived(
        &mut self,
        still_queued: &[String],
        now_ms: i64,
    ) -> Vec<OutlivedQueuedInput> {
        self.queued_at.retain(|id, _| {
            if still_queued.contains(id) {
                true
            } else {
                self.reported.remove(id);
                false
            }
        });
        let mut outlived = Vec::new();
        for id in still_queued {
            if let Some(&(run, at)) = self.queued_at.get(id)
                && run < self.runs_started && self.reported.insert(id.clone()) {
                    outlived.push(OutlivedQueuedInput {
                        queued_id: id.clone(),
                        age_ms: now_ms - at,
                    });
                }
        }
        outlived
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_once_only_after_a_later_turn_and_cleans_consumed_inputs() {
        let mut ages = QueuedInputAges::default();
        let ids = vec!["input".into()];
        ages.note_queued("input", 10);
        assert!(ages.take_outlived(&ids, 20).is_empty());
        ages.note_run_started();
        assert_eq!(
            ages.take_outlived(&ids, 30),
            vec![OutlivedQueuedInput {
                queued_id: "input".into(),
                age_ms: 20
            }]
        );
        assert!(ages.take_outlived(&ids, 40).is_empty());
        ages.take_outlived(&[], 50);
        ages.note_queued("input", 60);
        ages.note_run_started();
        assert_eq!(ages.take_outlived(&ids, 70)[0].age_ms, 10);
    }
}
