//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-worker-pool.ts` and
//! `packages/coding-agent/src/modes/rpc/worker-capacity.ts`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::loopless_runtime_settings::LooplessCliConfiguration;
use crate::loopless_worker_client::LooplessWorker;
use crate::session_worker_protocol::SESSION_WORKER_LIMITS;

pub type WorkerCountFn = Arc<dyn Fn() -> usize + Send + Sync>;

/// One process's session-worker budget (`SESSION_WORKER_LIMITS.workers`), shared by every pool
/// that starts session-worker isolates (`Z2`, `senpi#5413`).
#[derive(Default)]
pub struct WorkerCapacity {
    counts: Mutex<Vec<(u64, WorkerCountFn)>>,
    next_id: AtomicU64,
}

pub struct WorkerCapacityRegistration {
    capacity: Arc<WorkerCapacity>,
    id: u64,
}

impl Drop for WorkerCapacityRegistration {
    fn drop(&mut self) {
        self.capacity
            .counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, _)| *id != self.id);
    }
}

impl WorkerCapacity {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn track(self: &Arc<Self>, count: WorkerCountFn) -> WorkerCapacityRegistration {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((id, count));
        WorkerCapacityRegistration {
            capacity: Arc::clone(self),
            id,
        }
    }

    pub fn in_use(&self) -> usize {
        self.counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, f)| f())
            .sum()
    }
}

pub type CreateLooplessWorkerFn =
    Arc<dyn Fn() -> Result<Arc<dyn LooplessWorker>, String> + Send + Sync>;

pub struct LooplessWorkerPoolOptions {
    pub configuration: LooplessCliConfiguration,
    /// The process's one worker budget; the session registry of the same process shares it.
    pub capacity: Arc<WorkerCapacity>,
    /// How long a closing worker may dispose its runtime before it is terminated.
    pub close_grace_ms: u64,
    /// Creates one worker isolate (production or deterministic test double).
    pub create_worker: CreateLooplessWorkerFn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenLooplessWorker {
    pub id: String,
    pub tools: Vec<String>,
}

/// Loopless workers (`senpi#5413`): one session-worker isolate per session, each hosting a
/// runtime with the agent loop removed. Every worker, opening ones included, counts against the
/// process's worker cap (`SESSION_WORKER_LIMITS.workers`).
pub struct LooplessWorkerPool {
    workers: Arc<Mutex<HashMap<String, Arc<dyn LooplessWorker>>>>,
    configuration: LooplessCliConfiguration,
    capacity: Arc<WorkerCapacity>,
    close_grace_ms: u64,
    create_worker: CreateLooplessWorkerFn,
    serial: AtomicU64,
    _registration: WorkerCapacityRegistration,
}

impl LooplessWorkerPool {
    pub fn new(options: LooplessWorkerPoolOptions) -> Self {
        let workers: Arc<Mutex<HashMap<String, Arc<dyn LooplessWorker>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let workers_count = Arc::clone(&workers);
        let registration = options.capacity.track(Arc::new(move || {
            workers_count
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
        }));
        Self {
            workers,
            configuration: options.configuration,
            capacity: options.capacity,
            close_grace_ms: options.close_grace_ms,
            create_worker: options.create_worker,
            serial: AtomicU64::new(0),
            _registration: registration,
        }
    }

    pub fn size(&self) -> usize {
        self.workers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub async fn open(&self, cwd: impl Into<String>) -> Result<OpenLooplessWorker, String> {
        if self.capacity.in_use() >= SESSION_WORKER_LIMITS.workers {
            return Err("too_many_sessions".to_owned());
        }
        let seq = self.serial.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("loopless-{seq}");
        let worker = (self.create_worker)()?;
        self.workers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.clone(), Arc::clone(&worker));

        let workers_cleanup = Arc::clone(&self.workers);
        let id_cleanup = id.clone();
        let worker_cleanup = Arc::clone(&worker);
        tokio::spawn(async move {
            worker_cleanup.wait_exited().await;
            let mut guard = workers_cleanup
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if guard
                .get(&id_cleanup)
                .is_some_and(|cur| Arc::ptr_eq(cur, &worker_cleanup))
            {
                guard.remove(&id_cleanup);
            }
        });

        match worker.start(self.configuration.clone(), cwd.into()).await {
            Ok(tools) => Ok(OpenLooplessWorker { id, tools }),
            Err(err) => {
                worker.close(self.close_grace_ms).await;
                self.workers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                Err(err)
            }
        }
    }

    pub async fn close(&self, id: &str) -> Result<(), String> {
        let worker = self
            .workers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| "unknown_session".to_owned())?;
        worker.close(self.close_grace_ms).await;
        self.workers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::Notify;

    struct MockWorker {
        exited: AtomicBool,
        notify: Notify,
        fail_start: Option<String>,
    }

    impl MockWorker {
        fn new(fail_start: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                exited: AtomicBool::new(false),
                notify: Notify::new(),
                fail_start: fail_start.map(str::to_owned),
            })
        }
    }

    #[async_trait]
    impl LooplessWorker for MockWorker {
        async fn wait_exited(&self) {
            loop {
                let n = self.notify.notified();
                if self.is_exited() {
                    return;
                }
                n.await;
            }
        }
        fn is_exited(&self) -> bool {
            self.exited.load(Ordering::SeqCst)
        }
        async fn start(
            &self,
            _configuration: LooplessCliConfiguration,
            _cwd: String,
        ) -> Result<Vec<String>, String> {
            match &self.fail_start {
                Some(err) => Err(err.clone()),
                None => Ok(vec!["read".into(), "write".into()]),
            }
        }
        async fn close(&self, _grace_ms: u64) {
            self.exited.store(true, Ordering::SeqCst);
            self.notify.notify_waiters();
        }
    }

    #[tokio::test]
    async fn enforces_shared_worker_cap_and_cleans_up_failed_opens() {
        let capacity = WorkerCapacity::new();
        let _external = capacity.track(Arc::new(|| SESSION_WORKER_LIMITS.workers - 1));

        let pool = LooplessWorkerPool::new(LooplessWorkerPoolOptions {
            configuration: LooplessCliConfiguration::default(),
            capacity: Arc::clone(&capacity),
            close_grace_ms: 50,
            create_worker: Arc::new(|| Ok(MockWorker::new(None))),
        });

        let open = pool.open("/w1").await.expect("fits cap");
        assert_eq!(pool.size(), 1);
        assert_eq!(capacity.in_use(), SESSION_WORKER_LIMITS.workers);

        let full = pool.open("/w2").await.expect_err("cap reached");
        assert_eq!(full, "too_many_sessions");

        pool.close(&open.id).await.expect("close");
        assert_eq!(pool.size(), 0);

        let failing_pool = LooplessWorkerPool::new(LooplessWorkerPoolOptions {
            configuration: LooplessCliConfiguration::default(),
            capacity: Arc::clone(&capacity),
            close_grace_ms: 50,
            create_worker: Arc::new(|| Ok(MockWorker::new(Some("boom")))),
        });
        assert_eq!(
            failing_pool.open("/w3").await.expect_err("start failed"),
            "boom"
        );
        assert_eq!(failing_pool.size(), 0);
    }
}
