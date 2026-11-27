//! Wall-clock pass budget and store-walk cursor (`runtime/retention.json`) (#12532).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RetentionStore {
    Runs,
    Completions,
    ReflectionSessions,
    Journals,
    Sidecars,
    Ledgers,
    RecallRuns,
}

impl RetentionStore {
    pub const ALL: &[Self] = &[
        Self::Runs,
        Self::Completions,
        Self::ReflectionSessions,
        Self::Journals,
        Self::Sidecars,
        Self::Ledgers,
        Self::RecallRuns,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Runs => "runs",
            Self::Completions => "completions",
            Self::ReflectionSessions => "reflection-sessions",
            Self::Journals => "journals",
            Self::Sidecars => "sidecars",
            Self::Ledgers => "ledgers",
            Self::RecallRuns => "recall-runs",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "runs" => Some(Self::Runs),
            "completions" => Some(Self::Completions),
            "reflection-sessions" => Some(Self::ReflectionSessions),
            "journals" => Some(Self::Journals),
            "sidecars" => Some(Self::Sidecars),
            "ledgers" => Some(Self::Ledgers),
            "recall-runs" => Some(Self::RecallRuns),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionCursor {
    pub store: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
}

pub struct PassBudget<'a> {
    now_ms: &'a dyn Fn() -> i64,
    started_at: i64,
    budget_ms: i64,
    aborted_fn: Option<&'a dyn Fn() -> bool>,
    resume: Option<RetentionCursor>,
    last: Option<RetentionCursor>,
    pub stopped: Option<RetentionCursor>,
    pub aborted: bool,
    admitted_any: bool,
}

impl<'a> PassBudget<'a> {
    pub fn new(
        now_ms: &'a dyn Fn() -> i64,
        budget_ms: u64,
        aborted_fn: Option<&'a dyn Fn() -> bool>,
        resume: Option<RetentionCursor>,
    ) -> Self {
        let started_at = now_ms();
        Self {
            now_ms,
            started_at,
            budget_ms: i64::try_from(budget_ms).unwrap_or(i64::MAX),
            aborted_fn,
            resume,
            last: None,
            stopped: None,
            aborted: false,
            admitted_any: false,
        }
    }

    /// True when an earlier pass already finished this item (or its whole store).
    pub fn skip(&self, store: RetentionStore, name: &str) -> bool {
        let Some(ref resume) = self.resume else {
            return false;
        };
        let Some(resume_at) = RetentionStore::parse(&resume.store) else {
            return false;
        };
        if store > resume_at {
            return false;
        }
        if store < resume_at {
            return true;
        }
        resume.last_name.as_deref().is_some_and(|last| name <= last)
    }

    /// False once the budget is spent or the pass was aborted; cursor points at last finished item.
    pub fn admit(&mut self, store: RetentionStore) -> bool {
        if !self.proceed(store) {
            return false;
        }
        if self.admitted_any && (self.now_ms)() - self.started_at >= self.budget_ms {
            self.stop(store);
            return false;
        }
        self.admitted_any = true;
        true
    }

    /// False once aborted or already stopped; time budget never stops a metadata classification read.
    pub fn proceed(&mut self, store: RetentionStore) -> bool {
        if self.stopped.is_some() {
            return false;
        }
        if !self.aborted_fn.is_some_and(|f| f()) {
            return true;
        }
        self.aborted = true;
        self.stop(store);
        false
    }

    fn stop(&mut self, store: RetentionStore) {
        if let Some(last) = &self.last {
            self.stopped = Some(last.clone());
        } else if let Some(ref resume) = self.resume
            && RetentionStore::parse(&resume.store).is_some_and(|r| r >= store)
        {
            self.stopped = Some(resume.clone());
        } else {
            self.stopped = Some(RetentionCursor {
                store: store.as_str().to_owned(),
                last_name: None,
            });
        }
    }

    /// Milliseconds remaining in the budget (for the size cap running last).
    pub fn remaining_ms(&self) -> u64 {
        let elapsed = (self.now_ms)().saturating_sub(self.started_at);
        u64::try_from(self.budget_ms.saturating_sub(elapsed).max(0)).unwrap_or(0)
    }

    pub fn done(&mut self, store: RetentionStore, name: &str) {
        self.last = Some(RetentionCursor {
            store: store.as_str().to_owned(),
            last_name: Some(name.to_owned()),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn budget_skips_prior_items_and_resumes_at_last_done() {
        let clock = Cell::new(1000i64);
        let now = || clock.get();
        let mut budget = PassBudget::new(&now, 50, None, None);
        assert!(budget.admit(RetentionStore::Runs));
        budget.done(RetentionStore::Runs, "run-1");
        clock.set(1060);
        assert!(!budget.admit(RetentionStore::Runs));
        let stopped = budget.stopped.clone().unwrap();
        assert_eq!(stopped.store, "runs");
        assert_eq!(stopped.last_name.as_deref(), Some("run-1"));

        let resumed = PassBudget::new(&now, 50, None, Some(stopped));
        assert!(resumed.skip(RetentionStore::Runs, "run-1"));
        assert!(!resumed.skip(RetentionStore::Runs, "run-2"));
        assert!(!resumed.skip(RetentionStore::Journals, "s1"));
    }
}
