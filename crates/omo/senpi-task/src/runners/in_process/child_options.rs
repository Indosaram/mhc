//! Child session option assembly (`runners/in-process/child-options.ts`).

use std::any::Any;
use std::sync::Arc;

use crate::agents::curated_readonly_agent_names;
use crate::runners::in_process::child_handle::RunnerFailureKind;
use crate::runners::in_process::curated_readonly_bash::CuratedReadonlyBashTool;
use crate::runners::in_process::runner::ChildSpec;
use crate::runners::in_process::runner_error::RunnerError;
use crate::runners::in_process::runtime_fallback_settings::{
    RetryFallbackSettings, create_runtime_fallback_settings,
};
use crate::runners::in_process::session_manager::ChildSessionManager;
use crate::runners::in_process::shared_tool_filter::{ChildToolRef, merge_child_custom_tools};

/// An opaque host object (auth storage, model registry, model runtime, model) passed through
/// to the host untouched; identity is preserved.
pub type HostHandle = Arc<dyn Any + Send + Sync>;

/// The host's minimal extension-free resource loader (`createChildResourceLoader`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildResourceLoader {
    Minimal,
}

impl ChildResourceLoader {
    pub fn extension_count(self) -> usize {
        match self {
            Self::Minimal => 0,
        }
    }
}

/// The host's `CreateAgentSessionOptions` subset the runner fills.
#[derive(Clone)]
pub struct ChildSessionOptions {
    pub cwd: String,
    pub session_manager: Arc<ChildSessionManager>,
    pub resource_loader: ChildResourceLoader,
    pub custom_tools: Vec<ChildToolRef>,
    pub agent_dir: Option<String>,
    pub auth_storage: Option<HostHandle>,
    pub model_registry: Option<HostHandle>,
    pub model_runtime: Option<HostHandle>,
    pub model: Option<HostHandle>,
    pub thinking_level: Option<String>,
    pub settings: RetryFallbackSettings,
    /// The allowlist; `tools` alone does not deny.
    pub tools: Option<Vec<String>>,
    /// The denylist on the host's real deny field.
    pub exclude_tools: Option<Vec<String>>,
}

/// Without a session dir the transcript would land outside `stateDir/children/<taskId>/`,
/// so a legacy spec fails typed instead of falling back to a default location.
pub fn require_child_session_dir(spec: &ChildSpec) -> Result<&str, RunnerError> {
    spec.session_dir
        .as_deref()
        .filter(|dir| !dir.is_empty())
        .ok_or_else(|| {
            RunnerError::new(
                RunnerFailureKind::SessionCreateFailed,
                format!(
                    "Child {} has no sessionDir; refusing to fall back to a default session location.",
                    spec.task_id
                ),
            )
        })
}

/// Re-resolve persisted member-scoped tool names against the raw live shared tools; a
/// duplicate, missing, or ambiguous name is a typed `tools_unavailable` failure.
pub fn resolve_member_scoped_tool_names(
    names: &[String],
    shared_parent_tools: &[ChildToolRef],
) -> Result<Vec<ChildToolRef>, RunnerError> {
    let mut resolved: Vec<ChildToolRef> = Vec::new();
    for (index, name) in names.iter().enumerate() {
        if names[..index].contains(name) {
            return Err(RunnerError::new(
                RunnerFailureKind::ToolsUnavailable,
                format!("Member-scoped tool name \"{name}\" is duplicated in the persisted spec."),
            ));
        }
        let matches: Vec<&ChildToolRef> = shared_parent_tools
            .iter()
            .filter(|tool| tool.name() == name)
            .collect();
        match matches.as_slice() {
            [tool] => resolved.push(Arc::clone(tool)),
            [] => {
                return Err(RunnerError::new(
                    RunnerFailureKind::ToolsUnavailable,
                    format!(
                        "Member-scoped tool \"{name}\" is not available in the live parent tools."
                    ),
                ));
            }
            many => {
                return Err(RunnerError::new(
                    RunnerFailureKind::ToolsUnavailable,
                    format!(
                        "Member-scoped tool \"{name}\" matches {} live parent tools.",
                        many.len()
                    ),
                ));
            }
        }
    }
    Ok(resolved)
}

pub fn build_child_session_options(
    spec: &ChildSpec,
    session_manager: ChildSessionManager,
    shared_parent_tools: &[ChildToolRef],
    ui_only_tool_names: &[String],
) -> ChildSessionOptions {
    let merged = merge_child_custom_tools(
        shared_parent_tools,
        spec.member_scoped_tools.as_deref(),
        ui_only_tool_names,
    );
    let curated = spec
        .agent_type
        .as_deref()
        .is_some_and(|agent| curated_readonly_agent_names().contains(agent));
    let custom_tools = if curated {
        let mut tools: Vec<ChildToolRef> = merged
            .into_iter()
            .filter(|tool| tool.name() != CuratedReadonlyBashTool::NAME)
            .collect();
        tools.push(Arc::new(CuratedReadonlyBashTool::new(&spec.cwd, None)));
        tools
    } else {
        merged
    };
    ChildSessionOptions {
        cwd: spec.cwd.clone(),
        session_manager: Arc::new(session_manager),
        resource_loader: ChildResourceLoader::Minimal,
        custom_tools,
        agent_dir: spec.agent_dir.clone(),
        auth_storage: spec.auth_storage.clone(),
        model_registry: spec.model_registry.clone(),
        model_runtime: spec.model_runtime.clone(),
        model: spec.model.clone(),
        thinking_level: spec.thinking_level.clone(),
        settings: create_runtime_fallback_settings(
            spec.selected_model.as_deref(),
            spec.fallback_models.as_deref(),
        ),
        tools: spec.tool_allowlist.clone(),
        exclude_tools: spec.tool_denylist.clone(),
    }
}
