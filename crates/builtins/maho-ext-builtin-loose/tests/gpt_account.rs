use maho_ext_builtin_loose::gpt_account::{parse_action, AccountAction, GptAccount, PROVIDER_ID};
use maho_ai::auth::types::{AccountLoginOrigin, AccountLoginReceipt};
use maho_ext_api::*;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[test]
fn account_dispatch_preserves_aliases_and_missing_id_behavior() {
    assert_eq!(parse_action("").expect("parse"), AccountAction::List);
    assert_eq!(parse_action("pin unpin").expect("parse"), AccountAction::Unpin);
    assert_eq!(parse_action("pin slot ignored").expect("parse"), AccountAction::Pin("slot".into()));
    assert_eq!(parse_action("remove").expect("parse"), AccountAction::Usage);
    assert_eq!(parse_action("other").expect("parse"), AccountAction::Usage);
}

struct FixtureSession;
impl ToolSessionManager for FixtureSession {
    fn session_id(&self) -> &str {
        "session"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for FixtureSession {
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

struct FixtureRegistry;
impl ModelRegistry for FixtureRegistry {
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
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Default)]
struct RecordingUi(pub Mutex<Vec<(String, NotificationType)>>);
impl ExtensionUi for RecordingUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((message.into(), kind));
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn context(ui: Arc<RecordingUi>) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode: ExtensionMode::Tui,
        has_ui: true,
        cwd: "/tmp".into(),
        agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(FixtureSession),
        model_registry: Arc::new(FixtureRegistry),
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
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

#[tokio::test]
async fn a_successful_add_routes_accounts_changed_through_the_core_registry_and_unsubscribes() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let sink = observed.clone();
    let unsubscribe = maho_core::provider_account_events::subscribe_provider_account_events(Arc::new(move |event| {
        if let maho_core::provider_account_events::ProviderAccountEvent::AccountsChanged { provider } = event
            && provider == PROVIDER_ID {
            sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(provider);
        }
    }));

    let mut api = ExtensionApi::new(LoadedExtension::new("builtin-loose", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    GptAccount {
        login: Arc::new(|_| Box::pin(async { Ok(Some(AccountLoginReceipt { provider_id: PROVIDER_ID.into(), name: "primary".into(), origin: AccountLoginOrigin::Generated })) })),
        open_browser: Arc::new(|_| {}),
    }
    .register(&mut api);
    let handler = api.registered.commands.iter().find(|command| command.name == "gpt-account").expect("gpt-account command").handler.clone();

    let ui = Arc::new(RecordingUi::default());
    let ctx = context(ui.clone());
    handler("add", &ctx).await.expect("add");

    assert_eq!(*observed.lock().unwrap_or_else(std::sync::PoisonError::into_inner), vec![PROVIDER_ID.to_owned()], "exactly one AccountsChanged for the provider");
    assert!(ui.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|(_, kind)| *kind == NotificationType::Info));

    unsubscribe();
    maho_core::provider_account_events::emit_provider_accounts_changed(PROVIDER_ID);
    assert_eq!(observed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 1, "unsubscribe stops delivery");
}
