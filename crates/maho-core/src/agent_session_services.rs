//! Port of senpi `packages/coding-agent/src/core/agent-session-services.ts`.
//!
//! Adaptations forced by the Rust tree (plan D-M5): extensions are native crates, so there is no
//! `DefaultResourceLoader` and no queued provider registrations to replay here (the session binds
//! extension providers when it installs its runner). The services bundle carries the auth storage,
//! model registry/runtime and settings manager. Extensions load after services are created, so
//! [`create_agent_session_from_services`] applies the CLI extension flag values against the
//! loaded extensions' flag registry (`applyExtensionFlagValues`).

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ext_api::FlagValue;

use crate::auth_storage::AuthStorage;
use crate::model_registry::ModelRegistry;
use crate::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use crate::settings_manager::SettingsManager;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentSessionRuntimeDiagnosticType {
    Info,
    Warning,
    Error,
}

/// Non-fatal issues collected while creating services or sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSessionRuntimeDiagnostic {
    pub kind: AgentSessionRuntimeDiagnosticType,
    pub message: String,
}

#[derive(Default)]
pub struct CreateAgentSessionServicesOptions {
    pub cwd: String,
    pub agent_dir: Option<String>,
    pub settings_manager: Option<SettingsManager>,
    pub model_runtime: Option<ModelRuntime>,
}

#[derive(Default)]
pub struct CreateAgentSessionFromServicesOptions {
    pub session_manager: Option<crate::session_manager::SessionManager>,
    pub session_start_event: Option<maho_ext_api::SessionStartEvent>,
    pub model: Option<maho_ai::model::Model>,
    pub thinking_level: Option<maho_ai::types::ThinkingLevel>,
    pub thinking_selection: Option<maho_ai::types::ThinkingSelection>,
    pub scoped_models: Vec<crate::agent_session::SessionModelEntry>,
    pub favorite_models: Vec<crate::agent_session::SessionModelEntry>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub no_tools: Option<crate::sdk::NoToolsMode>,
    pub custom_tools: Vec<maho_ext_api::ToolDefinition>,
    pub auto_title_sessions: Option<bool>,
    pub extension_factories: Vec<maho_ext_host::loader::NativeAsyncExtensionFactory>,
    pub loaded_extensions: Option<maho_ext_host::loader::LoadExtensionsResult>,
    pub defer_extension_start: bool,
    pub session_profile: maho_ext_api::ExtensionSessionProfile,
    /// CLI extension flags, applied against the flags the loaded extensions registered.
    pub extension_flag_values: Option<BTreeMap<String, FlagValue>>,
}

pub struct MountedAgentSessionServices {
    pub cwd: String,
    pub agent_dir: String,
    pub auth_storage: Arc<AuthStorage>,
    pub model_registry: ModelRegistry,
    pub settings_manager: Arc<std::sync::Mutex<SettingsManager>>,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

impl From<AgentSessionServices> for MountedAgentSessionServices {
    fn from(services: AgentSessionServices) -> Self {
        Self { cwd: services.cwd, agent_dir: services.agent_dir, auth_storage: services.auth_storage,
            model_registry: services.model_registry, settings_manager: Arc::new(std::sync::Mutex::new(services.settings_manager)),
            diagnostics: services.diagnostics }
    }
}

impl MountedAgentSessionServices {
    pub fn model_runtime(&self) -> &ModelRuntime { &self.model_registry.model_runtime }
}

pub struct CreateAgentSessionFromServicesResult {
    pub session: crate::agent_session::AgentSession,
    pub services: MountedAgentSessionServices,
    pub model_fallback_message: Option<String>,
}

pub async fn create_agent_session_from_services(services: AgentSessionServices, options: CreateAgentSessionFromServicesOptions)
    -> Result<CreateAgentSessionFromServicesResult, String> {
    let AgentSessionServices { cwd, agent_dir, auth_storage, model_registry, settings_manager, mut diagnostics } = services;
    let mut options = options;
    // senpi applies extension flag values once the resource loader has loaded the extensions; the
    // native host loads them here so the session constructor sees the applied `runtime.flagValues`.
    if options.extension_flag_values.is_some() {
        let loaded = match options.loaded_extensions.take() {
            Some(loaded) => loaded,
            None => maho_ext_host::loader::load_extensions_async(options.extension_factories.clone(),
                std::path::Path::new(&cwd), options.session_profile.clone()).await,
        };
        diagnostics.extend(apply_extension_flag_values(Some(&loaded), options.extension_flag_values.as_ref()));
        options.loaded_extensions = Some(loaded);
    }
    let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(agent_dir.clone()), auth_storage: Some(auth_storage.clone()),
        model_registry: Some(model_registry.clone()), settings_manager: Some(settings_manager),
        session_manager: options.session_manager, session_start_event: options.session_start_event,
        model: options.model, thinking_level: options.thinking_level, thinking_selection: options.thinking_selection,
        scoped_models: options.scoped_models, favorite_models: options.favorite_models, tools: options.tools,
        exclude_tools: options.exclude_tools, no_tools: options.no_tools, custom_tools: options.custom_tools,
        auto_title_sessions: options.auto_title_sessions, extension_factories: options.extension_factories,
        loaded_extensions: options.loaded_extensions, defer_extension_start: options.defer_extension_start,
        session_profile: options.session_profile, ..Default::default()
    }).await?;
    let settings_manager = created.session.shared_settings_manager();
    Ok(CreateAgentSessionFromServicesResult { services: MountedAgentSessionServices {
        cwd, agent_dir, auth_storage, model_registry, settings_manager, diagnostics },
        session: created.session, model_fallback_message: created.model_fallback_message })
}

