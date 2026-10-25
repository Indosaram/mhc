//! Port of senpi packages/ai/src/types.ts.
//!
//! String unions whose TS type is open (`KnownApi | (string & {})`) are plain `String`s here;
//! the known members are exported as constants. Closed unions are enums serialized with the
//! exact TS string values.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[path = "utils/transcript.rs"]
pub mod transcript;

pub use crate::model::{
    CursorAgentCompat, CursorReasoning, DevinAgentCompat, Model, ModelCompat,
    ModelImageInputLimits, ModelImageResizeOptions, ModelInputLimits, ModelPromptCache,
};
pub use crate::openai_responses_compat::{OpenAIResponsesCompat, SessionAffinityFormat};
pub use crate::utils::diagnostics::AssistantMessageDiagnostic;
pub use crate::utils::event_stream::AssistantMessageEventStream;

pub type Api = String;
pub type ImagesApi = String;
pub type ClassifierApi = String;
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

pub const KNOWN_CLASSIFIER_APIS: &[&str] =
    &["typesafe-system-one", "cloudflare-workers-ai-system-one"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClassifierQuestion {
    Choice {
        instructions: String,
        criteria: indexmap::IndexMap<String, String>,
    },
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    Bool {
        instructions: String,
        criteria: ClassifierBoolCriteria,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierBoolCriteria {
    pub r#true: String,
    pub r#false: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClassifierContext {
    pub state: Map<String, Value>,
    pub questions: indexmap::IndexMap<String, ClassifierQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClassifierAnswer {
    Choice {
        choice: String,
        probabilities: indexmap::IndexMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        confidence: f64,
    },
    Bool {
        probability: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierStopReason {
    Stop,
    Error,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierResult {
    pub api: ClassifierApi,
    pub provider: ProviderId,
    pub model: String,
    pub answers: indexmap::IndexMap<String, ClassifierAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub stop_reason: ClassifierStopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: i64,
}

pub type ClassifierOnPayload = std::sync::Arc<
    dyn Fn(Value, ClassifierModel) -> BoxFuture<'static, Result<Option<Value>, String>>
        + Send
        + Sync,
>;
pub type ClassifierOnResponse = std::sync::Arc<
    dyn Fn(ProviderResponse, ClassifierModel) -> BoxFuture<'static, Result<(), String>>
        + Send
        + Sync,
>;

#[derive(Clone, Default)]
pub struct ClassifierOptions {
    pub request: ProviderRequestOptions,
    pub temperature: Option<f64>,
    pub on_payload: Option<ClassifierOnPayload>,
    pub on_response: Option<ClassifierOnResponse>,
}

pub trait ProviderClassifier: Send + Sync {
    fn classify<'a>(
        &'a self,
        model: &'a ClassifierModel,
        context: &'a ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> BoxFuture<'a, ClassifierResult>;
}

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
    "typesafe",
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
    "meta",
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
pub type OnPayload = std::sync::Arc<
    dyn Fn(&Value, &Model, Option<&ProviderRequestMetadata>) -> Option<Value> + Send + Sync,
>;
pub type OnResponse = std::sync::Arc<dyn Fn(&ProviderResponse, &Model) + Send + Sync>;
pub type AsyncOnPayload = std::sync::Arc<
    dyn Fn(
            Value,
            Model,
            Option<ProviderRequestMetadata>,
        ) -> BoxFuture<'static, Result<Option<Value>, String>>
        + Send
        + Sync,
>;
pub type AsyncOnResponse = std::sync::Arc<
    dyn Fn(ProviderResponse, Model) -> BoxFuture<'static, Result<(), String>> + Send + Sync,
>;
pub type OnProviderStreamEvent = std::sync::Arc<dyn Fn(&Value, &Model) + Send + Sync>;
pub type AsyncOnProviderStreamEvent =
    std::sync::Arc<dyn Fn(Value, Model) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// `telemetryContext`: the explicit telemetry parent for this logical request. The harness
/// `TelemetryContext` lives above this crate, so it is carried as an opaque shared handle that
/// telemetry-aware providers downcast.
pub type ProviderTelemetryContext = std::sync::Arc<dyn std::any::Any + Send + Sync>;

/// `ProviderRequestOptions`. `fetch` is a host concern of the wire lanes and is carried as the
/// reqwest client.
#[derive(Clone, Default)]
pub struct ProviderRequestOptions {
    pub signal: Option<crate::utils::abort::AbortSignal>,
    pub abort_server_side_fallback: Option<bool>,
    pub telemetry_context: Option<ProviderTelemetryContext>,
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
    pub async fn apply_payload_hook(
        &self,
        payload: &Value,
        model: &Model,
        metadata: Option<&ProviderRequestMetadata>,
    ) -> Result<Option<Value>, String> {
        if let Some(signal) = &self.signal {
            signal
                .throw_if_aborted()
                .map_err(|error| error.to_string())?;
        }
        let mut replacement = self
            .on_payload
            .as_ref()
            .and_then(|hook| hook(payload, model, metadata));
        if let Some(signal) = &self.signal {
            signal
                .throw_if_aborted()
                .map_err(|error| error.to_string())?;
        }
        if let Some(hook) = &self.async_on_payload {
            let future = hook(
                replacement.as_ref().unwrap_or(payload).clone(),
                model.clone(),
                metadata.cloned(),
            );
            let next = match &self.signal {
                Some(signal) => tokio::select! {
                    biased;
                    () = signal.cancelled() => return Err(signal.reason().map_or_else(|| "Operation aborted".into(), |reason| reason.to_string())),
                    result = future => result?,
                },
                None => future.await?,
            };
            if next.is_some() {
                replacement = next;
            }
        }
        if let Some(signal) = &self.signal {
            signal
                .throw_if_aborted()
                .map_err(|error| error.to_string())?;
        }
        Ok(replacement)
    }

    pub async fn apply_response_hook(
        &self,
        response: &ProviderResponse,
        model: &Model,
    ) -> Result<(), String> {
        if let Some(signal) = &self.signal {
            signal
                .throw_if_aborted()
                .map_err(|error| error.to_string())?;
        }
        if let Some(hook) = &self.on_response {
            hook(response, model);
        }
        if let Some(signal) = &self.signal {
            signal
                .throw_if_aborted()
                .map_err(|error| error.to_string())?;
        }
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
        if let Some(signal) = &self.signal {
            signal
                .throw_if_aborted()
                .map_err(|error| error.to_string())?;
        }
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

#[derive(Clone, Default)]
pub struct StreamOptions {
    pub request: ProviderRequestOptions,
    /// Observes parsed, adapter-owned events before normalization. Adapter support is explicit.
    pub on_provider_stream_event: Option<OnProviderStreamEvent>,
    pub async_on_provider_stream_event: Option<AsyncOnProviderStreamEvent>,
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

impl std::fmt::Debug for StreamOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamOptions")
            .field("request", &self.request)
            .field("temperature", &self.temperature)
            .field("sampling_params", &self.sampling_params)
            .field("max_tokens", &self.max_tokens)
            .field("transport", &self.transport)
            .field("cache_retention", &self.cache_retention)
            .field("session_id", &self.session_id)
            .field("extra_body", &self.extra_body)
            .field(
                "websocket_connect_timeout_ms",
                &self.websocket_connect_timeout_ms,
            )
            .field("metadata", &self.metadata)
            .field("extra", &self.extra)
            .finish_non_exhaustive()
    }
}

pub type ProviderStreamOptions = StreamOptions;

impl StreamOptions {
    /// Observe an adapter-owned event before normalizing it into assistant content.
    pub async fn observe_provider_stream_event(
        &self,
        event: &Value,
        model: &Model,
    ) -> Result<(), String> {
        if let Some(hook) = &self.on_provider_stream_event {
            hook(event, model);
        }
        if let Some(hook) = &self.async_on_provider_stream_event {
            hook(event.clone(), model.clone()).await?;
        }
        Ok(())
    }
}

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
    Ultrafast,
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
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream;
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream;
    fn generate_images<'a>(
        &'a self,
        _model: &'a ImagesModel,
        _context: &'a ImagesContext,
        _options: Option<ImagesOptions>,
    ) -> BoxFuture<'a, Result<AssistantImages, String>> {
        Box::pin(async { Err("API does not support image generation".to_owned()) })
    }
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
    ) -> BoxFuture<'a, Result<AssistantImages, String>>;
}

pub type StreamFunction = std::sync::Arc<
    dyn Fn(&Model, &Context, Option<StreamOptions>) -> AssistantMessageEventStream + Send + Sync,
>;
pub type SimpleStreamFunction = std::sync::Arc<
    dyn Fn(&Model, &Context, Option<SimpleStreamOptions>) -> AssistantMessageEventStream
        + Send
        + Sync,
>;
pub type ImagesFunction = std::sync::Arc<
    dyn for<'a> Fn(
            &'a ImagesModel,
            &'a ImagesContext,
            Option<ImagesOptions>,
        ) -> BoxFuture<'a, Result<AssistantImages, String>>
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
        ContentBlock::Text(TextContent {
            text: text.into(),
            ..TextContent::default()
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub input: u64,
    pub output: u64,
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

pub type RequestContextUsage = ContextUsage;

fn deserialize_context_usage<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Option<RequestContextUsage>>, D::Error> {
    Ok(Some(Option::<RequestContextUsage>::deserialize(deserializer)?))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Latest request context when billing spans multiple internal requests.
    /// None (outer Option) preserves the legacy single-request contract; Some(None)
    /// means no request measurement is available.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_context_usage")]
    pub context_usage: Option<Option<ContextUsage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_unknown: Option<bool>,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(
        default,
        rename = "cacheWrite1h",
        skip_serializing_if = "Option::is_none"
    )]
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

/// Instructions and ordered declaration deltas at one point in the transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    pub content: UserContent,
    #[serde(default, skip_serializing_if = "indexmap::IndexMap::is_empty")]
    pub sections: indexmap::IndexMap<String, Option<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_added: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_removed: Vec<ToolReference>,
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

#[path = "utils/tool_name_match.rs"]
pub mod tool_name_match;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDiagnosticCategory {
    Auth,
    RateLimit,
    Quota,
    ContextLimit,
    InvalidRequest,
    ProviderUnavailable,
    Network,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDiagnosticEvidence {
    StructuredStatus,
    StructuredCode,
    StructuredTransport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDiagnostic {
    pub category: ProviderDiagnosticCategory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub evidence: ProviderDiagnosticEvidence,
    /// When a `quota` or `rate_limit` failure resets (epoch ms), when the provider named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
}

pub const PROVIDER_DIAGNOSTIC_MAX_BYTES: usize = 512;

pub fn sanitize_provider_diagnostic(value: &ProviderDiagnostic) -> Option<ProviderDiagnostic> {
    use ProviderDiagnosticCategory as Category;
    use ProviderDiagnosticEvidence as Evidence;
    let status = value.http_status;
    if status.is_some_and(|status| !(400..=599).contains(&status)) {
        return None;
    }
    if value
        .resets_at
        .is_some_and(|at| at <= 946_684_800_000 || at >= 4_102_444_800_000)
    {
        return None;
    }
    let valid = match value.evidence {
        Evidence::StructuredStatus => {
            let category = match status? {
                401 => Category::Auth,
                400 => Category::InvalidRequest,
                500..=599 => Category::ProviderUnavailable,
                _ => Category::Unknown,
            };
            value.code.is_none() && value.category == category
        }
        Evidence::StructuredTransport => {
            status.is_none()
                && value.category == Category::Network
                && matches!(
                    value.code.as_deref(),
                    Some(
                        "ECONNRESET"
                            | "ECONNREFUSED"
                            | "ETIMEDOUT"
                            | "ENOTFOUND"
                            | "EAI_AGAIN"
                            | "EPIPE"
                            | "UND_ERR_SOCKET"
                            | "UND_ERR_CONNECT_TIMEOUT"
                            | "UND_ERR_HEADERS_TIMEOUT"
                            | "UND_ERR_BODY_TIMEOUT"
                            | "UND_ERR_CLOSED"
                            | "APIConnectionError"
                            | "APIConnectionTimeoutError"
                    )
                )
        }
        Evidence::StructuredCode => {
            let code = value.code.as_deref()?;
            if crate::utils::provider_diagnostic_vocabulary::is_opaque_provider_code(code) {
                return if value.category == Category::Unknown && status.is_none() {
                    finish_provider_diagnostic(value)
                } else {
                    None
                };
            }
            let (category, compatible) = match code {
                "authentication_error" | "invalid_api_key" => {
                    (Category::Auth, status.is_none_or(|s| s == 401))
                }
                "rate_limit_error" | "rate_limit_exceeded" => {
                    (Category::RateLimit, status.is_none_or(|s| s == 429))
                }
                "insufficient_quota" | "usage_limit_reached" | "usage_not_included" => {
                    (Category::Quota, status.is_none_or(|s| s == 429))
                }
                "context_length_exceeded" => {
                    (Category::ContextLimit, status.is_none_or(|s| s == 400))
                }
                "invalid_request_error" => {
                    (Category::InvalidRequest, status.is_none_or(|s| s == 400))
                }
                "overloaded_error" | "api_error" | "timeout_error" => (
                    Category::ProviderUnavailable,
                    status.is_none_or(|s| (500..=599).contains(&s)),
                ),
                "billing_error" => (Category::Unknown, status.is_none_or(|s| s == 402)),
                "permission_error" => (Category::Unknown, status.is_none_or(|s| s == 403)),
                "not_found_error" => (Category::Unknown, status.is_none_or(|s| s == 404)),
                _ => return None,
            };
            compatible && value.category == category
        }
    };
    if !valid {
        return None;
    }
    finish_provider_diagnostic(value)
}

fn finish_provider_diagnostic(value: &ProviderDiagnostic) -> Option<ProviderDiagnostic> {
    let mut value = value.clone();
    // A reset instant is only meaningful on a limit, and never rides a transport diagnostic.
    let limit = matches!(
        value.category,
        ProviderDiagnosticCategory::Quota | ProviderDiagnosticCategory::RateLimit
    );
    if !limit || value.evidence == ProviderDiagnosticEvidence::StructuredTransport {
        value.resets_at = None;
    }
    let value = &value;
    match serde_json::to_vec(value) {
        Ok(wire) if wire.len() <= PROVIDER_DIAGNOSTIC_MAX_BYTES => {}
        Ok(_) | Err(_) => return None,
    }
    Some(value.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageLimitScope {
    Account,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantErrorCause {
    #[serde(rename = "usage_limit")]
    UsageLimit { scope: UsageLimitScope },
}

/// When the agent loop asked for a response and when it streamed (Unix ms), stamped on the final
/// message so it is persisted with the session (senpi#3879).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageTiming {
    /// Just before the stream function was called; an empty-response retry keeps the first value.
    pub requested_at: i64,
    /// First streamed text, thinking or tool-call delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_at: Option<i64>,
    /// Last streamed text, thinking or tool-call delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_token_at: Option<i64>,
    /// When the response settled: done, error or abort.
    pub ended_at: i64,
    /// Streamed generation time in ms by the TPS notice's rule; also stamped on aborted/failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
    /// Request-local stable-prefix cache checkpoint hint for provider adapters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_breakpoint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_diagnostics: Option<
        crate::api::openai_responses_shared::openai_responses_prompt_cache::PromptCacheDiagnostics,
    >,
    pub api: Api,
    pub provider: ProviderId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Requested agent effort, distinct from the provider-native effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
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
    pub error_cause: Option<AssistantErrorCause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_diagnostic: Option<ProviderDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_source: Option<AbortSource>,
    /// Set when the run failed in local code rather than a provider request; retry and model
    /// fallback must not act on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_source: Option<AssistantFailureSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
    /// Response timing stamped by the agent loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<AssistantMessageTiming>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssistantFailureSource {
    Local,
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
        let mut state = serializer.serialize_struct("AssistantMessage", 25)?;
        state.serialize_field("role", "assistant")?;
        state.serialize_field("content", &self.content)?;
        state.serialize_field("api", &self.api)?;
        state.serialize_field("provider", &self.provider)?;
        state.serialize_field("model", &self.model)?;
        state.serialize_field("usage", &self.usage)?;
        state.serialize_field("stopReason", &self.stop_reason)?;
        state.serialize_field("timestamp", &self.timestamp)?;
        serialize_optional(&mut state, "responseId", self.response_id.as_ref())?;
        serialize_optional(
            &mut state,
            "promptCacheDiagnostics",
            self.prompt_cache_diagnostics.as_ref(),
        )?;
        serialize_optional(
            &mut state,
            "promptCacheBreakpoint",
            self.prompt_cache_breakpoint.as_ref(),
        )?;
        serialize_optional(&mut state, "thinkingLevel", self.thinking_level.as_ref())?;
        serialize_optional(
            &mut state,
            "providerDiagnostic",
            self.provider_diagnostic.as_ref(),
        )?;
        serialize_optional(&mut state, "rawStopReason", self.raw_stop_reason.as_ref())?;
        serialize_optional(
            &mut state,
            "providerThinkingLevel",
            self.provider_thinking_level.as_ref(),
        )?;
        serialize_optional(&mut state, "errorMessage", self.error_message.as_ref())?;
        serialize_optional(&mut state, "errorCause", self.error_cause.as_ref())?;
        serialize_optional(&mut state, "stopDetails", self.stop_details.as_ref())?;
        serialize_optional(&mut state, "abortSource", self.abort_source.as_ref())?;
        serialize_optional(&mut state, "failureSource", self.failure_source.as_ref())?;
        serialize_optional(&mut state, "endTurn", self.end_turn.as_ref())?;
        serialize_optional(&mut state, "responseModel", self.response_model.as_ref())?;
        serialize_optional(&mut state, "diagnostics", self.diagnostics.as_ref())?;
        serialize_optional(&mut state, "deferred", self.deferred.as_ref())?;
        serialize_optional(&mut state, "timing", self.timing.as_ref())?;
        state.end()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NestedToolCallStatus {
    Ok,
    Error,
    Unfinished,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCallRecord {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments_bytes: Option<usize>,
    pub status: NestedToolCallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NestedToolCalls {
    pub calls: Vec<NestedToolCallRecord>,
    pub complete: bool,
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
    pub nested_calls: Option<NestedToolCalls>,
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
    #[serde(rename = "system")]
    System(SystemMessage),
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
            Message::System(_) => "system",
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
    /// Structured classification of a provider rejection (HTTP status, category); set only on `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_diagnostic: Option<ProviderDiagnostic>,
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
    pub output_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeform: Option<FreeformToolFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<ConstrainedSampling>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReference {
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tool_names: Option<Vec<String>>,
}

/// Provider-facing context, constructed by `transcript::normalize_context`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TranscriptContext {
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_tool_names: Option<Vec<String>>,
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
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AssistantMessageEvent {
    Start {
        partial: AssistantMessage,
    },
    TextStart {
        content_index: usize,
        partial: AssistantMessage,
    },
    TextDelta {
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    TextEnd {
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    ThinkingStart {
        content_index: usize,
        partial: AssistantMessage,
    },
    ThinkingDelta {
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    ThinkingEnd {
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_start")]
    ToolcallStart {
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_delta")]
    ToolcallDelta {
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_end")]
    ToolcallEnd {
        content_index: usize,
        tool_call: ToolCall,
        partial: AssistantMessage,
    },
    Done {
        reason: DoneReason,
        message: AssistantMessage,
    },
    Error {
        reason: ErrorReason,
        error: AssistantMessage,
    },
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
    pub supports_web_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_tool_additions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_forced_tool_choice: Option<bool>,
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
    #[serde(
        default,
        rename = "supportsOpenAIGrammarTools",
        skip_serializing_if = "Option::is_none"
    )]
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
    pub supports_gzip_request_body: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_tool_changes: Option<bool>,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MistralConversationsCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
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
pub struct ModelCostCard {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostEffectiveRate {
    /// ISO 8601 UTC instant from which this card replaces all earlier cards.
    pub effective_from: String,
    #[serde(flatten)]
    pub card: ModelCostCard,
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
    /// Scheduled list-price changes, inclusive of their effective instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_rates: Option<Vec<ModelCostEffectiveRate>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<bool>,
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
    pub effective_rates: Option<Vec<ModelCostEffectiveRate>>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierModel {
    #[serde(rename = "type")]
    pub model_type: ClassifierModelType,
    pub id: String,
    pub name: String,
    pub api: ClassifierApi,
    pub provider: ProviderId,
    pub base_url: String,
    pub input: Vec<InputModality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    pub context_window: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierModelType {
    Classifier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    Chat,
    Image,
    Classifier,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnyModel {
    Chat(Model),
    Image(ImagesModel),
    Classifier(ClassifierModel),
}

impl Serialize for AnyModel {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = match self {
            Self::Chat(model) => serde_json::to_value(model),
            Self::Image(model) => serde_json::to_value(model).map(|mut value| {
                value["type"] = Value::String("image".into());
                value
            }),
            Self::Classifier(model) => serde_json::to_value(model),
        }
        .map_err(serde::ser::Error::custom)?;
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AnyModel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match value.get("type").and_then(Value::as_str) {
            None | Some("chat") => serde_json::from_value(value).map(Self::Chat),
            Some("image") => serde_json::from_value(value).map(Self::Image),
            Some("classifier") => serde_json::from_value(value).map(Self::Classifier),
            Some(kind) => {
                return Err(serde::de::Error::custom(format!(
                    "Unknown model type: {kind}"
                )));
            }
        }
        .map_err(serde::de::Error::custom)
    }
}

impl AnyModel {
    pub const fn model_type(&self) -> ModelType {
        match self {
            Self::Chat(_) => ModelType::Chat,
            Self::Image(_) => ModelType::Image,
            Self::Classifier(_) => ModelType::Classifier,
        }
    }
}
