//! Parent-child dialog & approval-mode link (`runners/parent-link.ts`, `#12839`, `#12853`).

use std::sync::{Arc, Condvar, Mutex, PoisonError};

use serde_json::{Value, json};

use crate::runners::child_dialogs::forwarder::{
    ChildDialogForwarder, ChildDialogSignal, ChildUiAnswering, answer_child_ui_request,
};
use crate::runners::types::RpcRunnerSpec;

pub type PermissionUnsubscribe = Box<dyn FnOnce() + Send>;
pub type WarningRelease = Box<dyn FnOnce() + Send>;
pub type WarningCallback = Arc<dyn Fn(&str, &str) -> Option<WarningRelease> + Send + Sync>;
pub type PermissionPushFn = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// The parent session's approval mode as its children follow it (`#6189 F2c/F2e`, `#12853`).
pub trait ParentPermissionMode: Send + Sync {
    fn current(&self) -> Option<String>;
    fn on_change(&self, listener: Arc<dyn Fn() + Send + Sync>) -> PermissionUnsubscribe;
}

struct LinkState {
    abort: ChildDialogSignal,
    unfollow: Option<PermissionUnsubscribe>,
    applied: Option<String>,
    push_queued: bool,
    closed: bool,
    generation: u64,
    release_warning: Option<WarningRelease>,
}

/// Ties one child connection to its parent: forwards UI dialogs (cancelled on turn abort, close,
/// or transport loss), names the parent's approval mode on launch/open, and serializes/coalesces
/// every later mode move onto the child.
pub struct ChildParentLink {
    task_id: String,
    forwarder: Option<Arc<dyn ChildDialogForwarder>>,
    permission: Option<Arc<dyn ParentPermissionMode>>,
    on_warning: Option<WarningCallback>,
    shared: Arc<(Mutex<LinkState>, Condvar)>,
}

impl ChildParentLink {
    pub fn new(
        task_id: impl Into<String>,
        forwarder: Option<Arc<dyn ChildDialogForwarder>>,
        permission: Option<Arc<dyn ParentPermissionMode>>,
        on_warning: Option<WarningCallback>,
    ) -> Arc<Self> {
        Arc::new(Self {
            task_id: task_id.into(),
            forwarder,
            permission,
            on_warning,
            shared: Arc::new((
                Mutex::new(LinkState {
                    abort: ChildDialogSignal::default(),
                    unfollow: None,
                    applied: None,
                    push_queued: false,
                    closed: false,
                    generation: 0,
                    release_warning: None,
                }),
                Condvar::new(),
            )),
        })
    }

    pub fn answer<F>(&self, frame: &Value, send: F)
    where
        F: FnOnce(Value) + Send + 'static,
    {
        let answering = self.forwarder.as_ref().map(|forwarder| ChildUiAnswering {
            forwarder: Some(Arc::clone(forwarder)),
            task_id: self.task_id.clone(),
        });
        let signal = self
            .shared
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .abort
            .clone();
        answer_child_ui_request(frame, answering.as_ref(), &signal, send);
    }

