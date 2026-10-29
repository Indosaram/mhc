use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::timing::Clock;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InternalPromptDispatchMode {
    #[default]
    Async,
    Sync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InternalPromptQueueBehavior {
    #[default]
    Enqueue,
    Defer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PromptSessionName {
    PromptAsync,
    Prompt,
}

impl PromptSessionName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PromptAsync => "promptAsync",
            Self::Prompt => "prompt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptAsyncReservation {
    pub source: String,
    pub dedupe_key: String,
    pub reserved_at: u64,
    pub token: u64,
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum InternalPromptDispatchResult {
    #[serde(rename = "dispatched")]
    Dispatched { response: Value },
    #[serde(rename = "queued")]
    Queued {
        #[serde(rename = "queuedBy")]
        queued_by: String,
        position: usize,
    },
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "reserved")]
    Reserved {
        #[serde(rename = "reservedBy")]
        reserved_by: String,
    },
    #[serde(rename = "unavailable")]
    Unavailable,
    #[serde(rename = "failed")]
    Failed {
        error: Value,
        #[serde(rename = "dispatchAttempted")]
        dispatch_attempted: bool,
    },
}

impl InternalPromptDispatchResult {
    pub fn status(&self) -> &'static str {
        match self {
            Self::Dispatched { .. } => "dispatched",
            Self::Queued { .. } => "queued",
            Self::Active => "active",
            Self::Reserved { .. } => "reserved",
            Self::Unavailable => "unavailable",
            Self::Failed { .. } => "failed",
        }
    }
}

pub type PromptAsyncGateResult = InternalPromptDispatchResult;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptAsyncReservationReleaseOptions {
    pub reserved_by: Option<Vec<String>>,
    pub reserved_by_prefix: Option<Vec<String>>,
    pub supersede_transient_retry_owners: bool,
}

impl PromptAsyncReservationReleaseOptions {
    pub fn with_reserved_by(mut self, source: impl Into<String>) -> Self {
        let mut list = self.reserved_by.unwrap_or_default();
        list.push(source.into());
        self.reserved_by = Some(list);
        self
    }

    pub fn with_reserved_by_prefix(mut self, prefix: impl Into<String>) -> Self {
        let mut list = self.reserved_by_prefix.unwrap_or_default();
        list.push(prefix.into());
        self.reserved_by_prefix = Some(list);
        self
    }

    pub fn with_supersede_transient_retry_owners(mut self, flag: bool) -> Self {
        self.supersede_transient_retry_owners = flag;
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptMessagesQuery {
    pub directory: String,
    pub limit: Option<usize>,
}

pub trait PromptGateClient: Send + Sync {
    fn session_status(&self) -> impl Future<Output = Result<Value, String>> + Send {
        async { Err("session.status unavailable".to_string()) }
    }

    fn session_messages(
        &self,
        _session_id: &str,
        _query: &Value,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        async { Err("session.messages unavailable".to_string()) }
    }

    fn prompt_async(&self, _input: &Value) -> impl Future<Output = Result<Value, String>> + Send {
        async { Err("session.promptAsync unavailable".to_string()) }
    }

    fn prompt(&self, _input: &Value) -> impl Future<Output = Result<Value, String>> + Send {
        async { Err("session.prompt unavailable".to_string()) }
    }

    fn has_status(&self) -> bool {
        false
    }

    fn has_messages(&self) -> bool {
        false
    }

    fn has_prompt_async(&self) -> bool {
        false
    }

    fn has_prompt(&self) -> bool {
        false
    }
}

pub type BoxDispatchFn =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;
pub type BoxStatusFn = Arc<dyn Fn() -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;
pub type BoxMessagesFn =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct QueuedClient {
    pub status_fn: Option<BoxStatusFn>,
    pub messages_fn: Option<BoxMessagesFn>,
}

impl PromptGateClient for QueuedClient {
    fn session_status(&self) -> impl Future<Output = Result<Value, String>> + Send {
        let sf = self.status_fn.clone();
        async move {
            match sf {
                Some(f) => f().await,
                None => Err("session.status unavailable".to_string()),
            }
        }
    }

    fn session_messages(
        &self,
        session_id: &str,
        query: &Value,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        let mf = self.messages_fn.clone();
        let sid = session_id.to_string();
        let q = query.clone();
        async move {
            match mf {
                Some(f) => f(sid, q).await,
                None => Err("session.messages unavailable".to_string()),
            }
        }
    }

    fn has_status(&self) -> bool {
        self.status_fn.is_some()
    }

    fn has_messages(&self) -> bool {
        self.messages_fn.is_some()
    }
}

impl<T: PromptGateClient + ?Sized> PromptGateClient for Arc<T> {
    fn session_status(&self) -> impl Future<Output = Result<Value, String>> + Send {
        (**self).session_status()
    }

    fn session_messages(
        &self,
        session_id: &str,
        query: &Value,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        (**self).session_messages(session_id, query)
    }

    fn prompt_async(&self, input: &Value) -> impl Future<Output = Result<Value, String>> + Send {
        (**self).prompt_async(input)
    }

    fn prompt(&self, input: &Value) -> impl Future<Output = Result<Value, String>> + Send {
        (**self).prompt(input)
    }

    fn has_status(&self) -> bool {
        (**self).has_status()
    }

    fn has_messages(&self) -> bool {
        (**self).has_messages()
    }

    fn has_prompt_async(&self) -> bool {
        (**self).has_prompt_async()
    }

    fn has_prompt(&self) -> bool {
        (**self).has_prompt()
    }
}

pub struct InternalPromptDispatchArgs<C> {
    pub mode: InternalPromptDispatchMode,
    pub client: C,
    pub session_id: String,
    pub input: Value,
    pub source: String,
    pub dedupe_key: Option<String>,
    pub queue_behavior: Option<InternalPromptQueueBehavior>,
    pub queue: Option<bool>,
    pub queue_retry_ms: Option<u64>,
    pub settle_ms: Option<u64>,
    pub post_dispatch_hold_ms: Option<u64>,
    pub semantic_dedupe_hold_ms: Option<u64>,
    pub dispatch_timeout_ms: Option<u64>,
    pub check_status: Option<bool>,
    pub check_tool_state: Option<bool>,
    pub clock: Option<Arc<dyn Clock>>,
}

#[derive(Clone)]
pub struct QueuedInternalPrompt {
    pub id: u64,
    pub session_id: String,
    pub session_name: PromptSessionName,
    pub client: QueuedClient,
    pub input: Value,
    pub source: String,
    pub dedupe_key: String,
    pub settle_ms: u64,
    pub post_dispatch_hold_ms: u64,
    pub semantic_dedupe_hold_ms: u64,
    pub dispatch_timeout_ms: u64,
    pub queue_retry_ms: u64,
    pub check_status: bool,
    pub check_tool_state: bool,
    pub dispatch: BoxDispatchFn,
    pub clock: Option<Arc<dyn Clock>>,
}
