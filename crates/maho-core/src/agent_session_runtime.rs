//! Port of senpi `packages/coding-agent/src/core/agent-session-runtime.ts`.
//!

use crate::agent_session::AgentSession;
use crate::agent_session_services::{AgentSessionRuntimeDiagnostic, AgentSessionServices};
use crate::session_cwd::assert_session_cwd_exists;

pub struct ExtensionModelRuntimeActions(pub std::sync::Mutex<crate::model_runtime::ModelRuntime>);
impl maho_ext_api::ExtensionProviderActions for ExtensionModelRuntimeActions {
    fn register_provider(&self, registration: maho_ext_api::ProviderRegistration, _: &str) -> Result<(), maho_ext_api::ExtensionFailure> {
        let mut runtime = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match registration {
            maho_ext_api::ProviderRegistration::Native(provider) => { runtime.register_native_provider(provider); Ok(()) }
            maho_ext_api::ProviderRegistration::Config { name, config } => {
                let models = config.models.map(|models| models.into_iter().map(extension_provider_model).collect());
                let refresh_models = config.refresh_models.map(|refresh| -> crate::provider_composer::ExtensionRefresh {
                    std::sync::Arc::new(move |context| {
                        let future = refresh(context);
                        Box::pin(async move { future.await.map(|models| models.into_iter().map(extension_provider_model).collect()).map_err(|error| error.message) })
                    })
                });
                let input = crate::provider_composer::ProviderConfigInput {
                    config: crate::model_config_schema::ModelsJsonProvider {
                        name: config.name, base_url: config.base_url, api_key: config.api_key, api: config.api,
                        headers: config.headers, extra_body: config.extra_body.and_then(|value| value.as_object().cloned()),
                        auth_header: config.auth_header, models, ..Default::default()
                    },
                    stream_simple: config.stream_simple, oauth: config.oauth, fallback_eligible: config.fallback_eligible,
                    refresh_models, retry_policy: None,
                };
                runtime.register_provider(&name, input).map_err(maho_ext_api::ExtensionFailure::new)
            }
        }
    }
    fn unregister_provider(&self, name: &str, _: &str) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).unregister_provider(name); Ok(())
    }
}
fn extension_provider_model(model: maho_ext_api::ProviderModelConfig) -> crate::model_config_schema::ModelsJsonModel {
    crate::model_config_schema::ModelsJsonModel {
        id: model.id, name: Some(model.name), upstream_model_id: model.upstream_model_id, api: model.api, base_url: model.base_url,
        reasoning: Some(model.reasoning), recover_text_tool_calls: model.recover_text_tool_calls, thinking_level_map: model.thinking_level_map,
        input: Some(model.input), cost: Some(model.cost), context_window: Some(model.context_window as f64), max_tokens: Some(model.max_tokens as f64),
        headers: model.headers, extra_body: model.extra_body.and_then(|value| value.as_object().cloned()), compat: model.compat.map(|compat| compat.0),
        ..Default::default()
    }
}

/// Thrown when `/import` references a JSONL file path that does not exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionImportFileNotFoundError {
    pub file_path: String,
}

impl std::fmt::Display for SessionImportFileNotFoundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "File not found: {}", self.file_path)
    }
}

impl std::error::Error for SessionImportFileNotFoundError {}

/// Immutable flags selected when a runtime is first launched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSessionLaunchProfile {
    pub cwd: String,
    pub permission_preset: Option<String>,
    pub creation_model: Option<(String, String)>,
    pub initial_thinking_level: Option<String>,
    pub auto_title: Option<bool>,
}

/// Result returned by runtime creation.
pub struct CreateAgentSessionRuntimeResult {
    pub session: AgentSession,
    pub services: AgentSessionServices,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    pub model_fallback_message: Option<String>,
}

pub type RebindSession = Box<dyn Fn(&AgentSession) + Send + Sync>;
pub type SessionInvalidateHook = Box<dyn Fn() + Send + Sync>;

/// Owns the current AgentSession plus its cwd-bound services.
pub struct AgentSessionRuntime {
    rebind_session: Option<RebindSession>,
    before_session_invalidate: Option<SessionInvalidateHook>,
    session: AgentSession,
    services: AgentSessionServices,
    diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    model_fallback_message: Option<String>,
    launch_profile: Option<AgentSessionLaunchProfile>,
}

