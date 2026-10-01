//! Port of senpi `packages/coding-agent/src/core/agent-session-runtime.ts`.
//!
//! Not ported (documented in `parity.d/21.md`): the session replacement flows (`switchSession`,
//! `newSession`, `fork`, `importFromJsonl` and `createAgentSessionRuntime`) all call
//! `AgentSession::abort` and `createReplacedSessionContext` plus a runtime factory from `sdk.ts`,
//! none of which are ported yet, so only the launch-profile types, the runtime holder's accessors
//! and `dispose` are here.

use crate::agent_session::AgentSession;
use crate::agent_session_services::{AgentSessionRuntimeDiagnostic, AgentSessionServices};
use crate::session_cwd::assert_session_cwd_exists;

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
    fn a_missing_import_file_names_the_path() {
        let error = SessionImportFileNotFoundError { file_path: "/tmp/missing.jsonl".to_owned() };
        assert_eq!(error.to_string(), "File not found: /tmp/missing.jsonl");
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
