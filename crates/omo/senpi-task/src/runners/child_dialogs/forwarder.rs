//! Async-safe forwarding of child UI requests (`runners/child-dialogs/forwarder.ts`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use super::forwardable_dialog::{
    CANCELLED_CHILD_DIALOG, ForwardableDialog, ForwardedAnswer, forwardable_dialog_of,
    forwarded_ui_response,
};
use crate::runners::rpc::ui_auto_answer::build_auto_ui_response;

#[derive(Default)]
struct SignalInner {
    aborted: AtomicBool,
    listeners: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

/// Thread-safe abort token shared between parent link lifecycle events and forwarded UI dialogs.
#[derive(Clone, Default)]
pub struct ChildDialogSignal {
    inner: Arc<SignalInner>,
}

impl std::fmt::Debug for ChildDialogSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildDialogSignal")
            .field("aborted", &self.is_aborted())
            .finish()
    }
}

impl ChildDialogSignal {
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::Acquire)
    }

    pub fn on_abort(&self, callback: impl FnOnce() + Send + 'static) {
        let mut listeners = self
            .inner
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.inner.aborted.load(Ordering::Acquire) {
            drop(listeners);
            callback();
        } else {
            listeners.push(Box::new(callback));
        }
    }

    pub fn abort(&self) {
        if self.inner.aborted.swap(true, Ordering::AcqRel) {
            return;
        }
        let callbacks = std::mem::take(
            &mut *self
                .inner
                .listeners
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for cb in callbacks {
            cb();
        }
    }
}

#[derive(Clone)]
pub struct ForwardedChild {
    pub task_id: String,
    pub signal: ChildDialogSignal,
}

/// Raises a child's dialog on the parent session's UI and returns what the person answered.
pub trait ChildDialogForwarder: Send + Sync {
    fn forward(&self, dialog: ForwardableDialog, child: ForwardedChild) -> ForwardedAnswer;
}

#[derive(Clone)]
pub struct ChildUiAnswering {
    pub forwarder: Option<Arc<dyn ChildDialogForwarder>>,
    pub task_id: String,
}

/// Answers one child UI request without blocking the child's record stream: forwardable dialogs run
/// on a worker thread against the parent's UI; non-forwardable or unforwarded dialogs get the safe
/// auto-answer immediately.
pub fn answer_child_ui_request<F>(
    frame: &Value,
    answering: Option<&ChildUiAnswering>,
    signal: &ChildDialogSignal,
    send: F,
) where
    F: FnOnce(Value) + Send + 'static,
{
    let forwarder =
        answering.and_then(|a| a.forwarder.as_ref().map(|f| (f.clone(), a.task_id.clone())));
    let dialog = forwarder
        .as_ref()
        .and_then(|_| forwardable_dialog_of(frame));

    let (Some((forwarder, task_id)), Some(dialog)) = (forwarder, dialog) else {
        if let Some(answer) = build_auto_ui_response(frame) {
            send(answer);
        }
        return;
    };

    let id = frame.get("id").cloned().unwrap_or(Value::Null);
    let signal = signal.clone();
    let method = frame
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let spawn = std::thread::Builder::new()
        .name(format!("child-ui-{task_id}"))
        .spawn(move || {
            let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                forwarder.forward(
                    dialog,
                    ForwardedChild {
                        task_id: task_id.clone(),
                        signal: signal.clone(),
                    },
                )
            }))
            .unwrap_or_else(|_| {
                utils::logger::log(
                    "senpi-task child dialog forward panicked",
                    Some(&serde_json::json!({ "taskId": task_id, "method": method })),
                );
                CANCELLED_CHILD_DIALOG
            });
            if signal.is_aborted() {
                return;
            }
            send(forwarded_ui_response(&id, &answer));
        });
    if let Err(error) = spawn {
        utils::logger::log(
            "senpi-task child ui answer thread failed",
            Some(&serde_json::json!({ "error": error.to_string() })),
        );
    }
}