impl AgentSessionRuntime {
    pub fn new(
        session: AgentSession,
        services: AgentSessionServices,
        diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
        model_fallback_message: Option<String>,
        launch_profile: Option<AgentSessionLaunchProfile>,
    ) -> Self {
        Self {
            rebind_session: None,
            before_session_invalidate: None,
            session,
            services,
            diagnostics,
            model_fallback_message,
            launch_profile,
        }
    }

    pub fn services(&self) -> &AgentSessionServices {
        &self.services
    }

    pub fn session(&self) -> &AgentSession {
        &self.session
    }

    pub fn cwd(&self) -> &str {
        &self.services.cwd
    }

    pub fn diagnostics(&self) -> &[AgentSessionRuntimeDiagnostic] {
        &self.diagnostics
    }

    pub fn model_fallback_message(&self) -> Option<&str> {
        self.model_fallback_message.as_deref()
    }

    pub fn launch_profile(&self) -> Option<&AgentSessionLaunchProfile> {
        self.launch_profile.as_ref()
    }

    pub fn set_rebind_session(&mut self, rebind: Option<RebindSession>) {
        self.rebind_session = rebind;
    }

    pub fn set_before_session_invalidate(&mut self, callback: Option<SessionInvalidateHook>) {
        self.before_session_invalidate = callback;
    }

    pub async fn switch_session(&mut self, session_path: &str) -> Result<bool, String> {
        let manager = crate::session_manager::SessionManager::open(session_path, None, None, None);
        assert_session_cwd_exists(&manager, &self.services.cwd).map_err(|error| error.to_string())?;
        let cwd = manager.cwd().to_owned();
        self.replace_session(manager, cwd, maho_ext_api::SessionReason::Resume).await
    }

    pub async fn new_session(&mut self, cwd: Option<&str>, parent_session: Option<String>) -> Result<bool, String> {
        let cwd = cwd.unwrap_or(&self.services.cwd).to_owned();
        let options = Some(crate::session_manager::NewSessionOptions { id: None, parent_session });
        let manager = self.session.with_session_manager(|current| {
            if current.is_persisted() {
                crate::session_manager::SessionManager::create(&cwd, Some(current.session_dir()), options)
            } else {
                crate::session_manager::SessionManager::in_memory(&cwd, options, None)
            }
        });
        assert_session_cwd_exists(&manager, &cwd).map_err(|error| error.to_string())?;
        self.replace_session(manager, cwd, maho_ext_api::SessionReason::New).await
    }

    async fn replace_session(&mut self, manager: crate::session_manager::SessionManager, cwd: String,
        reason: maho_ext_api::SessionReason) -> Result<bool, String>
    {
        if self.session.runtime_before_switch(reason, manager.session_file().map(str::to_owned)).await? { return Ok(false); }
        self.apply_replacement(manager, cwd, reason).await?;
        Ok(true)
    }

