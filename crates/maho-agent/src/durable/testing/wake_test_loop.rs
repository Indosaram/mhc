//! Port of senpi `packages/agent/src/durable/testing/wake-test-loop.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use serde_json::json;

use crate::durable::store::{DurableStore, DurableStoreError, StoreWrite};
use crate::durable::store_admission::{AdmissionLine, create_admission_line};
use crate::durable::wake_host::Wake;

const TASK_PREFIX: &str = "task/";

pub type CommittedHook =
    Arc<dyn Fn() -> BoxFuture<'static, Result<(), DurableStoreError>> + Send + Sync>;

/// A minimal loop over the store: tasks due at a time, each run in one commit. Enough to observe wake ordering.
#[derive(Clone)]
pub struct TestLoop {
    store: Arc<dyn DurableStore>,
    epoch: u64,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    committed: CommittedHook,
    line: AdmissionLine,
    tasks: Arc<Mutex<BTreeMap<String, i64>>>,
}

impl TestLoop {
    async fn commit(
        &self,
        writes: Vec<StoreWrite>,
        adopt: Box<dyn FnOnce() + Send>,
    ) -> Result<(), DurableStoreError> {
        self.store
            .commit(self.epoch, writes, Box::new(move |_| adopt()))
            .await?;
        (self.committed)().await
    }

    /// Schedule task `id` at `due`, in one commit.
    pub async fn schedule(&self, id: &str, due: i64) -> Result<(), DurableStoreError> {
        let id_owned = id.to_owned();
        let tasks = Arc::clone(&self.tasks);
        self.line
            .admit(|| async move {
                let tasks_adopt = Arc::clone(&tasks);
                let id_adopt = id_owned.clone();
                self.commit(
                    vec![StoreWrite::Put {
                        key: format!("{TASK_PREFIX}{id_owned}"),
                        data: json!(due),
                    }],
                    Box::new(move || {
                        tasks_adopt
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .insert(id_adopt, due);
                    }),
                )
                .await
            })
            .await
    }

    /// Run every due task, one commit each: a `ran` record and the task's removal, atomically.
    pub async fn resume(&self) -> Result<(), DurableStoreError> {
        // Yield once so any task spawned from a concurrent wake point can arm its admission.
        tokio::task::yield_now().await;
        self.line
            .admit(|| async move {
                let current_now = (self.now)();
                let mut due: Vec<(String, i64)> = {
                    let guard = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
                    guard
                        .iter()
                        .filter(|(_, at)| **at <= current_now)
                        .map(|(id, at)| (id.clone(), *at))
                        .collect()
                };
                due.sort_by(|(id_a, at_a), (id_b, at_b)| {
                    at_a.cmp(at_b).then_with(|| id_a.cmp(id_b))
                });
                for (id, _) in due {
                    let writes = vec![
                        StoreWrite::Append {
                            id: format!("ran:{id}"),
                            kind: "ran".to_owned(),
                            data: json!({ "id": id }),
                        },
                        StoreWrite::Delete {
                            key: format!("{TASK_PREFIX}{id}"),
                        },
                    ];
                    let tasks_adopt = Arc::clone(&self.tasks);
                    let id_adopt = id.clone();
                    self.commit(
                        writes,
                        Box::new(move || {
                            tasks_adopt
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .remove(&id_adopt);
                        }),
                    )
                    .await?;
                }
                Ok(())
            })
            .await
    }

    /// `active` while a task is due now; `at`, the earliest task due later.
    pub fn wake(&self) -> Wake {
        let time = (self.now)();
        let guard = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        let mut active = false;
        let mut at: Option<i64> = None;
        for &due in guard.values() {
            if due <= time {
                active = true;
            } else if at.is_none_or(|prev| due < prev) {
                at = Some(due);
            }
        }
        Wake { active, at }
    }
}

pub async fn open_test_loop(
    store: Arc<dyn DurableStore>,
    epoch: u64,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    committed: CommittedHook,
) -> Result<TestLoop, DurableStoreError> {
    let line = create_admission_line();
    let mut tasks = BTreeMap::new();
    for document in store.scan_documents(TASK_PREFIX).await? {
        if let Some(due) = document.data.as_i64()
            && let Some(key) = document.key.strip_prefix(TASK_PREFIX)
        {
            tasks.insert(key.to_owned(), due);
        }
    }
    Ok(TestLoop {
        store,
        epoch,
        now,
        committed,
        line,
        tasks: Arc::new(Mutex::new(tasks)),
    })
}
