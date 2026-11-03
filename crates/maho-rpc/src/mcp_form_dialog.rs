//! One private MCP form entry (senpi `mcp-form-dialog.ts`): prepared -> presented -> settled, a single terminal
//! latch, a host-random token, and human admission only on a presenter action.
use crate::mcp_form_types::{McpFormInput, McpFormOutcome, McpFormOutcomeKind, McpFormSource};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// senpi `McpFormClock`: a monotonic `now` in milliseconds and cancellable one-shot timers.
pub trait McpFormClock: Send + Sync {
    fn now(&self) -> f64;
    fn schedule(&self, callback: Box<dyn FnOnce() + Send>, delay_ms: f64) -> u64;
    fn cancel(&self, timer: u64);
}

/// senpi `mcpFormClock`: `performance.now()` and tokio timers.
#[derive(Default)]
pub struct TokioFormClock {
    timers: Mutex<std::collections::BTreeMap<u64, tokio::task::JoinHandle<()>>>,
    next: std::sync::atomic::AtomicU64,
}
fn origin() -> std::time::Instant {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(std::time::Instant::now)
}
impl McpFormClock for TokioFormClock {
    fn now(&self) -> f64 {
        origin().elapsed().as_secs_f64() * 1000.
    }
    fn schedule(&self, callback: Box<dyn FnOnce() + Send>, delay_ms: f64) -> u64 {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let delay = std::time::Duration::from_secs_f64(delay_ms.max(0.) / 1000.);
        let task = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            callback();
        });
        lock(&self.timers).insert(id, task);
        id
    }
    fn cancel(&self, timer: u64) {
        if let Some(task) = lock(&self.timers).remove(&timer) {
            task.abort();
        }
    }
}
pub fn mcp_form_clock() -> Arc<dyn McpFormClock> {
    static CLOCK: std::sync::OnceLock<Arc<TokioFormClock>> = std::sync::OnceLock::new();
    CLOCK.get_or_init(Arc::default).clone()
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// senpi `McpPresenterReply`.
#[derive(Debug, Clone, PartialEq)]
pub enum McpPresenterReply {
    Ok(Option<Value>),
    Error(&'static str),
}
impl McpPresenterReply {
    pub fn to_value(&self) -> Value {
        match self {
            Self::Ok(None) => json!({"ok":true}),
            Self::Ok(Some(form)) => json!({"ok":true,"form":form}),
            Self::Error(error) => json!({"error":error}),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    PresentationLost,
    PresentationTimeout,
    OperationAborted,
}

/// The routing envelope an entry is sent under (senpi `McpFormRequest` minus the per-dialog fields).
#[derive(Debug, Clone)]
pub struct McpFormEnvelope {
    pub session_id: String,
    pub binding_epoch: String,
    pub attachment_epoch: String,
    pub grant_id: String,
    pub dialog_id: String,
    pub form_revision: String,
}

pub struct McpFormDialogContext {
    pub clock: Arc<dyn McpFormClock>,
    pub envelope: McpFormEnvelope,
    pub current: Box<dyn Fn() -> bool + Send + Sync>,
    pub remove: Box<dyn Fn() + Send + Sync>,
    pub admit: Arc<dyn Fn() + Send + Sync>,
    pub send: Arc<dyn Fn(&Value) -> bool + Send + Sync>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Prepared,
    Presented,
    Settled,
}
struct DialogState {
    stage: Stage,
    request: Option<Value>,
    token: [u8; 32],
    cleanups: Vec<Box<dyn FnOnce() + Send>>,
    resolve: Option<tokio::sync::oneshot::Sender<McpFormOutcome>>,
}

/// One private entry. Lifecycle closures retain this object even after its grant is removed.
pub struct McpFormDialog {
    pub deadline: f64,
    input: McpFormInput,
    context: McpFormDialogContext,
    state: Mutex<DialogState>,
}

impl McpFormDialog {
    pub fn new(
        input: McpFormInput,
        context: McpFormDialogContext,
    ) -> (Arc<Self>, tokio::sync::oneshot::Receiver<McpFormOutcome>) {
        let mut token = [0u8; 32];
        // A failed draw keeps the zero token, which `matches` can never accept after the latch either.
        let _ = getrandom::fill(&mut token);
        let deadline_ms = input.wire.deadline_ms.max(0.);
        let deadline = context.clock.now() + deadline_ms;
        let envelope = &context.envelope;
        let request = json!({
            "sessionId": envelope.session_id,
            "bindingEpoch": envelope.binding_epoch,
            "attachmentEpoch": envelope.attachment_epoch,
            "grantId": envelope.grant_id,
            "dialogId": envelope.dialog_id,
            "formRevision": envelope.form_revision,
            "type": "mcp_form_request",
            "dialogToken": hex::encode(token),
            "server": input.wire.server,
            "message": input.wire.message,
            "schema": input.wire.schema,
            "deadlineAtMs": crate::connection_handler::now_ms() + deadline_ms,
        });
        let (resolve, result) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                deadline,
                input,
                context,
                state: Mutex::new(DialogState {
                    stage: Stage::Prepared,
                    request: Some(request),
                    token,
                    cleanups: Vec::new(),
                    resolve: Some(resolve),
                }),
            }),
            result,
        )
    }

    pub fn start(self: &Arc<Self>) {
        let watch = |signal: &maho_ai::utils::abort::AbortSignal, cause: Termination| {
            let weak = Arc::downgrade(self);
            let id = signal.add_abort_listener(move |_| {
                if let Some(dialog) = weak.upgrade() {
                    dialog.terminate(cause);
                }
            });
            let owned = signal.clone();
            lock(&self.state)
                .cleanups
                .push(Box::new(move || owned.remove_abort_listener(id)));
            if signal.aborted() {
                self.terminate(cause);
            }
        };
        watch(&self.input.operation_signal, Termination::OperationAborted);
        watch(
            &self.input.presentation_signal,
            Termination::PresentationLost,
        );
        if lock(&self.state).stage == Stage::Settled {
            let cleanups = std::mem::take(&mut lock(&self.state).cleanups);
            for cleanup in cleanups {
                cleanup();
            }
            return;
        }
        let weak = Arc::downgrade(self);
        let timer = self.context.clock.schedule(
            Box::new(move || {
                if let Some(dialog) = weak.upgrade() {
                    dialog.terminate(Termination::PresentationTimeout);
                }
            }),
            self.input.wire.deadline_ms,
        );
        let clock = Arc::clone(&self.context.clock);
        lock(&self.state)
            .cleanups
            .push(Box::new(move || clock.cancel(timer)));
        let request = lock(&self.state).request.clone();
        let sent = (self.context.current)()
            && request.is_some_and(|request| (self.context.send)(&request));
        if !sent {
            self.terminate(Termination::PresentationLost);
        }
    }

    pub fn sync(&self) -> Option<Value> {
        if !self.live() {
            return None;
        }
        lock(&self.state).request.clone()
    }

    pub fn matches(&self, token: &str, revision: &str) -> bool {
        use subtle::ConstantTimeEq;
        let state = lock(&self.state);
        let Some(request) = &state.request else {
            return false;
        };
        if request["formRevision"].as_str() != Some(revision)
            || token.len() != 64
            || !token
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return false;
        }
        hex::decode(token).is_ok_and(|bytes| bool::from(state.token.as_slice().ct_eq(&bytes)))
    }

    pub fn presented(&self) -> McpPresenterReply {
        if !self.live() {
            return McpPresenterReply::Error("form_expired");
        }
        lock(&self.state).stage = Stage::Presented;
        McpPresenterReply::Ok(None)
    }

    pub fn respond(&self, action: &str, content: Option<&Value>) -> McpPresenterReply {
        if !self.live() {
            return McpPresenterReply::Error("form_expired");
        }
        let schema = {
            let state = lock(&self.state);
            match (&state.request, state.stage) {
                (Some(request), Stage::Presented) => request["schema"].clone(),
                _ => return McpPresenterReply::Error("invalid_form_response"),
            }
        };
        let result = match action {
            "accept" => {
                let Some(content) = content else {
                    return McpPresenterReply::Error("invalid_form_response");
                };
                if !crate::mcp_form_schema::valid_mcp_form_content(&schema, content)
                    || content.to_string().len() > 1024 * 1024
                {
                    return McpPresenterReply::Error("invalid_form_response");
                }
                json!({"action":action,"content":content})
            }
            "decline" | "cancel" => {
                if content.is_some() {
                    return McpPresenterReply::Error("invalid_form_response");
                }
                json!({"action":action})
            }
            _ => return McpPresenterReply::Error("invalid_form_response"),
        };
        if !self.live() {
            return McpPresenterReply::Error("form_expired");
        }
        self.commit(McpFormOutcome {
            kind: McpFormOutcomeKind::Result(result),
            source: McpFormSource::PresenterAction,
        });
        McpPresenterReply::Ok(None)
    }

    /// Internal-only: never called with a cause or entry reference decoded from RPC.
    pub fn terminate(&self, cause: Termination) {
        if cause == Termination::OperationAborted || self.input.operation_stale() {
            self.commit(McpFormOutcome::operation_aborted());
        } else if lock(&self.state).stage == Stage::Presented {
            self.commit(McpFormOutcome::terminated(McpFormOutcomeKind::Result(
                json!({"action":"cancel"}),
            )));
        } else {
            self.commit(McpFormOutcome::processing_failure());
        }
    }

    fn live(&self) -> bool {
        if lock(&self.state).stage == Stage::Settled {
            return false;
        }
        if self.input.operation_stale() {
            self.terminate(Termination::OperationAborted);
            return false;
        }
        if self.context.clock.now() >= self.deadline || !(self.context.current)() {
            self.terminate(Termination::PresentationTimeout);
            return false;
        }
        true
    }

    fn commit(&self, outcome: McpFormOutcome) {
        let (request, cleanups, resolve) = {
            let mut state = lock(&self.state);
            if state.stage == Stage::Settled {
                return;
            }
            state.stage = Stage::Settled;
            state.token = [0; 32];
            (
                state.request.take(),
                std::mem::take(&mut state.cleanups),
                state.resolve.take(),
            )
        };
        (self.context.remove)();
        for cleanup in cleanups {
            cleanup();
        }
        // No callback, resolution or admission precedes the synchronous terminal claim.
        if outcome.human_admission() {
            (self.context.admit)();
        }
        let reason = match &outcome.kind {
            McpFormOutcomeKind::Result(result) => match result["action"].as_str() {
                Some("accept") => "answered",
                Some("decline") => "declined",
                _ => "cancelled",
            },
            McpFormOutcomeKind::OperationAborted => "operation_aborted",
            McpFormOutcomeKind::ProcessingFailure => "processing_failed",
        };
        if let Some(resolve) = resolve {
            let _ = resolve.send(outcome);
        }
        if let Some(request) = request
            && (self.context.current)()
        {
            (self.context.send)(&json!({
                "type": "mcp_form_resolved",
                "sessionId": request["sessionId"],
                "grantId": request["grantId"],
                "dialogId": request["dialogId"],
                "reason": reason,
            }));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mcp_form_types::McpFormInputWire;
    use maho_ai::utils::abort::AbortController;

    type TimerCallback = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;
    type TimerMap = std::collections::BTreeMap<u64, TimerCallback>;

    /// senpi test fixture clock: settable time, recorded timers that only `expire` fires.
    #[derive(Default)]
    pub struct ManualClock {
        pub now: Mutex<f64>,
        pub timers: Mutex<TimerMap>,
        next: std::sync::atomic::AtomicU64,
    }
    impl McpFormClock for ManualClock {
        fn now(&self) -> f64 {
            *lock(&self.now)
        }
        fn schedule(&self, callback: Box<dyn FnOnce() + Send>, _: f64) -> u64 {
            let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            lock(&self.timers).insert(id, Arc::new(Mutex::new(Some(callback))));
            id
        }
        fn cancel(&self, timer: u64) {
            lock(&self.timers).remove(&timer);
        }
    }
    impl ManualClock {
        pub fn expire(&self, at: f64) {
            *lock(&self.now) = at;
            let pending: Vec<_> = lock(&self.timers).values().cloned().collect();
            for timer in pending {
                let callback = lock(&timer).take();
                if let Some(callback) = callback {
                    callback();
                }
            }
        }
        pub fn size(&self) -> usize {
            lock(&self.timers).len()
        }
    }

    pub fn input(operation: &AbortController) -> McpFormInput {
        McpFormInput {
            wire: McpFormInputWire {
                operation_id: "operation".into(),
                input_id: "input".into(),
                round: 0.,
                server: json!({"label":"server-canary","source":"user"}),
                message: "message-canary".into(),
                schema: json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}),
                deadline_ms: 100.,
            },
            operation_signal: operation.signal(),
            operation_current: None,
            presentation_signal: AbortController::new().signal(),
        }
    }

    #[tokio::test]
    async fn a_presented_entry_settles_once_and_admits_only_on_presenter_action() {
        let clock = Arc::new(ManualClock::default());
        let sent = Arc::new(Mutex::new(Vec::<Value>::new()));
        let admitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let operation = AbortController::new();
        let (sink, count) = (Arc::clone(&sent), Arc::clone(&admitted));
        let (dialog, result) = McpFormDialog::new(
            input(&operation),
            McpFormDialogContext {
                clock: clock.clone(),
                envelope: McpFormEnvelope {
                    session_id: "s".into(),
                    binding_epoch: "b".into(),
                    attachment_epoch: "a".into(),
                    grant_id: "g".into(),
                    dialog_id: "d".into(),
                    form_revision: "r".into(),
                },
                current: Box::new(|| true),
                remove: Box::new(|| {}),
                admit: Arc::new(move || {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }),
                send: Arc::new(move |frame| {
                    lock(&sink).push(frame.clone());
                    true
                }),
            },
        );
        dialog.start();
        let token = lock(&sent)[0]["dialogToken"].as_str().unwrap().to_owned();
        assert!(dialog.matches(&token, "r"));
        assert!(!dialog.matches(&token, "other"));
        assert_eq!(
            dialog.respond("accept", Some(&json!({"value":"x"}))),
            McpPresenterReply::Error("invalid_form_response")
        );
        assert_eq!(dialog.presented(), McpPresenterReply::Ok(None));
        assert_eq!(
            dialog.respond("decline", Some(&json!({}))),
            McpPresenterReply::Error("invalid_form_response")
        );
        assert_eq!(dialog.respond("decline", None), McpPresenterReply::Ok(None));
        let outcome = result.await.unwrap();
        assert!(outcome.human_admission());
        assert_eq!(admitted.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(lock(&sent)[1]["reason"], "declined");
        assert!(!dialog.matches(&token, "r"));
        assert_eq!(clock.size(), 0);
        dialog.terminate(Termination::OperationAborted);
        assert_eq!(lock(&sent).len(), 2);
    }
}
