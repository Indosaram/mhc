//! Port of senpi packages/ai/src/utils/prompt-cache-ttl.ts.

use crate::model::Model;
use crate::types::{
    AllowedFallbackModel, AnthropicSessionAffinityFormat, CacheControlFormat, CacheRetention, ChatTemplateKwargValue, DeferredToolsMode,
    MaxTokensField, OpenRouterRouting, ProviderEnv, SessionAffinityFormat, ThinkingFormat, ThinkingTokenBudgetField, ToolSchemaFlavor,
    UnsignedThinkingReplay, VeniceParameters, VercelGatewayRouting,
};
use crate::utils::provider_env::get_provider_env_value;
use regex::{Regex, RegexBuilder};
use serde_json::Map;
use std::sync::LazyLock;

pub const PROMPT_CACHE_TTL_SHORT_SECONDS: u64 = 300;
pub const PROMPT_CACHE_TTL_LONG_SECONDS: u64 = 3600;

pub fn is_anthropic_api_base_url(base_url: &str) -> bool {
    url::Url::parse(base_url).ok().and_then(|url| url.host_str().map(|h| h == "api.anthropic.com")).unwrap_or(false)
}

static FORCED_TOOL_CHOICE_REJECTING_MODEL_ID: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"^claude-(?:(?:fable|mythos)(?:-|$)|opus-5[.-]5(?:[.-]|$))").case_insensitive(true).build().unwrap_or_else(|e| panic!("{e}"))
});
static TOOL_REFERENCE_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^claude-(?:opus|sonnet|fable)-(\d+)(?:-(\d+))?(?:-|$)").unwrap_or_else(|e| panic!("{e}")));
static BEDROCK_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s_.:]+").unwrap_or_else(|e| panic!("{e}")));

fn default_supports_tool_references(model: &Model) -> bool {
    if model.provider != "anthropic" || model.id.contains("haiku") {
        return false;
    }
    let Some(version) = TOOL_REFERENCE_VERSION.captures(&model.id) else { return false };
    let major: f64 = version[1].parse().unwrap_or(0.0);
    let minor: f64 = version.get(2).filter(|m| m.as_str().len() < 8).map_or(0.0, |m| m.as_str().parse().unwrap_or(0.0));
    major > 4.0 || (major == 4.0 && minor >= 5.0)
}

/// `getAnthropicCompat` result: every field resolved except the optional affinity format.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAnthropicCompat {
    pub supports_eager_tool_input_streaming: bool,
    pub supports_long_cache_retention: bool,
    pub send_session_affinity_headers: bool,
    pub session_affinity_format: Option<AnthropicSessionAffinityFormat>,
    pub supports_cache_control_on_tools: bool,
    pub supports_disabled_thinking: bool,
    pub supports_temperature: bool,
    pub supports_tool_choice: bool,
    pub supports_forced_tool_choice: bool,
    pub allow_empty_signature: bool,
    pub unsigned_thinking_replay: UnsignedThinkingReplay,
    pub allowed_fallback_models: Vec<AllowedFallbackModel>,
    pub supports_strict_tools: bool,
    pub supports_tool_references: bool,
    pub supports_web_search: bool,
}

