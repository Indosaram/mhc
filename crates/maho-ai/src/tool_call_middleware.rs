//! Port of senpi packages/ai/src/tool-call-middleware/index.ts.

pub mod context_transformer;
pub mod protocols;
pub mod recovery_code_mask;
pub mod recovery_content_lifecycle;
pub mod recovery_diagnostics;
pub mod recovery_event_stream;
pub mod recovery_message_snapshot;
pub mod recovery_native_projection;
pub mod recovery_stream_failure;
pub mod recovery_stream_terminal;
pub mod recovery_stream_wrapper;
pub mod recovery_text_projection;
pub mod stream_message_metadata;
pub mod stream_thinking_projection;
pub mod stream_wrapper;
pub mod stream_wrapper_shared;
pub mod types;

use std::sync::LazyLock;

use regex::Regex;

use crate::model::Model;
use crate::types::{AssistantMessageEventStream, Tool};
use protocols::kimi_xtml::recovery_stream::create_xtml_recovery_stream_parser;
use protocols::kimi_xtml::thinking_recovery_stream::wrap_stream_with_kimi_thinking_recovery;
use recovery_stream_wrapper::{wrap_stream_with_invoke_recovery, InvokeRecoveryOptions};
use types::ToolCallFormat;

pub use context_transformer::{get_protocol, transform_context};
pub use stream_wrapper::wrap_stream_with_tool_call_middleware;

/// The Claude-family identifier pattern (case-insensitive, ASCII-alphanumeric delimited).
static CLAUDE_MODEL_ID_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(^|[^a-z0-9])claude([^a-z0-9]|$)").expect("CLAUDE_MODEL_ID_PATTERN is a valid fixed regex"));

/// The Kimi-family identifier pattern (case-insensitive, ASCII-alphanumeric delimited).
static KIMI_MODEL_ID_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(^|[^a-z0-9])kimi([^a-z0-9]|$)").expect("KIMI_MODEL_ID_PATTERN is a valid fixed regex"));

/// Extracts the tool call format from a model's compatibility settings.
/// Only applies to models using the openai-completions API with compat settings.
/// "morph-xml" is canonical; "xml" remains a deprecated alias.
pub fn get_tool_call_format(model: &Model) -> Option<ToolCallFormat> {
    if model.api != "openai-completions" {
        return None;
    }
    let compat = model.compat.as_ref()?;
    let format = compat.openai_completions().tool_call_format?;
    match format.as_str() {
        "hermes" => Some(ToolCallFormat::Hermes),
        "xml" => Some(ToolCallFormat::Xml),
        "morph-xml" => Some(ToolCallFormat::MorphXml),
        "yaml-xml" => Some(ToolCallFormat::YamlXml),
        "gemma4-delimiter" => Some(ToolCallFormat::Gemma4Delimiter),
        "anthropic-xml" => Some(ToolCallFormat::AnthropicXml),
        "antml" => Some(ToolCallFormat::Antml),
        "kimi-xtml" => Some(ToolCallFormat::KimiXtml),
        _ => None,
    }
}

pub fn should_recover_text_tool_calls(model: &Model) -> bool {
    if get_tool_call_format(model).is_some() {
        return false;
    }
    if let Some(recover) = model.recover_text_tool_calls {
        return recover;
    }
    if model.api == "cursor-agent" {
        return false;
    }
    CLAUDE_MODEL_ID_PATTERN.is_match(&model.id) || KIMI_MODEL_ID_PATTERN.is_match(&model.id)
}

/// Whether the model leaks Kimi XTML channel markers when tool calling fails,
/// selecting the XTML recovery parser over the default invoke recovery parser.
pub fn has_kimi_text_tool_call_recovery(model: &Model) -> bool {
    KIMI_MODEL_ID_PATTERN.is_match(&model.id)
}

