//! The worker's half of private MCP forms (senpi `mcp-form-worker.ts`): bind through the host, present through
//! the host's grant, and settle only on the host's provenance. Private worker messages never pass through the
//! output writer or a snapshot.
use crate::mcp_form_dialog::lock;
use crate::mcp_form_types::{
    Listener, McpFormBinding, McpFormOutcome, McpFormPresentation, PrivateFormsRegistration,
};
use crate::session_worker_protocol::{
    HostToSessionWorker, McpFormCancelLifetime, SessionWorkerWire,
};
use maho_ai::utils::abort::{AbortController, ListenerId};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

const BIND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
const SETTLEMENT_MEMORY: usize = 256;

type Send = Arc<dyn Fn(SessionWorkerWire) + std::marker::Send + Sync>;
type Admit = Arc<dyn Fn() + std::marker::Send + Sync>;

struct BindingState {
    version: Option<String>,
    active: bool,
    admit: Admit,
    listeners: BTreeMap<u64, Listener>,
}
struct Entry {
    epoch: String,
    resolve: tokio::sync::oneshot::Sender<McpFormOutcome>,
    cleanup: Box<dyn FnOnce() + std::marker::Send>,
    /// The host-observable operation fence: set before the cancel frame is posted.
    fence: Arc<std::sync::atomic::AtomicBool>,
}
struct Bound {
    epoch: String,
    version: Option<String>,
}

#[derive(Default)]
struct State {
    binds: BTreeMap<String, tokio::sync::oneshot::Sender<Bound>>,
    ready: BTreeMap<String, tokio::sync::oneshot::Sender<()>>,
    settlements: VecDeque<String>,
    bindings: BTreeMap<String, Arc<Mutex<BindingState>>>,
    entries: BTreeMap<String, Entry>,
    next: u64,
}

