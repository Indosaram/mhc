//! Port of senpi packages/ai/src/openai-responses-compat.ts.

use serde::{Deserialize, Serialize};

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
    #[serde(default, rename = "supportsRemoteCompactionV2", skip_serializing_if = "Option::is_none")]
    pub supports_remote_compaction_v2: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_web_search_preview: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_image_generation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    #[serde(default, rename = "supportsOpenAIGrammarTools", skip_serializing_if = "Option::is_none")]
    pub supports_openai_grammar_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_explicit_prompt_cache_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_max_output_tokens: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_additional_tools: Option<bool>,
}