    pub async fn fork(&mut self, entry_id: &str, include_entry: bool) -> Result<crate::agent_session::AssistantEditResult, String> {
        if self.session.runtime_before_fork(entry_id, include_entry).await? {
            return Ok(crate::agent_session::AssistantEditResult { cancelled: true, ..Default::default() });
        }
        let entry = self.session.with_session_manager(|manager| manager.entry(entry_id))
            .ok_or_else(|| "Invalid entry ID for forking".to_owned())?;
        if !include_entry && (entry["type"] != "message" || entry["message"]["role"] != "user") {
            return Err("Invalid entry ID for forking".to_owned());
        }
        let leaf = if include_entry { Some(entry_id) } else { entry["parentId"].as_str() };
        let previous = self.session.session_file();
        if self.session.with_session_manager(|manager| manager.is_persisted()) {
            let file = previous.as_deref().ok_or("Persisted session is missing a session file")?;
            if leaf.is_some() && !std::path::Path::new(file).exists() {
                return Err("This session has not been saved yet. Wait for the first assistant response before cloning or forking it.".to_owned());
            }
        }
        let manager = self.session.with_session_manager(|current| {
            let options = Some(crate::session_manager::NewSessionOptions { parent_session: previous.clone(), ..Default::default() });
            let mut manager = if current.is_persisted() {
                crate::session_manager::SessionManager::create(&self.services.cwd, Some(current.session_dir()), options)
            } else { crate::session_manager::SessionManager::in_memory(&self.services.cwd, options, None) };
            if let Some(leaf) = leaf {
                let persisted;
                let current = if current.is_persisted() {
                    persisted = crate::session_manager::SessionManager::open(previous.as_deref().expect("validated session file"), Some(current.session_dir()), None, None);
                    &persisted
                } else { current };
                let mut parent = serde_json::Value::Null;
                let mut pending_labels = Vec::new();
                let mut replacements = std::collections::BTreeMap::new();
                let mut retained_ids = Vec::new();
                for mut entry in current.branch(Some(leaf)) {
                    if entry["type"] == "label" {
                        if let Some(id) = entry["id"].as_str() { pending_labels.push(id.to_owned()); }
                        continue;
                    }
                    for id in pending_labels.drain(..) { replacements.insert(id, entry["id"].clone()); }
                    entry["parentId"] = parent;
                    if entry["type"] == "compaction"
                        && let Some(replacement) = entry["firstKeptEntryId"].as_str().and_then(|id| replacements.get(id))
                    { entry["firstKeptEntryId"] = replacement.clone(); }
                    parent = entry["id"].clone();
                    if let Some(id) = parent.as_str() { retained_ids.push(id.to_owned()); }
                    manager.append_entry_raw(entry);
                }
                for id in retained_ids {
                    if let Some(label) = current.label(&id) {
                        let mut entry = serde_json::json!({"type":"label", "id":uuid::Uuid::new_v4().to_string(),
                            "parentId":manager.leaf_id(), "targetId":id, "label":label});
                        if let Some(timestamp) = current.label_timestamp(&id) {
                            entry["timestamp"] = timestamp.into();
                        }
                        manager.append_entry_raw(entry);
                    }
                }
            }
            manager
        });
        self.apply_replacement(manager, self.services.cwd.clone(), maho_ext_api::SessionReason::Fork).await?;
        Ok(crate::agent_session::AssistantEditResult { editor_text: (!include_entry).then(|| extract_user_message_text(&entry["message"]["content"])),
            ..Default::default() })
    }

