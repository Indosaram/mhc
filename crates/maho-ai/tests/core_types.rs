//! Port of senpi packages/ai/test/types.provider-native.test.ts.

use maho_ai::types::{AssistantMessage, ContentBlock, ProviderNativeContent, StopReason, TextContent, Usage};
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
        content: vec![
            ContentBlock::Text(TextContent { text: "native block follows".to_owned(), ..Default::default() }),
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