/// Coherent cwd-bound runtime services for one effective session cwd.
///
/// Adaptation: the Rust `ModelRegistry` owns its `ModelRuntime`, so the bundle carries the registry
/// and exposes the runtime through [`AgentSessionServices::model_runtime`] instead of storing both.
pub struct AgentSessionServices {
    pub cwd: String,
    pub agent_dir: String,
    pub auth_storage: Arc<AuthStorage>,
    pub model_registry: ModelRegistry,
    pub settings_manager: SettingsManager,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

impl AgentSessionServices {
    pub fn model_runtime(&self) -> &ModelRuntime {
        &self.model_registry.model_runtime
    }
}

/// Validate the CLI-provided extension flag values against the flags the loaded extensions
/// registered, writing accepted values into the extension runtime's flag values.
pub fn apply_extension_flag_values(
    extensions: Option<&maho_ext_host::loader::LoadExtensionsResult>,
    extension_flag_values: Option<&BTreeMap<String, FlagValue>>,
) -> Vec<AgentSessionRuntimeDiagnostic> {
    let Some(extension_flag_values) = extension_flag_values else {
        return Vec::new();
    };
    let mut registered_flags = BTreeMap::new();
    for extension in extensions.map(|loaded| loaded.extensions.as_slice()).unwrap_or_default() {
        for flag in &extension.flags {
            registered_flags.entry(flag.name.clone()).or_insert_with(|| flag.kind.clone());
        }
    }
    let mut diagnostics = Vec::new();
    let mut unknown = Vec::new();
    for (name, value) in extension_flag_values {
        let (Some(kind), Some(loaded)) = (registered_flags.get(name), extensions) else {
            unknown.push(name.clone());
            continue;
        };
        match (kind, value) {
            (maho_ext_api::FlagType::Boolean { .. }, _) => loaded.runtime.set_flag(name, FlagValue::Boolean(true)),
            (maho_ext_api::FlagType::String { .. }, FlagValue::String(value)) => loaded.runtime.set_flag(name, FlagValue::String(value.clone())),
            (maho_ext_api::FlagType::String { .. }, FlagValue::Boolean(_)) => diagnostics.push(AgentSessionRuntimeDiagnostic {
                kind: AgentSessionRuntimeDiagnosticType::Error,
                message: format!("Extension flag \"--{name}\" requires a value"),
            }),
        }
    }
    if !unknown.is_empty() {
        let suffix = if unknown.len() == 1 { "" } else { "s" };
        let names = unknown.iter().map(|name| format!("--{name}")).collect::<Vec<_>>().join(", ");
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            kind: AgentSessionRuntimeDiagnosticType::Error,
            message: format!("Unknown option{suffix}: {names}"),
        });
    }
    diagnostics
}

