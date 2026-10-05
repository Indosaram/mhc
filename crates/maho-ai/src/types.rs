//! Port of senpi packages/ai/src/types.ts.
//!
//! String unions whose TS type is open (`KnownApi | (string & {})`) are plain `String`s here;
//! the known members are exported as constants. Closed unions are enums serialized with the
//! exact TS string values.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub use crate::model::{CursorAgentCompat, CursorReasoning, DevinAgentCompat, Model, ModelCompat};
pub use crate::openai_responses_compat::{OpenAIResponsesCompat, SessionAffinityFormat};
pub use crate::utils::diagnostics::AssistantMessageDiagnostic;
pub use crate::utils::event_stream::AssistantMessageEventStream;

pub type Api = String;
pub type ImagesApi = String;
pub type ProviderId = String;
pub type ImagesProviderId = String;

pub const KNOWN_APIS: &[&str] = &[
    "openai-completions",
    "mistral-conversations",
    "openai-responses",
    "azure-openai-responses",
    "openai-codex-responses",
    "cursor-agent",
    "devin-agent",
    "anthropic-messages",
    "bedrock-converse-stream",
    "google-generative-ai",
    "google-vertex",
    "pi-messages",
];

pub const KNOWN_IMAGES_APIS: &[&str] = &["openrouter-images", "openai-images"];

pub const KNOWN_PROVIDERS: &[&str] = &[
    "alibaba-token-plan",
    "amazon-bedrock",
    "ant-ling",
    "anthropic",
    "google",
    "google-vertex",
    "openai",
    "azure-openai-responses",
    "bai",
    "openai-codex",
    "chatgpt-subscription",
    "ollama",
    "cursor",
    "radius",
    "nvidia",
    "deepseek",
    "github-copilot",
    "xai",
    "groq",
    "cerebras",
    "openrouter",
    "vercel-ai-gateway",
    "opengateway",
    "zai",
    "zai-coding-cn",
    "mistral",
    "minimax",
    "minimax-cn",
    "moonshotai",
    "moonshotai-cn",
    "huggingface",
    "fireworks",
    "together",
    "baseten",
    "opencode",
    "opencode-go",
    "kimi-coding",
    "cloudflare-workers-ai",
    "cloudflare-ai-gateway",
    "qwen-token-plan",
    "qwen-token-plan-cn",
    "qwen-token-plan-individual",
    "xiaomi",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-sgp",
    "venice",
];

pub const KNOWN_IMAGES_PROVIDERS: &[&str] = &["openai", "openrouter"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    Auto,
    None,
}

/// Reasoning effort requested from a provider (`ThinkingLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// `"off" | ThinkingLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ModelThinkingLevel {
    pub const ALL: [ModelThinkingLevel; 7] = [
        ModelThinkingLevel::Off,
        ModelThinkingLevel::Minimal,
        ModelThinkingLevel::Low,
        ModelThinkingLevel::Medium,
        ModelThinkingLevel::High,
        ModelThinkingLevel::Xhigh,
        ModelThinkingLevel::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ModelThinkingLevel::Off => "off",
            ModelThinkingLevel::Minimal => "minimal",
            ModelThinkingLevel::Low => "low",
            ModelThinkingLevel::Medium => "medium",
            ModelThinkingLevel::High => "high",
            ModelThinkingLevel::Xhigh => "xhigh",
            ModelThinkingLevel::Max => "max",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == value)
    }
}

impl ThinkingLevel {
    pub const ALL: [ThinkingLevel; 6] = [
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == value)
    }
}

