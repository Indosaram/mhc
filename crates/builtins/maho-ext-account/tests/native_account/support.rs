use maho_core::{auth_storage::AuthStorage, credential_accounts, credential_pool::state_store::CredentialSlotRepository};
use maho_ext_api::{CredentialAccountSource, CredentialAccountSummary, ExtensionFailure, ExtensionFuture, Model, ModelRegistry};
use std::sync::Arc;

pub struct SyntheticRegistry { pub storage: Arc<tokio::sync::Mutex<AuthStorage>>, pub repository: Arc<CredentialSlotRepository> }
pub fn environment(name: &str) -> Option<String> {
    (name == "ANTHROPIC_API_KEY").then(|| "synthetic-env-secret".into())
}
impl ModelRegistry for SyntheticRegistry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
    fn get_credential_accounts<'a>(&'a self, provider: &'a str) -> ExtensionFuture<'a, Vec<CredentialAccountSummary>> {
        let storage = self.storage.clone();
        let repository = self.repository.clone();
        let provider = provider.to_owned();
        Box::pin(async move {
            let accounts = tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
                runtime.block_on(async {
                    let storage = storage.lock().await;
                    credential_accounts::get_credential_accounts(&storage, &provider, &environment, &repository, 0).await
                })
            }).await.map_err(|error| ExtensionFailure::new(error.to_string()))??;
            Ok(accounts.into_iter().map(|account| CredentialAccountSummary {
                name: account.name, display_name: account.display_name, blocked: account.blocked, pinned: account.pinned,
                source: match account.source {
                    credential_accounts::CredentialAccountSource::Login => CredentialAccountSource::Login,
                    credential_accounts::CredentialAccountSource::Import => CredentialAccountSource::Import,
                    credential_accounts::CredentialAccountSource::Env => CredentialAccountSource::Env,
                },
            }).collect())
        })
    }
    fn pin_credential_account<'a>(&'a self, provider: &'a str, name: Option<&'a str>) -> ExtensionFuture<'a, ()> {
        let storage = self.storage.clone();
        let repository = self.repository.clone();
        let provider = provider.to_owned();
        let name = name.map(str::to_owned);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
                runtime.block_on(async {
                    let storage = storage.lock().await;
                    credential_accounts::pin_credential_account(&storage, &provider, name.as_deref(), &environment, &repository, 0).await
                })
            }).await.map_err(|error| ExtensionFailure::new(error.to_string()))??;
            Ok(())
        })
    }
    fn remove_credential_account<'a>(&'a self, provider: &'a str, name: &'a str) -> ExtensionFuture<'a, ()> {
        let storage = self.storage.clone();
        let repository = self.repository.clone();
        let provider = provider.to_owned();
        let name = name.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
                runtime.block_on(async {
                    let storage = storage.lock().await;
                    credential_accounts::remove_credential_account(&storage, &provider, &name, &environment, &repository, 0).await
                })
            }).await.map_err(|error| ExtensionFailure::new(error.to_string()))??;
            Ok(())
        })
    }
    fn rename_credential_account<'a>(&'a self, provider: &'a str, name: &'a str, display_name: Option<&'a str>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let storage = self.storage.lock().await;
            credential_accounts::rename_credential_account(&storage, provider, name, display_name).await?;
            Ok(())
        })
    }
}


use maho_ext_api::*;
use std::{path::Path, sync::Mutex};
struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str { "session" }
    fn session_file(&self) -> Option<&Path> { None }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}
#[derive(Default)]
pub struct TestUi(pub Mutex<Vec<(String, NotificationType)>>);
impl ExtensionUi for TestUi {
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> { Some(self) }
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, message: &str, kind: NotificationType) { self.0.lock().expect("notifications").push((message.into(), kind)); }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("UI not available".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
impl ExtensionUiFactories for TestUi {
    fn set_widget_factory(&self, _: &str, _: Option<TuiComponentFactory>, _: ExtensionWidgetOptions) {}
    fn set_header_factory(&self, _: Option<TuiComponentFactory>) {}
    fn set_footer_factory(&self, _: Option<FooterComponentFactory>) {}
    fn custom_factory(&self, _: CustomComponentFactory, _: CustomUiFactoryOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("UI not available".into()) }) }
}
pub fn context(registry: Arc<dyn ModelRegistry>, ui: Arc<TestUi>) -> ExtensionContext {
    ExtensionContext { ui, mode: ExtensionMode::Print, has_ui: false, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession), model_registry: registry, model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default() }
}


pub struct CommandActions(pub Mutex<Vec<String>>);
impl ExtensionCommandContextActions for CommandActions {
    fn wait_for_idle(&self) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn new_session(&self, _: NewSessionOptions) -> ExtensionFuture<'_, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult { cancelled: true }) }) }
    fn fork<'a>(&'a self, _: &'a str, _: ForkOptions) -> ExtensionFuture<'a, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult { cancelled: false }) }) }
    fn navigate_tree<'a>(&'a self, target: &'a str, _: ExtensionTreeNavigationOptions) -> ExtensionFuture<'a, SessionNavigationResult> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(target.into()); Box::pin(async { Ok(SessionNavigationResult { cancelled: false }) })
    }
    fn edit_assistant_message<'a>(&'a self, _: &'a str, _: &'a str, _: EditMessageOptions) -> ExtensionFuture<'a, EditMessageResult> { Box::pin(async { Ok(EditMessageResult { unchanged: Some(true), ..Default::default() }) }) }
    fn edit_user_message<'a>(&'a self, _: &'a str, _: &'a str, _: EditMessageOptions) -> ExtensionFuture<'a, EditMessageResult> { Box::pin(async { Ok(EditMessageResult { entry_id: Some("edited".into()), ..Default::default() }) }) }
    fn switch_session<'a>(&'a self, _: &'a str, _: SwitchSessionOptions) -> ExtensionFuture<'a, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult { cancelled: false }) }) }
    fn reload(&self) -> ExtensionFuture<'_, ()> { Box::pin(async move { self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("reload".into()); Ok(()) }) }
}
