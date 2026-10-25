use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::{Arc, Condvar, Mutex, PoisonError},
    task::{Context, Poll, Waker},
};

use maho_ext_api::{
    EvalHandleCallContext, EvalHandleCancelReceipt, EvalHandleError, EvalHandleKind,
    EvalHandleOutcome, EvalHandleOutputRequest, EvalHandleOutputSnapshot, EvalHandlePhase,
    EvalHandleRef, EvalHandleSnapshot, EvalHandleUpdateStream, EvalHandleWatch, ExtensionFailure,
    ExtensionFuture,
};
use senpi_task::{
    manager::TaskManager,
    state::{TaskRecord, TaskStatus},
    steering::{CancelOptions, SendInput},
    workpool::{
        engine::WorkpoolEngine,
        types::{WorkpoolCaller, WorkpoolRecord, WorkpoolStatus},
    },
};

#[derive(Default)]
pub struct TaskMutationObserver {
    state: Mutex<ObserverState>,
    changed: Condvar,
}
#[derive(Default)]
struct ObserverState {
    revision: u64,
    closed: bool,
    wakers: Vec<Waker>,
}
impl TaskMutationObserver {
    pub fn notify(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.revision = state.revision.wrapping_add(1);
        for waker in state.wakers.drain(..) {
            waker.wake();
        }
        self.changed.notify_all();
    }
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.closed = true;
        for waker in state.wakers.drain(..) {
            waker.wake();
        }
        self.changed.notify_all();
    }
    fn revision(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .revision
    }
}

pub struct NativeEvalHandleHost {
    manager: Arc<TaskManager>,
    workpools: Arc<tokio::sync::Mutex<WorkpoolEngine>>,
    observer: Arc<TaskMutationObserver>,
    owner: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}