    /// Aborts forwarded dialogs from the current turn and resets for the next turn.
    pub fn turn_aborted(&self) {
        let old = {
            let mut state = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut state.abort)
        };
        old.abort();
    }

    /// Stamps `permission_preset` on the launch spec when the parent holds one and records it as applied.
    pub fn named(&self, mut spec: RpcRunnerSpec) -> RpcRunnerSpec {
        let preset = self.permission.as_ref().and_then(|p| p.current());
        self.shared
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .applied = preset.clone();
        if let Some(preset) = preset {
            spec.permission_preset = Some(preset);
        }
        spec
    }

    /// Reads the parent's current preset and records it as the mode applied at open.
    pub fn named_preset(&self) -> Option<String> {
        let preset = self.permission.as_ref().and_then(|p| p.current());
        self.shared
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .applied = preset.clone();
        preset
    }

    /// Subscribes to parent mode moves and pushes changes serially, coalescing bursts to the
    /// latest preset. Also checks immediately in case the parent moved while the child opened.
    pub fn follow(self: &Arc<Self>, push: PermissionPushFn) {
        let Some(permission) = self.permission.clone() else {
            return;
        };
        let old_unfollow = {
            let mut state = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
            if state.closed {
                return;
            }
            state.generation += 1;
            state.push_queued = true;
            state.unfollow.take()
        };
        if let Some(unfollow) = old_unfollow {
            unfollow();
        }

        let wake_shared = Arc::clone(&self.shared);
        let unsub = permission.on_change(Arc::new(move || {
            let mut state = wake_shared.0.lock().unwrap_or_else(PoisonError::into_inner);
            if state.closed || state.unfollow.is_none() {
                return;
            }
            state.push_queued = true;
            wake_shared.1.notify_all();
        }));

        let generation = {
            let mut state = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
            if state.closed {
                drop(state);
                unsub();
                return;
            }
            state.unfollow = Some(unsub);
            state.push_queued = true;
            state.generation
        };
        self.shared.1.notify_all();

        let this = Arc::clone(self);
        let task_id = self.task_id.clone();
        let spawn = std::thread::Builder::new()
            .name(format!("parent-perm-{task_id}"))
            .spawn(move || this.run_follow_loop(permission, push, generation));
        if let Err(error) = spawn {
            utils::logger::log(
                "senpi-task permission follow thread failed",
                Some(&json!({ "taskId": task_id, "error": error.to_string() })),
            );
        }
    }

    fn run_follow_loop(
        &self,
        permission: Arc<dyn ParentPermissionMode>,
        push: PermissionPushFn,
        generation: u64,
    ) {
        loop {
            let mut state = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
            while !state.push_queued && !state.closed && state.generation == generation {
                state = self
                    .shared
                    .1
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.closed || state.generation != generation || state.unfollow.is_none() {
                return;
            }
            state.push_queued = false;
            let applied_snapshot = state.applied.clone();
            drop(state);

            let Some(preset) = permission.current() else {
                continue;
            };
            if applied_snapshot.as_deref() == Some(preset.as_str()) {
                continue;
            }

            let push_result = push(&preset);

            let (release_old, next_warning) = {
                let mut state = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
                if state.closed || state.generation != generation || state.unfollow.is_none() {
                    let _ = push_result;
                    return;
                }
                match push_result {
                    Ok(()) => {
                        state.applied = Some(preset);
                        (state.release_warning.take(), None)
                    }
                    Err(error_msg) => {
                        let confirmed = state.applied.take();
                        (
                            state.release_warning.take(),
                            Some((preset, confirmed, error_msg)),
                        )
                    }
                }
            };
            if let Some(release) = release_old {
                release();
            }
            if let Some((preset, confirmed, error_msg)) = next_warning {
                utils::logger::log(
                    "senpi-task child did not follow its parent's approval mode",
                    Some(&json!({
                        "taskId": self.task_id,
                        "preset": preset,
                        "error": error_msg,
                    })),
                );
                if let Some(on_warning) = &self.on_warning {
                    let message = format!(
                        "task {} may not have followed its parent's approval mode ({preset}); its mode is unknown (last confirmed {}) until the parent's next change reaches it: {error_msg}",
                        self.task_id,
                        confirmed.as_deref().unwrap_or("none"),
                    );
                    let token = format!("follow:{}", self.task_id);
                    if let Some(release) = on_warning(&message, &token) {
                        let mut state =
                            self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
                        if state.closed {
                            drop(state);
                            release();
                        } else {
                            if let Some(prev) = state.release_warning.replace(release) {
                                drop(state);
                                prev();
                            }
                        }
                    }
                }
            }
        }
    }

    /// Aborts dialogs, unsubscribes permission changes, and releases any standing follow notice.
    pub fn close(&self) {
        let (abort, unfollow, release) = {
            let mut state = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
            state.closed = true;
            state.push_queued = false;
            (
                state.abort.clone(),
                state.unfollow.take(),
                state.release_warning.take(),
            )
        };
        self.shared.1.notify_all();
        abort.abort();
        if let Some(unfollow) = unfollow {
            unfollow();
        }
        if let Some(release) = release {
            release();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    struct FakePermission {
        current: Mutex<Option<String>>,
        listeners: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
    }

    impl FakePermission {
        fn new(initial: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                current: Mutex::new(initial.map(str::to_string)),
                listeners: Mutex::new(Vec::new()),
            })
        }

        fn set(&self, next: &str) {
            *self.current.lock().expect("lock") = Some(next.to_string());
            let listeners = self.listeners.lock().expect("lock").clone();
            for listener in listeners {
                listener();
            }
        }
    }

    impl ParentPermissionMode for FakePermission {
        fn current(&self) -> Option<String> {
            self.current.lock().expect("lock").clone()
        }

        fn on_change(&self, listener: Arc<dyn Fn() + Send + Sync>) -> PermissionUnsubscribe {
            self.listeners.lock().expect("lock").push(listener);
            Box::new(|| {})
        }
    }

    #[test]
    fn follow_coalesces_bursts_surfaces_warning_and_clears_on_close() {
        let perm = FakePermission::new(Some("ask"));
        let released = Arc::new(AtomicBool::new(false));
        let warnings: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let warn_log = Arc::clone(&warnings);
        let rel_flag = Arc::clone(&released);
        let (warn_tx, warn_rx) = std::sync::mpsc::channel();
        let on_warning: WarningCallback = Arc::new(move |msg, tok| {
            warn_log
                .lock()
                .expect("lock")
                .push((msg.to_string(), tok.to_string()));
            let _ = warn_tx.send(tok.to_string());
            let flag = Arc::clone(&rel_flag);
            Some(Box::new(move || flag.store(true, Ordering::Release)))
        });

        let link = ChildParentLink::new("t_follow", None, Some(perm.clone()), Some(on_warning));
        let spec = link.named(RpcRunnerSpec::default());
        assert_eq!(spec.permission_preset.as_deref(), Some("ask"));

        // Mode moves while child opens; link pushes at once once follow starts.
        perm.set("read-only");

        let pushed: Arc<Mutex<Vec<String>>> = Arc::default();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let fail_next = Arc::new(AtomicBool::new(false));
        let fail_flag = Arc::clone(&fail_next);
        let pushed_clone = Arc::clone(&pushed);
        link.follow(Arc::new(move |preset| {
            pushed_clone.lock().expect("lock").push(preset.to_string());
            let res = if fail_flag.load(Ordering::Acquire) {
                Err("lost reply".to_string())
            } else {
                Ok(())
            };
            let _ = done_tx.send(preset.to_string());
            res
        }));

        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("initial push"),
            "read-only"
        );

        // Next change fails -> unknown mode & warning registered under `follow:t_follow`.
        fail_next.store(true, Ordering::Release);
        perm.set("yolo");
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("failed push"),
            "yolo"
        );
        assert_eq!(
            warn_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("warning callback"),
            "follow:t_follow"
        );
        // A return to `read-only` MUST still be pushed because the failed push cleared `applied`.
        perm.set("read-only");
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("retry earlier mode"),
            "read-only"
        );
        assert!(
            warnings
                .lock()
                .expect("lock")
                .iter()
                .any(|(msg, tok)| tok == "follow:t_follow"
                    && msg.contains("last confirmed read-only"))
        );

        link.close();
        assert!(released.load(Ordering::Acquire));
    }
}
