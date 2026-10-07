use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use maho_ext_api::AgentMessage;
use memory_core::recall::RecallNudge;

/// One child observation the tracked turn consumes, mirroring the three `ChildSessionEvent` types
/// `sidecar-turn.ts` `observe()` reads: a tool start, a tool end, and a message end.
///
/// Starts and ends are DISTINCT phases, not one undifferentiated counter: a start past the budget
/// aborts the turn at once, so an overlapping parallel batch can exceed the start budget before any
/// end, while `tool_call_id` correlates a start with its end so one call is never counted twice.
///
/// The end carries the REAL reported result metadata, never a fabricated one. `is_error` is the
/// native event's error flag, `terminate` the native result's stop hint (absent = false), and
/// `refusal` the structured refusal code - `is_error` alone cannot tell an exhausted-budget refusal
/// from a bad-argument rejection, so the code is the machine-consumed discriminator.
#[derive(Clone, Debug, PartialEq)]
pub enum KibitzerChildObservation {
    /// `tool_execution_start`: the call is now executing.
    ToolStart { tool_call_id: String, name: String },
    /// `tool_execution_end`: the call finished, with its reported result/refusal metadata.
    ToolEnd {
        tool_call_id: String,
        name: String,
        is_error: bool,
        terminate: bool,
        /// The `rejected` code of the tool's refusal body (`tool_budget_exceeded`,
        /// `invalid_pattern`, `missing_argument`, `not_committed`, `unsupported_operation`, ...),
        /// or `None` when the call was not refused.
        refusal: Option<String>,
    },
    /// `message_end`: the native message, carried WHOLE so the lifecycle projects consumption
    /// (`UserContent` text) and provider/model/`Usage` from the real contract instead of an invented
    /// optional or non-finite usage case.
    MessageEnd { message: AgentMessage },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JudgeSettle {
    pub completed: bool,
    pub cancelled: bool,
    pub failure_message: Option<String>,
}

#[derive(Clone, Debug)]
pub struct KibitzerChildSpawnInput {
    pub session_id: String,
    pub generation: u64,
    pub tools: Vec<String>,
    pub max_items: usize,
}

pub trait KibitzerChild: Send + Sync {
    fn steer<'a>(&'a self, text: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
    fn follow_up<'a>(&'a self, text: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
    fn abort(&self);
    fn subscribe_nudges(&self, listener: Arc<dyn Fn(RecallNudge) + Send + Sync>) -> Box<dyn FnOnce() + Send>;
    /// The child's observation channel (tool starts/ends and message ends). REQUIRED: no default
    /// implementation, so no observation can be silently dropped.
    fn subscribe_observations(&self, listener: Arc<dyn Fn(KibitzerChildObservation) + Send + Sync>) -> Box<dyn FnOnce() + Send>;
    fn settle(&self) -> Pin<Box<dyn Future<Output = JudgeSettle> + Send>>;
    fn dispose<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

pub trait KibitzerChildSpawner: Send + Sync {
    fn spawn<'a>(&'a self, input: KibitzerChildSpawnInput) -> Pin<Box<dyn Future<Output = Result<Arc<dyn KibitzerChild>, crate::kibitzer_sidecar_model::KibitzerSidecarStartError>> + Send + 'a>>;
}