impl NativeEvalHandleHost {
    pub fn new(
        manager: Arc<TaskManager>,
        workpools: WorkpoolEngine,
        observer: Arc<TaskMutationObserver>,
        owner: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        let changes = observer.clone();
        workpools
            .store
            .set_mutation_listener(Some(Arc::new(move || changes.notify())));
        Self {
            manager,
            workpools: Arc::new(tokio::sync::Mutex::new(workpools)),
            observer,
            owner,
        }
    }
    pub fn with_shared_workpools(
        manager: Arc<TaskManager>,
        workpools: Arc<tokio::sync::Mutex<WorkpoolEngine>>,
        observer: Arc<TaskMutationObserver>,
        owner: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Result<Self, ExtensionFailure> {
        let changes = observer.clone();
        workpools
            .try_lock()
            .map_err(|error| ExtensionFailure::new(error.to_string()))?
            .store
            .set_mutation_listener(Some(Arc::new(move || changes.notify())));
        Ok(Self {
            manager,
            workpools,
            observer,
            owner,
        })
    }
    fn caller(&self, context: &EvalHandleCallContext) -> Result<WorkpoolCaller, ExtensionFailure> {
        if (self.owner)().as_deref() != Some(context.owner_session_id.as_str()) {
            return Err(failure(
                "Forbidden",
                "The eval handle belongs to another session.",
            ));
        }
        Ok(WorkpoolCaller {
            session_id: context.owner_session_id.clone(),
            root_session_id: context.owner_session_id.clone(),
            depth: 0,
            cwd: String::new(),
        })
    }
    fn task_snapshot(
        &self,
        reference: EvalHandleRef,
        owner: &str,
    ) -> Result<EvalHandleSnapshot, ExtensionFailure> {
        let record = self
            .manager
            .get(&reference.id)
            .ok_or_else(|| failure("NotFound", "No task exists for this handle."))?;
        self.check_task(&record, &reference, owner)?;
        Ok(EvalHandleSnapshot {
            reference,
            phase: task_phase(record.status),
            host_status: format!("{:?}", record.status).to_lowercase(),
            revision: record.notification.run_epoch * 2 + i64::from(record.status.is_terminal()),
        })
    }
    fn check_task(
        &self,
        record: &TaskRecord,
        reference: &EvalHandleRef,
        owner: &str,
    ) -> Result<(), ExtensionFailure> {
        if record.parent_session_id != owner {
            return Err(failure(
                "Forbidden",
                "This handle belongs to another session.",
            ));
        }
        if reference.run_epoch < record.notification.run_epoch {
            return Err(failure("Stale", "This task has started a newer run."));
        }
        if reference.run_epoch > record.notification.run_epoch {
            return Err(failure("NotFound", "This task has not reached that run."));
        }
        Ok(())
    }
    async fn pool(
        &self,
        reference: &EvalHandleRef,
        caller: &WorkpoolCaller,
    ) -> Result<WorkpoolRecord, ExtensionFailure> {
        if reference.kind != EvalHandleKind::Workpool {
            return Err(failure(
                "Unsupported",
                "Operation requires a workpool handle.",
            ));
        }
        let engine = self.workpools.lock().await;
        let record = engine
            .inspect(caller, &reference.id)
            .map_err(workpool_failure)?;
        Ok(record)
    }
    async fn snapshot(
        &self,
        reference: EvalHandleRef,
        caller: &WorkpoolCaller,
    ) -> Result<EvalHandleSnapshot, ExtensionFailure> {
        if reference.kind == EvalHandleKind::Agent {
            return self.task_snapshot(reference, &caller.session_id);
        }
        if reference.kind == EvalHandleKind::Completion {
            return Err(failure(
                "eval_handle_operation_unsupported",
                "completion refs are not served by the task host",
            ));
        }
        let pool = self.pool(&reference, caller).await?;
        Ok(EvalHandleSnapshot {
            reference,
            phase: pool_phase(&pool),
            host_status: format!("{:?}", pool.status).to_lowercase(),
            revision: i64::from(pool_settled(&pool)),
        })
    }
}
impl maho_ext_api::EvalHandleHost for NativeEvalHandleHost {
    fn watch<'a>(
        &'a self,
        references: Vec<EvalHandleRef>,
        context: EvalHandleCallContext,
    ) -> ExtensionFuture<'a, EvalHandleWatch> {
        Box::pin(async move {
            let caller = self.caller(&context)?;
            let store = self.workpools.lock().await.store.clone();
            let abort = context.signal.clone().map(|signal| {
                Box::pin(async move {
                    signal.cancelled().await;
                }) as Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            });
            let mut updates = HostUpdateStream {
                observer: self.observer.clone(),
                references: references.clone(),
                owner: context.owner_session_id.clone(),
                manager: self.manager.clone(),
                workpools: store,
                seen: BTreeMap::new(),
                pending: Vec::new(),
                closed: false,
                last_revision: 0,
                reconcile: false,
                close_stream: Arc::new(Mutex::new(false)),
                abort,
            };
            updates.last_revision = self.observer.revision();
            let mut initial = Vec::with_capacity(references.len());
            for reference in references {
                let snapshot = self.snapshot(reference, &caller).await?;
                updates.seen.insert(
                    (
                        snapshot.reference.kind,
                        snapshot.reference.id.clone(),
                        snapshot.reference.run_epoch,
                    ),
                    snapshot.revision,
                );
                initial.push(snapshot);
            }
            updates.reconcile = true;
            let close_stream = updates.close_stream.clone();
            let observer = self.observer.clone();
            Ok(EvalHandleWatch {
                initial,
                updates: Box::pin(updates),
                close: Arc::new(move || {
                    *close_stream.lock().unwrap_or_else(PoisonError::into_inner) = true;
                    observer.notify();
                }),
            })
        })
    }
    fn result<'a>(
        &'a self,
        reference: EvalHandleRef,
        context: EvalHandleCallContext,
    ) -> ExtensionFuture<'a, EvalHandleOutcome> {
        Box::pin(async move {
            let caller = self.caller(&context)?;
            if reference.kind == EvalHandleKind::Completion {
                return Err(failure(
                    "eval_handle_operation_unsupported",
                    "completion refs are not served by the task host",
                ));
            }
            if reference.kind == EvalHandleKind::Workpool {
                let pool = self.pool(&reference, &caller).await?;
                if !pool_settled(&pool) {
                    return Err(failure("Pending", "The workpool is still running."));
                }
                if pool.status == WorkpoolStatus::Cancelled {
                    return Ok(EvalHandleOutcome::Rejected {
                        reference,
                        error: EvalHandleError {
                            code: "eval_handle_cancelled".into(),
                            message: "Workpool was cancelled".into(),
                            details: None,
                        },
                    });
                }
                let value = serde_json::Value::Array(pool.items.iter().map(|item| {
                    if item.status == senpi_task::workpool::types::WorkpoolItemStatus::Completed {
                        serde_json::json!({"key":item.key,"data":item.data})
                    } else {
                        serde_json::json!({"key":item.key,"error":item.error.as_ref().map(|error|serde_json::json!(error)).unwrap_or_else(||serde_json::json!({"code":format!("{:?}",item.status).to_lowercase(),"message":"Pool item did not complete."}))})
                    }
                }).collect());
                return Ok(EvalHandleOutcome::Fulfilled { reference, value });
            }
            let record = self
                .manager
                .get(&reference.id)
                .ok_or_else(|| failure("NotFound", "No task exists for this handle."))?;
            self.check_task(&record, &reference, &caller.session_id)?;
            if !record.status.is_terminal() {
                return Err(failure("Pending", "The task is still running."));
            }
            if record.status == TaskStatus::Completed {
                Ok(EvalHandleOutcome::Fulfilled {
                    reference,
                    value: serde_json::Value::String(record.final_response.unwrap_or_default()),
                })
            } else {
                let status = format!("{:?}", record.status).to_lowercase();
                let message = record
                    .error_message
                    .unwrap_or_else(|| format!("{} ended {status}", reference.id));
                Ok(EvalHandleOutcome::Rejected {
                    reference,
                    error: EvalHandleError {
                        code: format!("task_{status}"),
                        message,
                        details: None,
                    },
                })
            }
        })
    }
    fn send<'a>(
        &'a self,
        reference: EvalHandleRef,
        message: String,
        context: EvalHandleCallContext,
    ) -> ExtensionFuture<'a, EvalHandleSnapshot> {
        Box::pin(async move {
            let caller = self.caller(&context)?;
            if reference.kind != EvalHandleKind::Agent {
                return Err(failure(
                    "Unsupported",
                    "send is only available for agent handles.",
                ));
            }
            let record = self
                .manager
                .get(&reference.id)
                .ok_or_else(|| failure("NotFound", "No task exists for this handle."))?;
            self.check_task(&record, &reference, &caller.session_id)?;
            let outcome = self
                .manager
                .send_to_task(&SendInput {
                    id_or_name: reference.id.clone(),
                    message,
                    caller_session_id: Some(caller.session_id.clone()),
                    expected_run_epoch: Some(reference.run_epoch),
                    ..SendInput::default()
                })
                .map_err(|error| failure("SendRefused", &error.to_string()))?;
            let mut delivered = reference;
            match outcome {
                senpi_task::steering::SendOutcome::Revived { run_epoch, .. } => {
                    delivered.run_epoch = run_epoch
                }
                senpi_task::steering::SendOutcome::Steered { .. }
                | senpi_task::steering::SendOutcome::Held { .. }
                | senpi_task::steering::SendOutcome::Queued { .. } => {}
                senpi_task::steering::SendOutcome::Stale { reason, .. } => {
                    return Err(failure("Stale", &reason));
                }
                senpi_task::steering::SendOutcome::NotFound { reason, .. } => {
                    return Err(failure("NotFound", &reason));
                }
                senpi_task::steering::SendOutcome::ScopeDenied { reason, .. } => {
                    return Err(failure("Forbidden", &reason));
                }
                senpi_task::steering::SendOutcome::NotContinuable {
                    reason, suggestion, ..
                } => return Err(failure("SendRefused", &format!("{reason} {suggestion}"))),
                senpi_task::steering::SendOutcome::OneShotAgent { message, .. } => {
                    return Err(failure("SendRefused", &message));
                }
            }
            self.task_snapshot(delivered, &caller.session_id)
        })
    }
    fn cancel<'a>(
        &'a self,
        reference: EvalHandleRef,
        context: EvalHandleCallContext,
    ) -> ExtensionFuture<'a, EvalHandleCancelReceipt> {
        Box::pin(async move {
            let caller = self.caller(&context)?;
            if reference.kind == EvalHandleKind::Agent {
                let record = self
                    .manager
                    .get(&reference.id)
                    .ok_or_else(|| failure("NotFound", "No task exists for this handle."))?;
                self.check_task(&record, &reference, &caller.session_id)?;
                let outcome = self
                    .manager
                    .cancel_task(
                        &reference.id,
                        Some("cancelled from an eval handle"),
                        CancelOptions {
                            expected_run_epoch: Some(reference.run_epoch),
                            ..Default::default()
                        },
                    )
                    .map_err(|error| failure("SendRefused", &error.to_string()))?;
                if let senpi_task::steering::CancelOutcome::Stale { reason, .. } = &outcome {
                    return Err(failure("Stale", reason));
                }
                if matches!(
                    outcome,
                    senpi_task::steering::CancelOutcome::Cancelled { .. }
                ) {
                    let current = self
                        .manager
                        .get(&reference.id)
                        .ok_or_else(|| failure("NotFound", "No task exists for this handle."))?;
                    if current.notification.run_epoch != reference.run_epoch {
                        return Ok(EvalHandleCancelReceipt {
                            reference,
                            cancelled: true,
                            phase: EvalHandlePhase::Cancelled,
                        });
                    }
                }
                let snapshot = self.task_snapshot(reference.clone(), &caller.session_id)?;
                return Ok(EvalHandleCancelReceipt {
                    reference,
                    cancelled: matches!(
                        outcome,
                        senpi_task::steering::CancelOutcome::Cancelled { .. }
                            | senpi_task::steering::CancelOutcome::CancelPending { .. }
                    ),
                    phase: snapshot.phase,
                });
            }
            if reference.kind != EvalHandleKind::Workpool {
                return Err(failure(
                    "Unsupported",
                    "Completion refs are not served by the task host.",
                ));
            }
            let mut engine = self.workpools.lock().await;
            let current = engine
                .inspect(&caller, &reference.id)
                .map_err(workpool_failure)?;
            if pool_settled(&current) {
                return Ok(EvalHandleCancelReceipt {
                    reference,
                    cancelled: false,
                    phase: pool_phase(&current),
                });
            }
            let cancelled = engine
                .cancel(&caller, &reference.id)
                .await
                .map_err(workpool_failure)?;
            Ok(EvalHandleCancelReceipt {
                reference,
                cancelled: current.status != WorkpoolStatus::Cancelled,
                phase: pool_phase(&cancelled),
            })
        })
    }
    fn output<'a>(
        &'a self,
        reference: EvalHandleRef,
        request: EvalHandleOutputRequest,
        context: EvalHandleCallContext,
    ) -> ExtensionFuture<'a, EvalHandleOutputSnapshot> {
        Box::pin(async move {
            let caller = self.caller(&context)?;
            if reference.kind != EvalHandleKind::Agent {
                return Err(failure(
                    "eval_handle_operation_unsupported",
                    "output is for agent handles",
                ));
            }
            let record = self
                .manager
                .get(&reference.id)
                .ok_or_else(|| failure("NotFound", "No task exists for this handle."))?;
            self.check_task(&record, &reference, &caller.session_id)?;
            let state_dir = self.manager.state_dir().to_string_lossy();
            let read = senpi_task::tools::output::transcript::reader::default_transcript_reader(
                &senpi_task::tools::output::types::TranscriptReaderInput {
                    task_id: &reference.id,
                    state_dir: &state_dir,
                },
            )
            .map_err(|error| ExtensionFailure::new(error.to_string()))?;
            let current = self.manager.get(&reference.id).ok_or_else(|| {
                failure("eval_handle_not_found", "No task exists for this handle.")
            })?;
            self.check_task(&current, &reference, &caller.session_id)?;
            let rendered = senpi_task::tools::output::render::render_transcript(
                &read.entries,
                &senpi_task::tools::output::render::RenderOptions {
                    mode: senpi_task::tools::output::types::TranscriptMode::Full,
                    tail_lines: 0,
                },
            );
            let mut snapshot = output_snapshot(reference, rendered.text, request);
            snapshot.truncated |= rendered.truncated || read.truncated == Some(true);
            Ok(snapshot)
        })
    }
}

