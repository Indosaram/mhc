use maho_ext_api::Model;
use maho_ext_ask_user::{family::{pick_variant, tool_name}, schema::AskUserVariant};

fn model(api: &str, id: &str) -> Model {
    Model { id: id.into(), name: id.into(), api: api.into(), provider: "custom-gateway".into(), base_url: "https://example.invalid".into(), reasoning: false, thinking_level_map: None, input: vec![], cost: Default::default(), context_window: 100, max_tokens: 10, sampling_params: None, headers: None, cache_retention: None, upstream_model_id: None, service_tier: None, recover_text_tool_calls: None, compat: None }
}

#[test]
fn wire_family_uses_api_and_boundary_aware_id_not_provider() {
    for api in ["openai-responses", "azure-openai-responses", "openai-codex-responses", "openai-completions"] {
        for id in ["gpt-6-sol", "codex/gpt-6-astra", "global.openai.gpt-6-astra", "gateway:GPT_6_ASTRA", "gpt5"] {
            assert_eq!(pick_variant(Some(&model(api, id))), AskUserVariant::Codex, "{api}/{id}");
        }
        for id in ["xgpt-5.6-proxy", "deepseek-v3-gptq", "gpt", "claude-fable-5-1"] {
            assert_eq!(pick_variant(Some(&model(api, id))), AskUserVariant::Claude, "{api}/{id}");
        }
    }
    for api in ["anthropic-messages", "google-generative-ai", "unknown"] {
        assert_eq!(pick_variant(Some(&model(api, "gpt-6-sol"))), AskUserVariant::Claude);
    }
    assert_eq!(pick_variant(None), AskUserVariant::Claude);
    assert_eq!(tool_name(AskUserVariant::Codex), "request_user_input");
    assert_eq!(tool_name(AskUserVariant::Claude), "ask_user_question");
}