    pub async fn import_from_jsonl(&mut self, input_path: &str, cwd_override: Option<&str>) -> Result<bool, String> {
        let base = std::env::current_dir().map_err(|error| error.to_string())?;
        let resolved = crate::paths::resolve_path(input_path, &base.to_string_lossy(),
            &crate::paths::PathInputOptions::expanding_tilde(crate::config::home_dir()));
        let source = std::path::Path::new(&resolved);
        if !source.exists() {
            return Err(SessionImportFileNotFoundError { file_path: resolved }.to_string());
        }
        let directory = self.session.with_session_manager(|manager| manager.session_dir().to_owned());
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let filename = source.file_name().ok_or("Invalid import filename")?;
        let mut destination = std::path::Path::new(&directory).join(filename);
        let stored = crate::paths::resolve_path(&destination.to_string_lossy(), &base.to_string_lossy(),
            &crate::paths::PathInputOptions::default()) == resolved;
        if !stored {
            let stem = source.file_stem().unwrap_or_default().to_string_lossy();
            let extension = source.extension().map(|extension| format!(".{}", extension.to_string_lossy())).unwrap_or_default();
            let mut suffix = 1;
            while destination.exists() {
                destination = std::path::Path::new(&directory).join(format!("{stem}-{suffix}{extension}"));
                suffix += 1;
            }
        }
        let destination = destination.to_string_lossy().into_owned();
        if self.session.runtime_before_switch(maho_ext_api::SessionReason::Resume, Some(destination.clone())).await? { return Ok(false); }
        if !stored {
            let mut input = std::fs::File::open(source).map_err(|error| error.to_string())?;
            let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&destination).map_err(|error| error.to_string())?;
            std::io::copy(&mut input, &mut output).map_err(|error| error.to_string())?;
        }
        let manager = crate::session_manager::SessionManager::open(&destination, Some(&directory), cwd_override, None);
        assert_session_cwd_exists(&manager, &self.services.cwd).map_err(|error| error.to_string())?;
        let cwd = manager.cwd().to_owned();
        self.apply_replacement(manager, cwd, maho_ext_api::SessionReason::Resume).await?;
        Ok(true)
    }

    async fn apply_replacement(&mut self, manager: crate::session_manager::SessionManager, cwd: String,
        reason: maho_ext_api::SessionReason) -> Result<(), String>
    {
        let settings = crate::settings_manager::SettingsManager::create(&cwd, &self.services.agent_dir,
            &std::env::var("HOME").unwrap_or_default(), self.services.settings_manager.is_project_trusted());
        let (system_prompt, append_system_prompt) = self.session.system_prompt_sources();
        let context = manager.build_context(manager.leaf_id());
        let restored_model = context.model.as_ref().and_then(|(provider, id)| self.services.model_registry.find(provider, id));
        let launch_model = self.launch_profile.as_ref().and_then(|profile| profile.creation_model.as_ref())
            .and_then(|(provider, id)| self.services.model_registry.find(provider, id));
        let launch_thinking = self.launch_profile.as_ref().and_then(|profile| profile.initial_thinking_level.as_deref())
            .and_then(maho_ai::types::ModelThinkingLevel::parse);
        let thinking_selection = if manager.branch(manager.leaf_id()).iter().any(|entry| entry["type"] == "thinking_level_change") {
            None
        } else { self.session.thinking_selection() };
        let options = crate::sdk::CreateAgentSessionOptions {
            system_prompt, append_system_prompt,
            cwd: Some(cwd.clone()), agent_dir: Some(self.services.agent_dir.clone()),
            model_runtime: Some(self.services.model_runtime().clone()), model_registry: Some(self.services.model_registry.clone()),
            model: launch_model.or(restored_model).or_else(|| Some(self.session.model())),
            thinking_level: launch_thinking.and_then(|level| serde_json::from_value(serde_json::Value::from(level.as_str())).ok()),
            thinking_selection: launch_thinking.map(|level| maho_ai::types::ThinkingSelection {
                level, source: maho_ai::types::ThinkingSelectionSource::Explicit, legacy_variant_id: None,
            }).or(thinking_selection),
            scoped_models: self.session.scoped_models(), favorite_models: self.session.favorite_models(),
            session_manager: Some(manager), settings_manager: Some(settings), tools: Some(self.session.get_active_tool_names()),
            custom_tools: self.session.replacement_custom_tools(),
            auto_title_sessions: self.launch_profile.as_ref().and_then(|profile| profile.auto_title).or(Some(self.session.replacement_auto_title())),
            session_start_event: Some(maho_ext_api::SessionStartEvent { reason, initial_model_provenance: None,
                previous_session_file: self.session.session_file() }), ..Default::default()
        };
        self.session.abort().await;
        self.session.runtime_shutdown(reason).await;
        if let Some(before) = &self.before_session_invalidate { before(); }
        self.session.dispose().await;
        let created = crate::sdk::create_agent_session(options).await?;
        self.session = created.session;
        self.services.cwd = cwd;
        self.services.settings_manager = crate::settings_manager::SettingsManager::create(&self.services.cwd, &self.services.agent_dir,
            &std::env::var("HOME").unwrap_or_default(), self.services.settings_manager.is_project_trusted());
        self.model_fallback_message = created.model_fallback_message;
        if let Some(rebind) = &self.rebind_session { rebind(&self.session); }
        Ok(())
    }

    pub async fn dispose(&self) {
        self.session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;
        if let Some(callback) = &self.before_session_invalidate {
            callback();
        }
        self.session.dispose().await;
    }
}

/// Reject a session manager whose cwd does not exist, exactly as the runtime factory does.
pub fn assert_runtime_cwd(
    manager: &crate::session_manager::SessionManager,
    cwd: &str,
) -> Result<(), crate::session_cwd::MissingSessionCwdError> {
    assert_session_cwd_exists(manager, cwd)
}