pub fn get_anthropic_compat(model: &Model) -> ResolvedAnthropicCompat {
    let compat = model.compat.as_ref().map(|c| c.anthropic_messages()).unwrap_or_default();
    let is_fireworks = model.provider == "fireworks";
    let is_cloudflare_anthropic = model.provider == "cloudflare-ai-gateway" && model.base_url.contains("anthropic");
    let is_xiaomi = model.provider == "xiaomi" || model.provider.starts_with("xiaomi-token-plan-");
    let is_open_router = model.provider == "openrouter" || model.base_url.contains("openrouter.ai");
    ResolvedAnthropicCompat {
        supports_eager_tool_input_streaming: compat.supports_eager_tool_input_streaming.unwrap_or(!is_fireworks),
        supports_long_cache_retention: compat.supports_long_cache_retention.unwrap_or(!is_fireworks),
        send_session_affinity_headers: compat.send_session_affinity_headers.unwrap_or(is_fireworks || is_cloudflare_anthropic || is_open_router),
        session_affinity_format: compat.session_affinity_format.or(is_open_router.then_some(AnthropicSessionAffinityFormat::Openrouter)),
        supports_cache_control_on_tools: compat.supports_cache_control_on_tools.unwrap_or(!is_fireworks),
        supports_disabled_thinking: compat.supports_disabled_thinking.unwrap_or(!is_xiaomi),
        supports_temperature: compat.supports_temperature.unwrap_or(true),
        supports_tool_choice: compat.supports_tool_choice.unwrap_or(true),
        supports_forced_tool_choice: compat.supports_forced_tool_choice.unwrap_or(!FORCED_TOOL_CHOICE_REJECTING_MODEL_ID.is_match(&model.id)),
        allow_empty_signature: compat.allow_empty_signature.unwrap_or(false),
        unsigned_thinking_replay: compat.unsigned_thinking_replay.unwrap_or(if compat.allow_empty_signature == Some(true) {
            UnsignedThinkingReplay::EmptySignature
        } else {
            UnsignedThinkingReplay::Text
        }),
        allowed_fallback_models: compat.allowed_fallback_models.unwrap_or_default(),
        supports_strict_tools: compat.supports_strict_tools.unwrap_or(false),
        supports_tool_references: compat.supports_tool_references.unwrap_or_else(|| default_supports_tool_references(model)),
        supports_web_search: compat.supports_web_search.unwrap_or_else(|| is_anthropic_api_base_url(&model.base_url)),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedOpenAICompletionsCompat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub supports_finish_reason: bool,
    pub max_tokens_field: MaxTokensField,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_thinking_as_text: bool,
    pub requires_reasoning_content_on_assistant_messages: bool,
    pub thinking_format: ThinkingFormat,
    pub supports_disabled_thinking: bool,
    pub open_router_routing: OpenRouterRouting,
    pub vercel_gateway_routing: VercelGatewayRouting,
    pub chat_template_kwargs: Map<String, ChatTemplateKwargValue>,
    pub chat_template_args: Option<Map<String, ChatTemplateKwargValue>>,
    pub zai_tool_stream: bool,
    pub supports_thinking_token_budget: Option<bool>,
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    pub supports_strict_mode: bool,
    pub tool_schema_flavor: Option<ToolSchemaFlavor>,
    pub tool_call_format: Option<String>,
    pub supports_openai_grammar_tools: bool,
    pub cache_control_format: Option<CacheControlFormat>,
    pub send_session_affinity_headers: bool,
    pub deferred_tools_mode: Option<DeferredToolsMode>,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_prompt_cache_key: Option<bool>,
    pub supports_max_output_tokens: bool,
    pub vllm_priority: Option<i64>,
    pub venice_parameters: Option<VeniceParameters>,
    pub supports_long_cache_retention: bool,
}

fn detect_openai_completions_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let provider = model.provider.as_str();
    let base_url = model.base_url.as_str();
    let is_zai = matches!(provider, "zai" | "zai-coding-cn") || base_url.contains("api.z.ai") || base_url.contains("open.bigmodel.cn");
    let is_together = provider == "together" || base_url.contains("api.together.ai") || base_url.contains("api.together.xyz");
    let is_moonshot = matches!(provider, "moonshotai" | "moonshotai-cn") || base_url.contains("api.moonshot.");
    let is_open_router = provider == "openrouter" || base_url.contains("openrouter.ai");
    let is_cf_workers = provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cf_gateway = provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
    let is_deepseek = provider == "deepseek" || base_url.to_lowercase().contains("deepseek.com");
    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_non_standard = is_nvidia
        || provider == "cerebras"
        || base_url.contains("cerebras.ai")
        || is_grok
        || is_together
        || base_url.contains("chutes.ai")
        || is_deepseek
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cf_workers
        || is_cf_gateway
        || is_ant_ling;
    let use_max_tokens =
        base_url.contains("chutes.ai") || is_deepseek || is_moonshot || is_cf_gateway || is_together || is_nvidia || is_ant_ling || is_zai;
    let developer_role_model = is_open_router && (model.id.starts_with("anthropic/") || model.id.starts_with("openai/"));
    let cache_model_id = model.id.strip_prefix('~').unwrap_or(&model.id);
    let open_router_cache_control = ["anthropic/", "qwen/", "google/"].iter().any(|p| cache_model_id.starts_with(p));
    let thinking_format = if is_deepseek {
        ThinkingFormat::Deepseek
    } else if is_zai {
        ThinkingFormat::Zai
    } else if is_together {
        ThinkingFormat::Together
    } else if is_ant_ling {
        ThinkingFormat::AntLing
    } else if is_open_router {
        ThinkingFormat::Openrouter
    } else {
        ThinkingFormat::Openai
    };
    ResolvedOpenAICompletionsCompat {
        supports_store: !is_non_standard,
        supports_developer_role: developer_role_model || (!is_non_standard && !is_open_router),
        supports_reasoning_effort: !is_grok && !is_zai && !is_moonshot && !is_together && !is_cf_gateway && !is_nvidia && !is_ant_ling,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens { MaxTokensField::MaxTokens } else { MaxTokensField::MaxCompletionTokens },
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deepseek,
        thinking_format,
        supports_disabled_thinking: true,
        open_router_routing: Map::new(),
        vercel_gateway_routing: VercelGatewayRouting::default(),
        chat_template_kwargs: Map::new(),
        chat_template_args: None,
        zai_tool_stream: false,
        supports_thinking_token_budget: None,
        thinking_token_budget_field: None,
        supports_strict_mode: !is_moonshot && !is_together && !is_cf_gateway && !is_nvidia,
        tool_schema_flavor: is_moonshot.then_some(ToolSchemaFlavor::MoonshotMfjs),
        tool_call_format: None,
        supports_openai_grammar_tools: false,
        cache_control_format: (provider == "openrouter" && open_router_cache_control).then_some(CacheControlFormat::Anthropic),
        send_session_affinity_headers: is_open_router,
        deferred_tools_mode: None,
        session_affinity_format: if is_open_router { SessionAffinityFormat::Openrouter } else { SessionAffinityFormat::Openai },
        supports_prompt_cache_key: Some(is_moonshot || base_url.contains("api.openai.com")),
        supports_max_output_tokens: true,
        vllm_priority: None,
        venice_parameters: None,
        supports_long_cache_retention: !(is_together || is_cf_workers || is_cf_gateway || is_nvidia || is_ant_ling),
    }
}

