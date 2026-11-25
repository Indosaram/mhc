//! Parent-session side of child dialog forwarding and approval-mode tracking (`child-parent-wiring.ts`, `#12839`, `#12853`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{
    AbortSignal, EventKind, ExtensionApi, ExtensionEvent, ExtensionUi, ExtensionUiDialogOptions,
    UiUnsubscribe,
};
use senpi_task::{
    ChildDialogForwarder, ForegroundDialogOrigins, ForwardableDialog, ForwardedAnswer,
    ForwardedChild, ParentPermissionMode, shared_foreground_dialog_origins, store::TaskRecordStore,
};

use crate::runtime_context::TaskRuntimeContext;

const PERMISSION_PRESET_CHANGED: &str = "permission_preset_changed";

/// Host permission-mode source (`pi.getPermissionPreset()` + optional event bus).
pub trait ParentPermissionSource: Send + Sync {
    fn current(&self) -> Option<String>;
    fn on_preset_changed(&self, _wake: Arc<dyn Fn() + Send + Sync>) -> Option<UiUnsubscribe> {
        None
    }
}

pub struct ChildParentWiringInput {
    pub runtime: Arc<Mutex<TaskRuntimeContext>>,
    pub session_dialog: Option<Arc<dyn ExtensionUi>>,
    pub permission: Option<Arc<dyn ParentPermissionSource>>,
    pub label_of: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub origins: Option<&'static ForegroundDialogOrigins>,
}

#[derive(Clone)]
pub struct ChildParentWiring {
    pub dialog_forwarder: Arc<dyn ChildDialogForwarder>,
    pub parent_permission: Arc<dyn ParentPermissionMode>,
    wake_fn: Arc<dyn Fn() + Send + Sync>,
}

impl ChildParentWiring {
    /// Wake listeners on parent `session_start` / `turn_start` (`#12853`).
    pub fn wake(&self) {
        (self.wake_fn)();
    }

    pub fn bind_lifecycle_wakes(&self, api: &mut ExtensionApi) {
        for kind in [EventKind::SessionStart, EventKind::TurnStart] {
            let wake = Arc::clone(&self.wake_fn);
            api.on(
                kind,
                Arc::new(move |event, _| {
                    let wake = Arc::clone(&wake);
                    let fire = matches!(
                        event,
                        ExtensionEvent::SessionStart(_) | ExtensionEvent::TurnStart { .. }
                    );
                    Box::pin(async move {
                        if fire {
                            wake();
                        }
                        Ok(maho_ext_api::EventResult::None)
                    })
                }),
            );
        }
    }
}

pub fn child_label(store: &TaskRecordStore, task_id: &str) -> String {
    let record = store.load(task_id).ok().flatten();
    record
        .as_ref()
        .and_then(|rec| {
            rec.name
                .clone()
                .or_else(|| rec.task_summary.clone())
                .or_else(|| rec.description.clone())
        })
        .unwrap_or_else(|| task_id.to_string())
}

struct WiredForwarder {
    runtime: Arc<Mutex<TaskRuntimeContext>>,
    session_dialog: Option<Arc<dyn ExtensionUi>>,
    label_of: Arc<dyn Fn(&str) -> String + Send + Sync>,
    origins: &'static ForegroundDialogOrigins,
}

impl ChildDialogForwarder for WiredForwarder {
    fn forward(&self, dialog: ForwardableDialog, child: ForwardedChild) -> ForwardedAnswer {
        if child.signal.is_aborted() {
            return ForwardedAnswer::Cancelled;
        }
        let title = format!("{}: {}", (self.label_of)(&child.task_id), dialog.title());
        if self.origins.is_foreground(&child.task_id) {
            let ui = self
                .runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .ui()
                .cloned();
            if let Some(ui) = ui {
                return raise_on_ui(ui.as_ref(), &dialog, &title, &child);
            }
        }
        match &self.session_dialog {
            Some(ui) => raise_on_ui(ui.as_ref(), &dialog, &title, &child),
            None => ForwardedAnswer::Cancelled,
        }
    }
}