/// Create cwd-bound runtime services (no AgentSession).
pub async fn create_agent_session_services(mut options: CreateAgentSessionServicesOptions) -> AgentSessionServices {
    let agent_dir = options.agent_dir.clone().unwrap_or_else(crate::config::get_agent_dir);
    let auth_path = std::path::Path::new(&agent_dir).join("auth.json");
    let auth_storage = Arc::new(AuthStorage::create(&auth_path.to_string_lossy()));
    let owns_model_runtime = options.model_runtime.is_none();
    let mut model_runtime = options.model_runtime.take().unwrap_or_else(|| {
        ModelRuntime::create_sync(CreateModelRuntimeOptions {
            models_path: Some(std::path::Path::new(&agent_dir).join("models.json")),
            auth_path: Some(auth_path.clone()),
            credentials: Some(Arc::clone(&auth_storage)),
            providers: None,
        })
    });
    let auth_storage = model_runtime.credentials.clone();
    let settings_manager = options
        .settings_manager
        .take()
        .unwrap_or_else(|| SettingsManager::create(&options.cwd, &agent_dir, &crate::config::home_dir(), false));
    // A runtime built for these services refreshes everything it composed (offline); a runtime
    // shared across a host's sessions was already refreshed by earlier opens (senpi#1844).
    if owns_model_runtime {
        model_runtime.refresh(maho_ai::models::ModelsRefreshOptions { allow_network: Some(false), ..Default::default() }).await;
    }
    let model_registry = ModelRegistry::new(model_runtime);
    // Extension flag values are validated by `create_agent_session_from_services`, once the
    // extensions that register them are loaded.
    let diagnostics = Vec::new();
    AgentSessionServices {
        cwd: options.cwd.clone(),
        agent_dir,
        auth_storage,
        model_registry,
        settings_manager,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_flag_without_a_value_is_an_error() {
        let loaded = loaded_with_flag("name", maho_ext_api::FlagType::String { default: None });
        let flags = BTreeMap::from([("name".to_owned(), FlagValue::Boolean(true))]);
        let diagnostics = apply_extension_flag_values(Some(&loaded), Some(&flags));
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.message.contains("requires a value")));
    }

    fn loaded_with_flag(name: &str, kind: maho_ext_api::FlagType) -> maho_ext_host::loader::LoadExtensionsResult {
        let mut extension = maho_ext_api::LoadedExtension::new("<test:flags>", std::path::PathBuf::from("/tmp"), Default::default());
        extension.flags.push(maho_ext_api::ExtensionFlag { name: name.to_owned(), description: None, kind, extension_path: "<test:flags>".to_owned() });
        maho_ext_host::loader::LoadExtensionsResult {
            extensions: vec![extension], errors: Vec::new(), runtime: Default::default(), events: Default::default(),
        }
    }

    #[test]
    fn registered_flags_are_written_to_the_extension_runtime() {
        let loaded = loaded_with_flag("no-ask-user", maho_ext_api::FlagType::Boolean { default: Some(false) });
        let flags = BTreeMap::from([("no-ask-user".to_owned(), FlagValue::String("yes".to_owned()))]);
        assert!(apply_extension_flag_values(Some(&loaded), Some(&flags)).is_empty());
        assert_eq!(loaded.runtime.get_flag("no-ask-user"), Some(FlagValue::Boolean(true)));
    }

    #[test]
    fn an_unknown_flag_is_reported_once_with_all_names() {
        let mut flags = BTreeMap::new();
        flags.insert("alpha".to_owned(), FlagValue::Boolean(true));
        flags.insert("beta".to_owned(), FlagValue::Boolean(true));
        let diagnostics = apply_extension_flag_values(None, Some(&flags));
        let unknown: Vec<&AgentSessionRuntimeDiagnostic> = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message.starts_with("Unknown option"))
            .collect();
        assert_eq!(unknown.len(), 1);
        assert!(unknown[0].message.contains("--alpha"));
        assert!(unknown[0].message.contains("--beta"));
    }

    #[test]
    fn no_flag_values_produce_no_diagnostics() {
        assert!(apply_extension_flag_values(None, None).is_empty());
    }

    #[tokio::test]
    async fn services_carry_the_requested_cwd_and_agent_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            ..Default::default()
        }).await;
        assert_eq!(services.cwd, dir.path().to_string_lossy());
        assert!(services.agent_dir.ends_with("agent"));
        assert!(services.diagnostics.is_empty());
    }

    #[tokio::test]
    async fn supplied_runtime_and_services_share_the_authoritative_credential_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let credentials = Arc::new(AuthStorage::in_memory(Default::default()));
        let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
            credentials: Some(credentials.clone()), providers: Some(Vec::new()), ..Default::default()
        });
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model_runtime: Some(runtime), ..Default::default()
        }).await;
        assert!(Arc::ptr_eq(&services.auth_storage, &credentials));
        assert!(Arc::ptr_eq(&services.auth_storage, &services.model_runtime().credentials));
        services.auth_storage.set("fixture", Some(serde_json::json!({"type":"api_key","key":"fixture-key"})))
            .expect("write through service");
        assert_eq!(credentials.get("fixture"), services.model_runtime().credentials.get("fixture"));
    }

    #[tokio::test]
    async fn create_agent_session_from_services_shares_the_settings_manager() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().to_string_lossy().into_owned();
        let mut settings = SettingsManager::from_storage(Box::new(crate::settings_manager::InMemorySettingsStorage::default()), false);
        settings.set(crate::settings_manager::SettingsScope::Global,
            &serde_json::from_value(serde_json::json!({"defaultThinkingLevel":"high"})).expect("settings"))
            .expect("save");
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: cwd.clone(), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            settings_manager: Some(settings), ..Default::default()
        }).await;
        let provider = maho_ai::providers::faux::faux_provider(Default::default());
        let created = create_agent_session_from_services(services, CreateAgentSessionFromServicesOptions {
            model: provider.get_model(Some("faux-1")), tools: Some(Vec::new()), ..Default::default()
        }).await.expect("session");
        assert_eq!(created.session.with_settings_manager(|manager| manager.get_string("defaultThinkingLevel")), Some("high".to_owned()));
    }
}