pub fn get_openai_completions_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let detected = detect_openai_completions_compat(model);
    let Some(compat) = model.compat.as_ref().map(|c| c.openai_completions()) else { return detected };
    ResolvedOpenAICompletionsCompat {
        supports_store: compat.supports_store.unwrap_or(detected.supports_store),
        supports_developer_role: compat.supports_developer_role.unwrap_or(detected.supports_developer_role),
        supports_reasoning_effort: compat.supports_reasoning_effort.unwrap_or(detected.supports_reasoning_effort),
        supports_usage_in_streaming: compat.supports_usage_in_streaming.unwrap_or(detected.supports_usage_in_streaming),
        supports_finish_reason: compat.supports_finish_reason.unwrap_or(detected.supports_finish_reason),
        max_tokens_field: compat.max_tokens_field.unwrap_or(detected.max_tokens_field),
        requires_tool_result_name: compat.requires_tool_result_name.unwrap_or(detected.requires_tool_result_name),
        requires_assistant_after_tool_result: compat.requires_assistant_after_tool_result.unwrap_or(detected.requires_assistant_after_tool_result),
        requires_thinking_as_text: compat.requires_thinking_as_text.unwrap_or(detected.requires_thinking_as_text),
        requires_reasoning_content_on_assistant_messages: compat
            .requires_reasoning_content_on_assistant_messages
            .unwrap_or(detected.requires_reasoning_content_on_assistant_messages),
        thinking_format: compat.thinking_format.unwrap_or(detected.thinking_format),
        supports_disabled_thinking: compat.supports_disabled_thinking.unwrap_or(detected.supports_disabled_thinking),
        open_router_routing: compat.open_router_routing.unwrap_or(detected.open_router_routing),
        vercel_gateway_routing: compat.vercel_gateway_routing.unwrap_or(detected.vercel_gateway_routing),
        chat_template_kwargs: compat.chat_template_kwargs.unwrap_or(detected.chat_template_kwargs),
        chat_template_args: compat.chat_template_args.or(detected.chat_template_args),
        zai_tool_stream: compat.zai_tool_stream.unwrap_or(detected.zai_tool_stream),
        supports_thinking_token_budget: compat.supports_thinking_token_budget.or(detected.supports_thinking_token_budget),
        thinking_token_budget_field: compat.thinking_token_budget_field.or(detected.thinking_token_budget_field),
        supports_strict_mode: compat.supports_strict_mode.unwrap_or(detected.supports_strict_mode),
        tool_schema_flavor: compat.tool_schema_flavor.or(detected.tool_schema_flavor),
        tool_call_format: compat.tool_call_format.or(detected.tool_call_format),
        supports_openai_grammar_tools: compat.supports_openai_grammar_tools.unwrap_or(detected.supports_openai_grammar_tools),
        cache_control_format: compat.cache_control_format.or(detected.cache_control_format),
        send_session_affinity_headers: compat.send_session_affinity_headers.unwrap_or(detected.send_session_affinity_headers),
        deferred_tools_mode: compat.deferred_tools_mode.or(detected.deferred_tools_mode),
        session_affinity_format: compat.session_affinity_format.unwrap_or(detected.session_affinity_format),
        supports_prompt_cache_key: compat.supports_prompt_cache_key.or(detected.supports_prompt_cache_key),
        supports_max_output_tokens: compat.supports_max_output_tokens.unwrap_or(detected.supports_max_output_tokens),
        vllm_priority: compat.vllm_priority.or(detected.vllm_priority),
        // TS's override object omits veniceParameters.
        venice_parameters: None,
        supports_long_cache_retention: compat.supports_long_cache_retention.unwrap_or(detected.supports_long_cache_retention),
    }
}

