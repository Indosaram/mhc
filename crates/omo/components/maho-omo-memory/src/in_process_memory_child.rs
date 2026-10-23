//! Runs one memory child in-process under a deadline and a cancel signal (pin
//! `in-process-memory-child.ts`). The handle surface is the blocking `ManagedChildHandle`, so its
//! waits and aborts run on the blocking pool.
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use senpi_task::manager::ManagedChildHandle;
use senpi_task::runners::RunnerOutcome;
use tokio::sync::{OnceCell, watch};

pub const DEFAULT_DEADLINE_MS: u64 = 5 * 60_000;

pub type MemoryChildHandle = Arc<dyn ManagedChildHandle>;
pub type MemoryChildWarn = Arc<dyn Fn(&str) + Send + Sync>;
pub type OnMemoryChildHandle = Box<dyn FnOnce(&MemoryChildHandle) + Send>;
pub type OnMemoryChildState = Box<dyn FnOnce(&InProcessMemoryChildState) + Send>;

/// `released` settles (turns `true`) once a child that started (even after the run returned) has been disposed.
#[derive(Default)]
pub struct InProcessMemoryChildState {
    pub cancelled: Arc<AtomicBool>,
    pub cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    pub released: Option<watch::Receiver<bool>>,
}

impl InProcessMemoryChildState {
    /// Resolves when the child (if any) has been disposed; immediately when no run ever started.
    pub async fn wait_released(&self) {
        if let Some(released) = &self.released {
            let mut released = released.clone();
            let _ = released.wait_for(|done| *done).await;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryChildFailure {
    SessionCreateFailed,
    Deadline,
    Cancelled,
    ChildFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryChildResult {
    Completed,
    Failed(MemoryChildFailure),
}

pub type SetupFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

pub struct RunInProcessMemoryChildInput<'a> {
    pub run_id: String,
    pub deadline_ms: Option<u64>,
    pub state: &'a mut InProcessMemoryChildState,
    pub warn: Option<MemoryChildWarn>,
    pub setup: SetupFuture,
    /// Builds and starts the child (pin `createInProcessJudgeRunner(...).start(buildStart(...))`).
    pub start: Box<dyn FnOnce() -> Result<MemoryChildHandle, String> + Send>,
    pub on_handle: Option<OnMemoryChildHandle>,
    pub on_state: Option<OnMemoryChildState>,
    pub on_setup_settled: Option<Box<dyn Fn() + Send + Sync>>,
}

#[derive(Clone)]
struct Settled {
    raw: Result<Option<MemoryChildHandle>, String>,
    processed: Result<Option<MemoryChildHandle>, String>,
}

/// Disposes a handle at most once; the first caller decides whether the turn is aborted first.
#[derive(Clone, Default)]
struct Disposal {
    cell: Arc<OnceCell<()>>,
    started: Arc<AtomicBool>,
}

impl Disposal {
    async fn dispose(
        &self,
        handle: MemoryChildHandle,
        abort: bool,
        warn: Option<MemoryChildWarn>,
        run_id: String,
    ) {
        self.cell
            .get_or_init(|| async {
                self.started.store(true, Ordering::SeqCst);
                if abort {
                    abort_and_dispose(handle, warn, run_id).await
                } else {
                    let _ = handle.dispose();
                }
            })
            .await;
    }
    async fn settled(&self) {
        if self.started.load(Ordering::SeqCst) {
            self.cell.get_or_init(|| async {}).await;
        }
    }
}

/// Aborts the child's turn, then disposes it even when the abort itself failed.
async fn abort_and_dispose(
    handle: MemoryChildHandle,
    warn: Option<MemoryChildWarn>,
    run_id: String,
) {
    let aborting = handle.clone();
    match tokio::task::spawn_blocking(move || aborting.abort()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            if let Some(warn) = &warn {
                warn(&format!(
                    "memory child abort failed: {} (run {run_id})",
                    error.message
                ));
            }
        }
        Err(error) => {
            if let Some(warn) = &warn {
                warn(&format!(
                    "memory child abort failed: {error} (run {run_id})"
                ));
            }
        }
    }
    let _ = handle.dispose();
}

pub async fn run_in_process_memory_child(
    input: RunInProcessMemoryChildInput<'_>,
) -> MemoryChildResult {
    let RunInProcessMemoryChildInput {
        run_id,
        deadline_ms,
        state,
        warn,
        setup,
        start,
        on_handle,
        on_state,
        on_setup_settled,
    } = input;
    let deadline_reached = Arc::new(AtomicBool::new(false));
    let cancelled_flag = state.cancelled.clone();
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    {
        let cancelled_flag = cancelled_flag.clone();
        state.cancel = Some(Arc::new(move || {
            cancelled_flag.store(true, Ordering::SeqCst);
            let _ = cancel_tx.send(true);
        }));
    }
    if let Some(on_state) = on_state {
        on_state(state);
    }
    let (deadline_tx, mut deadline_rx) = watch::channel(false);
    let deadline_task = {
        let reached = deadline_reached.clone();
        let wait = Duration::from_millis(deadline_ms.unwrap_or(DEFAULT_DEADLINE_MS));
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            reached.store(true, Ordering::SeqCst);
            let _ = deadline_tx.send(true);
        })
    };
    let expired = {
        let reached = deadline_reached.clone();
        let cancelled = cancelled_flag.clone();
        Arc::new(move || reached.load(Ordering::SeqCst) || cancelled.load(Ordering::SeqCst))
    };
    let disposal = Disposal::default();
    let (settled_tx, settled_rx) = watch::channel::<Option<Settled>>(None);
    tokio::spawn({
        let expired = expired.clone();
        let disposal = disposal.clone();
        let warn = warn.clone();
        let run_id = run_id.clone();
        async move {
            let raw: Result<Option<MemoryChildHandle>, String> = async {
                setup.await?;
                let expired_now = expired();
                if let Some(settled) = &on_setup_settled {
                    settled();
                }
                if expired_now {
                    return Ok(None);
                }
                let handle = tokio::task::spawn_blocking(start)
                    .await
                    .map_err(|error| error.to_string())??;
                Ok(Some(handle))
            }
            .await;
            let processed = match &raw {
                Ok(None) => Ok(None),
                Ok(Some(handle)) if expired() => {
                    disposal.dispose(handle.clone(), true, warn, run_id).await;
                    Ok(None)
                }
                Ok(Some(handle)) => {
                    if let Some(on_handle) = on_handle {
                        on_handle(handle);
                    }
                    Ok(Some(handle.clone()))
                }
                Err(_) if expired() => Ok(None),
                Err(error) => Err(error.clone()),
            };
            let _ = settled_tx.send(Some(Settled { raw, processed }));
        }
    });
    let wait_settled = |mut rx: watch::Receiver<Option<Settled>>| async move {
        let settled = rx
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|settled| settled.clone());
        settled.unwrap_or(Settled {
            raw: Err("memory child setup was dropped".into()),
            processed: Err("memory child setup was dropped".into()),
        })
    };
    enum Race {
        Settled(Settled),
        Deadline,
        Cancelled,
    }
    let result = async {
        let race = tokio::select! {
            biased;
            settled = wait_settled(settled_rx.clone()) => Race::Settled(settled),
            _ = deadline_rx.wait_for(|reached| *reached) => Race::Deadline,
            _ = cancel_rx.wait_for(|cancelled| *cancelled) => Race::Cancelled,
        };
        let handle = match race {
            Race::Deadline => {
                if let Some(cancel) = &state.cancel { cancel(); }
                let settled_rx = settled_rx.clone();
                let disposal = disposal.clone();
                let warn = warn.clone();
                let run_id = run_id.clone();
                tokio::spawn(async move {
                    if let Ok(Some(handle)) = wait_settled(settled_rx).await.raw { disposal.dispose(handle, true, warn, run_id).await; }
                });
                return MemoryChildResult::Failed(MemoryChildFailure::Deadline);
            }
            Race::Cancelled => {
                match wait_settled(settled_rx.clone()).await.processed {
                    Ok(Some(handle)) => disposal.dispose(handle, true, warn.clone(), run_id.clone()).await,
                    _ => disposal.settled().await,
                }
                return MemoryChildResult::Failed(MemoryChildFailure::Cancelled);
            }
            Race::Settled(settled) => match settled.processed {
                Ok(Some(handle)) => handle,
                Ok(None) => return MemoryChildResult::Failed(if deadline_reached.load(Ordering::SeqCst) { MemoryChildFailure::Deadline } else { MemoryChildFailure::Cancelled }),
                Err(error) => {
                    if let Some(warn) = &warn { warn(&format!("memory child session creation failed: {error} (run {run_id})")); }
                    return MemoryChildResult::Failed(MemoryChildFailure::SessionCreateFailed);
                }
            },
        };
        let waiting = handle.clone();
        let mut turn = tokio::task::spawn_blocking(move || waiting.wait_for_outcome());
        tokio::select! {
            biased;
            outcome = &mut turn => match outcome {
                Ok(RunnerOutcome::Completed { .. }) => MemoryChildResult::Completed,
                Ok(_) => MemoryChildResult::Failed(MemoryChildFailure::ChildFailed),
                Err(error) => {
                    if let Some(warn) = &warn { warn(&format!("memory child session creation failed: {error} (run {run_id})")); }
                    MemoryChildResult::Failed(MemoryChildFailure::SessionCreateFailed)
                }
            },
            _ = deadline_rx.wait_for(|reached| *reached) => {
                cancelled_flag.store(true, Ordering::SeqCst);
                disposal.dispose(handle, true, warn.clone(), run_id.clone()).await;
                MemoryChildResult::Failed(MemoryChildFailure::Deadline)
            }
            _ = cancel_rx.wait_for(|cancelled| *cancelled) => {
                cancelled_flag.store(true, Ordering::SeqCst);
                disposal.dispose(handle, true, warn.clone(), run_id.clone()).await;
                MemoryChildResult::Failed(MemoryChildFailure::Cancelled)
            }
        }
    }.await;
    deadline_task.abort();
    state.cancel = None;
    let (released_tx, released_rx) = watch::channel(false);
    state.released = Some(released_rx);
    tokio::spawn(async move {
        if let Ok(Some(handle)) = wait_settled(settled_rx).await.raw {
            disposal.dispose(handle, expired(), warn, run_id).await;
        }
        let _ = released_tx.send(true);
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        state: &'a mut InProcessMemoryChildState,
        deadline_ms: Option<u64>,
        setup: SetupFuture,
    ) -> RunInProcessMemoryChildInput<'a> {
        RunInProcessMemoryChildInput {
            run_id: "run".into(),
            deadline_ms,
            state,
            warn: None,
            setup,
            start: Box::new(|| Err("must not start".into())),
            on_handle: None,
            on_state: None,
            on_setup_settled: None,
        }
    }

    #[tokio::test]
    async fn a_failing_setup_is_a_session_create_failure_and_releases() {
        let mut state = InProcessMemoryChildState::default();
        let result = run_in_process_memory_child(input(
            &mut state,
            None,
            Box::pin(async { Err("boom".into()) }),
        ))
        .await;
        assert_eq!(
            result,
            MemoryChildResult::Failed(MemoryChildFailure::SessionCreateFailed)
        );
        assert!(state.cancel.is_none());
        state.wait_released().await;
    }

    #[tokio::test]
    async fn an_elapsed_deadline_wins_over_a_setup_that_never_settles() {
        let mut state = InProcessMemoryChildState::default();
        let result = run_in_process_memory_child(input(
            &mut state,
            Some(0),
            Box::pin(std::future::pending()),
        ))
        .await;
        assert_eq!(
            result,
            MemoryChildResult::Failed(MemoryChildFailure::Deadline)
        );
        assert!(state.cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancel_during_setup_fails_cancelled() {
        // Like the pin, a cancel waits for the in-flight setup to settle before it reports, so the
        // setup is released only after `cancel()` ran: cancel lands strictly during setup.
        let mut state = InProcessMemoryChildState::default();
        let (setup_tx, setup_rx) = tokio::sync::oneshot::channel::<()>();
        let setup: SetupFuture = Box::pin(async move { setup_rx.await.map_err(|error| error.to_string()) });
        let mut run = input(&mut state, None, setup);
        run.on_state = Some(Box::new(move |state| {
            let cancel = state.cancel.clone().unwrap();
            tokio::spawn(async move {
                cancel();
                let _ = setup_tx.send(());
            });
        }));
        assert_eq!(
            run_in_process_memory_child(run).await,
            MemoryChildResult::Failed(MemoryChildFailure::Cancelled)
        );
    }
}
