//! Unified SessionControl port & ControlRefusal (`thread/control/port.ts`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::contracts::results::ThreadWaitOutcome;
use super::super::contracts::state::ThreadModel;
use super::super::errors::{ThreadToolFailure, thread_tool_failure};
use super::super::paged_reader::EntryPageRead;
use super::super::reader::ThreadTranscriptEntry;
use super::super::tools::model_match::ModelChoice;
use super::super::tools::ports::{BoxFuture, ThreadCloseAnswer, ThreadCloseOptions};
use super::super::tools::thread_state::thread_model;
use super::interrupt::InterruptOutcome;

pub const SESSION_ONLY_THINKING_SCOPE: &str = "turn";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionModelState {
    pub model: ThreadModel,
    pub thinking_level: String,
}

pub fn session_model_state(
    model: Option<&Value>,
    thinking_level: Option<&str>,
) -> Option<SessionModelState> {
    let model = thread_model(model)?;
    let thinking_level = thinking_level?.to_owned();
    Some(SessionModelState {
        model,
        thinking_level,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ControlRefusal {
    pub failure: ThreadToolFailure,
}

impl std::fmt::Display for ControlRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.failure.message)
    }
}

impl std::error::Error for ControlRefusal {}

#[derive(Debug)]
pub enum ControlError {
    Refusal(ControlRefusal),
    Transport(String),
}

impl ControlError {
    pub fn into_refusal(self) -> Result<ThreadToolFailure, String> {
        match self {
            Self::Refusal(r) => Ok(r.failure),
            Self::Transport(msg) => Err(msg),
        }
    }
}

impl From<ControlRefusal> for ControlError {
    fn from(r: ControlRefusal) -> Self {
        Self::Refusal(r)
    }
}

pub fn refuse(
    code: &str,
    message: impl Into<String>,
    next: impl Into<String>,
    details: Option<Value>,
) -> ControlRefusal {
    ControlRefusal {
        failure: thread_tool_failure(code, message, next, details),
    }
}

pub fn control_identity(thread_id: &str, device: Option<&str>) -> Value {
    match device {
        Some(d) => serde_json::json!({ "thread": thread_id, "device": d }),
        None => serde_json::json!({ "thread": thread_id }),
    }
}

#[derive(Debug, Clone)]
pub struct AvailableModels {
    pub models: Vec<ModelChoice>,
    pub held: bool,
}

pub trait SessionControl: Send + Sync {
    fn thread_id(&self) -> &str;
    fn device(&self) -> Option<&str> {
        None
    }
    fn title(&self) -> &str;
    fn visible(
        &self,
    ) -> BoxFuture<Result<Vec<super::super::contracts::results::ThreadSummary>, ControlError>>;
    fn interrupt(&self, turn_id: Option<&str>)
    -> BoxFuture<Result<InterruptOutcome, ControlError>>;
    fn close(
        &self,
        options: ThreadCloseOptions,
    ) -> BoxFuture<Result<ThreadCloseAnswer, ControlError>>;
    fn set_name(&self, name: &str) -> BoxFuture<Result<(), ControlError>>;
    fn get_available_models(&self, fresh: bool)
    -> BoxFuture<Result<AvailableModels, ControlError>>;
    fn get_model_state(&self) -> BoxFuture<Result<Option<SessionModelState>, ControlError>>;
    fn set_model(&self, provider: &str, id: &str) -> BoxFuture<Result<ThreadModel, ControlError>>;
    fn get_available_thinking_levels(&self) -> BoxFuture<Result<Vec<String>, ControlError>>;
    fn set_thinking_level(&self, level: &str) -> BoxFuture<Result<(), ControlError>>;
    fn get_messages(&self) -> BoxFuture<Result<Vec<ThreadTranscriptEntry>, ControlError>>;
    fn read_entry_page(
        &self,
        max_bytes: Option<usize>,
        cursor: Option<&str>,
    ) -> BoxFuture<Result<Option<EntryPageRead>, ControlError>> {
        let _ = (max_bytes, cursor);
        Box::pin(async { Ok(None) })
    }
    fn wait_settled(&self, timeout_ms: u64) -> BoxFuture<Result<ThreadWaitOutcome, ControlError>>;
    fn running(&self) -> BoxFuture<Result<bool, ControlError>>;
    fn set_archived(&self, archived: bool) -> BoxFuture<Result<(), ControlError>>;
    fn listed(&self, archived: bool) -> BoxFuture<Result<Value, ControlError>>;
}

pub type SharedSessionControl = Arc<dyn SessionControl>;
