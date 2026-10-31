//! Port of senpi `experimental/micro/api.ts` (pin 903b8336).
//!
//! Everything the micro TUI may render, plus its local control surface. No live harness, model,
//! storage or auth object crosses this boundary: the view is plain data over Pico3's
//! `ConversationView`, and the controller is a narrow command trait. Auth prompts and notices reuse
//! the mini protocol's plain-data shapes (`AuthPromptRequest` is senpi's `Omit<AuthPrompt, "signal">`).

use maho_agent::harness::pico3::types::{ConversationView, LocalBoxFuture, ModelRef};

pub use crate::experimental::mini::shared::protocol::{AuthNotice, AuthPromptRequest};

/// senpi `MicroModelSummary extends ModelRef`.
#[derive(Clone, Debug, PartialEq)]
pub struct MicroModelSummary {
    pub provider: String,
    pub model_id: String,
    pub name: String,
    pub context_window: u64,
    pub max_tokens: u64,
}

impl MicroModelSummary {
    pub fn model_ref(&self) -> ModelRef {
        ModelRef {
            provider: self.provider.clone(),
            model_id: self.model_id.clone(),
        }
    }
}

/// senpi `"oauth" | "api_key"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicroAuthType {
    Oauth,
    ApiKey,
}

/// senpi `MicroProviderAccount`.
#[derive(Clone, Debug, PartialEq)]
pub struct MicroProviderAccount {
    pub id: String,
    pub name: String,
    pub auth_type: MicroAuthType,
    pub configured: bool,
    pub source: Option<String>,
    pub interactive: bool,
    pub method_name: Option<String>,
}

/// senpi `MicroModelsView`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MicroModelsView {
    pub models: Vec<MicroModelSummary>,
    pub accounts: Vec<MicroProviderAccount>,
    pub refreshing: bool,
}

/// senpi `MicroAuthView.prompt`.
#[derive(Clone, Debug, PartialEq)]
pub struct MicroAuthPrompt {
    pub id: String,
    pub request: AuthPromptRequest,
}

/// senpi `MicroAuthView`.
#[derive(Clone, Debug, PartialEq)]
pub struct MicroAuthView {
    pub provider_id: String,
    pub provider_name: String,
    pub auth_type: MicroAuthType,
    pub notices: Vec<AuthNotice>,
    pub prompt: Option<MicroAuthPrompt>,
}

/// senpi `MicroNotice.level`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicroNoticeLevel {
    Info,
    Warning,
    Error,
}

/// senpi `MicroNotice`.
#[derive(Clone, Debug, PartialEq)]
pub struct MicroNotice {
    pub id: u64,
    pub level: MicroNoticeLevel,
    pub message: String,
}

/// senpi `MicroUsageView`. `None` context fields are senpi's `null` (unknown after compaction).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MicroUsageView {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total_cost: f64,
    /// Some turn had no known price, so `total_cost` is a lower bound (senpi#3844).
    pub total_cost_unknown: bool,
    pub last_cache_hit_rate: Option<f64>,
    pub context_tokens: Option<u64>,
    pub context_window: u64,
    pub context_percent: Option<f64>,
}

/// senpi `MicroView.session`.
#[derive(Clone, Debug, PartialEq)]
pub struct MicroSessionView {
    pub id: String,
    pub path: String,
    pub cwd: String,
}

/// senpi `MicroView`: everything the TUI may render.
#[derive(Clone, Debug)]
pub struct MicroView {
    pub session: MicroSessionView,
    pub conversation: ConversationView,
    pub models: MicroModelsView,
    pub usage: MicroUsageView,
    pub auth: Option<MicroAuthView>,
    pub notices: Vec<MicroNotice>,
    pub fatal: Option<String>,
}

/// senpi `MicroViewSource`. `subscribe` returns the unsubscribe callback.
pub trait MicroViewSource {
    fn current(&self) -> MicroView;
    fn subscribe(&self, listener: Box<dyn Fn()>) -> Box<dyn FnOnce()>;
}

/// senpi `MicroController`. It deliberately does not expose Pico's Harness or ConversationHandle.
pub trait MicroController {
    fn prompt(&self, text: String) -> LocalBoxFuture<'_, ()>;
    fn steer(&self, text: String) -> LocalBoxFuture<'_, ()>;
    fn follow_up(&self, text: String) -> LocalBoxFuture<'_, ()>;
    fn compact(&self, instructions: Option<String>) -> LocalBoxFuture<'_, ()>;
    fn abort(&self) -> LocalBoxFuture<'_, ()>;
    fn cycle_thinking(&self) -> LocalBoxFuture<'_, ()>;
    fn set_model(&self, model: ModelRef) -> LocalBoxFuture<'_, ()>;
    fn refresh_models(&self) -> LocalBoxFuture<'_, ()>;
    fn login(&self, provider_id: String, auth_type: MicroAuthType) -> LocalBoxFuture<'_, ()>;
    fn reply_auth(&self, request_id: String, answer: Option<String>) -> LocalBoxFuture<'_, ()>;
    fn cancel_login(&self) -> LocalBoxFuture<'_, ()>;
}
