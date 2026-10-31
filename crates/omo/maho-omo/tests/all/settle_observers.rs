use maho_ext_api::{
    EventBus, Extension, ExtensionApi, ExtensionContext, ExtensionMode, ExtensionRuntime,
    ExtensionSessionProfile, ExtensionUi, ExtensionUiDialogOptions, JsonValue, LoadedExtension,
    Model, ModelRegistry, NotificationType, SessionEntry, SessionManager, SourceInfo, Theme,
    ToolSessionManager, UiFuture,
};
use std::sync::{Arc, Mutex};

struct TestSession(String);
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        &self.0
    }
    fn session_file(&self) -> Option<&std::path::Path> {
        None
    }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

struct TestModels;
impl ModelRegistry for TestModels {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(
        &'a self,
        _: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Default)]
struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
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
    ) -> maho_ext_api::ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Ok(JsonValue::Null) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn test_context(session_id: &str) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Tui,
        has_ui: true,
        cwd: std::path::PathBuf::from("/workspace"),
        agent_dir: std::path::PathBuf::from("/workspace/.omo"),
        session_manager: Arc::new(TestSession(session_id.to_string())),
        model_registry: Arc::new(TestModels),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(maho_ext_api::BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        logger: None,
        idle_coordinator: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::{ComponentLogger, EventResult, ExtensionEvent, ExtensionHandler};
use maho_omo::settle_observers::{SettleObservers, current_settle_observer};

#[derive(Default)]
struct Warnings(Mutex<Vec<(String, Option<JsonValue>)>>);
impl ComponentLogger for Warnings {
    fn info(&self, _: &str, _: Option<&JsonValue>) {}
    fn warn(&self, message: &str, details: Option<&JsonValue>) {
        self.0
            .lock()
            .unwrap()
            .push((message.to_owned(), details.cloned()));
    }
    fn error(&self, _: &str, _: Option<&JsonValue>) {}
}

fn handler(
    body: impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    + Send
    + Sync
    + 'static,
) -> ExtensionHandler {
    Arc::new(move |_, _| {
        let work = body();
        Box::pin(async move {
            work.await;
            Ok(EventResult::None)
        })
    })
}

#[tokio::test]
async fn an_expired_budget_logs_the_stalled_phase_and_aborts_the_signal() {
    let warnings = Arc::new(Warnings::default());
    let observers =
        SettleObservers::with_budget(warnings.clone(), std::time::Duration::from_millis(30));
    let (signal_tx, signal_rx) = std::sync::mpsc::channel::<AbortSignal>();
    let signal_tx = Mutex::new(signal_tx);
    let wrapped = observers.wrap(
        "memory",
        Some("dreamTrigger"),
        handler(move || {
            let observer = current_settle_observer().expect("observer");
            observer.phase("awaiting-dream").unwrap();
            signal_tx
                .lock()
                .unwrap()
                .send(observer.signal().clone())
                .unwrap();
            Box::pin(std::future::pending())
        }),
    );
    let context = test_context("s-1");
    let mut event = ExtensionEvent::AgentSettled;
    assert!(matches!(
        wrapped(&mut event, &context).await,
        Ok(EventResult::None)
    ));
    assert!(signal_rx.recv().unwrap().aborted());
    assert_eq!(observers.pending_count(), 0);
    let warnings = warnings.0.lock().unwrap();
    assert_eq!(warnings.len(), 1);
    let details = warnings[0].1.as_ref().unwrap();
    assert_eq!(warnings[0].0, "extension_handler_stalled");
    assert_eq!(details["hook"], "awaiting-dream");
    assert_eq!(details["component"], "memory");
    assert_eq!(details["sessionId"], "s-1");
}

#[tokio::test]
async fn retiring_a_session_ends_its_observer_and_leaves_other_sessions_running() {
    let warnings = Arc::new(Warnings::default());
    let observers =
        SettleObservers::with_budget(warnings.clone(), std::time::Duration::from_secs(60));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let tx = Mutex::new(tx);
    let wrapped = observers.wrap(
        "memory",
        None,
        handler(move || {
            tx.lock().unwrap().send(()).unwrap();
            Box::pin(std::future::pending())
        }),
    );
    let context = test_context("a");
    let mut event = ExtensionEvent::AgentSettled;
    let retire = async {
        rx.recv().await.unwrap();
        observers.retire(Some("other"));
        assert_eq!(
            observers.pending_count(),
            1,
            "another session's retirement leaves this observer"
        );
        observers.retire(Some("a"));
    };
    let (result, ()) = tokio::join!(wrapped(&mut event, &context), retire);
    assert!(matches!(result, Ok(EventResult::None)));
    assert_eq!(observers.pending_count(), 0);
    assert!(
        warnings.0.lock().unwrap().is_empty(),
        "retirement is not a stall"
    );
}