fn raise_on_ui(
    ui: &dyn ExtensionUi,
    dialog: &ForwardableDialog,
    title: &str,
    child: &ForwardedChild,
) -> ForwardedAnswer {
    let signal = AbortSignal::default();
    let abort = signal.clone();
    child.signal.on_abort(move || abort.abort());
    let mut opts = ExtensionUiDialogOptions {
        signal: Some(signal),
        ..ExtensionUiDialogOptions::default()
    };
    match dialog {
        ForwardableDialog::Select {
            options,
            tool_call_id,
            parent_tool_call_id,
            timeout,
            ..
        } => {
            opts.timeout_ms = *timeout;
            opts.tool_call_id.clone_from(tool_call_id);
            opts.parent_tool_call_id.clone_from(parent_tool_call_id);
            let answer = futures::executor::block_on(ui.select(title, options, opts));
            answer.map_or(ForwardedAnswer::Cancelled, ForwardedAnswer::Value)
        }
        ForwardableDialog::Confirm {
            message, timeout, ..
        } => {
            opts.timeout_ms = *timeout;
            let confirmed = futures::executor::block_on(ui.confirm(title, message, opts));
            ForwardedAnswer::Confirmed(confirmed)
        }
        ForwardableDialog::Input {
            placeholder,
            tool_call_id,
            parent_tool_call_id,
            timeout,
            ..
        } => {
            opts.timeout_ms = *timeout;
            opts.tool_call_id.clone_from(tool_call_id);
            opts.parent_tool_call_id.clone_from(parent_tool_call_id);
            let answer = futures::executor::block_on(ui.input(title, placeholder.as_deref(), opts));
            answer.map_or(ForwardedAnswer::Cancelled, ForwardedAnswer::Value)
        }
        ForwardableDialog::Editor { prefill, .. } => {
            match futures::executor::block_on(ui.editor(title, prefill.as_deref())) {
                Ok(Some(value)) => ForwardedAnswer::Value(value),
                Ok(None) | Err(_) => ForwardedAnswer::Cancelled,
            }
        }
    }
}

struct PermissionState {
    listeners: BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>,
    bus_unsub: Option<UiUnsubscribe>,
}

struct SharedPermission(Arc<WiredPermissionInner>);

struct WiredPermissionInner {
    source: Option<Arc<dyn ParentPermissionSource>>,
    next_id: AtomicU64,
    state: Mutex<PermissionState>,
}

impl WiredPermissionInner {
    fn wake(self: &Arc<Self>) {
        let (has_listeners, current) = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            (
                !state.listeners.is_empty(),
                self.source.as_ref().and_then(|s| s.current()),
            )
        };
        if !has_listeners || current.is_none() {
            return;
        }
        let listeners: Vec<_> = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .listeners
            .values()
            .cloned()
            .collect();
        for listener in listeners {
            listener();
        }
    }
}

impl ParentPermissionMode for SharedPermission {
    fn current(&self) -> Option<String> {
        self.0.source.as_ref().and_then(|s| s.current())
    }

    fn on_change(
        &self,
        listener: Arc<dyn Fn() + Send + Sync>,
    ) -> senpi_task::runners::parent_link::PermissionUnsubscribe {
        let id = self.0.next_id.fetch_add(1, Ordering::AcqRel) + 1;
        let weak_wake = Arc::downgrade(&self.0);
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.listeners.insert(id, listener);
        if state.bus_unsub.is_none()
            && let Some(source) = &self.0.source
        {
            state.bus_unsub = source.on_preset_changed(Arc::new(move || {
                if let Some(inner) = weak_wake.upgrade() {
                    inner.wake();
                }
            }));
        }
        let weak = Arc::downgrade(&self.0);
        Box::new(move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let unsub = {
                let mut state = inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.listeners.remove(&id);
                if state.listeners.is_empty() {
                    state.bus_unsub.take()
                } else {
                    None
                }
            };
            if let Some(unsub) = unsub {
                unsub();
            }
        })
    }
}

/// Bus-backed `ParentPermissionSource` subscribing to `permission_preset_changed`.
pub struct EventBusPermissionSource {
    pub read_preset: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub events: maho_ext_api::EventBus,
}

impl ParentPermissionSource for EventBusPermissionSource {
    fn current(&self) -> Option<String> {
        (self.read_preset)()
    }

    fn on_preset_changed(&self, wake: Arc<dyn Fn() + Send + Sync>) -> Option<UiUnsubscribe> {
        let sub = self
            .events
            .on(PERMISSION_PRESET_CHANGED, Arc::new(move |_| wake()));
        Some(Box::new(move || drop(sub)))
    }
}