impl From<ThinkingLevel> for ModelThinkingLevel {
    fn from(level: ThinkingLevel) -> Self {
        match level {
            ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
            ThinkingLevel::Low => ModelThinkingLevel::Low,
            ThinkingLevel::Medium => ModelThinkingLevel::Medium,
            ThinkingLevel::High => ModelThinkingLevel::High,
            ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
            ThinkingLevel::Max => ModelThinkingLevel::Max,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingSelectionSource {
    Explicit,
    LegacyVariant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingSelection {
    pub level: ModelThinkingLevel,
    pub source: ThinkingSelectionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_variant_id: Option<String>,
}

/// `Partial<Record<ModelThinkingLevel, string | null>>`: a missing key is absent, `None` is null.
pub type ThinkingLevelMap = BTreeMap<ModelThinkingLevel, Option<String>>;

/// `ChatTemplateKwargValue`: a literal or a `{ $var, omitWhenOff? }` reference.
pub type ChatTemplateKwargValue = Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingTokenBudgetField {
    ThinkingTokenBudget,
    ThinkingBudget,
    ThinkingBudgetTokens,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingBudgets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimal: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    None,
    Short,
    Long,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Sse,
    Websocket,
    WebsocketCached,
    Auto,
}

pub type ProviderEnv = BTreeMap<String, String>;
/// Header overrides; `None` deletes a header (TS `null`).
pub type ProviderHeaders = BTreeMap<String, Option<String>>;

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRequestMetadata {
    pub model: Model,
    pub headers: ProviderHeaders,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Main,
    Auxiliary,
}

/// Payload hook: may return a replacement payload.
pub type OnPayload =
    std::sync::Arc<dyn Fn(&Value, &Model, Option<&ProviderRequestMetadata>) -> Option<Value> + Send + Sync>;
pub type OnResponse = std::sync::Arc<dyn Fn(&ProviderResponse, &Model) + Send + Sync>;
pub type AsyncOnPayload = std::sync::Arc<dyn Fn(Value, Model, Option<ProviderRequestMetadata>)
    -> BoxFuture<'static, Result<Option<Value>, String>> + Send + Sync>;
pub type AsyncOnResponse = std::sync::Arc<dyn Fn(ProviderResponse, Model)
    -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// `ProviderRequestOptions`. `fetch` and `telemetryContext` are host concerns of the wire lanes
/// and are carried as the reqwest client / opaque JSON respectively.
#[derive(Clone, Default)]
pub struct ProviderRequestOptions {
    pub signal: Option<crate::utils::abort::AbortSignal>,
    pub abort_server_side_fallback: Option<bool>,
    pub telemetry_context: Option<Value>,
    pub api_key: Option<String>,
    pub affinity_session_id: Option<String>,
    pub stream_kind: Option<StreamKind>,
    pub fetch: Option<reqwest::Client>,
    pub env: Option<ProviderEnv>,
    pub on_payload: Option<OnPayload>,
    pub on_response: Option<OnResponse>,
    pub async_on_payload: Option<AsyncOnPayload>,
    pub async_on_response: Option<AsyncOnResponse>,
    pub headers: Option<ProviderHeaders>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
}

impl ProviderRequestOptions {
    pub async fn apply_payload_hook(&self, payload: &Value, model: &Model,
        metadata: Option<&ProviderRequestMetadata>) -> Result<Option<Value>, String> {
        if let Some(signal) = &self.signal { signal.throw_if_aborted().map_err(|error| error.to_string())?; }
        let mut replacement = self.on_payload.as_ref().and_then(|hook| hook(payload, model, metadata));
        if let Some(signal) = &self.signal { signal.throw_if_aborted().map_err(|error| error.to_string())?; }
        if let Some(hook) = &self.async_on_payload {
            let future = hook(replacement.as_ref().unwrap_or(payload).clone(), model.clone(), metadata.cloned());
            let next = match &self.signal {
                Some(signal) => tokio::select! {
                    biased;
                    () = signal.cancelled() => return Err(signal.reason().map_or_else(|| "Operation aborted".into(), |reason| reason.to_string())),
                    result = future => result?,
                },
                None => future.await?,
            };
            if next.is_some() { replacement = next; }
        }
        if let Some(signal) = &self.signal { signal.throw_if_aborted().map_err(|error| error.to_string())?; }
        Ok(replacement)
    }

    pub async fn apply_response_hook(&self, response: &ProviderResponse, model: &Model) -> Result<(), String> {
        if let Some(signal) = &self.signal { signal.throw_if_aborted().map_err(|error| error.to_string())?; }
        if let Some(hook) = &self.on_response { hook(response, model); }
        if let Some(signal) = &self.signal { signal.throw_if_aborted().map_err(|error| error.to_string())?; }
        if let Some(hook) = &self.async_on_response {
            let future = hook(response.clone(), model.clone());
            match &self.signal {
                Some(signal) => tokio::select! {
                    biased;
                    () = signal.cancelled() => return Err(signal.reason().map_or_else(|| "Operation aborted".into(), |reason| reason.to_string())),
                    result = future => result?,
                },
                None => future.await?,
            }
        }
        if let Some(signal) = &self.signal { signal.throw_if_aborted().map_err(|error| error.to_string())?; }
        Ok(())
    }
}

impl std::fmt::Debug for ProviderRequestOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRequestOptions")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("stream_kind", &self.stream_kind)
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
    pub request: ProviderRequestOptions,
    pub temperature: Option<f64>,
    pub sampling_params: Option<Map<String, Value>>,
    pub max_tokens: Option<u64>,
    pub transport: Option<Transport>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub extra_body: Option<Map<String, Value>>,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub metadata: Option<Map<String, Value>>,
    /// API-specific options (`StreamOptions & Record<string, unknown>`).
    pub extra: Map<String, Value>,
}

pub type ProviderStreamOptions = StreamOptions;

#[derive(Debug, Clone, Default)]
pub struct DeferredFetchOptions {
    pub request: ProviderRequestOptions,
    pub wait: Option<u64>,
}

pub type DeferredCancelOptions = ProviderRequestOptions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTierPreference {
    Auto,
    Flex,
    Priority,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusalFallbackModel {
    pub model: String,
}

/// `"default" | readonly { model: string }[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicRefusalFallback {
    Default(DefaultTag),
    Models(Vec<RefusalFallbackModel>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultTag {
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferredWindow {
    #[serde(rename = "15m")]
    FifteenMinutes,
    #[serde(rename = "1h")]
    OneHour,
    #[serde(rename = "24h")]
    TwentyFourHours,
}

/// `boolean | { window?: "15m" | "1h" | "24h" }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeferredOption {
    Enabled(bool),
    Window {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<DeferredWindow>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct SimpleStreamOptions {
    pub stream: StreamOptions,
    pub tool_choice: Option<ToolChoice>,
    pub reasoning: Option<ThinkingLevel>,
    pub thinking_selection: Option<ThinkingSelection>,
    pub refusal_fallbacks: Option<AnthropicRefusalFallback>,
    pub deferred: Option<DeferredOption>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub service_tier: Option<ServiceTierPreference>,
}

#[derive(Debug, Clone, Default)]
pub struct ImagesOptions {
    pub request: ProviderRequestOptions,
    pub metadata: Option<Map<String, Value>>,
    pub extra: Map<String, Value>,
}

pub type ProviderImagesOptions = ImagesOptions;

/// The streaming surface a provider exposes (`ProviderStreams`).
pub trait ProviderStreams: Send + Sync {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream;
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream;
    fn fetch_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        None
    }
    fn supports_deferred(&self) -> bool {
        false
    }
    /// `cancelDeferred(model, handle, options)`: the operation future, or the pinned unsupported
    /// error. The default is genuinely unsupported (`ProviderStreams.cancelDeferred` undefined),
    /// never a no-op success; `supports_cancel_deferred()` is the model-free presence probe.
    fn cancel_deferred<'a>(
        &'a self,
        _model: &'a Model,
        _handle: &'a DeferredHandle,
        _options: Option<DeferredCancelOptions>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("API cannot cancel deferred responses".to_owned()) })
    }
    /// `ProviderStreams.cancelDeferred !== undefined` (model-free capability probe).
    fn supports_cancel_deferred(&self) -> bool {
        false
    }
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// The image surface a provider exposes (`ProviderImages`).
pub trait ProviderImages: Send + Sync {
    fn generate_images<'a>(
        &'a self,
        model: &'a ImagesModel,
        context: &'a ImagesContext,
        options: Option<ImagesOptions>,
    ) -> BoxFuture<'a, AssistantImages>;
}

pub type StreamFunction =
    std::sync::Arc<dyn Fn(&Model, &Context, Option<StreamOptions>) -> AssistantMessageEventStream + Send + Sync>;
pub type SimpleStreamFunction = std::sync::Arc<
    dyn Fn(&Model, &Context, Option<SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync,
>;
pub type ImagesFunction = std::sync::Arc<
    dyn for<'a> Fn(&'a ImagesModel, &'a ImagesContext, Option<ImagesOptions>) -> BoxFuture<'a, AssistantImages>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextPhase {
    Commentary,
    FinalAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSignatureV1 {
    pub v: u8,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<TextPhase>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextAudience {
    Model,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<TextAudience>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    pub thinking: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    /// base64 encoded image data
    pub data: String,
    pub mime_type: String,
}

pub fn is_video_mime_type(mime_type: &str) -> bool {
    mime_type.to_lowercase().starts_with("video/")
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderNativeContent {
    pub subtype: String,
    pub raw: Value,
}

/// Assistant content blocks (`TextContent | ThinkingContent | ToolCall | ProviderNativeContent`)
/// plus `ImageContent`, which user/tool-result content shares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "thinking")]
    Thinking(ThinkingContent),
    #[serde(rename = "image")]
    Image(ImageContent),
    #[serde(rename = "toolCall")]
    ToolCall(ToolCall),
    #[serde(rename = "providerNative")]
    ProviderNative(ProviderNativeContent),
}

impl ContentBlock {
    pub fn type_name(&self) -> &'static str {
        match self {
            ContentBlock::Text(_) => "text",
            ContentBlock::Thinking(_) => "thinking",
            ContentBlock::Image(_) => "image",
            ContentBlock::ToolCall(_) => "toolCall",
            ContentBlock::ProviderNative(_) => "providerNative",
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text(TextContent { text: text.into(), ..TextContent::default() })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(default, rename = "cacheWrite1h", skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub total_tokens: u64,
    pub cost: UsageCost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    Pending,
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
    Deferred,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Pending => "pending",
            StopReason::Stop => "stop",
            StopReason::Length => "length",
            StopReason::ToolUse => "toolUse",
            StopReason::Error => "error",
            StopReason::Aborted => "aborted",
            StopReason::Deferred => "deferred",
        }
    }
}

pub type JsonValue = Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredHandle {
    pub provider: String,
    pub model_id: String,
    pub api: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AssistantStopDetails {
    Refusal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explanation: Option<String>,
    },
    Sensitive,
}

/// `string | (TextContent | ImageContent)[]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    pub content: UserContent,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigurationUpdateMessage {
    pub content: Vec<ContentBlock>,
    pub effort: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AbortSource {
    Provider,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
    pub api: Api,
    pub provider: ProviderId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thinking_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_details: Option<AssistantStopDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_source: Option<AbortSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
    pub timestamp: i64,
}

/// Writes a field only when the TS optional is present (senpi omits an absent key rather than
/// serializing `null`).
fn serialize_optional<S: serde::ser::SerializeStruct, T: Serialize + ?Sized>(
    state: &mut S,
    key: &'static str,
    value: Option<&T>,
) -> Result<(), S::Error> {
    match value {
        Some(value) => state.serialize_field(key, value),
        None => Ok(()),
    }
}

/// senpi's `AssistantMessage` carries the literal `role: "assistant"` first, and the providers
/// attach the optional fields as they learn them; the recorded order (role, content, api, provider,
/// model, usage, stopReason, timestamp, then the optionals) is reproduced here so a serialized
/// message matches the TS object key for key.
impl Serialize for AssistantMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("AssistantMessage", 18)?;
        state.serialize_field("role", "assistant")?;
        state.serialize_field("content", &self.content)?;
        state.serialize_field("api", &self.api)?;
        state.serialize_field("provider", &self.provider)?;
        state.serialize_field("model", &self.model)?;
        state.serialize_field("usage", &self.usage)?;
        state.serialize_field("stopReason", &self.stop_reason)?;
        state.serialize_field("timestamp", &self.timestamp)?;
        serialize_optional(&mut state, "responseId", self.response_id.as_ref())?;
        serialize_optional(&mut state, "rawStopReason", self.raw_stop_reason.as_ref())?;
        serialize_optional(&mut state, "providerThinkingLevel", self.provider_thinking_level.as_ref())?;
        serialize_optional(&mut state, "errorMessage", self.error_message.as_ref())?;
        serialize_optional(&mut state, "stopDetails", self.stop_details.as_ref())?;
        serialize_optional(&mut state, "abortSource", self.abort_source.as_ref())?;
        serialize_optional(&mut state, "endTurn", self.end_turn.as_ref())?;
        serialize_optional(&mut state, "responseModel", self.response_model.as_ref())?;
        serialize_optional(&mut state, "diagnostics", self.diagnostics.as_ref())?;
        serialize_optional(&mut state, "deferred", self.deferred.as_ref())?;
        state.end()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    pub is_error: bool,
    pub timestamp: i64,
}

/// Message role union (`Message = UserMessage | AssistantMessage | ToolResultMessage |
/// ConfigurationUpdateMessage`). `ToolResult`'s ~296 bytes vs. the smallest variant's ~56 is
/// clippy::large_enum_variant; boxing it would touch `Message::ToolResult` call sites across
/// utils/ owned by other lanes, out of this module's scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role")]
#[allow(clippy::large_enum_variant)]
pub enum Message {
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "assistant")]
    Assistant(Box<AssistantMessage>),
    #[serde(rename = "toolResult")]
    ToolResult(ToolResultMessage),
    #[serde(rename = "configurationUpdate")]
    ConfigurationUpdate(ConfigurationUpdateMessage),
}

impl Message {
    pub fn role(&self) -> &'static str {
        match self {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
            Message::ConfigurationUpdate(_) => "configurationUpdate",
        }
    }
}

pub type ImagesInputContent = ContentBlock;
pub type ImagesOutputContent = ContentBlock;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImagesContext {
    pub input: Vec<ImagesInputContent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesStopReason {
    Stop,
    Error,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesBackground {
    Transparent,
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantImages {
    pub api: ImagesApi,
    pub provider: ImagesProviderId,
    pub model: String,
    pub output: Vec<ImagesOutputContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<ImagesBackground>,
    pub stop_reason: ImagesStopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeformToolFormat {
    /// always `"grammar"`
    #[serde(rename = "type")]
    pub kind: String,
    /// always `"lark"`
    pub syntax: String,
    pub definition: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrammarFormat {
    OpenaiLark,
    OpenaiRegex,
}

pub type GrammarVariants = BTreeMap<GrammarFormat, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JsonSchemaStrictness {
    Prefer,
    Require,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConstrainedSamplingConfig {
    JsonSchema { strict: JsonSchemaStrictness },
    Grammar { variants: GrammarVariants },
}

/// `false | ConstrainedSamplingConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConstrainedSampling {
    Disabled(bool),
    Config(ConstrainedSamplingConfig),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema (`TSchema`).
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeform: Option<FreeformToolFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<ConstrainedSampling>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DoneReason {
    Stop,
    Length,
    ToolUse,
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorReason {
    Aborted,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum AssistantMessageEvent {
    Start { partial: AssistantMessage },
    TextStart { content_index: usize, partial: AssistantMessage },
    TextDelta { content_index: usize, delta: String, partial: AssistantMessage },
    TextEnd { content_index: usize, content: String, partial: AssistantMessage },
    ThinkingStart { content_index: usize, partial: AssistantMessage },
    ThinkingDelta { content_index: usize, delta: String, partial: AssistantMessage },
    ThinkingEnd { content_index: usize, content: String, partial: AssistantMessage },
    #[serde(rename = "toolcall_start")]
    ToolcallStart { content_index: usize, partial: AssistantMessage },
    #[serde(rename = "toolcall_delta")]
    ToolcallDelta { content_index: usize, delta: String, partial: AssistantMessage },
    #[serde(rename = "toolcall_end")]
    ToolcallEnd { content_index: usize, tool_call: ToolCall, partial: AssistantMessage },
    Done { reason: DoneReason, message: AssistantMessage },
    Error { reason: ErrorReason, error: AssistantMessage },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    MaxCompletionTokens,
    MaxTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingFormat {
    Openai,
    Openrouter,
    Deepseek,
    Together,
    Baseten,
    Zai,
    Qwen,
    ChatTemplate,
    QwenChatTemplate,
    StringThinking,
    AntLing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolSchemaFlavor {
    #[serde(rename = "moonshot-mfjs")]
    MoonshotMfjs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheControlFormat {
    Anthropic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeferredToolsMode {
    Kimi,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VeniceParameters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_venice_system_prompt: Option<bool>,
}

/// `OpenAICompletionsCompat`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAICompletionsCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_store: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_usage_in_streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_finish_reason: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_tool_result_name: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_assistant_after_tool_result: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_thinking_as_text: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<ThinkingFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_disabled_thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<Map<String, ChatTemplateKwargValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_args: Option<Map<String, ChatTemplateKwargValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_router_routing: Option<OpenRouterRouting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venice_parameters: Option<VeniceParameters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vercel_gateway_routing: Option<VercelGatewayRouting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zai_tool_stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_thinking_token_budget: Option<bool>,
    #[serde(default, rename = "supportsOpenAIGrammarTools", skip_serializing_if = "Option::is_none")]
    pub supports_openai_grammar_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_schema_flavor: Option<ToolSchemaFlavor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control_format: Option<CacheControlFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_tools_mode: Option<DeferredToolsMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_prompt_cache_key: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_max_output_tokens: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vllm_priority: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnthropicAllowedFallbackModel {
    pub provider: ProviderId,
    pub model: String,
    pub cost: ModelCost,
}

/// `AnthropicAllowedFallbackModel | string`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AllowedFallbackModel {
    Model(AnthropicAllowedFallbackModel),
    Id(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnsignedThinkingReplay {
    Text,
    EmptySignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicSessionAffinityFormat {
    Openrouter,
}

/// `AnthropicMessagesCompat`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnthropicMessagesCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_eager_tool_input_streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<AnthropicSessionAffinityFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_cache_control_on_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_disabled_thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_temperature: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_choice: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_forced_tool_choice: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_adaptive_thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_empty_signature: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsigned_thinking_replay: Option<UnsignedThinkingReplay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_fallback_models: Option<Vec<AllowedFallbackModel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_references: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_web_search: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BedrockCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
}

/// `OpenRouterRouting`: provider routing preferences forwarded verbatim as the `provider` field.
pub type OpenRouterRouting = Map<String, Value>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VercelGatewayRouting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

/// $/million tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub input_tokens_above: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

impl ModelCost {
    pub fn rates(&self) -> ModelCostRates {
        ModelCostRates {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagesModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_input: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputModality {
    Text,
    Image,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesOutputModality {
    Text,
    Image,
}

/// `ImagesModel<TApi>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagesModel {
    pub id: String,
    pub name: String,
    pub api: ImagesApi,
    pub provider: ImagesProviderId,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Vec<InputModality>,
    pub output: Vec<ImagesOutputModality>,
    pub cost: ImagesModelCost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTierPreference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recover_text_tool_calls: Option<bool>,
}
