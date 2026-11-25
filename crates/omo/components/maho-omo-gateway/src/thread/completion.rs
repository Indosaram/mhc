//! Completion tracking across `agent_end` and `agent_settled` (`completion.ts`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl CompletionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentEndFacts {
    pub aborted: bool,
    pub last_assistant_stop_reason: Option<String>,
    pub last_error_message: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// Maps the final `agent_end` event onto a completion outcome.
pub fn completion_outcome(facts: &AgentEndFacts) -> CompletionOutcome {
    if facts.aborted {
        return CompletionOutcome::Cancelled;
    }
    match facts.last_assistant_stop_reason.as_deref() {
        Some("aborted") => CompletionOutcome::Cancelled,
        Some("error") => CompletionOutcome::Failed,
        _ => CompletionOutcome::Completed,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmittedCompletion {
    pub binding_id: String,
    pub cursor: i64,
}

pub type SettleFn = Arc<
    dyn Fn(
            String,
            CompletionOutcome,
            i64,
        )
            -> Pin<Box<dyn Future<Output = Result<Vec<EmittedCompletion>, SettleError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
pub struct SettleError {
    pub is_lock_wait_exceeded: bool,
    pub message: String,
}

#[derive(Debug, Clone, Copy)]
struct Arm {
    generation: u64,
    through_arm_seq: i64,
}

#[derive(Debug, Clone, Copy)]
struct SettledRun {
    generation: u64,
    through_arm_seq: i64,
    outcome: CompletionOutcome,
}

struct TrackerInner {
    last_outcome: HashMap<String, CompletionOutcome>,
    armed: HashMap<String, Arm>,
    writing: HashSet<String>,
    waiting: HashMap<String, VecDeque<SettledRun>>,
    retries: HashMap<String, JoinHandle<()>>,
    generation: u64,
    disposed: bool,
}

pub type WriteFailedCallback = Arc<dyn Fn(&str, bool) + Send + Sync>;

/// Tracks armed completion watches per session and flushes them once the run settles.
#[derive(Clone)]
pub struct CompletionTracker {
    inner: Arc<Mutex<TrackerInner>>,
    settle: SettleFn,
    retry_after_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    on_write_failed: Option<WriteFailedCallback>,
}

impl CompletionTracker {
    pub fn new(
        settle: SettleFn,
        retry_after_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
        on_write_failed: Option<WriteFailedCallback>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TrackerInner {
                last_outcome: HashMap::new(),
                armed: HashMap::new(),
                writing: HashSet::new(),
                waiting: HashMap::new(),
                retries: HashMap::new(),
                generation: 0,
                disposed: false,
            })),
            settle,
            retry_after_ms,
            on_write_failed,
        }
    }

    pub fn arm(&self, durable_id: &str, arm_seq: i64) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.generation += 1;
            let gen_id = guard.generation;
            let prev = guard
                .armed
                .get(durable_id)
                .map_or(arm_seq, |a| a.through_arm_seq);
            guard.armed.insert(
                durable_id.to_owned(),
                Arm {
                    generation: gen_id,
                    through_arm_seq: prev.max(arm_seq),
                },
            );
        }
    }

    pub fn agent_end(&self, durable_id: &str, facts: &AgentEndFacts) {
        let outcome = completion_outcome(facts);
        if let Ok(mut guard) = self.inner.lock() {
            guard.last_outcome.insert(durable_id.to_owned(), outcome);
        }
    }

    pub async fn settled(&self, durable_id: &str) -> Result<Vec<EmittedCompletion>, SettleError> {
        let run = {
            let Ok(mut guard) = self.inner.lock() else {
                return Ok(Vec::new());
            };
            let Some(outcome) = guard.last_outcome.remove(durable_id) else {
                return Ok(Vec::new());
            };
            let Some(arm) = guard.armed.get(durable_id).copied() else {
                return Ok(Vec::new());
            };
            let run = SettledRun {
                generation: arm.generation,
                through_arm_seq: arm.through_arm_seq,
                outcome,
            };
            if guard.writing.contains(durable_id) {
                guard
                    .waiting
                    .entry(durable_id.to_owned())
                    .or_default()
                    .push_back(run);
                return Ok(Vec::new());
            }
            guard.writing.insert(durable_id.to_owned());
            run
        };
        self.write_run(durable_id.to_owned(), run).await
    }

    fn write_run<'a>(
        &'a self,
        durable_id: String,
        run: SettledRun,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<EmittedCompletion>, SettleError>> + Send + 'a>>
    {
        Box::pin(async move {
            let res = (self.settle)(durable_id.clone(), run.outcome, run.through_arm_seq).await;
            match res {
                Ok(emitted) => {
                    let next_run = {
                        let Ok(mut guard) = self.inner.lock() else {
                            return Ok(emitted);
                        };
                        if guard
                            .armed
                            .get(&durable_id)
                            .is_some_and(|a| a.generation == run.generation)
                        {
                            guard.armed.remove(&durable_id);
                        }
                        pop_next_run(&mut guard, &durable_id)
                    };
                    if let Some(next_run) = next_run {
                        let tracker = self.clone();
                        let id_clone = durable_id.clone();
                        tokio::spawn(async move {
                            let _ = tracker.write_run(id_clone, next_run).await;
                        });
                    }
                    Ok(emitted)
                }
                Err(err) => {
                    let retrying = {
                        let Ok(guard) = self.inner.lock() else {
                            return Err(err);
                        };
                        err.is_lock_wait_exceeded && !guard.disposed
                    };
                    if let Some(cb) = &self.on_write_failed {
                        cb(&err.message, retrying);
                    }
                    if retrying {
                        let delay = (self.retry_after_ms)();
                        let tracker = self.clone();
                        let id_clone = durable_id.clone();
                        let handle = tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                            if let Ok(mut guard) = tracker.inner.lock() {
                                guard.retries.remove(&id_clone);
                                if guard.disposed {
                                    guard.writing.remove(&id_clone);
                                    return;
                                }
                            }
                            let _ = tracker.write_run(id_clone, run).await;
                        });
                        if let Ok(mut guard) = self.inner.lock() {
                            guard.retries.insert(durable_id, handle);
                        }
                    } else if let Some(next_run) = self
                        .inner
                        .lock()
                        .ok()
                        .and_then(|mut g| pop_next_run(&mut g, &durable_id))
                    {
                        let tracker = self.clone();
                        tokio::spawn(async move {
                            let _ = tracker.write_run(durable_id, next_run).await;
                        });
                    }
                    Err(err)
                }
            }
        })
    }

    pub fn dispose(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.disposed = true;
            for (_, handle) in guard.retries.drain() {
                handle.abort();
            }
            guard.waiting.clear();
        }
    }
}

fn pop_next_run(guard: &mut TrackerInner, durable_id: &str) -> Option<SettledRun> {
    let mut found = None;
    if let Some(queue) = guard.waiting.get_mut(durable_id) {
        while let Some(candidate) = queue.pop_front() {
            if guard.armed.contains_key(durable_id) {
                found = Some(candidate);
                break;
            }
        }
        if queue.is_empty() {
            guard.waiting.remove(durable_id);
        }
    }
    if found.is_none() || guard.disposed {
        guard.writing.remove(durable_id);
        return None;
    }
    found
}