pub fn create_child_parent_wiring(input: ChildParentWiringInput) -> ChildParentWiring {
    let dialog_forwarder: Arc<dyn ChildDialogForwarder> = Arc::new(WiredForwarder {
        runtime: input.runtime,
        session_dialog: input.session_dialog,
        label_of: input.label_of,
        origins: input
            .origins
            .unwrap_or_else(shared_foreground_dialog_origins),
    });
    let perm_inner = Arc::new(WiredPermissionInner {
        source: input.permission,
        next_id: AtomicU64::new(0),
        state: Mutex::new(PermissionState {
            listeners: BTreeMap::new(),
            bus_unsub: None,
        }),
    });
    let wake_inner = Arc::clone(&perm_inner);
    let wake_fn: Arc<dyn Fn() + Send + Sync> = Arc::new(move || wake_inner.wake());
    let parent_permission: Arc<dyn ParentPermissionMode> = Arc::new(SharedPermission(perm_inner));
    ChildParentWiring {
        dialog_forwarder,
        parent_permission,
        wake_fn,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use super::*;
    use senpi_task::runners::child_dialogs::ChildDialogSignal;

    struct FakeUi(Arc<Mutex<Vec<String>>>);
    impl ExtensionUi for FakeUi {
        fn select<'a>(
            &'a self,
            title: &'a str,
            _options: &'a [String],
            _opts: ExtensionUiDialogOptions,
        ) -> maho_ext_api::UiFuture<'a, Option<String>> {
            self.0.lock().expect("lock").push(format!("select:{title}"));
            Box::pin(async move { Some("yes".into()) })
        }
        fn confirm<'a>(
            &'a self,
            title: &'a str,
            _message: &'a str,
            _opts: ExtensionUiDialogOptions,
        ) -> maho_ext_api::UiFuture<'a, bool> {
            self.0
                .lock()
                .expect("lock")
                .push(format!("confirm:{title}"));
            Box::pin(async move { true })
        }
        fn input<'a>(
            &'a self,
            title: &'a str,
            _placeholder: Option<&'a str>,
            _opts: ExtensionUiDialogOptions,
        ) -> maho_ext_api::UiFuture<'a, Option<String>> {
            self.0.lock().expect("lock").push(format!("input:{title}"));
            Box::pin(async move { Some("val".into()) })
        }
        fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
        fn set_status(&self, _: &str, _: Option<&str>) {}
        fn set_widget(
            &self,
            _: &str,
            _: Option<maho_ext_api::WidgetContent>,
            _: maho_ext_api::ExtensionWidgetOptions,
        ) {
        }
        fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
        fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String {
            String::new()
        }
        fn custom(
            &self,
            _: maho_ext_api::ComponentFactory,
            _: maho_ext_api::CustomUiOptions,
        ) -> maho_ext_api::ExtensionFuture<'_, serde_json::Value> {
            Box::pin(async { Ok(serde_json::Value::Null) })
        }
        fn theme(&self) -> maho_ext_api::Theme {
            maho_ext_api::Theme::default()
        }
    }

    #[test]
    fn background_dialogs_use_session_dialog_and_permission_unsub_releases_bus() {
        let bg_calls = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(Mutex::new(TaskRuntimeContext::new(
            std::path::PathBuf::from("."),
        )));
        let bus = maho_ext_api::EventBus::default();
        let active = Arc::new(AtomicBool::new(true));
        let hits = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(EventBusPermissionSource {
            read_preset: {
                let active = Arc::clone(&active);
                Arc::new(move || active.load(Ordering::Acquire).then(|| "yolo".to_string()))
            },
            events: bus.clone(),
        });

        let wiring = create_child_parent_wiring(ChildParentWiringInput {
            runtime,
            session_dialog: Some(Arc::new(FakeUi(Arc::clone(&bg_calls)))),
            permission: Some(source),
            label_of: Arc::new(|id| format!("worker({id})")),
            origins: None,
        });

        // Background dialog routes to session_dialog and prefixes title with labelOf.
        let answer = wiring.dialog_forwarder.forward(
            ForwardableDialog::Confirm {
                title: "Approve bash?".into(),
                message: "rm -rf".into(),
                timeout: None,
            },
            ForwardedChild {
                task_id: "t1".into(),
                signal: ChildDialogSignal::default(),
            },
        );
        assert_eq!(answer, ForwardedAnswer::Confirmed(true));
        assert_eq!(
            bg_calls.lock().expect("lock").as_slice(),
            &["confirm:worker(t1): Approve bash?".to_string()]
        );

        let hits_clone = Arc::clone(&hits);
        let unsub = wiring.parent_permission.on_change(Arc::new(move || {
            hits_clone.fetch_add(1, Ordering::AcqRel);
        }));
        wiring.wake();
        assert_eq!(hits.load(Ordering::Acquire), 1);
        unsub();
        wiring.wake();
        assert_eq!(hits.load(Ordering::Acquire), 1);
    }
}
