use maho_ext_anthropic_subscription::{options::tool_options, settings};
use serde_json::{Value, json};

#[test]
fn normal_tool_request_keeps_selected_tools_without_turn_cap() {
    let settings = settings::load(&json!({}), &Value::Null, &Default::default());
    let options = tool_options(&settings, false, false, Some(&["mcp__custom-tools__lookup".into()]));
    assert_eq!(options["tools"], json!(["mcp__custom-tools__lookup"]));
    assert!(!options.contains_key("maxTurns"));
}

#[test]
fn explicit_strict_mcp_config_applies_to_normal_streams() {
    let settings = settings::load(&json!({"anthropicSubscriptionProvider":{"strictMcpConfig":true}}), &Value::Null, &Default::default());
    let options = tool_options(&settings, false, false, None);
    assert_eq!(options["extraArgs"], json!({"strict-mcp-config":null}));
}