pub fn get_bedrock_model_match_candidates(model_id: &str, model_name: Option<&str>) -> Vec<String> {
    std::iter::once(model_id)
        .chain(model_name.filter(|n| !n.is_empty()))
        .flat_map(|value| {
            let lower = value.to_lowercase();
            let dashed = BEDROCK_SEPARATORS.replace_all(&lower, "-").into_owned();
            [lower, dashed]
        })
        .collect()
}

pub fn supports_one_hour_cache_ttl(model: &Model) -> bool {
    get_bedrock_model_match_candidates(&model.id, Some(&model.name))
        .iter()
        .any(|candidate| ["opus-4-5", "sonnet-4-5", "haiku-4-5"].iter().any(|v| candidate.contains(v)))
}

pub fn supports_prompt_caching(model: &Model, env: Option<&ProviderEnv>) -> bool {
    let candidates = get_bedrock_model_match_candidates(&model.id, Some(&model.name));
    let any = |needle: &str| candidates.iter().any(|c| c.contains(needle));
    if !any("claude") {
        return get_provider_env_value("AWS_BEDROCK_FORCE_CACHE", env).as_deref() == Some("1");
    }
    any("fable-5") || any("opus-5") || any("sonnet-5") || any("-4-") || any("claude-3-7-sonnet") || any("claude-3-5-haiku")
}

fn resolve_cache_retention(cache_retention: Option<CacheRetention>, env: Option<&ProviderEnv>) -> CacheRetention {
    match cache_retention {
        Some(retention) => retention,
        None if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") => CacheRetention::Long,
        // The anthropic branch's "env set but not long" case and its fallback are both "short".
        None => CacheRetention::Short,
    }
}

