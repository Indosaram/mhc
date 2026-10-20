//! Port of senpi packages/ai/src/openai-responses-compat.ts.

use serde::{Deserialize, Serialize};

#[path = "api/openai_responses_allowed_tools.rs"]
pub mod allowed_tools;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionAffinityFormat {
    Openai,
    OpenaiNosession,
    Openrouter,
}

/// `OpenAIResponsesCompat` (types.ts widens it with `supportsAdditionalTools`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAIResponsesCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_web_socket: Option<bool>,
    #[serde(
        default,
        rename = "supportsRemoteCompactionV2",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_remote_compaction_v2: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_web_search_preview: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search_tool_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_image_generation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    #[serde(
        default,
        rename = "supportsOpenAIGrammarTools",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_openai_grammar_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_explicit_prompt_cache_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_max_output_tokens: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_additional_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_allowed_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_forced_tool_choice: Option<bool>,
}

pub fn supports_allowed_tool_choice(model: &crate::types::Model) -> bool {
    if !model
        .compat
        .as_ref()
        .is_some_and(|compat| compat.openai_responses().supports_allowed_tools == Some(true))
    {
        return false;
    }
    let base_url = if model.base_url.is_empty() {
        "https://api.openai.com/v1"
    } else {
        &model.base_url
    };
    url::Url::parse(base_url).ok().is_some_and(|url| {
        url.host_str()
            .is_some_and(|host| host == "api.openai.com" || host.ends_with(".api.openai.com"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn allowed_choice_requires_opt_in_and_native_host() {
        let mut model = crate::models_generated::get_builtin_model("anthropic", "claude-opus-4-8")
            .expect("model")
            .clone();
        model.compat =
            Some(serde_json::from_value(json!({"supportsAllowedTools": true})).expect("compat"));
        for host in [
            "",
            "https://api.openai.com/v1",
            "https://eu.api.openai.com/v1",
        ] {
            model.base_url = host.into();
            assert!(supports_allowed_tool_choice(&model));
        }
        for host in [
            "https://api.openai.com.evil.test/v1",
            "https://gateway.test/v1",
            "not a URL",
        ] {
            model.base_url = host.into();
            assert!(!supports_allowed_tool_choice(&model));
        }
        model.base_url = "https://api.openai.com/v1".into();
        model.compat = None;
        assert!(!supports_allowed_tool_choice(&model));
    }
}