struct HostUpdateStream {
    observer: Arc<TaskMutationObserver>,
    references: Vec<EvalHandleRef>,
    owner: String,
    manager: Arc<TaskManager>,
    workpools: senpi_task::workpool::store::WorkpoolStore,
    seen: BTreeMap<(EvalHandleKind, String, i64), i64>,
    pending: Vec<EvalHandleSnapshot>,
    closed: bool,
    last_revision: u64,
    reconcile: bool,
    close_stream: Arc<Mutex<bool>>,
    abort: Option<Pin<Box<dyn std::future::Future<Output = ()> + Send>>>,
}
impl EvalHandleUpdateStream for HostUpdateStream {
    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<EvalHandleSnapshot>> {
        if self
            .abort
            .as_mut()
            .is_some_and(|abort| abort.as_mut().poll(cx).is_ready())
        {
            self.closed = true;
        }
        if self.closed
            || *self
                .close_stream
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
            || self
                .observer
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .closed
        {
            return Poll::Ready(None);
        }
        if let Some(snapshot) = self.pending.pop() {
            return Poll::Ready(Some(snapshot));
        }
        let revision = self.observer.revision();
        if self.reconcile || revision != self.last_revision {
            self.reconcile = false;
            self.last_revision = revision;
            let manager = self.manager.clone();
            let owner = self.owner.clone();
            let workpools = self.workpools.clone();
            let references = self.references.clone();
            for reference in &references {
                if reference.kind == EvalHandleKind::Workpool {
                    if let Ok(pool) = workpools.load(&reference.id) {
                        if pool.parent_session_id != owner {
                            continue;
                        }
                        let snapshot = EvalHandleSnapshot {
                            reference: reference.clone(),
                            phase: pool_phase(&pool),
                            host_status: format!("{:?}", pool.status).to_lowercase(),
                            revision: i64::from(pool_settled(&pool)),
                        };
                        let key = (reference.kind, reference.id.clone(), reference.run_epoch);
                        if self
                            .seen
                            .get(&key)
                            .is_none_or(|old| snapshot.revision > *old)
                        {
                            self.seen.insert(key, snapshot.revision);
                            self.pending.push(snapshot);
                        }
                    }
                    continue;
                }
                if let Some(record) = manager.get(&reference.id) {
                    if record.parent_session_id != owner {
                        continue;
                    }
                    let snapshot = if record.notification.run_epoch != reference.run_epoch {
                        EvalHandleSnapshot {
                            reference: reference.clone(),
                            phase: EvalHandlePhase::Lost,
                            host_status: "no_longer_current_run".into(),
                            revision: reference.run_epoch * 2 + 1,
                        }
                    } else {
                        EvalHandleSnapshot {
                            reference: reference.clone(),
                            phase: task_phase(record.status),
                            host_status: format!("{:?}", record.status).to_lowercase(),
                            revision: record.notification.run_epoch * 2
                                + i64::from(record.status.is_terminal()),
                        }
                    };
                    let key = (reference.kind, reference.id.clone(), reference.run_epoch);
                    if self
                        .seen
                        .get(&key)
                        .is_none_or(|old| snapshot.revision > *old)
                    {
                        self.seen.insert(key, snapshot.revision);
                        self.pending.push(snapshot);
                    }
                }
            }
            if !self.pending.is_empty() {
                return Poll::Ready(self.pending.pop());
            }
        }
        let mut state = self
            .observer
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return Poll::Ready(None);
        }
        if state.revision != self.last_revision {
            cx.waker().wake_by_ref();
        }
        if !state.wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
            state.wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

fn task_phase(status: TaskStatus) -> EvalHandlePhase {
    match status {
        TaskStatus::Pending | TaskStatus::Running | TaskStatus::Paused => EvalHandlePhase::Pending,
        TaskStatus::Completed => EvalHandlePhase::Succeeded,
        TaskStatus::Error => EvalHandlePhase::Failed,
        TaskStatus::Cancelled | TaskStatus::Interrupted => EvalHandlePhase::Cancelled,
        TaskStatus::Lost => EvalHandlePhase::Lost,
    }
}
fn pool_settled(pool: &WorkpoolRecord) -> bool {
    pool.status == WorkpoolStatus::Cancelled
        || (pool.status == WorkpoolStatus::Closing
            && pool.items.iter().all(|item| {
                matches!(
                    item.status,
                    senpi_task::workpool::types::WorkpoolItemStatus::Completed
                        | senpi_task::workpool::types::WorkpoolItemStatus::Error
                        | senpi_task::workpool::types::WorkpoolItemStatus::Cancelled
                )
            }))
}
fn pool_phase(pool: &WorkpoolRecord) -> EvalHandlePhase {
    if !pool_settled(pool) {
        EvalHandlePhase::Pending
    } else if pool.status == WorkpoolStatus::Cancelled {
        EvalHandlePhase::Cancelled
    } else {
        EvalHandlePhase::Succeeded
    }
}
fn failure(code: &str, message: &str) -> ExtensionFailure {
    let code = match code {
        "Forbidden" => "eval_handle_forbidden",
        "NotFound" => "eval_handle_not_found",
        "Stale" => "eval_handle_stale",
        "Pending" => "eval_handle_pending",
        "Unsupported" => "eval_handle_operation_unsupported",
        "SendRefused" => "eval_handle_send_refused",
        code => code,
    };
    ExtensionFailure::classified(message, Some("EvalHandleError".into()), Some(code.into()))
}
fn workpool_failure(error: senpi_task::workpool::types::WorkpoolError) -> ExtensionFailure {
    failure(&format!("{:?}", error.code), &error.message)
}
fn output_snapshot(
    reference: EvalHandleRef,
    text: String,
    request: EvalHandleOutputRequest,
) -> EvalHandleOutputSnapshot {
    let lines = if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect::<Vec<_>>()
    };
    let total = lines.len();
    let start = if request.tail {
        total.saturating_sub(request.limit.unwrap_or(60))
    } else {
        request.offset.unwrap_or(0).min(total)
    };
    let end = start
        .saturating_add(request.limit.unwrap_or(total))
        .min(total);
    EvalHandleOutputSnapshot {
        reference,
        text: lines[start..end].join("\n"),
        offset: start,
        total,
        truncated: start > 0 || end < total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_request_applies_offset_and_limit() {
        let result = output_snapshot(
            EvalHandleRef {
                kind: EvalHandleKind::Agent,
                id: "task".into(),
                run_epoch: 0,
            },
            "a\nb\nc\nd\ne\nf".into(),
            EvalHandleOutputRequest {
                tail: false,
                offset: Some(2),
                limit: Some(3),
            },
        );
        assert_eq!(result.text, "c\nd\ne");
        assert_eq!(result.offset, 2);
        assert_eq!(result.total, 6);
        assert!(result.truncated);
    }

    #[test]
    fn output_tail_returns_final_characters() {
        let result = output_snapshot(
            EvalHandleRef {
                kind: EvalHandleKind::Agent,
                id: "task".into(),
                run_epoch: 0,
            },
            "a\nb\nc\nd\ne\nf".into(),
            EvalHandleOutputRequest {
                tail: true,
                offset: None,
                limit: Some(2),
            },
        );
        assert_eq!(result.text, "e\nf");
        assert_eq!(result.offset, 4);
    }
}
