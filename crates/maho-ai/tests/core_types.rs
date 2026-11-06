//! Port of senpi packages/ai/test/types.provider-native.test.ts.

use maho_ai::types::{
    AssistantMessage, ContentBlock, ProviderNativeContent, StopReason, TextContent, Usage,
};
use serde_json::json;

#[test]
fn provider_native_content_allows_provider_native_blocks_in_assistant_message_content() {
    let provider_native_block = ContentBlock::ProviderNative(ProviderNativeContent {
        subtype: "server_tool_use".to_owned(),
        raw: json!({
            "id": "srvtoolu_xx",
            "name": "web_search",
            "input": { "query": "test" },
        }),
    });

    let message = AssistantMessage {
        timing: None,
        failure_source: None,
        credential_served_recently: None,
        thinking_level: None,
        provider_diagnostic: None,
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: None,
        error_cause: None,
        content: vec![
            ContentBlock::Text(TextContent {
                text: "native block follows".to_owned(),
                ..Default::default()
            }),
            provider_native_block.clone(),
        ],
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        model: "claude-sonnet-4".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    };

    assert_eq!(message.content.len(), 2);
    assert_eq!(message.content[1], provider_native_block);
}

// senpi v2026.10.27-2 types.ts: `credentialServedRecently?: true` and the `model_config` errorCause
// round-trip under their TS keys; `ProviderRequestOptions.cwd` is a plain optional field.
#[test]
fn credential_served_recently_and_model_config_cause_round_trip_under_ts_keys() {
    let mut message: AssistantMessage = serde_json::from_value(json!({
        "role": "assistant", "content": [], "api": "faux", "provider": "faux", "model": "m",
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
        "stopReason": "error", "timestamp": 0,
        "errorCause": { "type": "model_config", "model": "faux/m" },
        "credentialServedRecently": true
    }))
    .expect("assistant message");
    assert_eq!(message.credential_served_recently, Some(true));
    assert_eq!(
        message.error_cause,
        Some(maho_ai::types::AssistantErrorCause::ModelConfig {
            model: "faux/m".into()
        })
    );
    let value = serde_json::to_value(&message).expect("serialize");
    assert_eq!(value["credentialServedRecently"], json!(true));
    assert_eq!(
        value["errorCause"],
        json!({ "type": "model_config", "model": "faux/m" })
    );
    message.credential_served_recently = None;
    let value = serde_json::to_value(&message).expect("serialize");
    assert!(value.get("credentialServedRecently").is_none());

    let options = maho_ai::types::ProviderRequestOptions {
        cwd: Some("/work/session".into()),
        ..Default::default()
    };
    assert_eq!(options.cwd.as_deref(), Some("/work/session"));
}
