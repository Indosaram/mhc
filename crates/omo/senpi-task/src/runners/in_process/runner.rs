//! The in-process runner (`runners/in-process.ts`).

use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::host::HostError;
use crate::runners::in_process::child_handle::{
    ChildSession, InProcessChildHandle, RunnerFailureKind,
};
use crate::runners::in_process::child_options::{
    ChildSessionOptions, HostHandle, build_child_session_options, require_child_session_dir,
    resolve_member_scoped_tool_names,
};
use crate::runners::in_process::runner_error::RunnerError;
use crate::runners::in_process::session_manager::ChildSessionManager;
use crate::runners::in_process::shared_tool_filter::ChildToolRef;
use crate::runners::in_process::subagent_prompt::{SubagentPromptInput, build_subagent_prompt};
use crate::state::ResolvedModelRecord;

pub const DEFAULT_MAX_CHILD_DEPTH: u32 = 4;

#[derive(Clone, Default)]
pub struct ChildSpec {
    pub task_id: String,
    pub cwd: String,
    /// Mandatory in practice; `None` models a legacy persisted spec.
    pub session_dir: Option<String>,
    pub agent_dir: Option<String>,
    pub auth_storage: Option<HostHandle>,
    pub model_registry: Option<HostHandle>,
    pub model_runtime: Option<HostHandle>,
    pub model: Option<HostHandle>,
    pub thinking_level: Option<String>,
    pub selected_model: Option<String>,
    pub requested_model: Option<ResolvedModelRecord>,
    pub fallback_models: Option<Vec<ResolvedModelRecord>>,
    pub resolved_model: Option<ResolvedModelRecord>,
    pub tool_allowlist: Option<Vec<String>>,
    pub tool_denylist: Option<Vec<String>>,
    pub member_scoped_tool_names: Option<Vec<String>>,
    pub member_scoped_tools: Option<Vec<ChildToolRef>>,
    pub depth: u32,
    pub parent_session_id: String,
    pub root_session_id: String,
    pub agent_type: Option<String>,
    pub instructions: Option<String>,
    pub prompt: String,
}

/// The host's `createAgentSession`; senpi-task never links the host, so the caller supplies it.
pub type CreateChildSession =
    Arc<dyn Fn(ChildSessionOptions) -> Result<Arc<dyn ChildSession>, HostError> + Send + Sync>;

pub struct InProcessRunnerOptions {
    pub shared_parent_tools: Vec<ChildToolRef>,
    pub ui_only_tool_names: Vec<String>,
    pub max_depth: Option<u32>,
    pub create_session: CreateChildSession,
}

pub struct InProcessRunner {
    shared_parent_tools: Vec<ChildToolRef>,
    ui_only_tool_names: Vec<String>,
    max_depth: u32,
    create_session: CreateChildSession,
}

impl InProcessRunner {
    pub fn new(options: InProcessRunnerOptions) -> Self {
        Self {
            shared_parent_tools: options.shared_parent_tools,
            ui_only_tool_names: options.ui_only_tool_names,
            max_depth: options.max_depth.unwrap_or(DEFAULT_MAX_CHILD_DEPTH),
            create_session: options.create_session,
        }
    }

    pub fn start(&self, spec: &ChildSpec) -> Result<Arc<InProcessChildHandle>, RunnerError> {
        if spec.depth > self.max_depth {
            return Err(RunnerError::new(
                RunnerFailureKind::DepthExceeded,
                format!(
                    "Child depth {} exceeds max depth {}.",
                    spec.depth, self.max_depth
                ),
            ));
        }
        let session_dir = require_child_session_dir(spec)?;
        let create_failed = |error: HostError| {
            RunnerError::caused_by(
                RunnerFailureKind::SessionCreateFailed,
                format!(
                    "Failed to create in-process child session: {}",
                    error.message
                ),
                error,
            )
        };
        let session_manager =
            ChildSessionManager::create(&spec.cwd, session_dir).map_err(|error| {
                create_failed(HostError {
                    message: error.to_string(),
                })
            })?;
        let options = build_child_session_options(
            spec,
            session_manager,
            &self.shared_parent_tools,
            &self.ui_only_tool_names,
        );
        let session = (self.create_session)(options).map_err(create_failed)?;
        let prompt_text = build_subagent_prompt(&SubagentPromptInput {
            task_id: &spec.task_id,
            parent_session_id: &spec.parent_session_id,
            root_session_id: &spec.root_session_id,
            depth: spec.depth,
            agent_type: spec.agent_type.as_deref(),
            instructions: spec.instructions.as_deref(),
            prompt: &spec.prompt,
        });
        Ok(InProcessChildHandle::start(
            &spec.task_id,
            session,
            &prompt_text,
        ))
    }

    pub fn resume(
        &self,
        spec: &ChildSpec,
        session_path: &Path,
    ) -> Result<Arc<InProcessChildHandle>, RunnerError> {
        let member_scoped_tools = resolve_member_scoped_tool_names(
            spec.member_scoped_tool_names.as_deref().unwrap_or_default(),
            &self.shared_parent_tools,
        )?;
        assert_usable_session_file(session_path)?;
        let session_dir = require_child_session_dir(spec)?;
        let spec = ChildSpec {
            member_scoped_tools: Some(member_scoped_tools),
            ..spec.clone()
        };
        let options = build_child_session_options(
            &spec,
            ChildSessionManager::open(session_path, session_dir, &spec.cwd),
            &self.shared_parent_tools,
            &self.ui_only_tool_names,
        );
        let session = (self.create_session)(options).map_err(|error| {
            RunnerError::caused_by(
                RunnerFailureKind::SessionUnavailable,
                format!(
                    "Failed to resume in-process child session: {}",
                    error.message
                ),
                error,
            )
        })?;
        Ok(InProcessChildHandle::restored(&spec.task_id, session))
    }
}

fn is_session_header(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("session")
        && entry.get("id").is_some_and(Value::is_string)
}

/// The first parseable line must be a session header; unparseable lines are skipped.
fn assert_usable_session_file(session_path: &Path) -> Result<(), RunnerError> {
    let content = fs::read_to_string(session_path).map_err(|error| {
        RunnerError::caused_by(
            RunnerFailureKind::SessionUnavailable,
            format!(
                "Child session file is unreadable: {}",
                session_path.display()
            ),
            HostError {
                message: error.to_string(),
            },
        )
    })?;
    let first_entry = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find_map(|line| serde_json::from_str::<Value>(line).ok());
    match first_entry {
        Some(entry) if is_session_header(&entry) => Ok(()),
        _ => Err(RunnerError::new(
            RunnerFailureKind::SessionUnavailable,
            format!(
                "Child session file has no session header: {}",
                session_path.display()
            ),
        )),
    }
}