/// Extract the text of a user message's content, for `fork`'s selected text.
pub fn extract_user_message_text(content: &serde_json::Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_provider_registration_reaches_the_live_session_catalog() {
        use maho_ext_api::*;
        let runtime = crate::model_runtime::ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions { providers: Some(vec![]), ..Default::default() });
        let actions = std::sync::Arc::new(ExtensionModelRuntimeActions(std::sync::Mutex::new(runtime.clone())));
        let extension_runtime = ExtensionRuntime::default();
        let api = ExtensionApi::new(LoadedExtension::new("fixture", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), extension_runtime.clone());
        let model = ProviderModelConfig {
            id: "fixture-model".into(), name: "Fixture".into(), upstream_model_id: None, api: None, base_url: None,
            reasoning: false, recover_text_tool_calls: None, thinking_level_map: None, input: vec![maho_ai::types::InputModality::Text],
            cost: Default::default(), context_window: 4096, max_tokens: 512, headers: None, extra_body: None, compat: None,
        };
        api.register_provider("fixture", ProviderConfig { base_url: Some("https://example.test/v1".into()), api: Some("openai-completions".into()), models: Some(vec![model]), ..Default::default() }).unwrap();
        assert!(runtime.get_model("fixture", "fixture-model").is_none());
        extension_runtime.bind_providers(actions).unwrap();
        let model = runtime.get_model("fixture", "fixture-model").unwrap();
        assert_eq!(model.context_window, 4096); assert_eq!(model.base_url, "https://example.test/v1");
        api.unregister_provider("fixture").unwrap();
        assert!(runtime.get_model("fixture", "fixture-model").is_none());
    }

    #[test]
    fn a_missing_import_file_names_the_path() {
        let error = SessionImportFileNotFoundError { file_path: "/tmp/missing.jsonl".to_owned() };
        assert_eq!(error.to_string(), "File not found: /tmp/missing.jsonl");
    }

    #[tokio::test]
    async fn persisted_fork_reads_durable_history_instead_of_modified_mirror() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let services = crate::agent_session_services::create_agent_session_services(
            crate::agent_session_services::CreateAgentSessionServicesOptions {
                cwd: cwd.clone(), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()), ..Default::default()
            });
        let provider = maho_ai::providers::faux::faux_provider(Default::default());
        let mut manager = crate::session_manager::SessionManager::create(&cwd, dir.path().join("sessions").to_str(), None);
        let user = manager.append_message(serde_json::json!({"role":"user","content":"durable","timestamp":0}));
        let assistant = manager.append_message(serde_json::to_value(maho_ai::providers::faux::faux_assistant_message("answer", Default::default())).expect("assistant"));
        let mut durable = crate::session_manager::SessionManager::open(manager.session_file().expect("file"), None, None, None);
        durable.append_label(user["id"].as_str().expect("user"), Some("durable label"));
        let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()), agent_dir: Some(services.agent_dir.clone()), model: provider.get_model(Some("faux-1")),
            session_manager: Some(manager), tools: Some(Vec::new()), ..Default::default()
        }).await.expect("session");
        let mut runtime = AgentSessionRuntime::new(created.session, services, Vec::new(), None, None);
        runtime.fork(assistant["id"].as_str().expect("assistant"), true).await.expect("persisted fork");
        assert_eq!(runtime.session().with_session_manager(|manager| manager.label(user["id"].as_str().expect("user")).map(str::to_owned)), Some("durable label".to_owned()));
    }

    #[tokio::test]
    async fn unsaved_persisted_fork_rejects_without_replacing_session() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let services = crate::agent_session_services::create_agent_session_services(
            crate::agent_session_services::CreateAgentSessionServicesOptions {
                cwd: cwd.clone(), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()), ..Default::default()
            });
        let provider = maho_ai::providers::faux::faux_provider(Default::default());
        let mut manager = crate::session_manager::SessionManager::create(&cwd, dir.path().join("sessions").to_str(), None);
        let entry = manager.append_message(serde_json::json!({"role":"user","content":"unsaved","timestamp":0}));
        let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()), agent_dir: Some(services.agent_dir.clone()), model: provider.get_model(Some("faux-1")),
            session_manager: Some(manager), tools: Some(Vec::new()), ..Default::default()
        }).await.expect("session");
        let mut runtime = AgentSessionRuntime::new(created.session, services, Vec::new(), None, None);
        let id = runtime.session().session_id();
        assert!(!std::path::Path::new(&runtime.session().session_file().expect("path")).exists());
        let error = runtime.fork(entry["id"].as_str().expect("entry"), true).await.expect_err("unsaved fork");
        assert!(error.starts_with("This session has not been saved yet."));
        assert_eq!(runtime.session().session_id(), id);
        assert_eq!(runtime.session().messages().len(), 1);
    }

    #[tokio::test]
    async fn replacement_restores_target_model_and_thinking() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let services = crate::agent_session_services::create_agent_session_services(
            crate::agent_session_services::CreateAgentSessionServicesOptions {
                cwd: cwd.clone(), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()), ..Default::default()
            });
        let provider = maho_ai::providers::faux::faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions {
            models: Some(vec![
                maho_ai::providers::faux::FauxModelDefinition { id: "original".to_owned(), ..Default::default() },
                maho_ai::providers::faux::FauxModelDefinition { id: "saved".to_owned(), reasoning: Some(true), ..Default::default() },
            ]), ..Default::default()
        });
        let mut model_runtime = services.model_runtime().clone();
        model_runtime.register_native_provider(provider.provider.clone());
        let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()), agent_dir: Some(services.agent_dir.clone()), model: provider.get_model(Some("original")),
            model_runtime: Some(model_runtime), session_manager: Some(crate::session_manager::SessionManager::in_memory(&cwd, None, None)),
            tools: Some(Vec::new()), ..Default::default()
        }).await.expect("session");
        let mut runtime = AgentSessionRuntime::new(created.session, services, Vec::new(), None, None);
        let mut target = crate::session_manager::SessionManager::in_memory(&cwd, None, None);
        target.append_model_change("faux", "saved", None, None);
        target.append_thinking_level_change("high", Some(serde_json::json!({"level":"high","source":"explicit"})));
        runtime.apply_replacement(target, cwd.clone(), maho_ext_api::SessionReason::Resume).await.expect("replacement");
        assert_eq!(runtime.session().model().id, "saved");
        assert_eq!(runtime.session().thinking_level(), maho_ai::types::ModelThinkingLevel::High);
        assert_eq!(runtime.session().thinking_selection().expect("selection").level, maho_ai::types::ModelThinkingLevel::High);
        runtime.launch_profile = Some(AgentSessionLaunchProfile { cwd: cwd.clone(), creation_model: Some(("faux".to_owned(), "original".to_owned())),
            initial_thinking_level: Some("off".to_owned()), auto_title: Some(false), ..Default::default() });
        let mut target = crate::session_manager::SessionManager::in_memory(&cwd, None, None);
        target.append_model_change("faux", "saved", None, None);
        target.append_thinking_level_change("high", Some(serde_json::json!({"level":"high","source":"explicit"})));
        runtime.apply_replacement(target, cwd, maho_ext_api::SessionReason::Resume).await.expect("launch replacement");
        assert_eq!(runtime.session().model().id, "original");
        assert_eq!(runtime.session().thinking_level(), maho_ai::types::ModelThinkingLevel::Off);
        assert!(!runtime.session().replacement_auto_title());
    }

    #[tokio::test]
    async fn replacing_session_preserves_custom_tools_and_auto_title_selection() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let agent_dir = dir.path().join("agent").to_string_lossy().into_owned();
        let services = crate::agent_session_services::create_agent_session_services(
            crate::agent_session_services::CreateAgentSessionServicesOptions {
                cwd: cwd.clone(), agent_dir: Some(agent_dir.clone()), ..Default::default()
            });
        let model = serde_json::from_value(serde_json::json!({
            "id":"faux-1", "name":"faux-1", "provider":"faux", "api":"faux", "baseUrl":"",
            "reasoning":false, "input":[], "contextWindow":128000, "maxTokens":4096,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
        })).expect("model");
        let custom = maho_ext_api::ToolDefinition::new("retained", "custom tool", serde_json::json!({"type":"object"}),
            std::sync::Arc::new(|_| Box::pin(async { Ok(maho_tools::definition::ToolResult::text("retained")) })));
        let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()), agent_dir: Some(agent_dir), model: Some(model),
            model_runtime: Some(services.model_runtime().clone()),
            session_manager: Some(crate::session_manager::SessionManager::in_memory(&cwd, None, None)),
            custom_tools: vec![custom], tools: Some(vec!["retained".to_owned()]), auto_title_sessions: Some(false),
            ..Default::default()
        }).await.expect("session");
        let mut runtime = AgentSessionRuntime::new(created.session, services, Vec::new(), None, None);
        let prompt_path = dir.path().join("system.md");
        std::fs::write(&prompt_path, "original prompt").expect("prompt fixture");
        runtime.session().set_system_prompt_sources(Some(prompt_path.to_string_lossy().into_owned()), Vec::new());
        runtime.set_before_session_invalidate(Some(Box::new(move || {
            std::fs::write(&prompt_path, "prompt after teardown").expect("updated prompt fixture");
        })));
        assert!(runtime.new_session(None, None).await.expect("replacement"));
        let expected = crate::system_prompt::build_system_prompt(&crate::system_prompt::BuildSystemPromptOptions {
            cwd: cwd.clone(), custom_prompt: Some("prompt after teardown".to_owned()), ..Default::default()
        });
        assert_eq!(runtime.session().system_prompt(), expected);
        runtime.set_before_session_invalidate(None);
        assert!(!runtime.session().with_session_manager(|manager| manager.is_persisted()));
        assert!(runtime.session().get_tool_definition("retained").is_some());
        assert!(!runtime.session().replacement_auto_title());
        let result = runtime.session().execute_tool("retained", serde_json::json!({}), Default::default()).await.expect("tool");
        assert_eq!(maho_ai::utils::text::content_text(&result.content, ""), "retained");
        let entry = runtime.session().with_session_manager_mut(|manager| manager.append_message(serde_json::json!({
            "role":"user","content":[{"type":"text","text":"selected text"}],"timestamp":0
        })));
        let before = runtime.fork(entry["id"].as_str().unwrap(), false).await.expect("fork before");
        assert!(!before.cancelled);
        assert_eq!(before.editor_text.as_deref(), Some("selected text"));
        assert!(runtime.session().messages().is_empty());
        let entry = runtime.session().with_session_manager_mut(|manager| manager.append_message(serde_json::json!({
            "role":"user","content":[{"type":"text","text":"retained text"}],"timestamp":0
        })));
        let at = runtime.fork(entry["id"].as_str().unwrap(), true).await.expect("fork at");
        assert!(!at.cancelled);
        assert!(at.editor_text.is_none());
        assert_eq!(runtime.session().messages().len(), 1);
        assert!(runtime.session().get_tool_definition("retained").is_some());
        let target_id = runtime.session().with_session_manager(|manager| manager.leaf_id().unwrap().to_owned());
        let label = runtime.session().with_session_manager_mut(|manager| manager.append_label(&target_id, Some("old label")));
        let descendant = runtime.session().with_session_manager_mut(|manager| manager.append_message(serde_json::json!({
            "role":"user","content":[{"type":"text","text":"descendant"}],"timestamp":0
        })));
        runtime.session().with_session_manager_mut(|manager| manager.append_label(&target_id, Some("resolved label")));
        let forked = runtime.fork(descendant["id"].as_str().unwrap(), true).await.expect("labeled fork");
        assert!(!forked.cancelled);
        runtime.session().with_session_manager(|manager| {
            assert_eq!(manager.label(&target_id), Some("resolved label"));
            assert!(manager.entry(label["id"].as_str().unwrap()).is_none());
            let child = manager.entry(descendant["id"].as_str().unwrap()).unwrap();
            assert_eq!(child["parentId"], target_id);
            assert_eq!(manager.branch(manager.leaf_id()).iter().filter(|entry| entry["type"] == "message").count(), 2);
        });
        assert!(runtime.import_from_jsonl("/missing-import-fixture.jsonl", None).await.unwrap_err().contains("File not found"));
        let source = dir.path().join("import-fixture.jsonl");
        runtime.session().export_to_jsonl(source.to_str()).expect("export fixture");
        let import_directory = dir.path().join("imported");
        runtime.session().with_session_manager_mut(|manager| {
            *manager = crate::session_manager::SessionManager::create(&cwd, import_directory.to_str(), None);
        });
        assert!(runtime.import_from_jsonl(source.to_str().unwrap(), None).await.expect("import"));
        let first_import = runtime.session().session_file().expect("imported path");
        assert!(first_import.ends_with("import-fixture.jsonl"));
        assert_eq!(runtime.session().messages().len(), 2);
        assert!(runtime.import_from_jsonl(source.to_str().unwrap(), None).await.expect("collision import"));
        assert!(runtime.session().session_file().unwrap().ends_with("import-fixture-1.jsonl"));
    }

    #[test]
    fn user_message_text_joins_text_parts_and_skips_others() {
        let content = serde_json::json!([
            { "type": "text", "text": "hello " },
            { "type": "image", "data": "x" },
            { "type": "text", "text": "world" }
        ]);
        assert_eq!(extract_user_message_text(&content), "hello world");
    }

    #[test]
    fn a_string_content_is_returned_verbatim() {
        assert_eq!(extract_user_message_text(&serde_json::json!("plain")), "plain");
    }
}