/// senpi `McpFormWorker`.
#[derive(Clone)]
pub struct McpFormWorker {
    send: Send,
    state: Arc<Mutex<State>>,
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl McpFormWorker {
    pub fn new(send: Send) -> Self {
        Self {
            send,
            state: Arc::default(),
        }
    }

    /// Binds this worker session's private forms through the host and registers them for the durable session.
    /// The returned registration is the binding's dispose: dropping it releases the host binding.
    pub async fn bind(
        &self,
        durable_id: &str,
        admit: Admit,
    ) -> Result<WorkerFormsBinding, &'static str> {
        let id = uuid();
        let (bound_tx, bound_rx) = tokio::sync::oneshot::channel();
        lock(&self.state).binds.insert(id.clone(), bound_tx);
        (self.send)(SessionWorkerWire::McpFormBind {
            id: id.clone(),
            durable_id: durable_id.to_owned(),
        });
        let bound = tokio::time::timeout(BIND_DEADLINE, bound_rx).await;
        lock(&self.state).binds.remove(&id);
        let Ok(Ok(bound)) = bound else {
            return Err("private_form_unavailable");
        };
        let lifetime = AbortController::new();
        let state = Arc::new(Mutex::new(BindingState {
            version: bound.version,
            active: true,
            admit,
            listeners: BTreeMap::new(),
        }));
        lock(&self.state)
            .bindings
            .insert(bound.epoch.clone(), Arc::clone(&state));
        let epoch = bound.epoch;
        let (subscribe_state, subscribe_shared) = (Arc::clone(&state), Arc::clone(&self.state));
        let (capture_state, capture_worker, capture_epoch) =
            (Arc::clone(&state), self.clone(), epoch.clone());
        let (dispose_state, dispose_worker, dispose_epoch, dispose_lifetime) = (
            Arc::clone(&state),
            self.clone(),
            epoch.clone(),
            lifetime.clone(),
        );
        let binding = Arc::new(McpFormBinding {
            id: McpFormBinding::next_id(),
            epoch: epoch.clone(),
            signal: lifetime.signal(),
            version: None,
            subscribe: Some(Box::new(move |listener| {
                let key = {
                    let mut shared = lock(&subscribe_shared);
                    shared.next += 1;
                    shared.next
                };
                lock(&subscribe_state).listeners.insert(key, listener);
                let state = Arc::clone(&subscribe_state);
                Box::new(move || {
                    lock(&state).listeners.remove(&key);
                })
            })),
            capture: Box::new(move || capture_worker.capture(&capture_state, &capture_epoch)),
            dispose: Box::new(move || {
                dispose_lifetime.abort(None);
                {
                    let mut state = lock(&dispose_state);
                    if !state.active {
                        return;
                    }
                    state.active = false;
                    state.listeners.clear();
                }
                {
                    let mut shared = lock(&dispose_worker.state);
                    for entry in shared.entries.values() {
                        if entry.epoch == dispose_epoch {
                            entry.fence.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                    shared.bindings.remove(&dispose_epoch);
                }
                (dispose_worker.send)(SessionWorkerWire::McpFormRelease {
                    epoch: dispose_epoch.clone(),
                });
            }),
        });
        let registration =
            crate::mcp_form_types::bind_private_mcp_forms(durable_id, Arc::clone(&binding));
        Ok(WorkerFormsBinding {
            binding,
            _registration: registration,
        })
    }

    fn capture(
        &self,
        state: &Arc<Mutex<BindingState>>,
        epoch: &str,
    ) -> Option<McpFormPresentation> {
        let version = {
            let state = lock(state);
            if !state.active {
                return None;
            }
            state.version.clone()?
        };
        let (ready_state, ready_version) = (Arc::clone(state), version.clone());
        let (sync_worker, sync_epoch) = (self.clone(), epoch.to_owned());
        let (present_worker, present_epoch) = (self.clone(), epoch.to_owned());
        Some(McpFormPresentation {
            signal: AbortController::new().signal(),
            ready: Arc::new(move || {
                let state = lock(&ready_state);
                state.active && state.version.as_deref() == Some(ready_version.as_str())
            }),
            synchronize: Some(Arc::new(move || {
                let request = uuid();
                let (tx, rx) = tokio::sync::oneshot::channel();
                lock(&sync_worker.state).ready.insert(request.clone(), tx);
                (sync_worker.send)(SessionWorkerWire::McpFormReady {
                    id: request,
                    epoch: sync_epoch.clone(),
                });
                Box::pin(async move {
                    let _ = rx.await;
                })
            })),
            present: Arc::new(move |input| {
                let (worker, epoch, version) = (
                    present_worker.clone(),
                    present_epoch.clone(),
                    version.clone(),
                );
                Box::pin(async move { worker.present(epoch, version, input).await })
            }),
        })
    }

    async fn present(
        &self,
        epoch: String,
        version: String,
        input: crate::mcp_form_types::McpFormInput,
    ) -> McpFormOutcome {
        let request = uuid();
        let fence = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (resolve, result) = tokio::sync::oneshot::channel();
        let listen = |signal: &maho_ai::utils::abort::AbortSignal,
                      lifetime: McpFormCancelLifetime|
         -> ListenerId {
            let (send, request, epoch, fence) = (
                Arc::clone(&self.send),
                request.clone(),
                epoch.clone(),
                Arc::clone(&fence),
            );
            signal.add_abort_listener(move |_| {
                if lifetime == McpFormCancelLifetime::Operation {
                    fence.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                send(SessionWorkerWire::McpFormCancel {
                    id: request,
                    epoch,
                    lifetime,
                });
            })
        };
        let operation = listen(&input.operation_signal, McpFormCancelLifetime::Operation);
        let presentation = listen(
            &input.presentation_signal,
            McpFormCancelLifetime::Presentation,
        );
        let (operation_signal, presentation_signal) = (
            input.operation_signal.clone(),
            input.presentation_signal.clone(),
        );
        lock(&self.state).entries.insert(
            request.clone(),
            Entry {
                epoch: epoch.clone(),
                resolve,
                fence: Arc::clone(&fence),
                cleanup: Box::new(move || {
                    operation_signal.remove_abort_listener(operation);
                    presentation_signal.remove_abort_listener(presentation);
                }),
            },
        );
        if input.operation_signal.aborted() {
            fence.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        (self.send)(SessionWorkerWire::McpFormCreate {
            id: request.clone(),
            epoch: epoch.clone(),
            version,
            input: input.wire.clone(),
        });
        // Listeners added to an already aborted signal never fire, so a pre-aborted input cancels here.
        if input.operation_signal.aborted() {
            (self.send)(SessionWorkerWire::McpFormCancel {
                id: request.clone(),
                epoch: epoch.clone(),
                lifetime: McpFormCancelLifetime::Operation,
            });
        }
        if input.presentation_signal.aborted() {
            (self.send)(SessionWorkerWire::McpFormCancel {
                id: request,
                epoch,
                lifetime: McpFormCancelLifetime::Presentation,
            });
        }
        result
            .await
            .unwrap_or_else(|_| McpFormOutcome::operation_aborted())
    }

    /// Consumes the private host messages first; `false` hands anything else to the ordinary worker dispatch.
    pub fn receive(&self, message: &HostToSessionWorker) -> bool {
        match message {
            HostToSessionWorker::McpFormBound { id, epoch, version } => {
                let resolve = lock(&self.state).binds.remove(id);
                match resolve {
                    Some(resolve) => {
                        if resolve
                            .send(Bound {
                                epoch: epoch.clone(),
                                version: version.clone(),
                            })
                            .is_err()
                        {
                            (self.send)(SessionWorkerWire::McpFormRelease {
                                epoch: epoch.clone(),
                            });
                        }
                    }
                    None => (self.send)(SessionWorkerWire::McpFormRelease {
                        epoch: epoch.clone(),
                    }),
                }
                true
            }
            HostToSessionWorker::McpFormAvailability { epoch, version, id } => {
                let binding = lock(&self.state).bindings.get(epoch).cloned();
                if let Some(binding) = binding {
                    let listeners = {
                        let mut binding = lock(&binding);
                        if binding.version != *version {
                            binding.version = version.clone();
                            binding.listeners.values().cloned().collect()
                        } else {
                            Vec::new()
                        }
                    };
                    for listener in listeners {
                        listener();
                    }
                }
                if let Some(id) = id {
                    let ready = lock(&self.state).ready.remove(id);
                    if let Some(ready) = ready {
                        let _ = ready.send(());
                    }
                }
                true
            }
            HostToSessionWorker::McpFormSettlement {
                id,
                epoch,
                settlement_id,
                outcome,
            } => {
                let (entry, binding) = {
                    let mut state = lock(&self.state);
                    let fits = state
                        .entries
                        .get(id)
                        .is_some_and(|entry| entry.epoch == *epoch);
                    if !fits || state.settlements.contains(settlement_id) {
                        return true;
                    }
                    state.settlements.push_back(settlement_id.clone());
                    if state.settlements.len() > SETTLEMENT_MEMORY {
                        state.settlements.pop_front();
                    }
                    let entry = state.entries.remove(id);
                    (entry, state.bindings.get(epoch).cloned())
                };
                let Some(entry) = entry else { return true };
                (entry.cleanup)();
                let active = binding.as_ref().map(|binding| {
                    let binding = lock(binding);
                    (binding.active, Arc::clone(&binding.admit))
                });
                let Some((true, admit)) = active else {
                    let _ = entry.resolve.send(McpFormOutcome::operation_aborted());
                    return true;
                };
                // Host-only union provenance, never inferred from cancel/decline/accept alone.
                let outcome = McpFormOutcome::from_wire(outcome)
                    .unwrap_or_else(McpFormOutcome::processing_failure);
                if outcome.human_admission() {
                    admit();
                }
                let _ = entry.resolve.send(outcome);
                true
            }
            _ => false,
        }
    }

    /// The worker process is leaving: every pending entry settles as aborted.
    pub fn abort_all(&self) {
        let entries = std::mem::take(&mut lock(&self.state).entries);
        for (_, entry) in entries {
            (entry.cleanup)();
            let _ = entry.resolve.send(McpFormOutcome::operation_aborted());
        }
    }
}

/// A bound worker form binding; dropping it (or calling `dispose`) releases the host side.
pub struct WorkerFormsBinding {
    pub binding: Arc<McpFormBinding>,
    _registration: PrivateFormsRegistration,
}
impl Drop for WorkerFormsBinding {
    fn drop(&mut self) {
        self.binding.dispose();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_form_types::{McpFormInput, McpFormInputWire, McpFormOutcomeKind};
    use serde_json::json;

    fn worker() -> (McpFormWorker, Arc<Mutex<Vec<SessionWorkerWire>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&sent);
        (
            McpFormWorker::new(Arc::new(move |message| lock(&sink).push(message))),
            sent,
        )
    }
    fn bind_id(sent: &Arc<Mutex<Vec<SessionWorkerWire>>>) -> String {
        match lock(sent).first() {
            Some(SessionWorkerWire::McpFormBind { id, .. }) => id.clone(),
            _ => String::new(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn bounds_a_missing_binding_acknowledgement_and_releases_a_late_binding() {
        let (worker, sent) = worker();
        let bind = worker.bind("durable-worker-late", Arc::new(|| {}));
        assert_eq!(bind.await.err(), Some("private_form_unavailable"));
        let id = bind_id(&sent);
        assert!(worker.receive(&HostToSessionWorker::McpFormBound {
            id,
            epoch: "late".into(),
            version: None
        }));
        assert!(matches!(
            lock(&sent).last(),
            Some(SessionWorkerWire::McpFormRelease { epoch }) if epoch == "late"
        ));
    }

    #[tokio::test]
    async fn publishes_availability_changes_and_settles_on_host_provenance() {
        let (worker, sent) = worker();
        let admitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&admitted);
        let bind = {
            let worker = worker.clone();
            tokio::spawn(async move {
                worker
                    .bind(
                        "durable-worker-settle",
                        Arc::new(move || {
                            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }),
                    )
                    .await
            })
        };
        while bind_id(&sent).is_empty() {
            tokio::task::yield_now().await;
        }
        worker.receive(&HostToSessionWorker::McpFormBound {
            id: bind_id(&sent),
            epoch: "epoch".into(),
            version: None,
        });
        let bound = bind.await.unwrap().unwrap();
        assert!(bound.binding.capture().is_none());
        let changed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = Arc::clone(&changed);
        let _unsubscribe = bound.binding.subscribe.as_ref().unwrap()(Arc::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        worker.receive(&HostToSessionWorker::McpFormAvailability {
            epoch: "epoch".into(),
            version: Some("grant".into()),
            id: None,
        });
        assert_eq!(changed.load(std::sync::atomic::Ordering::SeqCst), 1);
        let presentation = bound.binding.capture().unwrap();
        let operation = AbortController::new();
        let input = McpFormInput {
            wire: McpFormInputWire {
                operation_id: "o".into(),
                input_id: "i".into(),
                round: 0.,
                server: json!({"label":"s","source":"user"}),
                message: "m".into(),
                schema: json!({"type":"object","properties":{}}),
                deadline_ms: 100.,
            },
            operation_signal: operation.signal(),
            operation_current: None,
            presentation_signal: AbortController::new().signal(),
        };
        let pending = tokio::spawn((presentation.present)(input));
        let request = loop {
            let found = lock(&sent).iter().find_map(|message| match message {
                SessionWorkerWire::McpFormCreate { id, .. } => Some(id.clone()),
                _ => None,
            });
            if let Some(found) = found {
                break found;
            }
            tokio::task::yield_now().await;
        };
        // A forged settlement claiming admission without presenter provenance never admits.
        let settle = |outcome| HostToSessionWorker::McpFormSettlement {
            id: request.clone(),
            epoch: "epoch".into(),
            settlement_id: "s1".into(),
            outcome,
        };
        worker.receive(&settle(json!({"kind":"result","result":{"action":"accept","content":{}},"source":"host_termination","humanAdmission":true})));
        let outcome = pending.await.unwrap();
        assert_eq!(outcome.kind, McpFormOutcomeKind::ProcessingFailure);
        assert_eq!(admitted.load(std::sync::atomic::Ordering::SeqCst), 0);
        // A duplicate settlement id is ignored.
        assert!(worker.receive(&settle(json!({}))));
        drop(bound);
        assert!(matches!(
            lock(&sent).last(),
            Some(SessionWorkerWire::McpFormRelease { epoch }) if epoch == "epoch"
        ));
    }
}