pub fn wrap_stream_with_model_recovery(
    inner_stream: AssistantMessageEventStream,
    model: &Model,
    tools: &[Tool],
) -> AssistantMessageEventStream {
    let recovered_stream = if has_kimi_text_tool_call_recovery(model) {
        wrap_stream_with_kimi_thinking_recovery(inner_stream)
    } else {
        inner_stream
    };
    if !should_recover_text_tool_calls(model) || tools.is_empty() {
        return recovered_stream;
    }
    let options = if has_kimi_text_tool_call_recovery(model) {
        InvokeRecoveryOptions {
            create_parser: Some(std::sync::Arc::new(|tools| create_xtml_recovery_stream_parser(tools, None))),
            protocol: ToolCallFormat::KimiXtml,
        }
    } else {
        InvokeRecoveryOptions::default()
    };
    wrap_stream_with_invoke_recovery(recovered_stream, tools.to_vec(), options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{InputModality, ModelCost};
    use serde_json::{json, Map, Value};

    fn create_model(id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: "openai-completions".to_string(),
            provider: "test-provider".to_string(),
            base_url: "https://example.test/v1".to_string(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![InputModality::Text],
            cost: ModelCost::default(),
            context_window: 128000,
            max_tokens: 8192,
            sampling_params: None,
            headers: None,
            cache_retention: None,
            upstream_model_id: None,
            service_tier: None,
            recover_text_tool_calls: None,
            compat: None,
        }
    }

    fn with_compat(mut model: Model, format: &str) -> Model {
        let mut compat = Map::new();
        compat.insert("toolCallFormat".to_string(), Value::String(format.to_string()));
        model.compat = Some(crate::model::ModelCompat(compat));
        model
    }

    #[test]
    fn does_not_activate_antml_recovery_on_cursor_agent_claude_ids() {
        let mut cursor_fable = create_model("");
        cursor_fable.api = "cursor-agent".to_string();
        cursor_fable.provider = "cursor".to_string();
        cursor_fable.base_url = "https://api2.cursor.sh".to_string();
        cursor_fable.reasoning = true;
        cursor_fable.context_window = 200000;
        assert!(!should_recover_text_tool_calls(&cursor_fable));
        let mut overridden = cursor_fable.clone();
        overridden.recover_text_tool_calls = Some(true);
        assert!(should_recover_text_tool_calls(&overridden));
    }

    #[test]
    fn matches_the_locked_claude_family_identifiers_across_apis() {
        for id in [
            "claude-opus-4-8",
            "anthropic/claude-fable-5",
            "anthropic.claude-3-5-sonnet-20241022",
            "claude-sonnet@2025-01-01",
            "claude",
            "anthropic/claude-opus",
            "x-claude",
            "claude_opus",
            "CLAUDE.SONNET@2025",
            "\u{e9}claude",
            "claude\u{e9}",
        ] {
            assert!(should_recover_text_tool_calls(&create_model(id)), "{id}");
        }

        for id in ["exclaude", "claudius", "claudel", "myclaude", "claude3", "xclaude", "claudex", "gpt-5"] {
            assert!(!should_recover_text_tool_calls(&create_model(id)), "{id}");
        }

        let mut different_api = create_model("claude");
        different_api.api = "anthropic-messages".to_string();
        different_api.provider = "another-provider".to_string();
        assert!(should_recover_text_tool_calls(&different_api));
    }

    #[test]
    fn rejects_substring_false_positives_and_gives_text_protocol_mutual_exclusion_precedence() {
        for format in ["hermes", "xml", "morph-xml", "yaml-xml", "gemma4-delimiter", "anthropic-xml", "antml", "kimi-xtml"] {
            let mut model = with_compat(create_model(""), format);
            model.recover_text_tool_calls = Some(true);
            assert_eq!(get_tool_call_format(&model).map(|parsed| parsed.as_str()), Some(format));
            assert!(!should_recover_text_tool_calls(&model), "{format}");
        }

        let mut disabled = create_model("");
        disabled.recover_text_tool_calls = Some(false);
        assert!(!should_recover_text_tool_calls(&disabled));

        let mut enabled = create_model("gpt-5");
        enabled.recover_text_tool_calls = Some(true);
        assert!(should_recover_text_tool_calls(&enabled));

        let mut unrelated_provider = create_model("claude");
        unrelated_provider.provider = "unrelated-provider".to_string();
        assert!(should_recover_text_tool_calls(&unrelated_provider));

        // The TS case also feeds non-boolean recoverTextToolCalls values (null, "false", 0, "true", 1);
        // Rust's Option<bool> cannot represent them, so each collapses to None and the outcome is the
        // same false the TS asserts for those malformed inputs.
        assert!(!should_recover_text_tool_calls(&create_model("")));
    }

    #[test]
    fn exports_the_activation_helper_from_the_side_effect_free_root() {
        // The TS case inspects the module graph (no compat.ts imports, no added globals) through a
        // node subprocess; Rust has no equivalent. The observable contract ported here is that the
        // helper is reachable from the crate root.
        assert!(crate::should_recover_text_tool_calls(&create_model("claude")));
    }

    #[test]
    fn defaults_recovery_on_for_kimi_family_identifiers_like_the_claude_family() {
        for id in [
            "kimi-k3",
            "kimi-k3-ultrafast",
            "kimi-k3-256k",
            "kimi-k2-thinking",
            "kimi-for-coding-highspeed",
            "moonshot/kimi-k3",
            "apitopia/kimi-k3-unlocked",
            "x-kimi",
            "KIMI-K3",
        ] {
            assert!(should_recover_text_tool_calls(&create_model(id)), "{id}");
        }
    }

    #[test]
    fn rejects_substring_false_positives_for_kimi() {
        for id in ["kimiko", "sikimi", "kimi3", "grokimi", "kimiai"] {
            assert!(!should_recover_text_tool_calls(&create_model(id)), "{id}");
        }
    }

    #[test]
    fn keeps_text_protocol_mutual_exclusion_precedence_for_kimi_xtml() {
        let mut model = with_compat(create_model("kimi-k3"), "kimi-xtml");
        model.recover_text_tool_calls = Some(true);
        assert_eq!(get_tool_call_format(&model), Some(ToolCallFormat::KimiXtml));
        assert!(!should_recover_text_tool_calls(&model));
    }

    #[test]
    fn honors_the_explicit_recover_text_tool_calls_override_on_kimi_models() {
        let mut disabled = create_model("kimi-k3");
        disabled.recover_text_tool_calls = Some(false);
        assert!(!should_recover_text_tool_calls(&disabled));

        let mut enabled = create_model("gpt-5");
        enabled.recover_text_tool_calls = Some(true);
        assert!(should_recover_text_tool_calls(&enabled));
    }

    #[test]
    fn has_kimi_text_tool_call_recovery_follows_the_kimi_family_pattern() {
        assert!(has_kimi_text_tool_call_recovery(&create_model("kimi-k3")));
        assert!(!has_kimi_text_tool_call_recovery(&create_model("claude")));
        let _ = json!(null);
    }
}