pub fn resolve_prompt_cache_ttl_seconds(model: &Model, env: Option<&ProviderEnv>) -> Option<u64> {
    let retention = || resolve_cache_retention(model.cache_retention, env);
    let ttl = |long: bool| if long { PROMPT_CACHE_TTL_LONG_SECONDS } else { PROMPT_CACHE_TTL_SHORT_SECONDS };
    match model.api.as_str() {
        "claude-sdk-oauth" => Some(PROMPT_CACHE_TTL_SHORT_SECONDS),
        "anthropic-messages" => match retention() {
            CacheRetention::None => None,
            retention => Some(ttl(
                retention == CacheRetention::Long
                    && is_anthropic_api_base_url(&model.base_url)
                    && get_anthropic_compat(model).supports_long_cache_retention,
            )),
        },
        "bedrock-converse-stream" => match retention() {
            CacheRetention::None => None,
            _ if !supports_prompt_caching(model, env) => None,
            retention => Some(ttl(retention == CacheRetention::Long && supports_one_hour_cache_ttl(model))),
        },
        "openai-completions" => match retention() {
            CacheRetention::None => None,
            retention => {
                let compat = get_openai_completions_compat(model);
                let long = compat.cache_control_format == Some(CacheControlFormat::Anthropic)
                    && retention == CacheRetention::Long
                    && compat.supports_long_cache_retention;
                Some(ttl(long))
            }
        },
        "openai-responses" | "openai-codex-responses" | "azure-openai-responses" => match retention() {
            CacheRetention::None => None,
            _ => Some(PROMPT_CACHE_TTL_SHORT_SECONDS),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models_generated::get_builtin_model;

    fn model(provider: &str, id: &str) -> Model {
        get_builtin_model(provider, id).expect("model").clone()
    }

    fn long_env() -> ProviderEnv {
        [("PI_CACHE_RETENTION".to_owned(), "long".to_owned())].into_iter().collect()
    }

    #[test]
    fn anthropic_compat_defaults() {
        let opus = model("anthropic", "claude-opus-4-8");
        let compat = get_anthropic_compat(&opus);
        assert!(compat.supports_tool_references && compat.supports_web_search && compat.supports_forced_tool_choice);
        assert_eq!(compat.unsigned_thinking_replay, UnsignedThinkingReplay::Text);
        let fable = Model { id: "claude-fable-5".into(), ..opus.clone() };
        assert!(!get_anthropic_compat(&fable).supports_forced_tool_choice);
        let old = Model { id: "claude-sonnet-4-20250514".into(), ..opus.clone() };
        assert!(!get_anthropic_compat(&old).supports_tool_references);
        let router = Model { provider: "openrouter".into(), base_url: "https://openrouter.ai/api".into(), ..opus };
        let compat = get_anthropic_compat(&router);
        assert!(compat.send_session_affinity_headers && !compat.supports_web_search);
        assert_eq!(compat.session_affinity_format, Some(AnthropicSessionAffinityFormat::Openrouter));
    }

    #[test]
    fn openai_completions_compat_detection() {
        let base = model("anthropic", "claude-opus-4-8");
        let deepseek = Model { api: "openai-completions".into(), provider: "deepseek".into(), base_url: "https://api.deepseek.com".into(), compat: None, ..base.clone() };
        let compat = get_openai_completions_compat(&deepseek);
        assert_eq!((compat.thinking_format, compat.max_tokens_field), (ThinkingFormat::Deepseek, MaxTokensField::MaxTokens));
        assert!(!compat.supports_store && compat.requires_reasoning_content_on_assistant_messages);
        let router = Model { api: "openai-completions".into(), provider: "openrouter".into(), id: "~anthropic/claude".into(), base_url: "https://openrouter.ai/api/v1".into(), compat: None, ..base };
        let compat = get_openai_completions_compat(&router);
        assert_eq!(compat.cache_control_format, Some(CacheControlFormat::Anthropic));
        assert!(!compat.supports_developer_role && compat.send_session_affinity_headers);
    }

    #[test]
    fn prompt_cache_ttl_resolution() {
        let opus = model("anthropic", "claude-opus-4-8");
        assert_eq!(resolve_prompt_cache_ttl_seconds(&Model { cache_retention: Some(CacheRetention::Short), ..opus.clone() }, None), Some(300));
        assert_eq!(resolve_prompt_cache_ttl_seconds(&opus, Some(&long_env())), Some(3600));
        assert_eq!(resolve_prompt_cache_ttl_seconds(&Model { cache_retention: Some(CacheRetention::None), ..opus.clone() }, None), None);
        let proxied = Model { base_url: "https://proxy.example".into(), ..opus.clone() };
        assert_eq!(resolve_prompt_cache_ttl_seconds(&proxied, Some(&long_env())), Some(300));
        let bedrock = Model { api: "bedrock-converse-stream".into(), id: "us.anthropic.claude-haiku-4-5".into(), name: "Haiku".into(), compat: None, ..opus.clone() };
        assert_eq!(resolve_prompt_cache_ttl_seconds(&bedrock, Some(&long_env())), Some(3600));
        let titan = Model { id: "amazon.titan".into(), name: "Titan".into(), ..bedrock };
        assert_eq!(resolve_prompt_cache_ttl_seconds(&titan, Some(&long_env())), None);
        assert_eq!(resolve_prompt_cache_ttl_seconds(&Model { api: "google-generative-ai".into(), ..opus }, None), None);
        assert_eq!(get_bedrock_model_match_candidates("A.B:c", Some("X y")), ["a.b:c", "a-b-c", "x y", "x-y"]);
    }
}
