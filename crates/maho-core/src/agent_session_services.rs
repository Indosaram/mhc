//! Port of senpi `packages/coding-agent/src/core/agent-session-services.ts`.
//!
//! Adaptations forced by the Rust tree (plan D-M5): extensions are native crates, so there is no
//! `DefaultResourceLoader`, no extension-flag registry and no queued provider registrations to
//! replay. The services bundle therefore carries the auth storage, model registry/runtime and
//! settings manager, and [`apply_extension_flag_values`] treats every supplied flag as unknown
//! because no extension declared it.

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
    pub extension_flag_values: Option<BTreeMap<String, FlagValue>>,
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
}

pub struct MountedAgentSessionServices {
    pub cwd: String,
    pub agent_dir: String,
    pub auth_storage: Arc<AuthStorage>,
    pub model_registry: ModelRegistry,
    pub settings_manager: Arc<std::sync::Mutex<SettingsManager>>,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

pub struct CreateAgentSessionFromServicesResult {
    pub session: crate::agent_session::AgentSession,
    pub services: MountedAgentSessionServices,
    pub model_fallback_message: Option<String>,
}

pub async fn create_agent_session_from_services(services: AgentSessionServices, options: CreateAgentSessionFromServicesOptions)
    -> Result<CreateAgentSessionFromServicesResult, String> {
    let AgentSessionServices { cwd, agent_dir, auth_storage, model_registry, settings_manager, diagnostics } = services;
    let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(agent_dir.clone()), auth_storage: Some(auth_storage.clone()),
        model_registry: Some(model_registry.clone()), settings_manager: Some(settings_manager),
        session_manager: options.session_manager, session_start_event: options.session_start_event,
        model: options.model, thinking_level: options.thinking_level, thinking_selection: options.thinking_selection,
        scoped_models: options.scoped_models, favorite_models: options.favorite_models, tools: options.tools,
        exclude_tools: options.exclude_tools, no_tools: options.no_tools, custom_tools: options.custom_tools,
        auto_title_sessions: options.auto_title_sessions, extension_factories: options.extension_factories,
        loaded_extensions: options.loaded_extensions, ..Default::default()
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

/// Validate the CLI-provided extension flag values against the flags extensions registered.
///
/// Adaptation: with no extension flag registry (D-M5) every supplied flag is unknown, so a
/// boolean flag is accepted and a string flag is reported as requiring a value.
pub fn apply_extension_flag_values(
    extension_flag_values: Option<&BTreeMap<String, FlagValue>>,
) -> Vec<AgentSessionRuntimeDiagnostic> {
    let Some(extension_flag_values) = extension_flag_values else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    let mut unknown = Vec::new();
    for (name, value) in extension_flag_values {
        match value {
            FlagValue::Boolean(_) => {}
            FlagValue::String(value) if !value.is_empty() => {}
            FlagValue::String(_) => diagnostics.push(AgentSessionRuntimeDiagnostic {
                kind: AgentSessionRuntimeDiagnosticType::Error,
                message: format!("Extension flag \"--{name}\" requires a value"),
            }),
        }
        unknown.push(name.clone());
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
pub fn create_agent_session_services(mut options: CreateAgentSessionServicesOptions) -> AgentSessionServices {
    let agent_dir = options.agent_dir.clone().unwrap_or_else(crate::config::get_agent_dir);
    let auth_path = std::path::Path::new(&agent_dir).join("auth.json");
    let auth_storage = Arc::new(AuthStorage::create(&auth_path.to_string_lossy()));
    let model_runtime = options.model_runtime.take().unwrap_or_else(|| {
        ModelRuntime::create_sync(CreateModelRuntimeOptions {
            models_path: Some(std::path::Path::new(&agent_dir).join("models.json")),
            auth_path: Some(auth_path.clone()),
            credentials: Some(Arc::clone(&auth_storage)),
            providers: None,
        })
    });
    let settings_manager = options
        .settings_manager
        .take()
        .unwrap_or_else(|| SettingsManager::create(&options.cwd, &agent_dir, &crate::config::home_dir(), false));
    let model_registry = ModelRegistry::new(model_runtime);
    let diagnostics = apply_extension_flag_values(options.extension_flag_values.as_ref());
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
        let mut flags = BTreeMap::new();
        flags.insert("name".to_owned(), FlagValue::String(String::new()));
        let diagnostics = apply_extension_flag_values(Some(&flags));
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.message.contains("requires a value")));
    }

    #[test]
    fn an_unknown_flag_is_reported_once_with_all_names() {
        let mut flags = BTreeMap::new();
        flags.insert("alpha".to_owned(), FlagValue::Boolean(true));
        flags.insert("beta".to_owned(), FlagValue::Boolean(true));
        let diagnostics = apply_extension_flag_values(Some(&flags));
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
        assert!(apply_extension_flag_values(None).is_empty());
    }

    #[test]
    fn services_carry_the_requested_cwd_and_agent_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            ..Default::default()
        });
        assert_eq!(services.cwd, dir.path().to_string_lossy());
        assert!(services.agent_dir.ends_with("agent"));
        assert!(services.diagnostics.is_empty());
    }
}
